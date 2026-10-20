//! Async client for the local ollama OpenAI-compatible inference endpoint.
//!
//! Calls `POST {OLLAMA_URL}/v1/chat/completions` (default
//! `http://127.0.0.1:11434`) and returns the completion text plus a sha256
//! result hash (the hash is what gets recorded on-chain as the job result).

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Default ollama endpoint (override with env `OLLAMA_URL`).
pub fn default_ollama_url() -> String {
    std::env::var("OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string())
}

/// Marker error returned by providers when ollama is simply not running.
/// Call sites recognise it so they warn **once** (actionably) instead of
/// spamming `pull … failed: error sending request` every poll.
pub const OLLAMA_UNAVAILABLE: &str = "ollama-unavailable";

/// How long ollama should keep the model resident after a request (seconds).
///
/// ollama's default is 5 min; a serving provider that goes idle longer than
/// that pays a full model-load (~13 s on qwen2.5:0.5b) on its next job, which
/// pushes job e2e past the 5 s ceiling. Pin it for 1 h so an always-on node
/// keeps the model warm across idle gaps.
const KEEP_ALIVE_SECS: u64 = 3600;

/// The one-line, actionable guidance printed when ollama is missing.
pub fn ollama_missing_hint() -> String {
    format!(
        "ollama not reachable at {} — install & start it (Linux: scripts/install-ollama.sh · \
         Windows: scripts/install-ollama.ps1, or winget install Ollama.Ollama), then `ollama serve` \
         and `ollama pull llama3.2:1b`. The node still mines, but cannot serve LLM jobs until then.",
        default_ollama_url()
    )
}

/// How long a reachability answer may be reused. The provider asks on every
/// claim pass for every assigned-but-unfinished job; without this a single
/// probe became two `/api/tags` GETs per job per pass (measured ~2.4 req/s of
/// ollama journal noise for days, see change-log 2026-10-19 23:3x).
const REACHABLE_TTL: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a "this model is present" answer may be reused. Model pulls are
/// rare (on demand / baseline), so a stale *present* can only cost one failed
/// request if an operator deletes a model out from under a running provider.
pub const MODEL_PRESENCE_TTL: std::time::Duration = std::time::Duration::from_secs(120);

#[derive(Clone, Copy)]
struct CacheEntry {
    value: bool,
    at: std::time::Instant,
}

fn reachable_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, CacheEntry>> {
    static C: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, CacheEntry>>> =
        std::sync::OnceLock::new();
    C.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn model_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, CacheEntry>> {
    static C: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, CacheEntry>>> =
        std::sync::OnceLock::new();
    C.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Pure freshness predicate (kept separate so it is unit-testable).
/// `Instant::duration_since` saturates, so a clock that appears to go backwards
/// reads as fresh rather than panicking.
pub fn cache_is_fresh(
    recorded: std::time::Instant,
    now: std::time::Instant,
    ttl: std::time::Duration,
) -> bool {
    now.duration_since(recorded) < ttl
}

fn cache_get(
    map: &std::sync::Mutex<std::collections::HashMap<String, CacheEntry>>,
    key: &str,
    ttl: std::time::Duration,
) -> Option<bool> {
    let now = std::time::Instant::now();
    let guard = map.lock().ok()?;
    guard
        .get(key)
        .filter(|e| cache_is_fresh(e.at, now, ttl))
        .map(|e| e.value)
}

fn cache_put(
    map: &std::sync::Mutex<std::collections::HashMap<String, CacheEntry>>,
    key: &str,
    value: bool,
) {
    if let Ok(mut guard) = map.lock() {
        guard.insert(
            key.to_string(),
            CacheEntry {
                value,
                at: std::time::Instant::now(),
            },
        );
    }
}

/// Cached "is `model` already installed?" answer (no HTTP when fresh).
/// `None` = unknown / stale, the caller must look.
pub fn model_presence_cached(model: &str) -> Option<bool> {
    cache_get(model_cache(), model, MODEL_PRESENCE_TTL)
}

/// Record a model-presence answer so the next claim passes need no HTTP.
pub fn remember_model_presence(model: &str, present: bool) {
    cache_put(model_cache(), model, present);
}

/// Drop a cached answer (used when a run fails for a model we believed present).
pub fn forget_model_presence(model: &str) {
    if let Ok(mut guard) = model_cache().lock() {
        guard.remove(model);
    }
}

/// One probe. `Some(false)` = answered but unhappy, `None` = no answer at all.
fn probe_once(base: &str, timeout_ms: u64) -> Option<bool> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .build()
        .ok()?;
    client
        .get(format!("{base}/api/tags"))
        .send()
        .ok()
        .map(|r| r.status().is_success())
}

/// Quick liveness probe: does the local ollama server answer right now?
/// Blocking, but cached for [`REACHABLE_TTL`] so a polling caller cannot turn it
/// into a request stream.
///
/// A single short-timeout probe is **not** enough to declare ollama missing: a
/// measured 4.4 s host-side stall of `ollama serve` (2026-10-19 23:24) tripped
/// the old 1.5 s probe and logged "ollama not reachable — install & start it"
/// while ollama was up and healthy. So one retry with a longer budget is allowed
/// before answering `false`, which makes the warning mean what it says.
pub fn ollama_reachable() -> bool {
    if let Some(v) = cache_get(reachable_cache(), "ollama", REACHABLE_TTL) {
        return v;
    }
    let base = default_ollama_url();
    let base = base.trim_end_matches('/');
    let ok = match probe_once(base, 1500) {
        Some(v) => v,
        None => {
            // Transient stall vs genuinely down: give it one longer chance.
            std::thread::sleep(std::time::Duration::from_millis(250));
            probe_once(base, 6000).unwrap_or(false)
        }
    };
    cache_put(reachable_cache(), "ollama", ok);
    ok
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}
#[derive(Deserialize)]
struct Choice {
    message: Message,
}
#[derive(Deserialize)]
struct Message {
    content: String,
}

/// Run a single chat completion. Returns (text, sha256_hex_of_text).
pub async fn run_inference(
    model: &str,
    prompt: &str,
    temperature: f32,
    max_tokens: u32,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let url = format!("{}/v1/chat/completions", default_ollama_url());
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": false,
        "temperature": temperature,
        "max_tokens": max_tokens,
        "keep_alive": KEEP_ALIVE_SECS,
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;
    let resp = client.post(&url).json(&body).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("ollama error {}: {}", status, &text[..text.len().min(300)]).into());
    }
    let parsed: ChatResponse = resp.json().await?;
    let text = parsed
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .unwrap_or_default();

    let mut h = Sha256::new();
    h.update(text.as_bytes());
    let hash = hex::encode(h.finalize());
    Ok((text, hash))
}

#[derive(Deserialize)]
struct GenResponse {
    response: String,
}
#[derive(Deserialize)]
struct EmbResponse {
    embedding: Vec<f32>,
}

/// Normalize a generic ML task string to `"generate"` or `"embeddings"`.
/// Anything unrecognized (or absent) defaults to `"generate"` — the executor
/// never hard-fails on a malformed task hint.
pub fn normalize_ml_task(task: Option<&str>) -> &'static str {
    match task.map(|t| t.trim().to_ascii_lowercase()) {
        Some(t) if t == "embeddings" || t == "embedding" || t == "embed" => "embeddings",
        _ => "generate",
    }
}

/// Run a generic ML-inference task (non-chat) against the local ollama server.
///
/// `task` = `"generate"` (raw completion of `input`) or `"embeddings"` (a
/// vector for `input`). Returns `(result_json, sha256_hex_of_result)` — the
/// hash is what is committed on-chain; the payload itself stays off-chain.
/// This is the real executor behind the `MLInference` job type.
pub async fn run_ml_inference(
    model: &str,
    input: &str,
    task: &str,
    temperature: f32,
    max_tokens: u32,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let base = default_ollama_url();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    let result_json = if task == "embeddings" {
        let resp = client
            .post(format!("{base}/api/embeddings"))
            .json(&serde_json::json!({"model": model, "prompt": input, "keep_alive": KEEP_ALIVE_SECS}))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(
                format!("ollama error {}: {}", status, &text[..text.len().min(300)]).into(),
            );
        }
        let parsed: EmbResponse = resp.json().await?;
        serde_json::json!({
            "task": "embeddings",
            "model": model,
            "embedding": parsed.embedding,
        })
        .to_string()
    } else {
        let resp = client
            .post(format!("{base}/api/generate"))
            .json(&serde_json::json!({
                "model": model,
                "prompt": input,
                "stream": false,
                "keep_alive": KEEP_ALIVE_SECS,
                "options": {"temperature": temperature, "num_predict": max_tokens},
            }))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(
                format!("ollama error {}: {}", status, &text[..text.len().min(300)]).into(),
            );
        }
        let parsed: GenResponse = resp.json().await?;
        serde_json::json!({
            "task": "generate",
            "model": model,
            "response": parsed.response,
        })
        .to_string()
    };

    let mut h = Sha256::new();
    h.update(result_json.as_bytes());
    Ok((result_json, hex::encode(h.finalize())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ml_task_defaults_and_aliases() {
        assert_eq!(normalize_ml_task(None), "generate");
        assert_eq!(normalize_ml_task(Some("")), "generate");
        assert_eq!(normalize_ml_task(Some("generate")), "generate");
        assert_eq!(normalize_ml_task(Some(" Embeddings ")), "embeddings");
        assert_eq!(normalize_ml_task(Some("embed")), "embeddings");
        assert_eq!(normalize_ml_task(Some("nonsense")), "generate");
    }

    #[test]
    fn cache_freshness_is_bounded_by_its_ttl() {
        let now = std::time::Instant::now();
        let ttl = std::time::Duration::from_secs(120);
        assert!(cache_is_fresh(now, now, ttl));
        assert!(cache_is_fresh(
            now - std::time::Duration::from_secs(119),
            now,
            ttl
        ));
        // At/past the TTL the answer must be re-derived, not reused.
        assert!(!cache_is_fresh(
            now - std::time::Duration::from_secs(120),
            now,
            ttl
        ));
        assert!(!cache_is_fresh(
            now - std::time::Duration::from_secs(600),
            now,
            ttl
        ));
        // A clock that appears to move backwards must read as fresh, never panic.
        assert!(cache_is_fresh(now, now - std::time::Duration::from_secs(5), ttl));
    }

    #[test]
    fn model_presence_is_cached_so_repeat_passes_cost_no_http() {
        let model = "unit-test-cached-model:1b";
        assert_eq!(model_presence_cached(model), None, "unknown before any look");
        remember_model_presence(model, true);
        assert_eq!(model_presence_cached(model), Some(true), "a present model is reused");
        // A retained answer for a *different* model must not leak across keys.
        assert_eq!(model_presence_cached("unit-test-other-model:1b"), None);
        forget_model_presence(model);
        assert_eq!(model_presence_cached(model), None, "forget re-opens the probe");
    }
}
