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

/// Quick liveness probe: does the local ollama server answer right now?
/// Blocking + short timeout so providers can cheaply gate model pulls.
pub fn ollama_reachable() -> bool {
    let base = default_ollama_url();
    let base = base.trim_end_matches('/');
    let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(1500))
        .build()
    else {
        return false;
    };
    client
        .get(format!("{base}/api/tags"))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
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
}
