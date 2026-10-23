//! Provider-side admission rules for **channel-settled image jobs**.
//!
//! These live here (not inline in the diffusion provider binary) so the rules
//! that cost money — *should this box take this render?* — are pure functions
//! with tests, and so the scheduler can quote the same numbers.
//!
//! A refusal is not an error: the provider signs a **refund** for the
//! pre-authorised channel payment, which reverts the balances and advances the
//! nonce. The requestor therefore pays nothing, and the AP sees a
//! `NotAttempted` failure it may safely retry on-chain.

use crate::models::VramFit;

/// What the provider should do with an accepted assignment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServeDecision {
    Run,
    /// Sign a refund with this reason. Never charges the requestor.
    Refund(String),
}

/// Default cap on one render's pixels. The 8 GB testnet card renders 512² in
/// ~29 s and 1024² comfortably; 1536²/30 steps would be minutes and risks the
/// allocator, so it is refused up front rather than OOM-ing mid-render.
pub const DEFAULT_MAX_PIXELS: u64 = 1024 * 1024;

pub fn max_pixels() -> u64 {
    std::env::var("GPN_IMAGE_MAX_PIXELS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n >= 256 * 256)
        .unwrap_or(DEFAULT_MAX_PIXELS)
}

/// Rough VRAM requirement of a diffusion render in MiB.
///
/// `sd-turbo`/SD1.5 UNet activations scale with pixels; the measured 4060 usage
/// at 512² is ~2.6 GB including the fp16 pipeline. We charge 2 GB of model +
/// 6 MiB per 1024 pixels of latent work, and add 25 % headroom. Deliberately
/// conservative: refusing a render that *might* fit is better than an
/// allocator failure that leaves the caller waiting on a job that died.
pub fn estimated_render_vram_mb(width: u32, height: u32) -> u64 {
    let mpx = (width as u64 * height as u64) as f64 / 1_000_000.0;
    (2048.0 + mpx * 900.0) as u64 * 5 / 4
}

/// Decide whether this provider may run the render.
///
/// * **Idle** — same gate as the LLM provider: while the operator idles the
///   node, no new job of any kind is accepted (mining and claims are paused).
/// * **VRAM** — the model must fit the card *and* the requested resolution must
///   fit the estimated render budget. `vram_mb == 0` means "unknown hardware":
///   the model table cannot judge it, so only the pixel cap applies (blocking
///   on ignorance would empty the catalogue behind an unprobed box).
/// * **Pixels** — hard cap, independent of VRAM.
pub fn serve_decision(
    idle: bool,
    model: &str,
    vram_mb: u64,
    width: u32,
    height: u32,
) -> ServeDecision {
    if idle {
        return ServeDecision::Refund(
            "node is in idle mode; it does not accept new jobs (mining and claims are paused)"
                .to_string(),
        );
    }
    let pixels = width as u64 * height as u64;
    let cap = max_pixels();
    if pixels == 0 || pixels > cap {
        return ServeDecision::Refund(format!(
            "render {width}x{height} is {pixels} pixels, above this provider's cap of {cap} \
             (raise GPN_IMAGE_MAX_PIXELS to serve it)"
        ));
    }
    if vram_mb > 0 {
        if let VramFit::TooBig { needs_gb, have_gb } =
            crate::models::check_model_vram(model, vram_mb)
        {
            return ServeDecision::Refund(format!(
                "model '{model}' needs ~{needs_gb:.1} GB but this card has {have_gb:.1} GB"
            ));
        }
        let need = estimated_render_vram_mb(width, height);
        if need > vram_mb {
            return ServeDecision::Refund(format!(
                "a {width}x{height} render needs ~{need} MiB of VRAM but this card has {vram_mb} MiB"
            ));
        }
    }
    ServeDecision::Run
}

/// Capability key the AP uses to fetch the finished bytes. Deterministic (so
/// the provider needs no per-job map) but binds the channel *and* the off-chain
/// job id, neither of which is published — an artifact URL cannot be guessed
/// from a job id alone.
pub fn artifact_token(channel_id: u64, job_id: u64) -> String {
    let mut b = Vec::with_capacity(40);
    b.extend_from_slice(b"GPN-ARTIFACT-v1");
    b.extend_from_slice(&channel_id.to_le_bytes());
    b.extend_from_slice(&job_id.to_le_bytes());
    hex::encode(sp_core::blake2_256(&b))
}

pub fn token_matches(expected: &str, got: &str) -> bool {
    let (a, b) = (expected.as_bytes(), got.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        d |= x ^ y;
    }
    d == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_refuses_a_render_the_box_could_otherwise_run() {
        match serve_decision(true, "sd-turbo", 8188, 512, 512) {
            ServeDecision::Refund(r) => assert!(r.contains("idle")),
            ServeDecision::Run => panic!("idle must refuse"),
        }
    }

    #[test]
    fn a_render_above_the_vram_estimate_is_refused_not_o_omed() {
        // 1536² would exceed the 8 GB card's budget even if the pixel cap were raised.
        std::env::remove_var("GPN_IMAGE_MAX_PIXELS");
        match serve_decision(false, "sd-turbo", 2048, 1024, 1024) {
            ServeDecision::Refund(r) => assert!(r.contains("VRAM") || r.contains("MiB")),
            ServeDecision::Run => panic!("a 2 GB card cannot render 1024²"),
        }
    }

    #[test]
    fn a_normal_render_runs_on_the_anchor_card() {
        assert_eq!(
            serve_decision(false, "sd-turbo", 8188, 512, 512),
            ServeDecision::Run
        );
        assert_eq!(
            serve_decision(false, "sd-turbo", 8188, 1024, 1024),
            ServeDecision::Run
        );
    }

    #[test]
    fn unknown_hardware_is_not_blocked_by_ignorance() {
        assert_eq!(
            serve_decision(false, "sd-turbo", 0, 512, 512),
            ServeDecision::Run
        );
    }

    #[test]
    fn the_pixel_cap_is_hard() {
        std::env::set_var("GPN_IMAGE_MAX_PIXELS", "262144");
        match serve_decision(false, "sd-turbo", 8188, 1024, 1024) {
            ServeDecision::Refund(r) => assert!(r.contains("cap")),
            ServeDecision::Run => panic!("above the configured cap"),
        }
        std::env::remove_var("GPN_IMAGE_MAX_PIXELS");
    }

    #[test]
    fn artifact_tokens_bind_channel_and_job() {
        let t = artifact_token(7, 99);
        assert_eq!(t.len(), 64);
        assert!(token_matches(&t, &artifact_token(7, 99)));
        assert!(!token_matches(&t, &artifact_token(7, 100)));
        assert!(!token_matches(&t, "short"));
    }
}
