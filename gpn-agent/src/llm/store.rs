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
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Identity of one version of the store file: (last-write time in ns, length).
///
/// mtime ALONE is not a change detector on Windows: NTFS updates the directory
/// entry lazily, so a write can be readable while `metadata()` still reports the
/// **previous** last-write time — and then the cache serves the pre-write map
/// (seen live: a legacy-file read after a write returned `None`). The length is
/// free (same `metadata()` call) and changes for any realistic job update.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FileSig {
    mtime_ns: u128,
    len: u64,
}

/// Process-global cache of the parsed store, keyed by path, invalidated when the
/// file's signature changes. Without this, every `get()` re-reads + JSON-parses
/// the entire `llm_store.json` — the provider's 300ms poll loop called it
/// hundreds of times/second and pegged the CPU. Multi-process safe: writers
/// update the cache on save, and a changed signature forces a reload (so the AP
/// sees the provider's writes and vice-versa).
fn store_cache() -> &'static Mutex<HashMap<PathBuf, (FileSig, HashMap<u64, LlmJob>)>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, (FileSig, HashMap<u64, LlmJob>)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn file_sig(path: &Path) -> FileSig {
    match std::fs::metadata(path) {
        Ok(m) => FileSig {
            mtime_ns: m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            len: m.len(),
        },
        Err(_) => FileSig {
            mtime_ns: 0,
            len: 0,
        },
    }
}

/// Rate-limited notice that the ledger exists but could not be read.
///
/// The provider poll loop calls `get_job` ~128 times per 300 ms pass, so a
/// damaged file must not print a line per lookup. One line per 10 s names the
/// path and the reason; recovery is silent, because the next read simply
/// succeeds. Nothing here fabricates an empty store (item #183(a)).
fn note_store_read_failure(path: &Path, why: &str) {
    use std::sync::atomic::{AtomicI64, Ordering};
    static LAST_NOTICE: AtomicI64 = AtomicI64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let last = LAST_NOTICE.load(Ordering::Relaxed);
    if now.saturating_sub(last) >= 10
        && LAST_NOTICE
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        eprintln!(
            "[store] {} is UNKNOWN, not empty ({why}) — serving the last good ledger, \
             rewriting nothing, and retrying on the next read",
            path.display()
        );
    }
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

/// Portable cross-process advisory lock for a shared store file.
///
/// The access-point, the LLM/diffusion/ML providers and the provider heartbeat
/// all share `~/.gpn/llm_store.json` + `~/.gpn/assignments.json` and each does a
/// read-modify-write of the WHOLE file. A per-process `Mutex` does not stop one
/// process from clobbering another's write with a stale in-memory map (a lost
/// update — e.g. the AP's `put()` for job 4858 was overwritten by an unrelated
/// provider `complete()` that had loaded the file first). This serialises the
/// RMW across processes with an atomic lock DIRECTORY (`create_dir` is atomic on
/// every OS); a stale lock left by a crashed holder is broken after `STALE`.
/// Std-only ⇒ no new deps, builds on Linux + Windows; the critical sections here
/// are a few milliseconds.
pub struct StoreLock {
    dir: PathBuf,
}

impl StoreLock {
    pub fn acquire(target: &Path) -> std::io::Result<StoreLock> {
        let dir = target.with_extension("lockdir");
        if let Some(parent) = dir.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let start = Instant::now();
        loop {
            match std::fs::create_dir(&dir) {
                Ok(_) => return Ok(StoreLock { dir }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Break a stale lock left by a crashed writer.
                    if let Ok(md) = std::fs::metadata(&dir) {
                        if let Ok(modified) = md.modified() {
                            if modified
                                .elapsed()
                                .map(|d| d.as_secs() > 30)
                                .unwrap_or(false)
                            {
                                let _ = std::fs::remove_dir_all(&dir);
                                continue;
                            }
                        }
                    }
                    // Never deadlock the caller: after a bounded wait, proceed
                    // (a single raced write is far better than a stalled AP).
                    if start.elapsed() >= Duration::from_secs(10) {
                        return Ok(StoreLock {
                            dir: PathBuf::new(),
                        });
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        if !self.dir.as_os_str().is_empty() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// Read-modify-write one key into a shared JSON *object* file under the
/// cross-process [`StoreLock`] (used for `assignments.json`).
pub fn merge_json_object(path: &Path, key: &str, value: serde_json::Value) -> std::io::Result<()> {
    let _lock = StoreLock::acquire(path)?;
    let mut m: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    m.insert(key.to_string(), value);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(path, serde_json::Value::Object(m).to_string())
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
    /// Token counts reported by the inference backend (item #53). `None` when
    /// the backend did not report them (or for artifact/embedding jobs); never a
    /// guess. `#[serde(default)]` keeps every older store entry readable.
    #[serde(default)]
    pub prompt_tokens: Option<u32>,
    #[serde(default)]
    pub completion_tokens: Option<u32>,
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
            prompt_tokens: None,
            completion_tokens: None,
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
            prompt_tokens: None,
            completion_tokens: None,
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
            prompt_tokens: None,
            completion_tokens: None,
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
            prompt_tokens: None,
            completion_tokens: None,
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

    /// Read + parse the ledger, keeping "absent" and "unreadable" apart.
    ///
    /// `Ok(None)` = the file genuinely does not exist (a fresh host — an empty
    /// store is the truth). `Err` = the file exists but could not be read or
    /// parsed, which is UNKNOWN, never an empty store (item #183(a)): the AP
    /// published "no payload" for a job whose entry was still on disk, and a
    /// mutator that treated an unparseable read as `{}` would rewrite the whole
    /// 4.5k-entry ledger down to the single job it was adding.
    fn read_map(&self) -> Result<Option<HashMap<u64, LlmJob>>, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => match serde_json::from_str::<HashMap<u64, LlmJob>>(&s) {
                Ok(map) => Ok(Some(map)),
                Err(e) => Err(format!("unparseable: {e}")),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("unreadable: {e}")),
        }
    }

    fn load(&self) -> HashMap<u64, LlmJob> {
        let sig = file_sig(&self.path);
        let mut cache = store_cache().lock().unwrap();
        if let Some((s, map)) = cache.get(&self.path) {
            // Cache hit: file unchanged since we last parsed it.
            if *s == sig && !(sig.mtime_ns == 0 && map.is_empty()) {
                return map.clone();
            }
        }
        match self.read_map() {
            Ok(Some(map)) => {
                cache.insert(self.path.clone(), (sig, map.clone()));
                map
            }
            Ok(None) => {
                cache.insert(self.path.clone(), (sig, HashMap::new()));
                HashMap::new()
            }
            Err(why) => {
                // Do NOT cache an empty map here: cache it once and every
                // lookup in this process answers "no payload" until the file
                // happens to change again. Serve the last good map instead and
                // retry the file on the next call.
                let last = cache.get(&self.path).map(|(_, m)| m.clone());
                note_store_read_failure(&self.path, &why);
                last.unwrap_or_default()
            }
        }
    }

    /// Always read the store from disk, bypassing the process cache. Used by the
    /// mutators *under the cross-process lock* so a stale in-memory map can never
    /// overwrite another process's concurrent write.
    ///
    /// Returns `Err` rather than an empty map when the file exists but cannot be
    /// parsed: a read-modify-write that started from `{}` would persist that
    /// empty map and delete every other job in the ledger.
    fn load_fresh(&self) -> std::io::Result<HashMap<u64, LlmJob>> {
        match self.read_map() {
            Ok(Some(map)) => Ok(map),
            Ok(None) => Ok(HashMap::new()),
            Err(why) => {
                note_store_read_failure(&self.path, &why);
                Err(std::io::Error::other(format!(
                    "refusing to rewrite {}: {why}",
                    self.path.display()
                )))
            }
        }
    }

    fn save(&self, map: &HashMap<u64, LlmJob>) -> std::io::Result<()> {
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(map).unwrap_or_default())?;
        std::fs::rename(&tmp, &self.path)?;
        // Keep the cache warm + consistent after our own write.
        let sig = file_sig(&self.path);
        store_cache()
            .lock()
            .unwrap()
            .insert(self.path.clone(), (sig, map.clone()));
        Ok(())
    }

    pub fn put(&self, job_id: u64, job: LlmJob) -> std::io::Result<()> {
        let _g = self.lock.lock().unwrap();
        let _x = StoreLock::acquire(&self.path)?;
        let mut map = self.load_fresh()?;
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
        let sig = file_sig(&self.path);
        {
            let cache = store_cache().lock().unwrap();
            if let Some((s, map)) = cache.get(&self.path) {
                if *s == sig && !(sig.mtime_ns == 0 && map.is_empty()) {
                    return map.get(&job_id).cloned();
                }
            }
        }
        self.load().get(&job_id).cloned()
    }

    /// Split the ledger's ids into (matching `pred`, not matching) in ONE pass.
    ///
    /// Item #59: a caller that loops `for id in 0..next { get_job(id) }` pays a
    /// full ledger parse whenever another process (the LLM provider) has written
    /// the shared store since the last call — with a ~5 MB ledger written a few
    /// times a second, that is a per-id re-parse, not a per-id lookup. Ask for
    /// the two sets you need instead: one parse, then iterate `HashSet`s.
    pub fn ids_partitioned<F: Fn(&LlmJob) -> bool>(&self, pred: F) -> (Vec<u64>, Vec<u64>) {
        fn split<F: Fn(&LlmJob) -> bool>(
            map: &HashMap<u64, LlmJob>,
            pred: F,
        ) -> (Vec<u64>, Vec<u64>) {
            let (mut yes, mut no) = (Vec::new(), Vec::new());
            for (id, j) in map.iter() {
                if pred(j) {
                    yes.push(*id);
                } else {
                    no.push(*id);
                }
            }
            (yes, no)
        }
        let _g = self.lock.lock().unwrap();
        let sig = file_sig(&self.path);
        {
            let cache = store_cache().lock().unwrap();
            if let Some((s, map)) = cache.get(&self.path) {
                if *s == sig && !(sig.mtime_ns == 0 && map.is_empty()) {
                    return split(map, pred);
                }
            }
        }
        split(&self.load(), pred)
    }

    pub fn set_status(&self, job_id: u64, status: &str) -> std::io::Result<()> {
        let _g = self.lock.lock().unwrap();
        let _x = StoreLock::acquire(&self.path)?;
        let mut map = self.load_fresh()?;
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
        self.complete_with_usage(job_id, response, result_hash, salt, None)
    }

    /// Complete a job **and** record the backend token counts in ONE write
    /// (item #53 residual).
    ///
    /// `complete` + a following `set_usage` is two separate read-modify-writes,
    /// so `status == "done"` became visible a few hundred microseconds before the
    /// counts did. Every waiter (`gpn_scheduler`'s 300 ms chat poll, a remote
    /// AP's result callback) reads on `done` and would therefore report
    /// `usage {0,0,0}` for a job whose counts were in fact known — the exact
    /// symptom #53 was supposed to remove, still reachable via the race.
    /// Writing both in one locked save removes the window entirely: any observer
    /// that can see `done` can also see the counts.
    pub fn complete_with_usage(
        &self,
        job_id: u64,
        response: &str,
        result_hash: &str,
        salt: Option<&str>,
        usage: Option<(u32, u32)>,
    ) -> std::io::Result<()> {
        let _g = self.lock.lock().unwrap();
        let _x = StoreLock::acquire(&self.path)?;
        let mut map = self.load_fresh()?;
        if let Some(j) = map.get_mut(&job_id) {
            j.response = Some(response.to_string());
            j.result_hash = Some(result_hash.to_string());
            if let Some(s) = salt {
                j.salt = Some(s.to_string());
            }
            if let Some((p, c)) = usage {
                j.prompt_tokens = Some(p);
                j.completion_tokens = Some(c);
            }
            j.status = "done".to_string();
            self.save(&map)?;
        }
        Ok(())
    }

    /// Record the backend-reported token usage for a job (item #53). Separate
    /// from `complete` so every completion path keeps its existing signature.
    pub fn set_usage(
        &self,
        job_id: u64,
        prompt_tokens: Option<u32>,
        completion_tokens: Option<u32>,
    ) -> std::io::Result<()> {
        let _g = self.lock.lock().unwrap();
        let _x = StoreLock::acquire(&self.path)?;
        let mut map = self.load_fresh()?;
        if let Some(j) = map.get_mut(&job_id) {
            j.prompt_tokens = prompt_tokens;
            j.completion_tokens = completion_tokens;
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
        let _x = StoreLock::acquire(&self.path)?;
        let mut map = self.load_fresh()?;
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
        let _x = StoreLock::acquire(&self.path)?;
        let mut map = self.load_fresh()?;
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
/// Convenience: mark a job complete *with* its backend token counts in one write.
pub fn set_response_with_usage(
    job_id: u64,
    response: &str,
    result_hash: &str,
    salt: Option<&str>,
    usage: Option<(u32, u32)>,
) -> std::io::Result<()> {
    LlmStore::new().complete_with_usage(job_id, response, result_hash, salt, usage)
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

    /// Item #53: token usage is persisted per job and an entry written before the
    /// fields existed still loads (they default to `None`, never an error).
    #[test]
    fn completion_and_usage_land_in_one_write() {
        // Item #53 residual: `complete()` then `set_usage()` is two writes, so a
        // waiter that keys on `status == "done"` (the AP's 300 ms chat poll)
        // could see the completion before the counts and report `usage {0,0,0}`.
        // The atomic path must make every field visible at once, and reading the
        // record straight after it must already carry the counts.
        let dir = std::env::temp_dir().join(format!("gpn-usage-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("llm_store.json");
        let s = LlmStore::with_path(p.clone());
        s.put(11, LlmJob::new_llm("m", "p", 0.7, 16)).unwrap();
        s.complete_with_usage(11, "answer", "0xbeef", None, Some((36, 2)))
            .unwrap();
        let j = s.get(11).unwrap();
        assert_eq!(j.status, "done");
        assert_eq!(j.response.as_deref(), Some("answer"));
        assert_eq!(
            (j.prompt_tokens, j.completion_tokens),
            (Some(36), Some(2)),
            "counts must be visible in the same read that sees `done`"
        );
        // A backend that reported nothing leaves the counts unset — unknown is
        // never invented as a real 0.
        s.put(12, LlmJob::new_llm("m", "p", 0.7, 16)).unwrap();
        s.complete_with_usage(12, "answer", "0xbeef", None, None)
            .unwrap();
        let j = s.get(12).unwrap();
        assert_eq!(j.status, "done");
        assert_eq!((j.prompt_tokens, j.completion_tokens), (None, None));
        // `complete()` keeps working (delegates with no usage).
        s.put(13, LlmJob::new_llm("m", "p", 0.7, 16)).unwrap();
        s.complete(13, "x", "0x0", None).unwrap();
        assert_eq!(s.get(13).unwrap().status, "done");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn usage_roundtrips_and_legacy_entries_still_load() {
        let dir = std::env::temp_dir().join(format!("gpn-usage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("llm_store.json");
        let s = LlmStore::with_path(p.clone());
        s.put(7, LlmJob::new_llm("m", "p", 0.7, 16)).unwrap();
        // A fresh job carries no counts — 0 must not be fabricated.
        assert_eq!(s.get(7).unwrap().prompt_tokens, None);
        s.complete(7, "hello", "0xdead", None).unwrap();
        s.set_usage(7, Some(11), Some(3)).unwrap();
        let j = s.get(7).unwrap();
        assert_eq!(j.prompt_tokens, Some(11));
        assert_eq!(j.completion_tokens, Some(3));
        assert_eq!(j.status, "done", "usage must not disturb completion state");
        // A pre-#53 record (no usage keys at all) parses with `None`.
        std::fs::write(
            &p,
            r#"{"8":{"kind":"llm","model":"m","prompt":"p","temperature":0.7,"max_tokens":1,"dataset":null,"lora_r":null,"epochs":null,"task":null,"negative_prompt":null,"width":null,"height":null,"steps":null,"seed":null,"frames":null,"fps":null,"status":"done","response":"x","result_hash":"0x0","salt":null,"artifact_path":null,"artifact_media_type":null,"error":null}}"#,
        )
        .unwrap();
        let legacy = LlmStore::with_path(p).get(8).unwrap();
        assert_eq!(legacy.prompt_tokens, None);
        assert_eq!(legacy.completion_tokens, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_lock_is_mutually_exclusive() {
        let dir = std::env::temp_dir().join(format!("gpn-lock-{}", std::process::id()));
        let target = dir.join("store.json");
        let a = StoreLock::acquire(&target).unwrap();
        // A second acquire must block until the first is dropped.
        let handle = std::thread::spawn(move || {
            let b = StoreLock::acquire(&target).unwrap();
            drop(b);
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!handle.is_finished(), "second lock acquired while held");
        drop(a);
        handle.join().unwrap();
    }

    #[test]
    fn merge_json_object_preserves_existing_keys() {
        let dir = std::env::temp_dir().join(format!("gpn-merge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("assignments.json");
        merge_json_object(&p, "1", serde_json::json!({"a": 1})).unwrap();
        merge_json_object(&p, "2", serde_json::json!({"b": 2})).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["1"]["a"], 1);
        assert_eq!(v["2"]["b"], 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_puts_do_not_lose_entries() {
        // Two independent handles (as the AP and a provider are) writing to the
        // SAME file concurrently must not clobber each other — the lost-update
        // defect that dropped the #4858 payload.
        let dir = std::env::temp_dir().join(format!("gpn-conc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("llm_store.json");
        let p1 = p.clone();
        let p2 = p.clone();
        let t1 = std::thread::spawn(move || {
            let s = LlmStore::with_path(p1);
            for i in 0..50u64 {
                s.put(i, LlmJob::new_llm("m", "p", 0.7, 1)).unwrap();
            }
        });
        let t2 = std::thread::spawn(move || {
            let s = LlmStore::with_path(p2);
            for i in 50..100u64 {
                s.put(i, LlmJob::new_llm("m", "p", 0.7, 1)).unwrap();
            }
        });
        t1.join().unwrap();
        t2.join().unwrap();
        let s = LlmStore::with_path(p);
        for i in 0..100u64 {
            assert!(s.get(i).is_some(), "entry {i} lost to a concurrent write");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unparseable_store_is_unknown_not_empty_and_is_never_truncated() {
        // Item #183(a): `/assignments` handed out leases with no payload because
        // the store read collapsed an unreadable file to an empty map. Two
        // consequences were live: every lookup answered "no payload" for the
        // whole process (the cached empty map), and a mutator would have saved
        // that empty map over a 4.5k-entry ledger.
        let dir = std::env::temp_dir().join(format!("gpn-unknown-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("llm_store.json");
        let s = LlmStore::with_path(p.clone());
        for id in 1..=3u64 {
            s.put(id, LlmJob::new_llm("m", "p", 0.7, 16)).unwrap();
        }
        let good = std::fs::read_to_string(&p).unwrap();

        // Damage the file the way a torn/partial write would.
        std::fs::write(&p, &good[..good.len() / 2]).unwrap();

        // A read is UNKNOWN, not empty: the last good map is still served, so a
        // lease does not lose its payload just because the file is mid-repair.
        assert!(
            s.get(1).is_some(),
            "an unparseable store must not read as 'this job has no payload'"
        );

        // A write must refuse rather than persist an empty map + one job.
        let err = s.put(99, LlmJob::new_llm("m", "p", 0.7, 16));
        assert!(err.is_err(), "put() rewrote a ledger it could not read");
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            &good[..good.len() / 2],
            "a failed read must leave the file byte-identical"
        );

        // Once the file is valid again the same handle sees the items.
        std::fs::write(&p, &good).unwrap();
        assert!(s.get(2).is_some());
        s.put(99, LlmJob::new_llm("m", "p", 0.7, 16)).unwrap();
        for id in [1u64, 2, 3, 99] {
            assert!(s.get(id).is_some(), "entry {id} lost after a damaged read");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_store_is_genuinely_empty() {
        // Absent is not unreadable: a fresh host has no ledger and must still be
        // able to write the first job (otherwise #183's fix would deadlock it).
        let dir = std::env::temp_dir().join(format!("gpn-absent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("llm_store.json");
        let s = LlmStore::with_path(p);
        assert!(s.get(1).is_none());
        s.put(1, LlmJob::new_llm("m", "p", 0.7, 16)).unwrap();
        assert!(s.get(1).is_some());
        let _ = std::fs::remove_dir_all(&dir);
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
