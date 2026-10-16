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
use std::sync::{Mutex, OnceLock};

/// Process-global cache of the parsed store, keyed by path, invalidated when the
/// file's mtime changes. Without this, every `get()` re-reads + JSON-parses the
/// entire `llm_store.json` — the provider's 300ms poll loop called it hundreds of
/// times/second and pegged the CPU. Multi-process safe: writers update the cache
/// on save, and a changed mtime forces a reload (so the AP sees the provider's
/// writes and vice-versa).
fn store_cache() -> &'static Mutex<HashMap<PathBuf, (u128, HashMap<u64, LlmJob>)>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, (u128, HashMap<u64, LlmJob>)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn file_mtime_ns(path: &PathBuf) -> u128 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Default store location (override with env `GPN_LLM_STORE`).
///
/// Defaults to a PERSISTENT, shared path (`~/.gpn/llm_store.json`) so the API and
/// the providers agree even without the env var. (A temp path differs per process
/// and, worse, loses the store on reboot.)
pub fn default_store_path() -> PathBuf {
    std::env::var("GPN_LLM_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(format!("{home}/.gpn/llm_store.json"))
        })
}

/// A single generation job's off-chain payload. Used for LLM inference AND
/// image/video generation — the job-type is carried in `kind`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmJob {
    /// "llm" | "image" | "video" | "finetune" | "ml"
    pub kind: String,
    pub model: String,
    pub prompt: String,
    pub temperature: f32,
    pub max_tokens: u32,
    /// Fine-tune dataset: JSON array of {"prompt":..., "completion":...}
    pub dataset: Option<String>,
    /// Fine-tune hyperparams
    pub lora_r: Option<u32>,
    pub epochs: Option<u32>,
    /// Generic ML-inference task (kind = "ml"): "generate" (default) or
    /// "embeddings". `#[serde(default)]` keeps older store entries readable.
    #[serde(default)]
    pub task: Option<String>,
    /// Generation params (image/video):
    pub negative_prompt: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub steps: Option<u32>,
    pub seed: Option<i64>,
    pub frames: Option<u32>,
    pub fps: Option<u32>,
    /// "queued" -> "running" -> "done"
    pub status: String,
    pub response: Option<String>,
    /// sha256 of the response (LLM) or of the artifact file (image/video) —
    /// this is what gets submitted on-chain.
    pub result_hash: Option<String>,
    /// Random per-job salt. The on-chain commitment is `sha256(salt || payload)`,
    /// so the chain value is opaque to observers while anyone holding this salt
    /// can verify the payload. Kept off-chain (with the answer).
    pub salt: Option<String>,
    /// Where the generated artifact file was written (image/video).
    pub artifact_path: Option<String>,
    /// e.g. "image/png", "video/mp4"
    pub artifact_media_type: Option<String>,
    pub error: Option<String>,
}

impl LlmJob {
    /// New LLM-inference job.
    pub fn new_llm(model: &str, prompt: &str, temperature: f32, max_tokens: u32) -> Self {
        Self {
            kind: "llm".into(),
            model: model.to_string(),
            prompt: prompt.to_string(),
            temperature,
            max_tokens,
            dataset: None,
            lora_r: None,
            epochs: None,
            task: None,
            negative_prompt: None,
            width: None,
            height: None,
            steps: None,
            seed: None,
            frames: None,
            fps: None,
            status: "queued".into(),
            response: None,
            result_hash: None,
            artifact_path: None,
            artifact_media_type: None,
            error: None,
            salt: None,
        }
    }

    /// New image/video generation job.
    #[allow(clippy::too_many_arguments)]
    pub fn new_gen(
        kind: &str,
        model: &str,
        prompt: &str,
        negative_prompt: Option<&str>,
        width: u32,
        height: u32,
        steps: u32,
        seed: Option<i64>,
        frames: u32,
        fps: u32,
    ) -> Self {
        Self {
            kind: kind.to_string(),
            model: model.to_string(),
            prompt: prompt.to_string(),
            temperature: 0.7,
            max_tokens: 0,
            dataset: None,
            lora_r: None,
            epochs: None,
            task: None,
            negative_prompt: negative_prompt.map(|s| s.to_string()),
            width: Some(width),
            height: Some(height),
            steps: Some(steps),
            seed: Some(seed.unwrap_or_else(rand_like_seed)),
            frames: Some(frames),
            fps: Some(fps),
            status: "queued".into(),
            response: None,
            result_hash: None,
            artifact_path: None,
            artifact_media_type: None,
            error: None,
            salt: None,
        }
    }
    /// New fine-tuning job (kind = "finetune"). dataset is a JSON array string
    /// of {prompt, completion} pairs; the executor trains a LoRA adapter.
    pub fn new_finetune(model: &str, dataset: &str, lora_r: u32, epochs: u32) -> Self {
        Self {
            kind: "finetune".into(),
            model: model.to_string(),
            prompt: String::new(),
            temperature: 0.7,
            max_tokens: 0,
            dataset: Some(dataset.to_string()),
            lora_r: Some(lora_r),
            epochs: Some(epochs),
            task: None,
            negative_prompt: None,
            width: None,
            height: None,
            steps: None,
            seed: Some(rand_like_seed()),
            frames: None,
            fps: None,
            status: "queued".into(),
            response: None,
            result_hash: None,
            artifact_path: None,
            artifact_media_type: None,
            error: None,
            salt: None,
        }
    }

    /// New generic ML-inference job (kind = "ml"). `input` is the model input
    /// (a prompt / text to embed); `task` selects the executor ("generate" for
    /// raw completion, "embeddings" for a vector). Temperature/max_tokens are
    /// honoured by the `generate` task.
    pub fn new_ml(model: &str, input: &str, task: &str, temperature: f32, max_tokens: u32) -> Self {
        Self {
            kind: "ml".into(),
            model: model.to_string(),
            prompt: input.to_string(),
            temperature,
            max_tokens,
            dataset: None,
            lora_r: None,
            epochs: None,
            task: Some(task.to_string()),
            negative_prompt: None,
            width: None,
            height: None,
            steps: None,
            seed: None,
            frames: None,
            fps: None,
            status: "queued".into(),
            response: None,
            result_hash: None,
            artifact_path: None,
            artifact_media_type: None,
            error: None,
            salt: None,
        }
    }
}

fn rand_like_seed() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    (n as i64) & 0x7fff_ffff
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
        let mtime = file_mtime_ns(&self.path);
        let mut cache = store_cache().lock().unwrap();
        if let Some((m, map)) = cache.get(&self.path) {
            // Cache hit: file unchanged since we last parsed it.
            if *m == mtime && !(mtime == 0 && map.is_empty()) {
                return map.clone();
            }
        }
        let map: HashMap<u64, LlmJob> = match std::fs::read_to_string(&self.path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => HashMap::new(),
        };
        cache.insert(self.path.clone(), (mtime, map.clone()));
        map
    }

    fn save(&self, map: &HashMap<u64, LlmJob>) -> std::io::Result<()> {
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(map).unwrap_or_default())?;
        std::fs::rename(&tmp, &self.path)?;
        // Keep the cache warm + consistent after our own write.
        let mtime = file_mtime_ns(&self.path);
        store_cache()
            .lock()
            .unwrap()
            .insert(self.path.clone(), (mtime, map.clone()));
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
        // Fast path: on an mtime cache hit, return ONLY the requested entry
        // instead of deep-cloning the entire job map. `load()` clones the full
        // map on every hit, and the provider poll loop calls get_job() ~128x
        // per 300ms pass, so cloning the whole (~4.3k-entry) map each time was a
        // sustained ~90-100% CPU burn that scaled with the ledger. Semantics are
        // unchanged: on a miss (or stale cache) we fall back to load().
        let mtime = file_mtime_ns(&self.path);
        {
            let cache = store_cache().lock().unwrap();
            if let Some((m, map)) = cache.get(&self.path) {
                if *m == mtime && !(mtime == 0 && map.is_empty()) {
                    return map.get(&job_id).cloned();
                }
            }
        }
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
    pub fn complete(
        &self,
        job_id: u64,
        response: &str,
        result_hash: &str,
        salt: Option<&str>,
    ) -> std::io::Result<()> {
        let _g = self.lock.lock().unwrap();
        let mut map = self.load();
        if let Some(j) = map.get_mut(&job_id) {
            j.response = Some(response.to_string());
            j.result_hash = Some(result_hash.to_string());
            if let Some(s) = salt {
                j.salt = Some(s.to_string());
            }
            j.status = "done".to_string();
            self.save(&map)?;
        }
        Ok(())
    }

    /// Set a completed generation artifact (image/video file) + its sha256 hash.
    pub fn complete_artifact(
        &self,
        job_id: u64,
        artifact_path: &str,
        media_type: &str,
        result_hash: &str,
    ) -> std::io::Result<()> {
        let _g = self.lock.lock().unwrap();
        let mut map = self.load();
        if let Some(j) = map.get_mut(&job_id) {
            j.artifact_path = Some(artifact_path.to_string());
            j.artifact_media_type = Some(media_type.to_string());
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
pub fn set_response(
    job_id: u64,
    response: &str,
    result_hash: &str,
    salt: Option<&str>,
) -> std::io::Result<()> {
    LlmStore::new().complete(job_id, response, result_hash, salt)
}

impl Default for LlmStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_ml_sets_kind_task_and_roundtrips() {
        let j = LlmJob::new_ml("qwen2.5:1.5b", "hello world", "embeddings", 0.7, 64);
        assert_eq!(j.kind, "ml");
        assert_eq!(j.task.as_deref(), Some("embeddings"));
        assert_eq!(j.model, "qwen2.5:1.5b");
        assert_eq!(j.prompt, "hello world");
        let s = serde_json::to_string(&j).unwrap();
        let back: LlmJob = serde_json::from_str(&s).unwrap();
        assert_eq!(back.kind, "ml");
        assert_eq!(back.task.as_deref(), Some("embeddings"));
        assert_eq!(back.prompt, "hello world");
    }

    #[test]
    fn legacy_entry_without_task_field_still_loads() {
        // Entries written before the `task` field existed must still parse
        // (it is `#[serde(default)]`), so upgrades don't drop in-flight jobs.
        let legacy = r#"{"kind":"llm","model":"m","prompt":"p","temperature":0.7,"max_tokens":1,"dataset":null,"lora_r":null,"epochs":null,"negative_prompt":null,"width":null,"height":null,"steps":null,"seed":null,"frames":null,"fps":null,"status":"queued","response":null,"result_hash":null,"salt":null,"artifact_path":null,"artifact_media_type":null,"error":null}"#;
        let j: LlmJob = serde_json::from_str(legacy).unwrap();
        assert_eq!(j.kind, "llm");
        assert!(j.task.is_none());
    }
}
