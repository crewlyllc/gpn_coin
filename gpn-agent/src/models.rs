//! Model catalogue — **VRAM fit** and the **public (assistant-grade) allowlist**.
//!
//! Two independent questions live here, both answered from a model name alone
//! (no Ollama probe, so every caller — heartbeat, provider claim loop, access
//! point routing — can use the same rules without extra HTTP):
//!
//! **Q1 — does it fit?** A provider must not advertise or claim a model whose
//! weights cannot fit in its VRAM. [`MODEL_VRAM_GB`] is a deliberately small,
//! conservative table of common assistant models; the check applies a
//! headroom factor ([`DEFAULT_VRAM_HEADROOM`], 1.15× by default) so KV cache
//! plus runtime overhead never push a "just fits" model into an OOM. An unknown
//! model is **not** blocked (we cannot know its size) — it is reported as
//! [`VramFit::Unknown`] so the caller can log it. Operators override with
//! `GPN_MODEL_VRAM_OVERRIDE`.
//!
//! Why a static table: querying Ollama's `/api/show` for every model on every
//! `/status` poll is exactly the noise item #48 removed. The table is the
//! fast path; a size can be overridden per model
//! (`GPN_MODEL_VRAM_OVERRIDE="qwen2.5:7b=5.2"`).
//!
//! **Q2 — may it be listed publicly?** `GET /v1/models` (the OpenAI facade) and the
//! public site are a shop window: a fine-tune artifact (`tuned-<job>`), a toy
//! model (`stories260K`, anything under ~300M params) or an unvetted name has
//! no business there. [`is_public_model`] enforces a known-good allowlist;
//! GPN_PUBLIC_MODELS (CSV) replaces it for an operator who knows better. A
//! model excluded from the catalogue is still **usable internally**: a
//! provider keeps serving it and a caller who names it explicitly still gets
//! an answer — it just is not advertised.
//!
//! Env:
//! GPN_MODEL_VRAM_HEADROOM   headroom multiplier (default 1.15; 1.0 disables)
//! GPN_MODEL_VRAM_OVERRIDE   `all` (gate off) | CSV of `model` or `model=GB`
//! GPN_PUBLIC_MODELS         CSV allowlist replacing the built-in one;
//! `all`/`*` publishes everything not in the hard
//! exclusion set

use std::sync::OnceLock;

/// Default VRAM headroom over the parameter-derived weight size. 1.15 leaves
/// room for the KV cache, the CUDA/OpenCL context and tokenizer buffers.
pub const DEFAULT_VRAM_HEADROOM: f64 = 1.15;

/// Conservative weight footprint (GB) of common assistant models, at the
/// default quantisation each model ships with on Ollama. Matched as a prefix so
/// `llama3.2:1b-instruct-q4_K_M` resolves via `llama3.2:1b`.
pub const MODEL_VRAM_GB: &[(&str, f64)] = &[
    ("llama3.2:1b", 1.3),
    ("llama3.2:3b", 2.4),
    ("llama3.1:8b", 5.0),
    ("llama3:8b", 4.7),
    ("qwen2.5:0.5b", 0.4),
    ("qwen2.5:1.5b", 1.2),
    ("qwen2.5:3b", 2.0),
    ("qwen2.5:7b", 4.7),
    ("qwen2.5:14b", 9.0),
    ("deepseek-r1:1.5b", 1.2),
    ("deepseek-r1:7b", 4.7),
    ("deepseek-r1:8b", 5.0),
    ("deepseek-r1:14b", 9.0),
    ("mistral:7b", 4.4),
    ("phi3:mini", 2.4),
    ("gemma2:2b", 1.7),
    ("gemma2:9b", 5.5),
];

/// Anything under this many billion parameters is a toy/embedding model, not an
/// assistant: `stories260K` (0.00026B) is the canonical example.
pub const MIN_PUBLIC_PARAMS_B: f64 = 0.3;

/// The built-in public catalogue: real assistant-grade chat models only.
pub const PUBLIC_MODELS_DEFAULT: &[&str] = &[
    "llama3.2:1b",
    "llama3.2:3b",
    "llama3.1:8b",
    "qwen2.5:0.5b",
    "qwen2.5:1.5b",
    "qwen2.5:3b",
    "qwen2.5:7b",
    "qwen2.5:14b",
    "deepseek-r1:1.5b",
    "deepseek-r1:7b",
    "deepseek-r1:8b",
    "deepseek-r1:14b",
    "mistral:7b",
    "phi3:mini",
    "gemma2:2b",
    "gemma2:9b",
];

/// Lower-case a model name and drop an Ollama `:latest` tag (so `llama3.2:latest`
/// compares equal to `llama3.2`; the catalogue entries keep an explicit size tag).
fn norm(name: &str) -> String {
    let s = name.trim().to_ascii_lowercase();
    if let Some(base) = s.strip_suffix(":latest") {
        return base.to_string();
    }
    s
}

// ── VRAM fit ───────────────────────────────────────────────────────────────

/// How a model name resolved against the size table.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VramFit {
    /// Known size and it fits (after headroom).
    Fits { needs_gb: f64, have_gb: f64 },
    /// Known size and it does not fit.
    TooBig { needs_gb: f64, have_gb: f64 },
    /// Not in the table and not overridden — cannot be judged.
    Unknown,
    /// Exempted by `GPN_MODEL_VRAM_OVERRIDE`.
    Overridden,
}

impl VramFit {
    /// True unless the model is known not to fit.
    pub fn allowed(self) -> bool {
        !matches!(self, VramFit::TooBig { .. })
    }
    /// The operator-facing skip line, e.g.
    /// `skipping qwen2.5:14b: needs ~10.4 GB, node has 6.0 GB`.
    pub fn skip_reason(self, model: &str) -> Option<String> {
        match self {
            VramFit::TooBig { needs_gb, have_gb } => Some(format!(
                "skipping {model}: needs ~{needs_gb:.1} GB, node has {have_gb:.1} GB"
            )),
            _ => None,
        }
    }
}

/// Operator override parsed from `GPN_MODEL_VRAM_OVERRIDE`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VramOverride {
    /// The gate is off entirely (`all`, `1`, `true`, `off` is NOT this — see
    /// [`VramOverride::parse`]).
    pub disabled: bool,
    /// Models forced to fit regardless of the table.
    pub allow: Vec<String>,
    /// Explicit sizes (GB) that beat the table.
    pub sizes: Vec<(String, f64)>,
}

impl VramOverride {
    pub fn parse(raw: &str) -> Self {
        let mut out = VramOverride::default();
        for tok in raw.split(',') {
            let t = tok.trim();
            if t.is_empty() {
                continue;
            }
            let low = t.to_ascii_lowercase();
            if matches!(low.as_str(), "all" | "*" | "1" | "true" | "yes" | "on") {
                out.disabled = true;
                continue;
            }
            if let Some((name, size)) = t.rsplit_once('=') {
                if let Ok(gb) = size.trim().parse::<f64>() {
                    out.sizes.push((norm(name), gb));
                    continue;
                }
            }
            out.allow.push(norm(t));
        }
        out
    }

    fn size_for(&self, model: &str) -> Option<f64> {
        let n = norm(model);
        self.sizes
            .iter()
            .find(|(m, _)| n == *m || n.starts_with(&format!("{m}-")))
            .map(|(_, gb)| *gb)
    }

    fn allows(&self, model: &str) -> bool {
        if self.disabled {
            return true;
        }
        let n = norm(model);
        self.allow
            .iter()
            .any(|m| n == *m || n.starts_with(&format!("{m}-")))
    }
}

/// Headroom multiplier from `GPN_MODEL_VRAM_HEADROOM`. A value < 1.0 (or an
/// unparseable one) falls back to the default — a headroom below 1 would make
/// the gate *optimistic*, which is the one direction it must never go.
pub fn vram_headroom() -> f64 {
    std::env::var("GPN_MODEL_VRAM_HEADROOM")
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|h| *h >= 1.0 && h.is_finite())
        .unwrap_or(DEFAULT_VRAM_HEADROOM)
}

fn cached_override() -> &'static VramOverride {
    static C: OnceLock<VramOverride> = OnceLock::new();
    C.get_or_init(|| {
        VramOverride::parse(&std::env::var("GPN_MODEL_VRAM_OVERRIDE").unwrap_or_default())
    })
}

/// Weight footprint (GB) for a model name, from the table or an override.
pub fn model_vram_gb(model: &str) -> Option<f64> {
    cached_override()
        .size_for(model)
        .or_else(|| table_vram_gb(model))
}

/// The static table lookup only (no env).
fn table_vram_gb(model: &str) -> Option<f64> {
    let n = norm(model);
    MODEL_VRAM_GB
        .iter()
        .find(|(m, _)| n == *m || n.starts_with(&format!("{m}-")))
        .map(|(_, gb)| *gb)
}

/// The pure decision (no env) — used by tests and by the env wrapper.
pub fn check_model_vram_with(
    model: &str,
    vram_mb: u64,
    over: &VramOverride,
    headroom: f64,
) -> VramFit {
    if over.allows(model) {
        return VramFit::Overridden;
    }
    // 0 = unknown hardware (old heartbeat / no GPU probe). Never block on
    // ignorance: a box that reports no VRAM is gated by its own runtime, and
    // blocking would silently empty every catalogue behind an unprobed box.
    if vram_mb == 0 {
        return VramFit::Unknown;
    }
    // An explicit size in the override beats both the table and any env cache,
    // so a test / caller-supplied override is honoured in full.
    let Some(gb) = over.size_for(model).or_else(|| table_vram_gb(model)) else {
        return VramFit::Unknown;
    };
    let have_gb = vram_mb as f64 / 1024.0;
    let needs_gb = gb * headroom.max(1.0);
    if have_gb >= needs_gb {
        VramFit::Fits { needs_gb, have_gb }
    } else {
        VramFit::TooBig { needs_gb, have_gb }
    }
}

/// [`check_model_vram_with`] using the process env (headroom + override).
pub fn check_model_vram(model: &str, vram_mb: u64) -> VramFit {
    check_model_vram_with(model, vram_mb, cached_override(), vram_headroom())
}

/// Convenience boolean: may this model be advertised/claimed on this box?
pub fn model_fits_vram(model: &str, vram_mb: u64) -> bool {
    check_model_vram(model, vram_mb).allowed()
}

/// Why a job that asks for `model` cannot be served on **this** box, if it cannot.
///
/// `installed` is what the local runtime actually has (`/api/tags`); `vram_mb`
/// is this box's VRAM (0 = unknown, which is never a reason to refuse).
///
/// `None` means "serve it" — either the box has the model, or the request is
/// empty/`auto` and the box picks. `Some(reason)` is the refusal text.
///
/// # Why a refusal and not a substitution (item #188)
///
/// The provider used to answer a request for a model it did not have by running
/// a different installed model and settling the job under the requested name.
/// Measured live 2026-11-15 09:3x PT: `{"model":"deepseek-r1:32b"}` on an 8 GB
/// box returned **HTTP 200 in 109 ms** with `grapes`, served by `llama3.2:1b` —
/// the caller was billed for a model that never ran, and nothing anywhere said
/// so. A pinned model is a contract, not a hint: if this box cannot honour it,
/// the honest answer is a named refusal the caller can route around.
///
/// The "cannot fit" case is checked only when the model is **absent** — a model
/// already installed here is the runtime's business, and the VRAM table only
/// guards a download we decide to attempt.
pub fn model_unservable(
    model: &str,
    installed: &[String],
    vram_mb: u64,
    headroom: f64,
) -> Option<String> {
    let want = model.trim();
    if want.is_empty() || want == "auto" {
        return None;
    }
    let has = installed
        .iter()
        .any(|n| n == want || n.split(':').next() == Some(want));
    if has {
        return None;
    }
    if let Some(reason) =
        check_model_vram_with(want, vram_mb, &VramOverride::default(), headroom).skip_reason(want)
    {
        return Some(format!(
            "{reason} — this box cannot serve '{want}', so it will not run (or bill) a \
             different model under that name (#188)"
        ));
    }
    let has_txt = if installed.is_empty() {
        "no models installed".to_string()
    } else {
        format!("it has {}", installed.join(", "))
    };
    Some(format!(
        "'{want}' is not installed on this box ({has_txt}) — refusing rather than \
         substituting another model under the requested name (#188)"
    ))
}

/// Drop the models that cannot fit, preserving order. `log` emits the
/// operator-facing `skipping …` line on stderr (the heartbeat's stdout is the
/// service log, so both are visible in `journalctl --user -u …`).
pub fn filter_models_by_vram(models: &[String], vram_mb: u64, log: bool) -> Vec<String> {
    let mut out = Vec::with_capacity(models.len());
    for m in models {
        let fit = check_model_vram(m, vram_mb);
        if let Some(reason) = fit.skip_reason(m) {
            if log {
                eprintln!("[models] {reason}");
            }
            continue;
        }
        out.push(m.clone());
    }
    out
}

// ── Public catalogue ───────────────────────────────────────────────────────

/// Approximate parameter count in billions, parsed from the name
/// (`llama3.2:1b` → 1.0, `qwen2.5:0.5b` → 0.5, `stories260K` → 0.00026).
/// `None` when the name carries no size token.
pub fn params_b(model: &str) -> Option<f64> {
    let s = norm(model);
    let b = s.as_bytes();
    // Scan from the right for `<digits>[.<digits>](b|m|k)`.
    let mut i = b.len();
    while i > 0 {
        i -= 1;
        let unit = match b[i] {
            b'b' | b'B' => 1e9,
            b'm' | b'M' => 1e6,
            b'k' | b'K' => 1e3,
            _ => continue,
        };
        // Walk the numeric run immediately before the unit.
        let mut j = i;
        while j > 0 && (b[j - 1].is_ascii_digit() || b[j - 1] == b'.') {
            j -= 1;
        }
        if j == i {
            continue;
        }
        let num = &s[j..i];
        if let Ok(v) = num.parse::<f64>() {
            if v > 0.0 {
                return Some(v * unit / 1e9);
            }
        }
    }
    None
}

/// Hard exclusions that even `GPN_PUBLIC_MODELS=all` cannot publish: fine-tune
/// artifacts (per-job LoRA merges, named `tuned-<job_id>`) and known toy models.
pub fn is_hard_excluded(model: &str) -> bool {
    let n = norm(model);
    n.starts_with("tuned-")
        || n.contains("/tuned-")
        || n.contains("tuned-")
        || n.starts_with("stories")
        || n.contains("stories260k")
        // Embedding / reranker models are not chat assistants.
        || n.contains("embed")
        || n.contains("rerank")
}

/// The allowlist in force: `GPN_PUBLIC_MODELS` when set, else the built-in one.
pub fn public_allowlist() -> Vec<String> {
    let raw = std::env::var("GPN_PUBLIC_MODELS").unwrap_or_default();
    let list: Vec<String> = raw.split(',').map(norm).filter(|s| !s.is_empty()).collect();
    if list.is_empty() {
        PUBLIC_MODELS_DEFAULT.iter().map(|s| norm(s)).collect()
    } else {
        list
    }
}

fn allowlist_is_open(list: &[String]) -> bool {
    list.iter().any(|s| matches!(s.as_str(), "all" | "*"))
}

/// May `model` be listed in the public catalogue?
///
/// `true` when it is in the allowlist (prefix match, so a quantisation tag is
/// fine) **and** is not a hard exclusion **and** is not obviously a toy
/// (< 0.3B params). An unknown-size model is allowed if named explicitly —
/// otherwise no new model could ever be published without a code change.
pub fn is_public_model_with(model: &str, allow: &[String]) -> bool {
    if is_hard_excluded(model) {
        return false;
    }
    if let Some(p) = params_b(model) {
        if p < MIN_PUBLIC_PARAMS_B {
            return false;
        }
    }
    let n = norm(model);
    if allowlist_is_open(allow) {
        return true;
    }
    allow
        .iter()
        .any(|a| n == *a || n.starts_with(&format!("{a}-")) || n.starts_with(&format!("{a}:")))
}

/// [`is_public_model_with`] using the process env allowlist.
pub fn is_public_model(model: &str) -> bool {
    is_public_model_with(model, &public_allowlist())
}

/// Filter a model list down to the public catalogue (order preserved, deduped).
pub fn filter_public_models<I, S>(models: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let allow = public_allowlist();
    let mut out: Vec<String> = Vec::new();
    for m in models {
        let m = m.as_ref();
        if is_public_model_with(m, &allow) && !out.iter().any(|x| x == m) {
            out.push(m.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// Item #188: a pinned model this box cannot serve is a REFUSAL, never an
    /// invitation to substitute. The live shape this fixes: `deepseek-r1:32b`
    /// asked of an 8 GB box answered HTTP 200 from `llama3.2:1b`.
    #[test]
    fn a_model_this_box_cannot_serve_is_refused_not_substituted() {
        let installed = names(&["llama3.2:1b", "qwen2.5:0.5b"]);
        let card = 8 * 1024;

        // Installed → serve, exact tag or bare name.
        assert_eq!(
            model_unservable("llama3.2:1b", &installed, card, 1.15),
            None
        );
        assert_eq!(model_unservable("llama3.2", &installed, card, 1.15), None);
        // "let the box pick" is not a pinned request.
        assert_eq!(model_unservable("", &installed, card, 1.15), None);
        assert_eq!(model_unservable("auto", &installed, card, 1.15), None);
        assert_eq!(model_unservable("  auto \n", &installed, card, 1.15), None);

        // Absent and too large → refuse, naming the model and the size gap.
        // (`deepseek-r1:32b` is not in the deliberately small VRAM table, so it
        // lands on the "not installed" arm — see the unknown-size case below.)
        let big = model_unservable("deepseek-r1:14b", &installed, card, 1.15)
            .expect("a 14B model on an 8 GB card must be refused");
        assert!(big.contains("deepseek-r1:14b"), "{big}");
        assert!(big.contains("GB"), "{big}");
        assert!(big.contains("#188"), "{big}");

        // Absent and of unknown size (the live 32B shape) → refused as absent,
        // never waved through on a missing table entry.
        let unknown = model_unservable("deepseek-r1:32b", &installed, card, 1.15)
            .expect("an unknown-size model must never be substituted silently");
        assert!(unknown.contains("deepseek-r1:32b"), "{unknown}");
        assert!(unknown.contains("not installed"), "{unknown}");

        // Absent but small enough → still a refusal (we do not silently run
        // something else), and the reason says what IS here.
        let absent = model_unservable("gemma2:2b", &installed, card, 1.15)
            .expect("an uninstalled model must be refused even when it would fit");
        assert!(absent.contains("gemma2:2b"), "{absent}");
        assert!(absent.contains("llama3.2:1b"), "{absent}");
        assert!(absent.contains("not installed"), "{absent}");

        // Unknown VRAM (0) must never be the reason to refuse: an unprobed box
        // still refuses a missing model, but never on a size guess.
        let unprobed = model_unservable("gemma2:2b", &installed, 0, 1.15).unwrap();
        assert!(unprobed.contains("not installed"), "{unprobed}");
        assert!(!unprobed.contains("GB"), "{unprobed}");

        // A box with nothing installed says so plainly.
        let empty = model_unservable("llama3.2:1b", &[], card, 1.15).unwrap();
        assert!(empty.contains("no models installed"), "{empty}");
    }

    #[test]
    fn vram_table_answers_for_the_documented_models() {
        // 8 GB card (8188 MiB ≈ 8.0 GB): everything up to 7B fits, 14B does not.
        for m in [
            "llama3.2:1b",
            "qwen2.5:0.5b",
            "qwen2.5:7b",
            "deepseek-r1:7b",
        ] {
            assert!(model_fits_vram(m, 8188), "{m} must fit an 8 GB card");
        }
        for m in ["deepseek-r1:14b", "qwen2.5:14b"] {
            assert!(!model_fits_vram(m, 8188), "{m} must not fit an 8 GB card");
        }
        // 24 GB card runs the 14B models.
        assert!(model_fits_vram("deepseek-r1:14b", 24576));
        // 0.4 GB model on a 2 GB card.
        assert!(model_fits_vram("qwen2.5:0.5b", 2048));
        // 1.3 GB model on a 1 GB card -> no.
        assert!(!model_fits_vram("llama3.2:1b", 1024));
    }

    #[test]
    fn unknown_vram_or_unknown_model_is_not_blocked() {
        assert_eq!(check_model_vram("llama3.2:1b", 0), VramFit::Unknown);
        assert_eq!(
            check_model_vram("some-private-model:latest", 4096),
            VramFit::Unknown
        );
        assert!(model_fits_vram("some-private-model", 1024));
    }

    #[test]
    fn headroom_is_applied() {
        // llama3.2:1b is 1.3 GB; at 1.15x it needs 1.495 GB. A 1.45 GB box
        // (~1485 MiB) is just short, a 1.6 GB box clears it.
        let over = VramOverride::default();
        assert!(matches!(
            check_model_vram_with("llama3.2:1b", 1484, &over, 1.15),
            VramFit::TooBig { .. }
        ));
        assert!(matches!(
            check_model_vram_with("llama3.2:1b", 1536, &over, 1.15),
            VramFit::Fits { .. }
        ));
        // headroom 1.0 is the raw table.
        assert!(matches!(
            check_model_vram_with("llama3.2:1b", 1400, &over, 1.0),
            VramFit::Fits { .. }
        ));
    }

    #[test]
    fn quantised_tag_matches_the_base_entry() {
        assert_eq!(model_vram_gb("llama3.2:1b-instruct-q4_K_M"), Some(1.3));
        assert_eq!(model_vram_gb("qwen2.5:7b-instruct-q4_0"), Some(4.7));
        assert_eq!(model_vram_gb("llama3.2:latest"), None);
    }

    #[test]
    fn skip_reason_is_operator_readable() {
        let r = check_model_vram("deepseek-r1:14b", 8188)
            .skip_reason("deepseek-r1:14b")
            .expect("must be skipped");
        assert_eq!(
            r,
            "skipping deepseek-r1:14b: needs ~10.3 GB, node has 8.0 GB"
        );
        assert!(check_model_vram("llama3.2:1b", 8188)
            .skip_reason("x")
            .is_none());
    }

    #[test]
    fn filter_drops_only_the_ones_that_cannot_fit() {
        let all = names(&["llama3.2:1b", "qwen2.5:7b", "deepseek-r1:14b", "tuned-7"]);
        let kept = filter_models_by_vram(&all, 8188, false);
        assert_eq!(kept, names(&["llama3.2:1b", "qwen2.5:7b", "tuned-7"]));
        // Unknown VRAM filters nothing.
        assert_eq!(filter_models_by_vram(&all, 0, false), all);
    }

    #[test]
    fn vram_override_parses_the_documented_forms() {
        let all = VramOverride::parse("all");
        assert!(all.disabled);
        assert_eq!(
            check_model_vram_with("deepseek-r1:14b", 8188, &all, 1.15),
            VramFit::Overridden
        );
        let one = VramOverride::parse("deepseek-r1:14b, llama3.2:1b");
        assert!(one.allows("deepseek-r1:14b"));
        assert!(!one.allows("qwen2.5:14b"));
        let sized = VramOverride::parse("deepseek-r1:14b=6.5");
        assert_eq!(sized.size_for("deepseek-r1:14b"), Some(6.5));
        assert!(model_fits_vram_with(&sized, "deepseek-r1:14b", 8188));
    }

    // `model_fits_vram` reads the process env; a helper keeps the assertion
    // independent of whatever the test runner's env holds.
    fn model_fits_vram_with(over: &VramOverride, model: &str, vram_mb: u64) -> bool {
        check_model_vram_with(model, vram_mb, over, DEFAULT_VRAM_HEADROOM).allowed()
    }

    #[test]
    fn param_parser_reads_the_size_token() {
        assert_eq!(params_b("llama3.2:1b"), Some(1.0));
        assert_eq!(params_b("qwen2.5:0.5b"), Some(0.5));
        assert_eq!(params_b("deepseek-r1:14b"), Some(14.0));
        approx(params_b("stories260K").unwrap(), 0.00026);
        approx(params_b("all-minilm:33m").unwrap(), 0.033);
        assert_eq!(params_b("gpt-4o"), None);
        assert_eq!(params_b("llama3.2"), None);
    }

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn public_catalogue_hides_fine_tunes_and_toys() {
        let allow: Vec<String> = PUBLIC_MODELS_DEFAULT
            .iter()
            .map(|s| s.to_string())
            .collect();
        for good in [
            "llama3.2:1b",
            "qwen2.5:0.5b",
            "qwen2.5:7b",
            "deepseek-r1:14b",
            "llama3.2:1b-instruct-q4_K_M",
        ] {
            assert!(is_public_model_with(good, &allow), "{good} must be public");
        }
        for bad in [
            "tuned-7",
            "tuned-123",
            "stories260K",
            "stories260K:latest",
            "some-random-model:latest",
            "nomic-embed-text:latest",
        ] {
            assert!(!is_public_model_with(bad, &allow), "{bad} must be hidden");
        }
    }

    #[test]
    fn open_allowlist_still_hides_hard_exclusions() {
        let open = vec!["all".to_string()];
        assert!(is_public_model_with("my-custom-7b", &open));
        assert!(!is_public_model_with("tuned-42", &open));
        assert!(!is_public_model_with("stories260K", &open));
    }

    #[test]
    fn explicit_allowlist_can_publish_an_unlisted_model() {
        let allow = vec!["my-custom-7b".to_string()];
        assert!(is_public_model_with("my-custom-7b", &allow));
        assert!(!is_public_model_with("llama3.2:1b", &allow));
    }

    #[test]
    fn filter_public_models_keeps_order_and_dedups() {
        let allow = public_allowlist();
        let got = filter_public_models([
            "tuned-9",
            "llama3.2:1b",
            "llama3.2:1b",
            "stories260K",
            "qwen2.5:7b",
        ]);
        if allow.iter().any(|a| a == "llama3.2:1b") && allow.iter().any(|a| a == "qwen2.5:7b") {
            assert_eq!(got, names(&["llama3.2:1b", "qwen2.5:7b"]));
        }
    }
}
