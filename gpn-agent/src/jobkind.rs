//! What kind of AI work is this request? — ONE answer, shared by every layer.
//!
//! Queue item #76. Before this module the question was answered in five
//! different places with five slightly different model lists:
//!
//!   * `gpn_scheduler`: `JobKind::from_job_type` (sniffed `sd-`, `flux`, `ltx`…)
//!     plus a second `is_gen` sniff for pricing plus a third one in the payload
//!     writer;
//!   * `gpn_job` (the submit CLI): derived the payload kind from `--job-type`
//!     **only**, so `gpn_job --model sdxl-turbo …` with the default job type
//!     created an on-chain `LLMInference` job carrying an *LLM* payload;
//!   * `gpn_llm_provider`: its own "can this box serve it?" list;
//!   * `gpn_diffusion_provider`: claims only payload kinds `image`/`video`.
//!
//! A disagreement between any two of those is a job that is created, routed to
//! a box that cannot run it, and then either 404s on an Ollama pull or is
//! skipped forever while the on-chain state sits `Running` until the deadline
//! (the reported #76 symptom). So the classification lives here once, and every
//! caller asks the same question.
//!
//! Two rules make it safe to use as an *admission* gate:
//!
//! 1. **The model name wins over a generation-unaware `job_type`.** Clients that
//!    omit `--job-type` get the `LLMInference` default, so an explicit "text"
//!    type is not evidence that the caller means text — a diffusion model on an
//!    Ollama box can never work. `sdxl-turbo` is `Image` no matter what the
//!    caller typed.
//! 2. **An unknown `job_type` is refused, not silently downgraded to text.**
//!    `"ImageGen"`/`"Custom"` used to become an LLM job (a typo = an orphaned
//!    job); [`admit`] returns [`AdmissionError::UnknownJobType`] instead, which
//!    every entry point turns into a 4xx *before* any escrow is taken.

use std::fmt;

/// One variant per capability the network advertises.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobKind {
    Text,
    Image,
    Video,
    Ml,
    Finetune,
}

impl JobKind {
    /// The `capabilities[]` entry a provider must advertise to serve this work.
    pub fn capability(self) -> &'static str {
        match self {
            JobKind::Text => "LLMInference",
            JobKind::Image => "ImageGeneration",
            JobKind::Video => "VideoGeneration",
            JobKind::Ml => "MLInference",
            JobKind::Finetune => "FineTuning",
        }
    }

    /// The name the marketplace pallet stores — identical to [`Self::capability`]
    /// for every kind, but kept as its own method because the two contracts are
    /// independent (a capability rename must not silently change chain state).
    pub fn onchain_name(self) -> &'static str {
        self.capability()
    }

    /// Human-facing label for logs and error messages.
    pub fn label(self) -> &'static str {
        match self {
            JobKind::Text => "text",
            JobKind::Image => "image",
            JobKind::Video => "video",
            JobKind::Ml => "ml-inference",
            JobKind::Finetune => "fine-tune",
        }
    }

    /// Rendering work: served by a diffusion/video box, routed purely by
    /// capability because its model id (`sd-turbo`, `ltx-video`) is never in a
    /// heartbeat's Ollama `models[]`.
    pub fn is_generation(self) -> bool {
        matches!(self, JobKind::Image | JobKind::Video)
    }

    /// Does routing have to check that a provider can actually serve the model?
    /// Text and ML name an Ollama tag; fine-tuning names a HuggingFace base
    /// model the executor downloads (item #67), and generation has no Ollama
    /// tag at all — so neither of those gates on `models[]`.
    pub fn gates_on_model(self) -> bool {
        matches!(self, JobKind::Text | JobKind::Ml)
    }

    /// The off-chain payload kind (`LlmJob::kind`) for this work.
    pub fn payload_kind(self) -> &'static str {
        match self {
            JobKind::Text => "llm",
            JobKind::Image => "image",
            JobKind::Video => "video",
            JobKind::Ml => "ml",
            JobKind::Finetune => "finetune",
        }
    }

    /// Classify an already-stored payload (`LlmJob::kind`).
    pub fn from_payload_kind(kind: &str) -> Option<JobKind> {
        match kind.trim().to_ascii_lowercase().as_str() {
            "llm" | "text" => Some(JobKind::Text),
            "image" => Some(JobKind::Image),
            "video" => Some(JobKind::Video),
            "ml" => Some(JobKind::Ml),
            "finetune" | "fine-tune" | "ft" => Some(JobKind::Finetune),
            _ => None,
        }
    }

    /// Classify a caller-supplied `job_type`.
    ///
    /// Accepts the six pallet variants (case-insensitive) plus the handful of
    /// unambiguous spellings clients actually send. Anything else is `None` —
    /// the caller must refuse it rather than guess (a guess used to turn
    /// `job_type: "Custom"` into an LLM job).
    pub fn from_job_type(job_type: &str) -> Option<JobKind> {
        let t: String = job_type
            .trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        match t.as_str() {
            "llminference" | "llm" | "text" | "chat" => Some(JobKind::Text),
            "imagegeneration" | "image" | "img" => Some(JobKind::Image),
            "videogeneration" | "video" => Some(JobKind::Video),
            "mlinference" | "ml" => Some(JobKind::Ml),
            "finetuning" | "finetune" | "ft" => Some(JobKind::Finetune),
            _ => None,
        }
    }
}

/// The ONE model-name → kind table.
///
/// Only *generation* families are listed: an LLM tag (`llama3.2:1b`,
/// `qwen2.5:7b`, a HF base model for a tune) is deliberately `None` so the
/// caller's `job_type` decides. Every entry is a family that cannot be served
/// by an Ollama chat endpoint, which is exactly the mis-classification that
/// produced a `Running`-forever job in #76.
pub fn model_kind(model: &str) -> Option<JobKind> {
    let m = model.trim().to_ascii_lowercase();
    if m.is_empty() || m == "auto" {
        return None;
    }
    const VIDEO: &[&str] = &[
        "ltx",
        "ltx-video",
        "stable-video",
        "svd",
        "cogvideo",
        "hunyuan-video",
        "hunyuanvideo",
        "wan2",
        "mochi",
        "text-to-video",
        "t2v",
    ];
    const IMAGE: &[&str] = &[
        "sd-",
        "sd_",
        "sdxl",
        "stable-diffusion",
        "stable_diffusion",
        "stablediffusion",
        "diffusion",
        "flux",
        "playground",
        "kandinsky",
        "kolors",
        "dall-e",
        "dalle",
        "midjourney",
        "imagen",
        "juggernaut",
    ];
    if VIDEO.iter().any(|p| m.contains(p)) {
        return Some(JobKind::Video);
    }
    if IMAGE.iter().any(|p| m.contains(p)) {
        return Some(JobKind::Image);
    }
    None
}

/// Why a request was refused before any job existed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AdmissionError {
    /// `job_type` is not something this network can serve.
    UnknownJobType(String),
}

impl AdmissionError {
    /// The message an HTTP entry point should return (4xx).
    pub fn message(&self) -> String {
        match self {
            AdmissionError::UnknownJobType(t) => format!(
                "unknown job_type '{t}': supported are LLMInference, ImageGeneration, \
                 VideoGeneration, MLInference, FineTuning (omit job_type and the model \
                 decides); no job was created and nothing was charged"
            ),
        }
    }
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AdmissionError::UnknownJobType(_) => write!(f, "{}", self.message()),
        }
    }
}

impl std::error::Error for AdmissionError {}

/// Decide what a request is — the admission gate every entry point calls
/// **before** creating a job (item #76).
///
/// * `job_type` omitted/empty ⇒ the model name decides, defaulting to text.
/// * `job_type` unknown ⇒ [`AdmissionError::UnknownJobType`] (4xx, no escrow).
/// * a recognised generation model ⇒ that generation kind, even when the caller
///   left `job_type` at the `LLMInference` default (rule 1 above).
pub fn admit(job_type: Option<&str>, model: &str) -> Result<JobKind, AdmissionError> {
    let by_model = model_kind(model);
    let by_type = match job_type.map(str::trim).filter(|t| !t.is_empty()) {
        None => None,
        Some(t) => Some(
            JobKind::from_job_type(t)
                .ok_or_else(|| AdmissionError::UnknownJobType(t.to_string()))?,
        ),
    };
    Ok(match (by_type, by_model) {
        // A generation model cannot be served by a chat box, whatever the caller typed.
        (Some(_), Some(m)) if m.is_generation() => m,
        (Some(t), _) => t,
        (None, Some(m)) => m,
        (None, None) => JobKind::Text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generation_model_wins_over_the_default_text_job_type() {
        // The reported #76 shape: `--model sdxl-turbo` with no `--job-type`.
        assert_eq!(
            admit(Some("LLMInference"), "sdxl-turbo").unwrap(),
            JobKind::Image
        );
        assert_eq!(admit(None, "sdxl-turbo").unwrap(), JobKind::Image);
        assert_eq!(
            admit(Some("LLMInference"), "ltx-video").unwrap(),
            JobKind::Video
        );
        assert_eq!(
            admit(None, "black-forest-labs/FLUX.1-schnell").unwrap(),
            JobKind::Image
        );
        assert_eq!(admit(None, "playground-v2.5").unwrap(), JobKind::Image);
        assert_eq!(admit(None, "kandinsky-3").unwrap(), JobKind::Image);
    }

    #[test]
    fn an_unknown_job_type_is_refused_instead_of_becoming_text() {
        for bad in ["Custom", "ImageGen", "images", "video-gen", "banana"] {
            match admit(Some(bad), "llama3.2:1b") {
                Err(AdmissionError::UnknownJobType(t)) => assert_eq!(t, bad),
                other => panic!("{bad} must be refused, got {other:?}"),
            }
        }
        // The refusal text names the supported set, so a typo is self-correcting.
        let msg = admit(Some("Custom"), "m").unwrap_err().message();
        assert!(msg.contains("ImageGeneration") && msg.contains("nothing was charged"));
    }

    #[test]
    fn llm_and_ml_models_stay_led_by_the_job_type() {
        assert_eq!(admit(None, "llama3.2:1b").unwrap(), JobKind::Text);
        assert_eq!(
            admit(Some("LLMInference"), "llama3.2:1b").unwrap(),
            JobKind::Text
        );
        assert_eq!(admit(Some("MLInference"), "resnet50").unwrap(), JobKind::Ml);
        assert_eq!(
            admit(Some("FineTuning"), "HuggingFaceTB/SmolLM2-135M").unwrap(),
            JobKind::Finetune
        );
        assert_eq!(
            admit(Some("VideoGeneration"), "whatever").unwrap(),
            JobKind::Video
        );
        assert_eq!(admit(Some("ml"), "auto").unwrap(), JobKind::Ml);
        assert_eq!(admit(Some("image"), "auto").unwrap(), JobKind::Image);
        // Unambiguous aliases are accepted; a typo is not.
        assert_eq!(admit(Some("LLM"), "llama3.2:1b"), Ok(JobKind::Text));
    }

    #[test]
    fn payload_kinds_round_trip_through_the_classifier() {
        for k in [
            JobKind::Text,
            JobKind::Image,
            JobKind::Video,
            JobKind::Ml,
            JobKind::Finetune,
        ] {
            assert_eq!(JobKind::from_payload_kind(k.payload_kind()), Some(k));
            assert_eq!(JobKind::from_job_type(k.onchain_name()), Some(k));
        }
        assert_eq!(JobKind::from_payload_kind("banana"), None);
    }

    #[test]
    fn only_generation_gates_off_the_model() {
        assert!(JobKind::Text.gates_on_model());
        assert!(JobKind::Ml.gates_on_model());
        assert!(!JobKind::Image.gates_on_model());
        assert!(!JobKind::Video.gates_on_model());
        assert!(!JobKind::Finetune.gates_on_model());
        assert!(JobKind::Image.is_generation() && JobKind::Video.is_generation());
        assert!(!JobKind::Text.is_generation() && !JobKind::Ml.is_generation());
    }
}
