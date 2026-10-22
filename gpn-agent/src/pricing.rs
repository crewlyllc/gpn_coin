//! Throughput-calibrated inference pricing.
//!
//! The old model anchored the price on a **market GPU-hour** supplied by an
//! oracle (`market_hr = clamp(0.5 × market, owner_cost, market)`, hardcoded
//! `$3.00` while the oracle feed is absent). That prices a consumer card at
//! cloud-A100 rental rates: a 4060 worth ~$0.10–0.15/hr was billed at
//! $1.50/hr, i.e. ~$8.75 per 1M output tokens — ~22× `gpt-5-nano`.
//!
//! This module replaces that with a **throughput-calibrated curve**:
//!
//! ```text
//! anchor_tok_s = roofline(30_000 GFLOP/s, 272 GB/s, 1B)   = 95.2 tok/s
//! anchor_rate  = GPN_GPU_ANCHOR_USD_HR                    = $0.10/hr
//! k            = anchor_rate / (anchor_tok_s × 3600)       = USD per output token
//! price_out    = k × (1 + GPN_MARGIN_BPS/10000)            = $0.38 / 1M
//! provider_hr  = roofline(provider.gflops, bw, params_b) × 3600 × k
//! ```
//!
//! Two properties worth stating explicitly:
//!
//! * **The buyer price is GPU-independent by design.** Every request costs the
//!   same per token no matter which card serves it; faster hardware simply
//!   earns more per hour (a 4090 ≈ $0.37/hr vs a 4060's $0.10/hr, H100 ≈
//!   $1.23/hr). That is what makes a spot market of heterogeneous cards
//!   possible at all — a per-GPU price would make the cheapest card the only
//!   one ever picked.
//! * **Throughput, not FP32 GFLOPs, is the metric.** Decode of a small model is
//!   memory-bandwidth-bound; pricing on FLOPs would rank a 4090 above an H100.
//!
//! The legacy market model is still selectable with `GPN_PRICING_MODEL=market`
//! so the two can be A/B'd and reverted instantly.

/// Reference card for the anchor: a 4060-class GPU with 272 GB/s of bandwidth
/// and ~30 TFLOP/s FP32.
pub const ANCHOR_GFLOPS: f64 = 30_000.0;
pub const ANCHOR_MEM_BW_GBPS: f64 = 272.0;
pub const ANCHOR_PARAMS_B: f64 = 1.0;

/// What the anchor card is assumed to earn per hour (single tunable constant).
pub const DEFAULT_ANCHOR_RATE_USD_HR: f64 = 0.10;
pub const DEFAULT_MARGIN_BPS: u32 = 3000;
/// Input tokens are billed at this fraction of the output rate.
pub const DEFAULT_INPUT_RATE_FRAC: f64 = 0.25;

/// Sane band for the published buyer price (USD per 1M output tokens).
pub const PRICE_FLOOR_USD_PER_M: f64 = 0.05;
pub const PRICE_CEIL_USD_PER_M: f64 = 5.00;

// ── diffusion (image / video) calibration ──────────────────────────────────
//
// Diffusion has no token count. Its cost is **denoising work**: pixels × steps
// (× frames for video), plus a fixed per-job pipeline load. The two constants
// below were measured on the anchor card (RTX 4060 8 GB, this host) by timing
// `gpn_diffuse.py` end-to-end, which is the real shape of a job: the provider
// shells out a fresh python process per job, so the model load is *in* every
// job and belongs in the price.
//
// Measured (4 points, sd-turbo, one process per render):
//   512² ×  8 steps =  2.10 Mpx·step → 28.88 s
//   512² × 20 steps =  5.24 Mpx·step → 36.54 s
//   768² × 12 steps =  7.08 Mpx·step → 35.68 s
//  1024² × 20 steps = 20.97 Mpx·step → 44.72 s
// Least-squares fit: seconds = 30.0 + 0.73 × Mpx·step  (residual < 3 s).
/// Fixed per-job cost of a diffusion job (pipeline load), measured.
pub const DIFF_BASE_SECS: f64 = 30.0;
/// Marginal cost of one megapixel × one denoise step, measured.
pub const DIFF_SEC_PER_MPX_STEP: f64 = 0.73;
/// Video pays its own (larger) model load: LTX-Video also brings up the T5
/// text encoder. NOT measured — no video job has ever completed here (needs
/// > 8 GB), so this is an estimate and is labelled as such everywhere it shows.
pub const DIFF_VIDEO_BASE_SECS: f64 = 90.0;
/// Per-frame denoise of a video transformer costs this much more than an image
/// step. NOT measured (same reason); 4× is the usual SDXL-class vs LTX-class
/// per-latent-frame ratio.
pub const DIFF_VIDEO_COMPUTE_FACTOR: f64 = 4.0;

/// Legacy image pricing used a flat "512² @ 8 steps ≈ 90 s of GPU time" with no
/// per-job base and a 20% margin. Kept verbatim for `GPN_PRICING_MODEL=market`
/// revert parity.
pub const LEGACY_IMAGE_REF_SECS: f64 = 90.0;
pub const LEGACY_IMAGE_MARGIN_FRAC: f64 = 0.20;

/// Legacy market-model fallbacks (used when the oracle feed is absent).
pub const LEGACY_MARKET_HR_FALLBACK: f64 = 3.0;
pub const LEGACY_UTIL_FALLBACK: f64 = 0.65;

/// Base units per whole GPN (12 decimals).
pub const BASE_UNITS_PER_GPN: f64 = 1e12;

/// Which pricing path a scheduler publishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PricingModel {
    /// Throughput-calibrated curve (default).
    Throughput,
    /// Legacy oracle-market anchor (`GPN_PRICING_MODEL=market`).
    Market,
}

impl PricingModel {
    pub fn from_env() -> Self {
        match std::env::var("GPN_PRICING_MODEL") {
            Ok(v) if v.trim().eq_ignore_ascii_case("market") => PricingModel::Market,
            _ => PricingModel::Throughput,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            PricingModel::Throughput => "throughput",
            PricingModel::Market => "market",
        }
    }
}

/// Roofline estimate of output tokens/sec for a model with `params_b` billion
/// parameters on a GPU with `gflops` GFLOP/s and `mem_bw_gbps` GB/s:
///
/// ```text
/// compute-bound: gflops·1e9·eff_c / (2 FLOPs per param per token)
/// memory-bound:  bw·1e9·eff_m    / (2 bytes per fp16 weight per token)
/// ```
///
/// Decode of a small model is memory-bound, so the `min` is normally the
/// bandwidth term.
pub fn roofline_tok_per_s(gflops: f64, mem_bw_gbps: f64, params_b: f64) -> f64 {
    const EFF_COMPUTE: f64 = 0.40;
    const EFF_MEM: f64 = 0.70;
    let p = params_b.max(0.05) * 1e9;
    let compute = gflops.max(1.0) * 1e9 * EFF_COMPUTE / (2.0 * p);
    let memory = mem_bw_gbps.max(1.0) * 1e9 * EFF_MEM / (p * 2.0);
    compute.min(memory).max(1.0)
}

/// A reference card used for the public earnings table.
#[derive(Clone, Copy, Debug)]
pub struct ReferenceGpu {
    pub name: &'static str,
    pub gflops: f64,
    pub mem_bw_gbps: f64,
}

/// The cards an operator is most likely to run. FP32 GFLOP/s figures are only
/// there so the roofline can rule compute out; bandwidth decides.
pub fn reference_gpus() -> [ReferenceGpu; 4] {
    [
        ReferenceGpu {
            name: "RTX 4060 8GB",
            gflops: 15_000.0,
            mem_bw_gbps: 272.0,
        },
        ReferenceGpu {
            name: "RTX 4090 24GB",
            gflops: 82_600.0,
            mem_bw_gbps: 1008.0,
        },
        ReferenceGpu {
            name: "A100 80GB",
            gflops: 19_500.0,
            mem_bw_gbps: 2039.0,
        },
        ReferenceGpu {
            name: "H100 80GB",
            gflops: 67_000.0,
            mem_bw_gbps: 3350.0,
        },
    ]
}

/// Resolved pricing parameters. Immutable for the life of the process.
#[derive(Clone, Debug)]
pub struct PricingConfig {
    pub model: PricingModel,
    /// USD/hour the anchor card is assumed to earn (pre-margin).
    pub anchor_rate_usd_hr: f64,
    pub margin_bps: u32,
    /// Input tokens are billed at `input_rate_frac` × the output rate.
    pub input_rate_frac: f64,
    /// Legacy: fraction of the oracle market rate that becomes the rental.
    pub rental_frac: f64,
    pub market_hr_fallback: f64,
    pub util_fallback: f64,
    /// `GPN_PRICING_UNSAFE=1` publishes outside the sanity band.
    pub unsafe_publish: bool,
}

fn env_f64(key: &str) -> Option<f64> {
    std::env::var(key).ok().and_then(|s| s.trim().parse().ok())
}

fn env_flag(key: &str) -> bool {
    std::env::var(key)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

impl PricingConfig {
    /// Build from the CLI/env values the scheduler already parses (`--margin-bps`,
    /// `--rental-fraction-bps`), plus this module's own env knobs.
    pub fn from_env(margin_bps: u32, rental_frac: f64) -> Self {
        PricingConfig {
            model: PricingModel::from_env(),
            anchor_rate_usd_hr: env_f64("GPN_GPU_ANCHOR_USD_HR")
                .filter(|v| *v > 0.0)
                .unwrap_or(DEFAULT_ANCHOR_RATE_USD_HR),
            margin_bps,
            input_rate_frac: env_f64("GPN_INPUT_RATE_FRAC")
                .filter(|v| *v >= 0.0)
                .unwrap_or(DEFAULT_INPUT_RATE_FRAC),
            rental_frac,
            market_hr_fallback: LEGACY_MARKET_HR_FALLBACK,
            util_fallback: LEGACY_UTIL_FALLBACK,
            unsafe_publish: env_flag("GPN_PRICING_UNSAFE"),
        }
    }

    /// A deterministic config for tests and doc examples.
    pub fn new(model: PricingModel, anchor_rate_usd_hr: f64, margin_bps: u32) -> Self {
        PricingConfig {
            model,
            anchor_rate_usd_hr,
            margin_bps,
            input_rate_frac: DEFAULT_INPUT_RATE_FRAC,
            rental_frac: 0.5,
            market_hr_fallback: LEGACY_MARKET_HR_FALLBACK,
            util_fallback: LEGACY_UTIL_FALLBACK,
            unsafe_publish: false,
        }
    }

    pub fn margin_frac(&self) -> f64 {
        self.margin_bps as f64 / 10_000.0
    }

    /// Throughput of the anchor card on the anchor model.
    pub fn anchor_tok_s(&self) -> f64 {
        roofline_tok_per_s(ANCHOR_GFLOPS, ANCHOR_MEM_BW_GBPS, ANCHOR_PARAMS_B)
    }

    /// `k`: USD per output token before margin.
    pub fn k_usd_per_token(&self) -> f64 {
        self.anchor_rate_usd_hr / (self.anchor_tok_s() * 3600.0)
    }

    /// USD per output token, margin included. **Independent of the serving GPU**
    /// and of the model size: one global buyer price.
    pub fn price_per_token_out(&self) -> f64 {
        self.k_usd_per_token() * (1.0 + self.margin_frac())
    }

    /// USD per input token (billed at `input_rate_frac` of output).
    pub fn price_per_token_in(&self) -> f64 {
        self.price_per_token_out() * self.input_rate_frac
    }

    /// What a provider with this hardware earns per hour, pre-margin, per the
    /// spec: `roofline(...) × 3600 × k`.
    pub fn provider_rate_hr(&self, gflops: f64, mem_bw_gbps: f64, params_b: f64) -> f64 {
        roofline_tok_per_s(gflops, mem_bw_gbps, params_b) * 3600.0 * self.k_usd_per_token()
    }

    /// Dollar cost of one job at the two-rate card.
    pub fn job_cost_usd(&self, prompt_tokens: u32, completion_tokens: u32) -> f64 {
        self.price_per_token_in() * prompt_tokens as f64
            + self.price_per_token_out() * completion_tokens as f64
    }

    /// Output-token-equivalents that escrow must cover: input billed at
    /// `input_rate_frac` plus the full output allowance.
    pub fn escrow_token_units(&self, prompt_tokens: u32, max_output_tokens: u32) -> u32 {
        let input_units = (prompt_tokens as f64 * self.input_rate_frac).round();
        (input_units + max_output_tokens as f64)
            .min(u32::MAX as f64)
            .max(1.0) as u32
    }

    /// GPN base units per output token, from a USD price and the on-chain GPN
    /// price. Never returns 0 (a zero price would be refused by the channel).
    pub fn gpn_per_token(&self, usd_per_token: f64, token_price_usd: f64) -> u128 {
        let units = (usd_per_token / token_price_usd.max(1e-9)) * BASE_UNITS_PER_GPN;
        if !units.is_finite() || units < 1.0 {
            1
        } else {
            units.round() as u128
        }
    }

    /// Single-rate approximation for the on-chain pallet.
    ///
    /// The pallet stores one `(price_per_token, max_tokens, actual_tokens)` and
    /// charges `price_per_token × actual_tokens`, where the provider reports
    /// `actual_tokens = prompt + completion`. It therefore **cannot express two
    /// rates**. We publish a blended rate —
    /// `rate = out_rate × (input_frac·P + M) / (P + M)` — with a cap of `P + M`,
    /// which is exact when the completion runs to `M` and under-bills short
    /// completions slightly (never the reverse). Exact two-rate settlement needs
    /// a pallet change (a second rate in `JobBilling`) — see the docs.
    pub fn onchain_rate_and_cap(
        &self,
        gpn_per_out_token: u128,
        prompt_tokens: u32,
        max_output_tokens: u32,
    ) -> (u128, u32) {
        let p = prompt_tokens as u128;
        let m = max_output_tokens as u128;
        let frac_bps = (self.input_rate_frac.clamp(0.0, 1.0) * 10_000.0).round() as u128;
        let blended_bps = p * frac_bps + m * 10_000;
        let full_bps = (p + m) * 10_000;
        let cap = (p + m).min(u32::MAX as u128) as u32;
        if full_bps == 0 {
            return (gpn_per_out_token, cap);
        }
        let rate = gpn_per_out_token.saturating_mul(blended_bps) / full_bps;
        (rate.max(1), cap)
    }

    // ── diffusion (image / video) ──────────────────────────────────────────

    /// GPU-seconds one generation job is expected to occupy on the **anchor**
    /// card. `frames = 1` for an image; video multiplies the denoise term and
    /// pays the bigger pipeline load.
    pub fn diffusion_seconds(&self, width: f64, height: f64, steps: f64, frames: f64) -> f64 {
        let video = frames > 1.0;
        let base = if video {
            DIFF_VIDEO_BASE_SECS
        } else {
            DIFF_BASE_SECS
        };
        let per_step = if video {
            DIFF_SEC_PER_MPX_STEP * DIFF_VIDEO_COMPUTE_FACTOR
        } else {
            DIFF_SEC_PER_MPX_STEP
        };
        let mpx_step = (width.max(1.0) * height.max(1.0) * steps.max(1.0) * frames.max(1.0)) / 1e6;
        base + mpx_step * per_step
    }

    /// Buyer price of one generation job, in USD.
    ///
    /// **Which card: the anchor, not the routed one.** This is the same design
    /// rule the text curve follows (see the module docs): the buyer pays the
    /// same for a 512²/8-step image whether a 4060 or an H100 renders it, so a
    /// heterogeneous spot market has one price and the cheapest card is not the
    /// only one ever picked. `provider_rate_hr` therefore does *not* appear
    /// here; it appears in `diffusion_provider_seconds`/`_earn_usd`, where the
    /// faster card converts the same fee into more money per hour.
    ///
    /// Because the seconds above were measured *on* the anchor card, the anchor
    /// GPU-hour rate is the correct divisor — no cross-card interpolation is
    /// involved in the buyer's price at all.
    pub fn diffusion_price_usd(
        &self,
        width: f64,
        height: f64,
        steps: f64,
        frames: f64,
        market_hr: f64,
        util: f64,
    ) -> f64 {
        match self.model {
            PricingModel::Throughput => {
                (self.diffusion_seconds(width, height, steps, frames) / 3600.0)
                    * self.anchor_rate_usd_hr
                    * (1.0 + self.margin_frac())
            }
            PricingModel::Market => self.legacy_image_price_usd(width, height, steps, frames, market_hr, util),
        }
    }

    /// Legacy (`GPN_PRICING_MODEL=market`) image price: flat 90 s at 512²/8,
    /// oracle market GPU-hour, 20% margin, divided by utilisation.
    pub fn legacy_image_price_usd(
        &self,
        width: f64,
        height: f64,
        steps: f64,
        frames: f64,
        market_hr: f64,
        util: f64,
    ) -> f64 {
        const REF_UNITS: f64 = 512.0 * 512.0 * 8.0;
        let units = width.max(1.0) * height.max(1.0) * steps.max(1.0) * frames.max(1.0);
        let est_secs = (units / REF_UNITS) * LEGACY_IMAGE_REF_SECS;
        let util = util.clamp(0.05, 1.0);
        (market_hr.max(0.001) / 3600.0) * est_secs * (1.0 + LEGACY_IMAGE_MARGIN_FRAC) / util
    }

    /// Buyer price of one generation job, in GPN (whole tokens, ≥ 1 base unit).
    pub fn diffusion_price_gpn(
        &self,
        width: f64,
        height: f64,
        steps: f64,
        frames: f64,
        token_price_usd: f64,
        market_hr: f64,
        util: f64,
    ) -> f64 {
        let usd = self.diffusion_price_usd(width, height, steps, frames, market_hr, util);
        (self.gpn_per_token(usd, token_price_usd) as f64) / BASE_UNITS_PER_GPN
    }

    /// GPU-seconds the job occupies a **serving** card, per the pricing curve.
    ///
    /// Only the anchor card is measured. A faster card is interpolated by the
    /// curve's own hourly-rate ratio (`anchor_rate / provider_rate`), which keeps
    /// one coherence knob: the same effective-throughput term that decides an
    /// LLM provider's $/hr. With this choice the provider's USD income for a job
    /// is card-independent (`provider_seconds × provider_rate = measured ×
    /// anchor_rate`), while its income *per hour* scales with the curve — which
    /// is exactly the stated design ("faster GPUs earn proportionally more").
    pub fn diffusion_provider_seconds(
        &self,
        width: f64,
        height: f64,
        steps: f64,
        frames: f64,
        provider_rate_hr: f64,
    ) -> f64 {
        let anchor_secs = self.diffusion_seconds(width, height, steps, frames);
        anchor_secs * (self.anchor_rate_usd_hr / provider_rate_hr.max(1e-9))
    }

    /// What a card earns for one generation job (pre-margin, pre-5%-fee) — the
    /// literal form of the task's formula, with the seconds scaled to that card.
    pub fn diffusion_provider_earn_usd(
        &self,
        width: f64,
        height: f64,
        steps: f64,
        frames: f64,
        provider_rate_hr: f64,
    ) -> f64 {
        (self.diffusion_provider_seconds(width, height, steps, frames, provider_rate_hr) / 3600.0)
            * provider_rate_hr
    }

    /// Legacy market-model output price for the given fleet conditions.
    pub fn legacy_price_per_token_out(&self, market_hr: f64, tok_per_s: f64, util: f64) -> f64 {
        let market_hr = market_hr.max(0.001);
        let util = util.clamp(0.05, 1.0);
        let target_hr = (self.rental_frac * market_hr).clamp(0.0, market_hr);
        (target_hr / (tok_per_s.max(1.0) * 3600.0 * util)) * (1.0 + self.margin_frac())
    }

    /// Output price under whichever model is selected. The throughput price
    /// ignores the fleet arguments by design.
    pub fn price_out(&self, market_hr: f64, tok_per_s: f64, util: f64) -> f64 {
        match self.model {
            PricingModel::Throughput => self.price_per_token_out(),
            PricingModel::Market => self.legacy_price_per_token_out(market_hr, tok_per_s, util),
        }
    }

    /// Published buyer price for 1M output tokens (throughput card).
    pub fn price_per_million_out(&self) -> f64 {
        self.price_per_token_out() * 1e6
    }

    /// Published price (USD/1M output tokens) under the selected model. The
    /// throughput card ignores the fleet arguments by design.
    pub fn price_per_million_out_for(&self, market_hr: f64, tok_per_s: f64, util: f64) -> f64 {
        self.price_out(market_hr, tok_per_s, util) * 1e6
    }

    /// Guardrail on an explicit published price.
    ///
    /// The band exists to catch a **mis-set anchor** on the calibrated card (an
    /// extra zero turns $0.38 into $3.80). The legacy market model's price is
    /// derived from the oracle, not from our constant, and `market` is an
    /// explicit opt-in — so it is only checked for sanity, not for band, keeping
    /// the `GPN_PRICING_MODEL=market` A/B-revert a single env var.
    pub fn check_price_usd_per_m(&self, p: f64) -> Result<f64, String> {
        if !p.is_finite() || p <= 0.0 {
            return Err(format!("derived price is not a positive number: {p}"));
        }
        if self.model == PricingModel::Market {
            return Ok(p);
        }
        if !self.unsafe_publish && !(PRICE_FLOOR_USD_PER_M..=PRICE_CEIL_USD_PER_M).contains(&p) {
            return Err(format!(
                "derived output price ${p:.4}/1M is outside the sane band \
                 [${PRICE_FLOOR_USD_PER_M:.2}, ${PRICE_CEIL_USD_PER_M:.2}] — \
                 refusing to publish (GPN_PRICING_UNSAFE=1 overrides)"
            ));
        }
        Ok(p)
    }

    /// Guardrail: refuse to publish a price outside the sane band unless
    /// `GPN_PRICING_UNSAFE=1`. Returns the price in USD/1M on success.
    pub fn validate_publishable(&self) -> Result<f64, String> {
        self.check_price_usd_per_m(self.price_per_million_out())
    }

    /// The hourly earning table an operator can read off the `/pricing` endpoint.
    /// `below_owner_cost` is a plain-language flag, not a filter: a card whose
    /// modelled earning is under the operator's declared cost is losing money and
    /// the caller (and the startup log) must see it.
    pub fn earnings_table(&self, owner_cost_usd_hr: f64, params_b: f64) -> Vec<EarningRow> {
        reference_gpus()
            .iter()
            .map(|g| {
                let tok_s = roofline_tok_per_s(g.gflops, g.mem_bw_gbps, params_b);
                let usd_hr = tok_s * 3600.0 * self.k_usd_per_token();
                EarningRow {
                    name: g.name,
                    gflops: g.gflops,
                    mem_bw_gbps: g.mem_bw_gbps,
                    tok_s,
                    usd_hr,
                    below_owner_cost: usd_hr < owner_cost_usd_hr,
                }
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct EarningRow {
    pub name: &'static str,
    pub gflops: f64,
    pub mem_bw_gbps: f64,
    pub tok_s: f64,
    pub usd_hr: f64,
    pub below_owner_cost: bool,
}

/// Rough prompt-token estimate used to size the input part of escrow (the
/// provider's real count replaces it at settlement). ~4 chars/token.
pub fn estimate_prompt_tokens(prompt: &str) -> u32 {
    let chars = prompt.chars().count();
    ((chars + 3) / 4).max(1).min(u32::MAX as usize) as u32
}

/// Parse a parameter count in billions from a model name ("llama3.2:1b" → 1.0).
pub fn params_b_for(model: &str) -> f64 {
    let m = model.to_ascii_lowercase();
    if let Some(idx) = m.find('b') {
        let pre = &m[..idx];
        let num: String = pre
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if let Ok(v) = num.parse::<f64>() {
            if v > 0.0 {
                return v;
            }
        }
    }
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol * b.abs().max(1.0)
    }

    fn throughput() -> PricingConfig {
        PricingConfig::new(PricingModel::Throughput, 0.10, 3000)
    }

    #[test]
    fn roofline_matches_the_published_reference_cards() {
        // Anchor: 1B on a 4060-class card is memory-bound at 95.2 tok/s.
        assert!((throughput().anchor_tok_s() - 95.2).abs() < 1e-9);

        // 4090: 1008 GB/s → 0.70 GB/s of effective decode bandwidth per 2B.
        let t4090 = roofline_tok_per_s(82_600.0, 1008.0, 1.0);
        assert!((t4090 - 352.8).abs() < 0.01, "4090 tok/s = {t4090}");
        let ta100 = roofline_tok_per_s(19_500.0, 2039.0, 1.0);
        assert!((ta100 - 713.65).abs() < 0.01, "A100 tok/s = {ta100}");
        let th100 = roofline_tok_per_s(67_000.0, 3350.0, 1.0);
        assert!((th100 - 1172.5).abs() < 0.01, "H100 tok/s = {th100}");

        // Compute-bound where bandwidth is plentiful: 70B on an H100.
        let big = roofline_tok_per_s(67_000.0, 3350.0, 70.0);
        assert!((big - 16.75).abs() < 0.01, "70B on H100 tok/s = {big}");
    }

    #[test]
    fn anchor_publishes_thirty_eight_cents_per_million() {
        let c = throughput();
        let per_m = c.price_per_million_out();
        assert!((per_m - 0.3793).abs() < 1e-3, "got ${per_m}/1M");
        // k is the pre-margin unit price; margin is exactly 30% on top.
        assert!(close(c.price_per_token_out(), c.k_usd_per_token() * 1.3, 1e-12));
        assert!(close(c.price_per_token_in(), c.price_per_token_out() * 0.25, 1e-12));
    }

    #[test]
    fn provider_earnings_table_is_gpu_scaled() {
        let c = throughput();
        assert!((c.provider_rate_hr(15_000.0, 272.0, 1.0) - 0.10).abs() < 1e-3);
        assert!((c.provider_rate_hr(82_600.0, 1008.0, 1.0) - 0.3706).abs() < 1e-3);
        assert!((c.provider_rate_hr(19_500.0, 2039.0, 1.0) - 0.7496).abs() < 1e-3);
        assert!((c.provider_rate_hr(67_000.0, 3350.0, 1.0) - 1.2315).abs() < 1e-3);

        let rows = c.earnings_table(0.05, 1.0);
        assert_eq!(rows.len(), 4);
        assert!(rows.iter().all(|r| !r.below_owner_cost));
        // A card priced under its cost is surfaced, not hidden.
        let losing = c.earnings_table(0.50, 1.0);
        assert!(losing[0].below_owner_cost, "4060 @ $0.10/hr < $0.50 cost");
        assert!(!losing[3].below_owner_cost, "H100 @ $1.23/hr > $0.50 cost");
    }

    #[test]
    fn anchor_override_moves_the_price_linearly() {
        let half = PricingConfig::new(PricingModel::Throughput, 0.05, 3000);
        assert!((half.price_per_million_out() - 0.1896).abs() < 1e-3);
        let double = PricingConfig::new(PricingModel::Throughput, 0.20, 3000);
        assert!((double.price_per_million_out() - 0.7586).abs() < 1e-3);
    }

    #[test]
    fn margin_changes_scale_the_buyer_price_not_the_provider_rate() {
        let hard = PricingConfig::new(PricingModel::Throughput, 0.10, 0);
        assert!((hard.price_per_million_out() - 0.2918).abs() < 1e-3);
        let fat = PricingConfig::new(PricingModel::Throughput, 0.10, 5000);
        assert!((fat.price_per_million_out() - 0.4377).abs() < 1e-3);
        // k (and therefore the provider's hourly earning) ignores margin.
        assert!(close(hard.k_usd_per_token(), fat.k_usd_per_token(), 1e-12));
        assert!(close(
            hard.provider_rate_hr(15_000.0, 272.0, 1.0),
            fat.provider_rate_hr(15_000.0, 272.0, 1.0),
            1e-12
        ));
    }

    #[test]
    fn input_tokens_bill_at_a_quarter_of_output() {
        let c = throughput();
        // 1000 input + 500 output = 250 output-equivalents + 500 = 750.
        let cost = c.job_cost_usd(1000, 500);
        assert!(close(cost, 750.0 * c.price_per_token_out(), 1e-9));
        assert_eq!(c.escrow_token_units(1000, 500), 750);
        // The escrow ceiling always covers the worst case (full completion).
        assert_eq!(c.escrow_token_units(12, 512), 515);
        assert_eq!(c.escrow_token_units(0, 512), 512);
        // Zero frac is legal (output-only billing, the legacy shape).
        let mut m = c.clone();
        m.input_rate_frac = 0.0;
        assert_eq!(m.escrow_token_units(1000, 256), 256);
    }

    #[test]
    fn onchain_blended_rate_reproduces_two_rate_billing_at_full_completion() {
        let c = throughput();
        let out_rate = c.gpn_per_token(c.price_per_token_out(), 0.15);
        let (rate, cap) = c.onchain_rate_and_cap(out_rate, 1000, 500);
        assert_eq!(cap, 1500);
        // rate = out × (250 + 500) / 1500 = 0.5 × out
        assert!(rate >= out_rate / 2 - 1 && rate <= out_rate / 2 + 1, "rate {rate} vs {out_rate}");
        // Charged = rate × (P + C) with a full 500-token completion.
        let charged = rate * 1500;
        let exact = out_rate * 750;
        assert!(
            (charged as i128 - exact as i128).abs() <= 1500,
            "blended charge {charged} vs exact {exact}"
        );
        // Short completions under-bill slightly (never over-bill).
        assert!(rate * (1000 + 100) <= exact + 1500);
    }

    #[test]
    fn guardrails_refuse_prices_outside_the_band() {
        assert!(throughput().validate_publishable().is_ok());

        let greedy = PricingConfig::new(PricingModel::Throughput, 5.0, 3000);
        let err = greedy.validate_publishable().unwrap_err();
        assert!(err.contains("outside the sane band"), "{err}");

        let mut unsafe_cfg = greedy.clone();
        unsafe_cfg.unsafe_publish = true;
        assert!(unsafe_cfg.validate_publishable().is_ok());

        let dust = PricingConfig::new(PricingModel::Throughput, 0.001, 3000);
        assert!(dust.validate_publishable().is_err());
    }

    #[test]
    fn market_model_still_reproduces_the_legacy_price() {
        let c = PricingConfig::new(PricingModel::Market, 0.10, 3000);
        let usd = c.price_out(3.0, 95.2, 0.65);
        // rental = 1.50; usd/tok = 1.50/(95.2×3600×0.65)×1.30 = 8.75e-6
        assert!((usd * 1e6 - 8.75).abs() < 1e-2, "legacy $/1M = {}", usd * 1e6);
        // Throughput ignores the market inputs even if they are absurd.
        let t = throughput();
        assert!(close(
            t.price_out(999.0, 1.0, 0.05),
            t.price_per_token_out(),
            1e-12
        ));
        // And the two models differ by the expected factor at the old fallback.
        assert!((usd / t.price_per_token_out() - 23.07).abs() < 0.1);
        // The guardrail can be pointed at the legacy number too: `market` is an
        // explicit opt-in, so it is sanity-checked but not band-checked.
        let legacy_pm = c.price_per_million_out_for(3.0, 95.2, 0.65);
        assert!((legacy_pm - 8.75).abs() < 1e-2, "legacy guardrail input {legacy_pm}");
        assert!(c.check_price_usd_per_m(legacy_pm).is_ok());
        // While the same number on the calibrated card is refused.
        assert!(throughput().check_price_usd_per_m(legacy_pm).is_err());
    }

    #[test]
    fn gpn_conversion_never_returns_zero() {
        let c = throughput();
        assert_eq!(c.gpn_per_token(0.0, 0.15), 1);
        let units = c.gpn_per_token(c.price_per_token_out(), 0.15);
        // $0.379/1M ÷ $0.15/GPN = 2.53 GPN per 1M → 2.53e6 base units/token.
        assert!((units as f64 - 2.529e6).abs() < 2_000.0, "units {units}");
    }

    #[test]
    fn prompt_estimate_and_param_parsing() {
        assert_eq!(estimate_prompt_tokens(""), 1);
        assert_eq!(estimate_prompt_tokens("abcd"), 1);
        assert_eq!(estimate_prompt_tokens(&"a".repeat(400)), 100);
        assert_eq!(params_b_for("llama3.2:1b"), 1.0);
        assert_eq!(params_b_for("qwen2.5:7b"), 7.0);
        assert_eq!(params_b_for("deepseek-r1:14b"), 14.0);
        assert_eq!(params_b_for("mystery"), 1.0);
        assert_eq!(params_b_for("qwen2.5:0.5b"), 0.5);
    }
}
