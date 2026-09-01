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
