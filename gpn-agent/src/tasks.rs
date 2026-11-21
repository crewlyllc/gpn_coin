//! The task vocabulary — one word per kind of work (row 221).
//!
//! The network already speaks four shapes (`/v1/chat/completions`,
//! `/v1/images/generations`, `/v1/audio/speech`, `/v1/finetunes`) for one
//! primitive: *a job with inputs, a model and outputs*. This module names that
//! primitive and gives every kind of work a stable word, so a new modality is a
//! new **row**, not a new endpoint (design: `docs/design-task-modality-registry.md`).
//!
//! Two rules keep the table honest, both from the design doc:
//!
//! 1. **A word is a promise about modality, not about quality.** `chat` says
//!    "text in, text out"; it never promises a 70B model.
//! 2. **A word that needs a capability no provider advertises is a `503`, never
//!    a silent fallback to a different kind of work.** The caller asked for
//!    speech; answering with prose would be a wrong answer that bills.
//!
//! Words are stable forever: aliases may be added, a word is never renamed.
//! Words already listed here but with no executor in this build refuse with
//! `501`, naming the row that will give them one — an honest "not yet", which is
//! better than routing `caption` to an LLM because it shares a capability.

use crate::jobkind::{self, JobKind};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// What a task takes in / gives out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Modality {
    Text,
    Image,
    Audio,
    Video,
    Tensor,
    Json,
}

impl Modality {
    pub fn as_str(self) -> &'static str {
        match self {
            Modality::Text => "text",
            Modality::Image => "image",
            Modality::Audio => "audio",
            Modality::Video => "video",
            Modality::Tensor => "tensor",
            Modality::Json => "json",
        }
    }
}

/// Where a resolved task is sent: the OpenAI-shaped endpoint that already
/// implements it for this release. `None` = vocabulary only, no executor yet.
pub type Endpoint = &'static [&'static str];

/// One canonical word.
#[derive(Debug)]
pub struct Task {
    /// The canonical word. Never renamed.
    pub word: &'static str,
    /// Accepted spellings besides `word`.
    pub aliases: &'static [&'static str],
    pub accept: &'static [Modality],
    pub produce: &'static [Modality],
    /// Capabilities that can satisfy it — any one of them is enough.
    pub capabilities: &'static [&'static str],
    /// The endpoint that serves it in this build, if any.
    pub endpoint: Option<Endpoint>,
    /// Why it has no endpoint yet (for the 501 message).
    pub pending: &'static str,
}

const CHAT: Endpoint = &["v1", "chat", "completions"];
const IMAGES: Endpoint = &["v1", "images", "generations"];
const SPEECH: Endpoint = &["v1", "audio", "speech"];
const FINETUNES: Endpoint = &["v1", "finetunes"];

/// The vocabulary. Order is the order `GET /v1/tasks` and the docs print.
pub const VOCABULARY: &[Task] = &[
    Task {
        word: "chat",
        // `complete` is the legacy/"completion" phrasing; same promise.
        aliases: &["complete", "completion"],
        accept: &[Modality::Text],
        produce: &[Modality::Text],
        capabilities: &["LLMInference"],
        endpoint: Some(CHAT),
        pending: "",
    },
    Task {
        word: "embed",
        aliases: &[],
        accept: &[Modality::Text],
        produce: &[Modality::Tensor],
        capabilities: &["EmbeddingInference"],
        endpoint: None,
        pending: "no embedding executor in this build",
    },
    Task {
        word: "generate-image",
        aliases: &["image", "text-to-image", "t2i"],
        accept: &[Modality::Text],
        produce: &[Modality::Image],
        capabilities: &["ImageGeneration"],
        endpoint: Some(IMAGES),
        pending: "",
    },
    Task {
        word: "edit-image",
        aliases: &["inpaint"],
        accept: &[Modality::Image, Modality::Text],
        produce: &[Modality::Image],
        capabilities: &["ImageEdit"],
        endpoint: None,
        pending: "no inpainting executor in this build",
    },
    Task {
        word: "generate-video",
        aliases: &["video", "text-to-video", "t2v"],
        accept: &[Modality::Text],
        produce: &[Modality::Video],
        capabilities: &["VideoGeneration"],
        endpoint: None,
        pending: "video generation has a provider lane but no access-point route yet",
    },
    Task {
        word: "animate-image",
        aliases: &["image-to-video", "i2v"],
        accept: &[Modality::Image, Modality::Text],
        produce: &[Modality::Video],
        capabilities: &["VideoGeneration"],
        endpoint: None,
        pending: "image-conditioned video has no access-point route yet",
    },
    Task {
        word: "speak",
        aliases: &["tts", "speech", "text-to-speech"],
        // A reference clip is optional: with one, the request asks for cloning.
        accept: &[Modality::Text, Modality::Audio],
        produce: &[Modality::Audio],
        capabilities: &["AudioGeneration"],
        endpoint: Some(SPEECH),
        pending: "",
    },
    Task {
        word: "transcribe",
        aliases: &["stt", "speech-to-text"],
        accept: &[Modality::Audio],
        produce: &[Modality::Text],
        capabilities: &["SpeechToText"],
        endpoint: None,
        pending: "no speech-to-text executor in this build",
    },
    Task {
        word: "translate",
        aliases: &[],
        accept: &[Modality::Audio, Modality::Text],
        produce: &[Modality::Text],
        capabilities: &["SpeechToText", "LLMInference"],
        endpoint: None,
        pending: "no translation routing in this build",
    },
    Task {
        word: "caption",
        aliases: &["describe-image"],
        accept: &[Modality::Image],
        produce: &[Modality::Text],
        capabilities: &["VisionInference"],
        endpoint: None,
        pending: "no vision executor in this build",
    },
    Task {
        word: "classify",
        aliases: &["label"],
        accept: &[Modality::Text, Modality::Image],
        produce: &[Modality::Json],
        capabilities: &["LLMInference", "VisionInference"],
        endpoint: None,
        pending: "no label-returning executor in this build",
    },
    Task {
        word: "upscale",
        aliases: &["superres"],
        accept: &[Modality::Image],
        produce: &[Modality::Image],
        capabilities: &["ImageUpscale"],
        endpoint: None,
        pending: "no upscaler in this build",
    },
    Task {
        word: "finetune",
        aliases: &["fine-tune", "train"],
        accept: &[Modality::Json, Modality::Tensor],
        produce: &[Modality::Tensor],
        capabilities: &["FineTuning"],
        endpoint: Some(FINETUNES),
        pending: "",
    },
    Task {
        word: "segment",
        aliases: &["segment-image"],
        accept: &[Modality::Image],
        produce: &[Modality::Image, Modality::Json],
        capabilities: &["VisionSegmentation"],
        endpoint: None,
        pending: "no segmentation executor in this build",
    },
];

/// The canonical word for a job kind, when one exists.
pub fn word_for_kind(kind: JobKind) -> Option<&'static str> {
    match kind {
        JobKind::Text => Some("chat"),
        JobKind::Image => Some("generate-image"),
        JobKind::Video => Some("generate-video"),
        JobKind::Audio => Some("speak"),
        JobKind::Finetune => Some("finetune"),
        // ML inference has no word in the table yet; do not invent one.
        JobKind::Ml => None,
    }
}

/// The word a named endpoint stands for — the on-ramp for existing clients.
pub fn from_endpoint(segments: &[&str]) -> Option<&'static str> {
    match segments {
        ["v1", "chat", "completions"] => Some("chat"),
        ["v1", "completions"] => Some("chat"),
        ["v1", "embeddings"] => Some("embed"),
        ["v1", "images", "generations"] => Some("generate-image"),
        ["v1", "audio", "speech"] => Some("speak"),
        ["v1", "audio", "transcriptions"] => Some("transcribe"),
        ["v1", "finetunes"] | ["v1", "fine_tuning", "jobs"] => Some("finetune"),
        _ => None,
    }
}

fn norm(word: &str) -> String {
    word.trim().to_ascii_lowercase()
}

/// Look a word (or alias) up. Case- and whitespace-insensitive; hyphens and
/// underscores are the same character, because clients type `text_to_image`.
pub fn lookup(word: &str) -> Option<&'static Task> {
    let w = norm(word).replace('_', "-");
    VOCABULARY
        .iter()
        .find(|t| t.word == w || t.aliases.iter().any(|a| *a == w))
}

/// Every canonical word, in table order.
pub fn words() -> Vec<&'static str> {
    VOCABULARY.iter().map(|t| t.word).collect()
}

/// The valid vocabulary as one line, for a 400 body.
pub fn vocabulary_line() -> String {
    words().join(", ")
}

/// Why a request could not be turned into a job.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TaskError {
    /// Not a word in the table. 400 — never a fallback.
    UnknownTask(String),
    /// A word exists but nothing in this build executes it. 501.
    NotImplemented(&'static str, &'static str),
    /// The request was understood but is unusable as sent. 400.
    BadRequest(String),
}

impl TaskError {
    /// (http status, message)
    pub fn status_and_message(&self) -> (u16, String) {
        match self {
            TaskError::UnknownTask(t) => (
                400,
                format!(
                    "unknown task '{t}': the vocabulary is {} — no job was created and nothing \
                     was charged",
                    vocabulary_line()
                ),
            ),
            TaskError::NotImplemented(word, why) => (
                501,
                format!("task '{word}' is in the vocabulary but not served yet: {why}"),
            ),
            TaskError::BadRequest(m) => (400, m.clone()),
        }
    }
}

/// Resolution order from the design (§3). `task` is the caller's explicit word;
/// `model` narrows but never renames the work.
pub fn resolve(task: Option<&str>, model: &str) -> Result<&'static Task, TaskError> {
    // 1. explicit task wins (and an unknown one is refused, not defaulted).
    if let Some(t) = task.map(str::trim).filter(|t| !t.is_empty()) {
        return lookup(t).ok_or_else(|| TaskError::UnknownTask(t.to_string()));
    }
    // 3. a model with no task is inferred from the model's own family.
    if let Some(kind) = jobkind::model_kind(model) {
        if let Some(word) = word_for_kind(kind) {
            if let Some(t) = lookup(word) {
                return Ok(t);
            }
        }
    }
    // 4. neither ⇒ chat. `chat` is the only task whose input shape is
    //    unambiguous: a bare string of text can only mean conversation.
    Ok(lookup("chat").expect("chat is in the vocabulary"))
}

/// The text of an `inputs[]` array, joined — the shape every text-in task takes.
fn input_text(v: &Value) -> Option<String> {
    // `inputs: [{"type":"text","text":"…"}]`
    if let Some(arr) = v.get("inputs").and_then(|x| x.as_array()) {
        let parts: Vec<String> = arr
            .iter()
            .filter_map(|i| {
                let ty = i.get("type").and_then(|x| x.as_str()).unwrap_or("text");
                if ty.eq_ignore_ascii_case("text") {
                    i.get("text")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
            .filter(|s| !s.trim().is_empty())
            .collect();
        if !parts.is_empty() {
            return Some(parts.join("\n"));
        }
    }
    // `prompt` / `input` top level — the native and llama.cpp spellings.
    for k in ["prompt", "input", "text"] {
        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
            if !s.trim().is_empty() {
                return Some(s.to_string());
            }
        }
    }
    // OpenAI `messages[]` — passed through untouched by the chat adapter.
    if v.get("messages").and_then(|x| x.as_array()).is_some() {
        let parts: Vec<String> = v["messages"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();
        if !parts.is_empty() {
            return Some(parts.join("\n"));
        }
    }
    None
}

/// A resolved request: which word, what it needs, and the body to hand to the
/// endpoint that implements it.
#[derive(Clone, Debug)]
pub struct Adaptation {
    pub word: &'static str,
    pub capabilities: &'static [&'static str],
    pub endpoint: Endpoint,
    pub body: Value,
    /// The model pin the caller asked for, if any (echoed back in the 202).
    pub model: Option<String>,
}

/// Turn a `POST /v1/jobs` body into (word, endpoint, endpoint-shaped body).
///
/// This is the whole dispatcher: one parser, and each word's adapter is a few
/// lines that place the inputs where its endpoint already expects them. No
/// second job-creation path exists, so pricing, escrow, leasing and artifacts
/// are shared by construction.
pub fn adapt_v1_jobs(raw: &str) -> Result<Adaptation, TaskError> {
    let v: Value = serde_json::from_str(raw)
        .map_err(|e| TaskError::BadRequest(format!("body is not JSON: {e}")))?;
    if !v.is_object() {
        return Err(TaskError::BadRequest(
            "body must be a JSON object: {\"task\": \"chat\", \"inputs\": [...]}".to_string(),
        ));
    }
    let model = v
        .get("model")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto"));
    let task = resolve(
        v.get("task").and_then(|x| x.as_str()),
        model.as_deref().unwrap_or("auto"),
    )?;
    let endpoint = match task.endpoint {
        Some(e) => e,
        None => return Err(TaskError::NotImplemented(task.word, task.pending)),
    };
    let params = v.get("params").cloned().unwrap_or(json!({}));
    let text = input_text(&v);

    // ── per-word adapters ────────────────────────────────────────────────
    let body = match task.word {
        "chat" => {
            // A `messages[]` body is already OpenAI-shaped: pass it through,
            // model pin and generation params included.
            let mut out = json!({"model": model.clone().unwrap_or_else(|| "auto".into())});
            if let Some(msgs) = v.get("messages") {
                out["messages"] = msgs.clone();
            } else {
                let t = text.ok_or_else(|| {
                    TaskError::BadRequest(
                        "chat needs text: send inputs:[{\"type\":\"text\",\"text\":\"…\"}] \
                         or messages[]"
                            .to_string(),
                    )
                })?;
                out["messages"] = json!([{"role": "user", "content": t}]);
            }
            // Params pass through, `params` taking precedence over top level.
            for k in [
                "max_tokens",
                "temperature",
                "stream",
                "top_p",
                "mode",
                "wait",
            ] {
                if let Some(x) = v.get(k) {
                    out[k] = x.clone();
                }
                if let Some(x) = params.get(k) {
                    out[k] = x.clone();
                }
            }
            out
        }
        "generate-image" => {
            let t = text.ok_or_else(|| {
                TaskError::BadRequest(
                    "generate-image needs a prompt: send inputs:[{\"type\":\"text\",\"text\":\"…\"}]"
                        .to_string(),
                )
            })?;
            let mut out = json!({"prompt": t});
            if let Some(m) = &model {
                out["model"] = json!(m);
            }
            for k in ["size", "steps", "seed", "wait", "negative_prompt"] {
                if let Some(x) = params.get(k).or_else(|| v.get(k)) {
                    out[k] = x.clone();
                }
            }
            out
        }
        "speak" => {
            let t = text.ok_or_else(|| {
                TaskError::BadRequest(
                    "speak needs input text: send inputs:[{\"type\":\"text\",\"text\":\"…\"}]"
                        .to_string(),
                )
            })?;
            let mut out = json!({"input": t});
            if let Some(m) = &model {
                out["model"] = json!(m);
            }
            // Voice cloning is a policy decision: consent and the reference clip
            // are copied, never inferred. The audio handler refuses a clone
            // without both, so this adapter does not soften that gate.
            for k in [
                "voice_ref",
                "voice_ref_b64",
                "consent",
                "response_format",
                "wait",
            ] {
                if let Some(x) = params.get(k).or_else(|| v.get(k)) {
                    out[k] = x.clone();
                }
            }
            // An `inputs[{"type":"audio","artifact"/"base64":…}]` is the generic
            // spelling of the reference clip.
            if let Some(arr) = v.get("inputs").and_then(|x| x.as_array()) {
                for i in arr {
                    let ty = i.get("type").and_then(|x| x.as_str()).unwrap_or("");
                    if ty.eq_ignore_ascii_case("audio") {
                        if let Some(b) = i.get("base64").and_then(|x| x.as_str()) {
                            out["voice_ref"] = json!(b);
                        }
                    }
                }
            }
            out
        }
        "finetune" => {
            let mut out = if let Some(o) = params.as_object() {
                Value::Object(o.clone())
            } else {
                json!({})
            };
            if let Some(m) = &model {
                out["model"] = json!(m);
            }
            for k in ["dataset", "dataset_path", "epochs", "base_model", "wait"] {
                if let Some(x) = v.get(k) {
                    out[k] = x.clone();
                }
            }
            out
        }
        other => return Err(TaskError::NotImplemented(other, task.pending)),
    };

    Ok(Adaptation {
        word: task.word,
        capabilities: task.capabilities,
        endpoint,
        body,
        model,
    })
}

// ───────────────────────── provider advertisement (row 222) ─────────────────

/// A provider's advertisement as a **matrix**: which words it serves, with what
/// models, under what limits (row 222).
///
/// Before this the heartbeat said `capabilities: ['LLMInference','AudioGeneration']`
/// and a flat `models: [...]` — which answers "what kind of work" but not "with
/// what inputs", so the router could pin an image model to a chat job on a box
/// that lists both, and it could not tell "this box has no vision model" from
/// "this box never loaded one".
///
/// Backwards compatible by construction: an old heartbeat (capabilities + flat
/// models, no `tasks`) parses into the same matrix — the words are derived from
/// the capability names through [`tasks_for_capability`], and the flat model
/// list applies to every word it can serve. Nothing a 0.3.64 box says becomes
/// unroutable because of this type.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskMatrix {
    /// Canonical words this provider serves, in vocabulary order.
    pub tasks: Vec<&'static str>,
    /// word → models that serve it. An empty vec means "serves the word, model
    /// list not itemised" (a legacy flat list, or a task routed by capability).
    pub models: BTreeMap<&'static str, Vec<String>>,
    /// word → the limits object as advertised. Opaque here; the endpoint that
    /// serves the word enforces them (the AP must not invent a second set).
    pub limits: BTreeMap<&'static str, Value>,
    /// Did the heartbeat name `tasks` explicitly? `false` = legacy shape, where
    /// the words were derived from `capabilities`.
    pub tasks_declared: bool,
    /// Did the heartbeat declare a capability list at all? Mirrors the old
    /// `caps_declared` flag: a declared-but-empty list means "serves none",
    /// which must fail closed rather than expand to `default_capabilities()`.
    pub caps_declared: bool,
    /// Capability strings to publish for consumers that still read the flat
    /// list — derived from [`Self::tasks`], never hand-maintained.
    pub capabilities: Vec<String>,
}

impl TaskMatrix {
    pub fn advertises(&self, word: &str) -> bool {
        self.tasks.contains(&word)
    }

    /// The models the provider says serve `word` (empty = not itemised).
    pub fn models_for(&self, word: &str) -> &[String] {
        self.models.get(word).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Does this provider serve `word`, and can it serve `model` (when pinned)?
    ///
    /// `Err(reason)` is the router's refusal, worded for the 503/400 body. The
    /// three things it catches that a flat capability list could not:
    ///
    /// 1. **no executor** — the word exists but this build cannot run it;
    /// 2. **the box does not advertise the word** — a vision capability is not
    ///    evidence of ever having loaded a vision model (no more
    ///    `caption` → any-vision-capability box);
    /// 3. **the pinned model belongs to a different word** — `sd-turbo` is
    ///    `generate-image` even when the same box advertises `chat`.
    pub fn serves(&self, word: &str, model: Option<&str>) -> Result<(), String> {
        let task = lookup(word).ok_or_else(|| format!("unknown task '{word}'"))?;
        if task.endpoint.is_none() {
            return Err(format!(
                "task '{}' is in the vocabulary but not served yet: {}",
                task.word, task.pending
            ));
        }
        if self.tasks_declared {
            if !self.advertises(task.word) {
                return Err(format!("no provider advertises task={}", task.word));
            }
        } else if !self
            .capabilities
            .iter()
            .any(|c| task.capabilities.iter().any(|t| t.eq_ignore_ascii_case(c)))
        {
            return Err(format!("no provider advertises task={}", task.word));
        }
        if let Some(m) = model.map(str::trim).filter(|m| !m.is_empty()) {
            if m != "auto" {
                // The model's own family decides which word it can serve.
                if let Some(kind) = jobkind::model_kind(m) {
                    if let Some(owner) = word_for_kind(kind) {
                        if owner != task.word && kind_hard_owner(kind) {
                            return Err(format!(
                                "model '{}' serves task={}, not task={}",
                                m, owner, task.word
                            ));
                        }
                    }
                }
                let listed = self.models_for(task.word);
                if !listed.is_empty()
                    && !listed
                        .iter()
                        .any(|x| x == m || x.split(':').next() == Some(m))
                {
                    return Err(format!(
                        "provider does not list model '{}' for task={}",
                        m, task.word
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Is a model's family an *exclusive* claim on a word? Text models may serve
/// more than chat (`embed`, `classify`); a diffusion or TTS model serves one
/// word and nothing else. Used so "sd-turbo on a chat job" is refused while
/// "qwen2.5:0.5b on an embed job" is not invented here.
fn kind_hard_owner(kind: JobKind) -> bool {
    matches!(kind, JobKind::Image | JobKind::Video | JobKind::Audio)
}

/// Every word a capability string can serve.
pub fn tasks_for_capability(cap: &str) -> Vec<&'static Task> {
    VOCABULARY
        .iter()
        .filter(|t| t.capabilities.iter().any(|c| c.eq_ignore_ascii_case(cap)))
        .collect()
}

fn str_arr(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a heartbeat into a matrix. Accepts the new shape (`tasks`,
/// `task_models` or object `models`, `limits`) and the old one (`capabilities`
/// + flat `models[]`), and never invents a word it cannot find in the table.
pub fn parse_advertisement(v: &Value) -> TaskMatrix {
    let raw_tasks = v.get("tasks").and_then(|x| x.as_array()).map(|a| {
        a.iter()
            .filter_map(|s| s.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
    });
    let caps_raw = v.get("capabilities").and_then(|x| x.as_array());
    let caps = str_arr(v, "capabilities");
    let caps_declared = caps_raw.is_some()
        || v.get("caps_declared")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
    let tasks_declared = raw_tasks.is_some();

    // Words: explicit `tasks[]` when declared (normalised through the table),
    // otherwise derived from the capability names, in vocabulary order.
    let mut words: Vec<&'static str> = Vec::new();
    if let Some(list) = &raw_tasks {
        for w in list {
            if let Some(t) = lookup(w) {
                if !words.contains(&t.word) {
                    words.push(t.word);
                }
            }
        }
    } else {
        for cap in &caps {
            for t in tasks_for_capability(cap) {
                if !words.contains(&t.word) {
                    words.push(t.word);
                }
            }
        }
    }
    // Vocabulary order in both cases: `GET /v1/tasks` and the docs print it.
    words.sort_by_key(|w| VOCABULARY.iter().position(|t| t.word == *w).unwrap_or(0));

    // Per-task model lists: `task_models` (current name) or an object `models`.
    let mut models: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let map_src = v
        .get("task_models")
        .filter(|x| x.is_object())
        .or_else(|| v.get("models").filter(|x| x.is_object()));
    if let Some(obj) = map_src.and_then(|x| x.as_object()) {
        for (k, val) in obj {
            let Some(t) = lookup(k) else { continue };
            let list: Vec<String> = val
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            models.insert(t.word, list);
        }
    }
    // A flat `models[]` is the legacy spelling: it applies to every word the box
    // advertises unless the object form already itemised that word.
    let flat = str_arr(v, "models");
    if !flat.is_empty() {
        for w in &words {
            models.entry(*w).or_insert_with(|| flat.clone());
        }
    }

    let mut limits: BTreeMap<&'static str, Value> = BTreeMap::new();
    if let Some(obj) = v.get("limits").and_then(|x| x.as_object()) {
        for (k, val) in obj {
            if let Some(t) = lookup(k) {
                limits.insert(t.word, val.clone());
            }
        }
    }

    // Flat capabilities for legacy consumers. The DECLARED list is
    // authoritative — this change may never widen what a box says it serves —
    // and a declared word that no declared capability covers contributes its
    // own primary capability, so the two views cannot contradict each other.
    let mut capabilities: Vec<String> = caps.clone();
    if tasks_declared {
        for w in &words {
            if let Some(t) = lookup(w) {
                let covered = t
                    .capabilities
                    .iter()
                    .any(|c| capabilities.iter().any(|x| x.eq_ignore_ascii_case(c)));
                if !covered {
                    if let Some(primary) = t.capabilities.first() {
                        capabilities.push((*primary).to_string());
                    }
                }
            }
        }
    }

    TaskMatrix {
        tasks: words,
        models,
        limits,
        tasks_declared,
        caps_declared,
        capabilities,
    }
}

/// Can this task be served by a network advertising `capabilities`?
///
/// The access point's gate for `POST /v1/jobs`: a word whose capability no live
/// provider advertises answers `503 no provider for task=<word>`. It is the
/// whole reason the vocabulary cannot silently degrade into a different kind of
/// work — the decision is here, tested, not inline in the request handler.
pub fn serves(task: &Task, capabilities: &[String]) -> bool {
    task.endpoint.is_some()
        && task
            .capabilities
            .iter()
            .any(|cap| capabilities.iter().any(|c| c.eq_ignore_ascii_case(cap)))
}

/// `GET /v1/tasks` — the vocabulary as JSON, with whether this network can serve
/// each word right now. `served` is measured against the live registry, so the
/// website can generate its word table instead of hand-copying it (rows 219/220).
pub fn tasks_json(live_capabilities: &[String]) -> Value {
    let has = |cap: &str| {
        live_capabilities
            .iter()
            .any(|c| c.eq_ignore_ascii_case(cap))
    };
    let rows: Vec<Value> = VOCABULARY
        .iter()
        .map(|t| {
            let advertised = t.capabilities.iter().any(|c| has(c));
            json!({
                "task": t.word,
                "aliases": t.aliases,
                "accept": t.accept.iter().map(|m| m.as_str()).collect::<Vec<_>>(),
                "produce": t.produce.iter().map(|m| m.as_str()).collect::<Vec<_>>(),
                "capabilities": t.capabilities,
                "endpoint": t.endpoint.map(|e| format!("/{}", e.join("/"))),
                "served": serves(t, live_capabilities),
                "advertised": advertised,
                "pending": if t.endpoint.is_none() { t.pending } else { "" },
            })
        })
        .collect();
    json!({
        "object": "list",
        "default": "chat",
        "data": rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vocabulary_is_unique_and_lookup_covers_aliases() {
        let mut seen = std::collections::HashSet::new();
        for t in VOCABULARY {
            assert!(seen.insert(t.word), "duplicate word {}", t.word);
            for a in t.aliases {
                assert!(seen.insert(*a), "alias {a} collides with a word");
            }
            assert!(!t.accept.is_empty() && !t.produce.is_empty());
        }
        assert_eq!(lookup("chat").unwrap().word, "chat");
        assert_eq!(lookup(" Complete ").unwrap().word, "chat");
        assert_eq!(lookup("text_to_image").unwrap().word, "generate-image");
        assert_eq!(lookup("tts").unwrap().word, "speak");
        assert!(lookup("teleport").is_none());
    }

    #[test]
    fn an_unknown_word_is_refused_and_the_error_lists_the_vocabulary() {
        let e = adapt_v1_jobs(r#"{"task":"teleport"}"#).unwrap_err();
        let (code, msg) = e.status_and_message();
        assert_eq!(code, 400);
        assert!(msg.contains("unknown task 'teleport'"));
        assert!(msg.contains("generate-image") && msg.contains("speak"));
        assert!(msg.contains("nothing was charged"));
    }

    #[test]
    fn no_task_and_no_model_means_chat() {
        let a = adapt_v1_jobs(r#"{"inputs":[{"type":"text","text":"hi"}]}"#).unwrap();
        assert_eq!(a.word, "chat");
        assert_eq!(a.endpoint, CHAT);
        assert_eq!(a.body["messages"][0]["content"], "hi");
        assert_eq!(a.body["model"], "auto");
    }

    #[test]
    fn a_model_with_no_task_is_inferred_from_the_model_family() {
        let a = adapt_v1_jobs(r#"{"model":"sdxl-turbo","prompt":"a cat"}"#).unwrap();
        assert_eq!(a.word, "generate-image");
        let a = adapt_v1_jobs(r#"{"model":"xtts-v2","input":"hello"}"#).unwrap();
        assert_eq!(a.word, "speak");
    }

    #[test]
    fn every_live_word_reaches_its_endpoint_with_the_inputs_placed() {
        let a =
            adapt_v1_jobs(r#"{"task":"generate-image","inputs":[{"type":"text","text":"a cat"}]}"#)
                .unwrap();
        assert_eq!(a.endpoint, IMAGES);
        assert_eq!(a.body["prompt"], "a cat");
        let a = adapt_v1_jobs(
            r#"{"task":"speak","inputs":[{"type":"text","text":"hello"}],"params":{"consent":true,"voice_ref":"AAA"}}"#,
        )
        .unwrap();
        assert_eq!(a.endpoint, SPEECH);
        assert_eq!(a.body["input"], "hello");
        assert_eq!(a.body["consent"], true);
        assert_eq!(a.body["voice_ref"], "AAA");
    }

    #[test]
    fn a_word_without_an_executor_is_501_not_a_fallback() {
        for w in ["caption", "transcribe", "embed", "upscale"] {
            let e = adapt_v1_jobs(&format!(r#"{{"task":"{w}"}}"#)).unwrap_err();
            match &e {
                TaskError::NotImplemented(t, _) => assert_eq!(*t, w),
                other => panic!("{w} must be 501, got {other:?}"),
            }
            assert_eq!(e.status_and_message().0, 501);
        }
    }

    #[test]
    fn a_chat_request_with_messages_passes_them_through_untouched() {
        let a = adapt_v1_jobs(
            r#"{"task":"chat","model":"qwen2.5:0.5b","messages":[{"role":"system","content":"be terse"},{"role":"user","content":"hi"}],"params":{"max_tokens":8}}"#,
        )
        .unwrap();
        assert_eq!(a.body["messages"].as_array().unwrap().len(), 2);
        assert_eq!(a.body["messages"][0]["role"], "system");
        assert_eq!(a.body["model"], "qwen2.5:0.5b");
        assert_eq!(a.body["max_tokens"], 8);
    }

    #[test]
    fn a_chat_request_without_text_is_a_400_not_an_empty_prompt() {
        let e = adapt_v1_jobs(r#"{"task":"chat"}"#).unwrap_err();
        assert_eq!(e.status_and_message().0, 400);
        assert!(e.status_and_message().1.contains("chat needs text"));
    }

    #[test]
    fn endpoints_map_back_to_words_for_the_legacy_clients() {
        assert_eq!(from_endpoint(&["v1", "chat", "completions"]), Some("chat"));
        assert_eq!(from_endpoint(&["v1", "audio", "speech"]), Some("speak"));
        assert_eq!(from_endpoint(&["v1", "embeddings"]), Some("embed"));
        assert_eq!(from_endpoint(&["jobs"]), None);
    }

    #[test]
    fn the_serving_gate_needs_an_advertised_capability_and_an_executor() {
        let none: Vec<String> = vec![];
        // No provider at all ⇒ nothing is served; the handler answers 503.
        assert!(!serves(lookup("speak").unwrap(), &none));
        assert!(!serves(lookup("chat").unwrap(), &none));
        // One provider advertising the capability ⇒ served.
        let llm = vec!["llminference".to_string()]; // case-insensitive on purpose
        assert!(serves(lookup("chat").unwrap(), &llm));
        // A different capability does not stand in for a missing one: an LLM box
        // must never be offered as a speech provider.
        assert!(!serves(lookup("speak").unwrap(), &llm));
        // Advertised but no executor in this build ⇒ still not served.
        let vision = vec!["VisionInference".to_string()];
        assert!(!serves(lookup("caption").unwrap(), &vision));
        // Any-of: a word two capabilities can satisfy is served by either one
        // (constructed here because no such word has an executor yet).
        let any_of = Task {
            word: "translate",
            aliases: &[],
            accept: &[Modality::Text],
            produce: &[Modality::Text],
            capabilities: &["SpeechToText", "LLMInference"],
            endpoint: Some(CHAT),
            pending: "",
        };
        assert!(serves(&any_of, &["SpeechToText".to_string()]));
        assert!(serves(&any_of, &["LLMInference".to_string()]));
        assert!(!serves(&any_of, &["ImageGeneration".to_string()]));
    }

    #[test]
    fn the_new_matrix_shape_parses_into_words_models_and_limits() {
        let v: Value = serde_json::from_str(
            r#"{"tasks":["chat","speak","generate_image"],
                "task_models":{"chat":["qwen2.5:0.5b"],"speak":["xtts-v2"]},
                "limits":{"speak":{"max_seconds":60}}}"#,
        )
        .unwrap();
        let m = parse_advertisement(&v);
        assert!(m.tasks_declared);
        assert_eq!(m.tasks, vec!["chat", "generate-image", "speak"]);
        assert_eq!(m.models_for("chat"), &["qwen2.5:0.5b".to_string()]);
        assert_eq!(m.limits["speak"]["max_seconds"], 60);
        assert!(m.capabilities.iter().any(|c| c == "AudioGeneration"));
        assert_eq!(m.serves("speak", Some("xtts-v2")), Ok(()));
        // A declared word must never be published with a contradicting empty
        // capability list (legacy consumers read that list).
        let v: Value = serde_json::from_str(r#"{"tasks":["chat"]}"#).unwrap();
        let m = parse_advertisement(&v);
        assert_eq!(m.capabilities, vec!["LLMInference".to_string()]);
    }

    #[test]
    fn an_old_capability_plus_flat_model_list_still_parses_the_same_way() {
        let v: Value = serde_json::from_str(
            r#"{"capabilities":["LLMInference","ImageGeneration"],
                "models":["qwen2.5:0.5b","sd-turbo"]}"#,
        )
        .unwrap();
        let m = parse_advertisement(&v);
        assert!(
            !m.tasks_declared,
            "legacy shape must be recognised as legacy"
        );
        assert!(m.advertises("chat") && m.advertises("generate-image"));
        // Derived capabilities are exactly the declared ones: no widening.
        assert_eq!(
            m.capabilities,
            vec!["LLMInference".to_string(), "ImageGeneration".to_string()]
        );
        // The flat list applies to every advertised word, as before.
        assert_eq!(
            m.models_for("chat"),
            &["qwen2.5:0.5b".to_string(), "sd-turbo".to_string()]
        );
        assert_eq!(m.serves("chat", Some("qwen2.5:0.5b")), Ok(()));
        // The cross-word refusal works on a legacy advertisement too (this is the
        // check `POST /v1/jobs` makes when the caller pins a model): `sd-turbo`
        // appears in the flat list, but it serves generate-image.
        let e = m.serves("chat", Some("sd-turbo")).unwrap_err();
        assert!(e.contains("serves task=generate-image"), "{e}");
        // A capability no word covers is still published, never dropped.
        let v: Value = serde_json::from_str(r#"{"capabilities":["MLInference"]}"#).unwrap();
        assert!(parse_advertisement(&v)
            .capabilities
            .iter()
            .any(|c| c == "MLInference"));
    }

    #[test]
    fn a_box_with_a_vision_capability_but_no_vision_model_cannot_take_caption() {
        // The old shape advertised `VisionInference` and the router had nothing
        // else to ask. The matrix asks the second question: which models?
        let v: Value = serde_json::from_str(
            r#"{"capabilities":["VisionInference","ImageGeneration"],"models":["sd-turbo"]}"#,
        )
        .unwrap();
        let m = parse_advertisement(&v);
        // `caption` has no executor in this build: refused for that reason.
        let e = m.serves("caption", None).unwrap_err();
        assert!(e.contains("not served yet"), "{e}");
        // And with a declared task list that simply omits it: refused by name.
        let v: Value = serde_json::from_str(r#"{"tasks":["generate-image"]}"#).unwrap();
        let m = parse_advertisement(&v);
        assert!(!m.advertises("caption"));
        assert!(m
            .serves("speak", None)
            .unwrap_err()
            .contains("no provider advertises"));
    }

    #[test]
    fn a_model_from_another_word_is_refused_on_the_wrong_task() {
        let v: Value = serde_json::from_str(
            r#"{"tasks":["chat","generate-image"],
                "task_models":{"chat":["qwen2.5:0.5b"],"generate-image":["sd-turbo"]}}"#,
        )
        .unwrap();
        let m = parse_advertisement(&v);
        // The bug row 222 exists to close: a diffusion model pinned to chat.
        let e = m.serves("chat", Some("sd-turbo")).unwrap_err();
        assert!(e.contains("serves task=generate-image"), "{e}");
        // A text model pinned to chat is fine; an unlisted text model is not.
        assert_eq!(m.serves("chat", Some("qwen2.5:0.5b")), Ok(()));
        let e = m.serves("chat", Some("llama3.2:1b")).unwrap_err();
        assert!(e.contains("does not list model"), "{e}");
        // No pin: the word alone decides.
        assert_eq!(m.serves("chat", None), Ok(()));
    }

    #[test]
    fn tasks_json_reports_served_from_live_capabilities_not_from_hopes() {
        let caps = vec!["LLMInference".to_string(), "AudioGeneration".to_string()];
        let j = tasks_json(&caps);
        let rows = j["data"].as_array().unwrap();
        let served = |w: &str| {
            rows.iter()
                .find(|r| r["task"] == w)
                .map(|r| r["served"].as_bool().unwrap())
                .unwrap()
        };
        assert!(served("chat") && served("speak"));
        // Advertised by nobody: not served, and never invented as available.
        assert!(!served("generate-image"));
        // In the table, no executor: not served even if a box advertised it.
        assert!(!served("caption"));
        // The chat default is stated, not implied.
        assert_eq!(j["default"], "chat");
    }
}
