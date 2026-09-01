//! Off-chain store for LLM job payloads (prompt / model / params / response).
//!
//! The GPN chain deliberately does NOT carry model prompts or responses (size +
//! privacy — see `llm-inference-training-spec.md`). Instead the requestor writes
//! the job spec here (keyed by on-chain job id) and the provider reads it back,
//! runs inference, and writes the response here. The chain only ever sees the
//! sha256 *result hash*.
//!
//! On a single dev machine this is a local JSON file with atomic writes. A
//! distributed deployment would back this with a shared object store or a direct
//! requestor<->provider channel (WebSocket) — the file is a dev stand-in.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Default store location (override with env `GPN_LLM_STORE`).
pub fn default_store_path() -> PathBuf {
    std::env::var("GPN_LLM_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/gpn_llm_store.json"))
}

/// A single LLM job's off-chain payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmJob {
    pub model: String,
    pub prompt: String,
    pub temperature: f32,
    pub max_tokens: u32,
    /// "queued" -> "running" -> "done"
    pub status: String,
    pub response: Option<String>,
    /// sha256 of the response — this is what gets submitted on-chain.
    pub result_hash: Option<String>,
    pub error: Option<String>,
}

impl LlmJob {
    pub fn new(model: &str, prompt: &str, temperature: f32, max_tokens: u32) -> Self {
        Self {
            model: model.to_string(),
            prompt: prompt.to_string(),
            temperature,
            max_tokens,
            status: "queued".into(),
            response: None,
            result_hash: None,
            error: None,
        }
    }
}

/// Thread-safe handle to the store (lazy file load; atomic write-on-mutate).
pub struct LlmStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl LlmStore {
    pub fn new() -> Self {
        Self {
            path: default_store_path(),
            lock: Mutex::new(()),
        }
    }

    pub fn with_path(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    fn load(&self) -> HashMap<u64, LlmJob> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => HashMap::new(),
        }
    }

    fn save(&self, map: &HashMap<u64, LlmJob>) -> std::io::Result<()> {
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(map).unwrap_or_default())?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn put(&self, job_id: u64, job: LlmJob) -> std::io::Result<()> {
        let _g = self.lock.lock().unwrap();
        let mut map = self.load();
        map.insert(job_id, job);
        self.save(&map)
    }

    pub fn get(&self, job_id: u64) -> Option<LlmJob> {
        let _g = self.lock.lock().unwrap();
        self.load().get(&job_id).cloned()
    }

    pub fn set_status(&self, job_id: u64, status: &str) -> std::io::Result<()> {
        let _g = self.lock.lock().unwrap();
        let mut map = self.load();
        if let Some(j) = map.get_mut(&job_id) {
            j.status = status.to_string();
            self.save(&map)?;
        }
        Ok(())
    }

    /// Set the completed response + result hash.
    pub fn complete(&self, job_id: u64, response: &str, result_hash: &str) -> std::io::Result<()> {
        let _g = self.lock.lock().unwrap();
        let mut map = self.load();
        if let Some(j) = map.get_mut(&job_id) {
            j.response = Some(response.to_string());
            j.result_hash = Some(result_hash.to_string());
            j.status = "done".to_string();
            self.save(&map)?;
        }
        Ok(())
    }

    pub fn set_error(&self, job_id: u64, err: &str) -> std::io::Result<()> {
        let _g = self.lock.lock().unwrap();
        let mut map = self.load();
        if let Some(j) = map.get_mut(&job_id) {
            j.error = Some(err.to_string());
            j.status = "error".to_string();
            self.save(&map)?;
        }
        Ok(())
    }
}

/// Convenience: put a new job spec.
pub fn put_job(job_id: u64, job: LlmJob) -> std::io::Result<()> {
    LlmStore::new().put(job_id, job)
}
/// Convenience: get a job spec.
pub fn get_job(job_id: u64) -> Option<LlmJob> {
    LlmStore::new().get(job_id)
}
/// Convenience: mark a job's response complete.
pub fn set_response(job_id: u64, response: &str, result_hash: &str) -> std::io::Result<()> {
    LlmStore::new().complete(job_id, response, result_hash)
}

impl Default for LlmStore {
    fn default() -> Self {
        Self::new()
    }
}
