//! Public image-generation jobs: a durable, pollable record of one
//! `/v1/images/generations` request.
//!
//! The OpenAI image API is **asynchronous in practice**: a 512²/8-step
//! `sd-turbo` render measured 28.9 s here, and the cost grows with
//! `megapixels × steps` (a 1024²/30 render is ≈63 s), so holding the connection
//! would collide with the Cloudflare Worker's 110 s upstream limit. The AP
//! therefore answers `202` with an id and a `poll_url`, and this module is the
//! state that poll reads.
//!
//! ## Storage
//!
//! One JSON file per job plus one file per rendered image, under
//! `$GPN_IMAGE_DIR` (default `~/.gpn/images`). No database: the AP already
//! keeps its channel registry and job payloads as JSON on disk, and a job
//! record is rewritten a handful of times, never queried by anything but id.
//!
//! **Ids are capability URLs** — 128 bits of hash-derived hex — so the public
//! `GET /v1/images/{id}` needs no auth header and cannot be enumerated. The
//! id is validated before it touches the filesystem, which is what stops
//! `../` traversal.
//!
//! ## GC
//!
//! Every record carries `expires_at`. [`gc`] deletes expired records *and*
//! their images; the AP runs it at boot and once an hour. A client that polls
//! after expiry gets the same 404 as an unknown id, which is honest: the bytes
//! are gone.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// How long a finished image stays downloadable. Long enough to be useful on a
/// page, short enough that the testnet box's disk does not fill up.
pub const DEFAULT_TTL_SECS: u64 = 24 * 60 * 60;
/// Ids are 128 bits of hex. 32 characters, lowercase.
const ID_LEN: usize = 32;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageArtifact {
    /// Position in the caller's `n` sequence (0-based).
    pub index: u32,
    pub media_type: String,
    pub sha256: String,
    pub bytes: u64,
    /// The size actually produced (the helper may snap to a multiple of 8/64).
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageJob {
    pub id: String,
    pub object: String,
    /// `pending` | `completed` | `failed`.
    pub status: String,
    pub created: u64,
    pub updated: u64,
    pub expires_at: u64,
    pub model: String,
    pub prompt: String,
    pub revised_prompt: Option<String>,
    /// `"512x512"` exactly as the caller wrote it.
    pub size: String,
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    pub seed: i64,
    pub n: u32,
    /// `channel` (off-chain signed settlement), `onchain` (per-request escrow)
    /// or `pending` until the render settles.
    pub payment: String,
    pub channel_id: Option<u64>,
    pub job_id: Option<u64>,
    pub price_gpn: f64,
    pub estimated_seconds: f64,
    #[serde(default)]
    pub data: Vec<ImageArtifact>,
    pub error: Option<String>,
}

impl ImageJob {
    pub fn new(id: String, prompt: &str, model: &str, width: u32, height: u32, steps: u32, seed: i64, n: u32, estimated_seconds: f64, price_gpn: f64) -> Self {
        let now = now_secs();
        Self {
            id,
            object: "image.generation".to_string(),
            status: "pending".to_string(),
            created: now,
            updated: now,
            expires_at: now + ttl_secs(),
            model: model.to_string(),
            prompt: prompt.to_string(),
            revised_prompt: None,
            size: format!("{width}x{height}"),
            width,
            height,
            steps,
            seed,
            n,
            payment: "pending".to_string(),
            channel_id: None,
            job_id: None,
            price_gpn,
            estimated_seconds,
            data: Vec::new(),
            error: None,
        }
    }

    pub fn pending_shape(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "object": self.object,
            "created": self.created,
            "status": self.status,
            "model": self.model,
            "size": self.size,
            "n": self.n,
            "data": [],
            "poll_url": public_url(&format!("/v1/images/generations/{}", self.id)),
            "expires_at": self.expires_at,
            "gpn": {
                "payment": self.payment,
                "price_gpn": self.price_gpn,
                "estimated_seconds": (self.estimated_seconds * 100.0).round() / 100.0,
            }
        })
    }

    /// The terminal (or still-pending) OpenAI-shaped document.
    pub fn public_json(&self, b64: bool) -> serde_json::Value {
        let data: Vec<serde_json::Value> = self
            .data
            .iter()
            .map(|a| {
                let mut o = serde_json::json!({
                    "revised_prompt": self.revised_prompt.clone().unwrap_or_else(|| self.prompt.clone()),
                });
                if b64 {
                    match std::fs::read(artifact_path(&self.id, a.index)) {
                        Ok(bytes) => {
                            o["b64_json"] = serde_json::Value::String(base64_encode(&bytes));
                        }
                        Err(e) => {
                            o["url"] = serde_json::Value::String(image_url(&self.id, a.index));
                            o["error"] = serde_json::Value::String(format!("bytes unavailable: {e}"));
                        }
                    }
                } else {
                    o["url"] = serde_json::Value::String(image_url(&self.id, a.index));
                }
                o["sha256"] = serde_json::Value::String(a.sha256.clone());
                o["width"] = serde_json::json!(a.width);
                o["height"] = serde_json::json!(a.height);
                o
            })
            .collect();
        serde_json::json!({
            "id": self.id,
            "object": self.object,
            "created": self.created,
            "status": self.status,
            "model": self.model,
            "size": self.size,
            "n": self.n,
            "data": data,
            "expires_at": self.expires_at,
            "gpn": {
                "payment": self.payment,
                "channel_id": self.channel_id,
                "job_id": self.job_id,
                "price_gpn": self.price_gpn,
                "estimated_seconds": (self.estimated_seconds * 100.0).round() / 100.0,
            },
            "error": self.error,
        })
    }
}

/// Prefix a path with the public origin, when one is configured.
///
/// The AP is reached both directly (`http://<host>:8092`) and through the
/// Cloudflare Worker (`https://gpncoin.ai/api`), so only the deployment knows
/// which prefix a returned URL should carry. `GPN_PUBLIC_ORIGIN` is set to
/// `https://gpncoin.ai/api` in production and left unset in tests/dev.
pub fn public_url(path: &str) -> String {
    match std::env::var("GPN_PUBLIC_ORIGIN") {
        Ok(o) if !o.trim().is_empty() => {
            format!("{}{}", o.trim_end_matches('/'), path)
        }
        _ => path.to_string(),
    }
}

/// Stable, Worker-proxyable URL for one image.
pub fn image_url(id: &str, index: u32) -> String {
    let path = if index == 0 {
        format!("/v1/images/{id}")
    } else {
        format!("/v1/images/{id}/{index}")
    };
    public_url(&path)
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn ttl_secs() -> u64 {
    std::env::var("GPN_IMAGE_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n >= 60)
        .unwrap_or(DEFAULT_TTL_SECS)
}

pub fn image_dir() -> PathBuf {
    if let Ok(d) = std::env::var("GPN_IMAGE_DIR") {
        if !d.trim().is_empty() {
            return PathBuf::from(d);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".gpn").join("images")
}

/// 128 bits of hex, derived from the clock + pid + a process-local counter +
/// the prompt. Not a secret in the cryptographic sense, but unguessable and
/// never reused — which is all a capability URL needs to be.
pub fn new_id(prompt: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let n = CTR.fetch_add(1, Ordering::Relaxed);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = Sha256::new();
    h.update(t.to_le_bytes());
    h.update(n.to_le_bytes());
    h.update(std::process::id().to_le_bytes());
    h.update(prompt.as_bytes());
    let out = h.finalize();
    out[..ID_LEN / 2].iter().map(|b| format!("{b:02x}")).collect()
}

/// Ids are hex-only and exactly 32 chars — the path-traversal guard.
pub fn valid_id(id: &str) -> bool {
    id.len() == ID_LEN && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn record_path(id: &str) -> Option<PathBuf> {
    valid_id(id).then(|| image_dir().join(format!("{id}.json")))
}

pub fn save(job: &ImageJob) -> io::Result<()> {
    let Some(p) = record_path(&job.id) else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad image id"));
    };
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(job)?)?;
    std::fs::rename(&tmp, &p)
}

pub fn get(id: &str) -> Option<ImageJob> {
    let p = record_path(id)?;
    let raw = std::fs::read(p).ok()?;
    let job: ImageJob = serde_json::from_slice(&raw).ok()?;
    if job.expires_at <= now_secs() {
        return None;
    }
    Some(job)
}

/// Apply a mutation under a per-process lock and persist it. Returns the
/// updated record (or `None` if the id is unknown/expired).
pub fn update<F: FnOnce(&mut ImageJob)>(id: &str, f: F) -> Option<ImageJob> {
    use std::sync::Mutex;
    static LOCK: Mutex<()> = Mutex::new(());
    let _g = LOCK.lock().unwrap();
    let mut job = get(id)?;
    f(&mut job);
    job.updated = now_secs();
    let _ = save(&job);
    Some(job)
}

pub fn artifact_path(id: &str, index: u32) -> PathBuf {
    image_dir().join(format!("{id}-{index}.png"))
}

/// Persist one rendered image. The bytes are validated by the caller (PNG
/// magic + sha256); this only writes atomically.
pub fn put_artifact(id: &str, index: u32, bytes: &[u8]) -> io::Result<PathBuf> {
    if !valid_id(id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad image id"));
    }
    let p = artifact_path(id, index);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("png.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &p)?;
    Ok(p)
}

/// Delete expired records and their images. Returns `(records, files)` removed.
pub fn gc() -> (usize, usize) {
    let dir = image_dir();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return (0, 0);
    };
    let now = now_secs();
    let mut records = 0;
    let mut files = 0;
    for e in rd.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.ends_with(".json") {
            let id = name.trim_end_matches(".json");
            if !valid_id(id) {
                continue;
            }
            let expired = std::fs::read(&p)
                .ok()
                .and_then(|b| serde_json::from_slice::<ImageJob>(&b).ok())
                .map(|j| j.expires_at <= now)
                .unwrap_or(true); // unreadable record = garbage, remove it
            if expired {
                if std::fs::remove_file(&p).is_ok() {
                    records += 1;
                }
                for i in 0..16 {
                    let ap = artifact_path(id, i);
                    if std::fs::remove_file(&ap).is_ok() {
                        files += 1;
                    }
                }
            }
        } else if name.ends_with(".tmp") {
            let _ = std::fs::remove_file(&p);
        }
    }
    (records, files)
}

/// Standard-alphabet base64 with padding. Hand-rolled to keep the dependency
/// list of this crate unchanged (it is already deployed as a static binary).
pub fn base64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(T[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(T[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// True when `bytes` starts with the PNG signature. Every artifact this node
/// produces is a PNG (`gpn_diffuse.py` writes `image/png`); checking the magic
/// before publishing is what keeps a truncated/failed render from being served
/// as a "successful" image.
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.len() > 8 && bytes[..8] == [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
}

/// Parse an OpenAI `size` string (`"512x512"`, `"1024X768"`) into `(w, h)`.
///
/// Rejects anything that is not a positive multiple of 8 in `256..=1536` on
/// each axis: the helper would snap it anyway, and a caller who asked for
/// 5000² should be told the price/VRAM cannot cover it instead of silently
/// getting something else.
pub fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let t = s.trim().to_ascii_lowercase().replace('*', "x").replace(' ', "");
    let (w, h) = t
        .split_once('x')
        .ok_or_else(|| format!("size must look like 512x512, got '{s}'"))?;
    let w: u32 = w.parse().map_err(|_| format!("bad width in '{s}'"))?;
    let h: u32 = h.parse().map_err(|_| format!("bad height in '{s}'"))?;
    for (label, v) in [("width", w), ("height", h)] {
        if v < 256 || v > 1536 {
            return Err(format!("{label} must be 256..1536, got {v}"));
        }
        if v % 8 != 0 {
            return Err(format!("{label} must be a multiple of 8, got {v}"));
        }
    }
    Ok((w, h))
}

/// The AP's sync-mode rule, as a pure function so it is testable without a GPU.
///
/// Sync mode holds the HTTP connection, and the Worker gives up at 110 s. Keep
/// the budget well inside that: a render estimated at ≤60 s may run inline;
/// anything longer is refused with the estimate (the caller should use
/// `202 + poll`).
pub fn sync_refusal(estimated_seconds: f64) -> Option<String> {
    const SYNC_BUDGET_SECS: f64 = 60.0;
    if estimated_seconds > SYNC_BUDGET_SECS {
        Some(format!(
            "sync mode refused: this render is estimated at {estimated_seconds:.1}s on the anchor card, \
             above the {SYNC_BUDGET_SECS:.0}s synchronous budget (the proxy times out at 110s). \
             Submit without \"sync\" and poll poll_url."
        ))
    } else {
        None
    }
}

/// Directory for artifacts when a caller does not want the default (tests).
pub fn set_dir_for_test(dir: &Path) {
    std::env::set_var("GPN_IMAGE_DIR", dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_hex_and_traversal_is_rejected() {
        let id = new_id("a red fox");
        assert_eq!(id.len(), ID_LEN);
        assert!(valid_id(&id));
        assert!(!valid_id("../../etc/passwd"));
        assert!(!valid_id("ABC"));
        assert!(record_path("../x").is_none());
    }

    #[test]
    fn size_parsing_accepts_openai_sizes_and_rejects_nonsense() {
        assert_eq!(parse_size("512x512").unwrap(), (512, 512));
        assert_eq!(parse_size("1024X768").unwrap(), (1024, 768));
        assert_eq!(parse_size(" 256 x 256 ").unwrap(), (256, 256));
        assert!(parse_size("5000x5000").is_err());
        assert!(parse_size("500x500").is_err());
        assert!(parse_size("512").is_err());
        assert!(parse_size("0x0").is_err());
    }

    #[test]
    fn sync_mode_is_allowed_only_under_the_budget() {
        assert!(sync_refusal(30.0).is_none());
        assert!(sync_refusal(60.0).is_none());
        let msg = sync_refusal(60.1).expect("over budget must refuse");
        assert!(msg.contains("poll"), "the refusal must point at the poll path");
        assert!(sync_refusal(120.0).unwrap().contains("120.0"));
    }

    #[test]
    fn png_magic_is_enforced() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&[0u8; 16]);
        assert!(is_png(&png));
        assert!(!is_png(b"<html>not an image</html>"));
        assert!(!is_png(&png[..4]));
    }

    #[test]
    fn record_roundtrip_and_gc() {
        let dir = std::env::temp_dir().join(format!("gpn-img-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("GPN_IMAGE_DIR", &dir);
        let id = new_id("roundtrip");
        let mut job = ImageJob::new(id.clone(), "cat", "sd-turbo", 512, 512, 8, 0, 1, 30.0, 0.0076);
        save(&job).unwrap();
        assert_eq!(get(&id).unwrap().status, "pending");
        put_artifact(&id, 0, b"\x89PNG\r\n\x1a\nREST").unwrap();
        assert!(artifact_path(&id, 0).exists());
        job.status = "completed".into();
        save(&job).unwrap();
        assert_eq!(get(&id).unwrap().status, "completed");
        // Expire it: `get` must hide it and `gc` must remove the bytes.
        job.expires_at = now_secs() - 1;
        save(&job).unwrap();
        assert!(get(&id).is_none());
        let (r, f) = gc();
        assert!(r >= 1 && f >= 1);
        assert!(!artifact_path(&id, 0).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn public_json_shape_matches_openai() {
        let id = new_id("shape");
        let mut job = ImageJob::new(id.clone(), "cat", "sd-turbo", 512, 512, 8, 0, 1, 30.0, 0.0076);
        let pending = job.pending_shape();
        assert_eq!(pending["status"], "pending");
        assert_eq!(pending["data"].as_array().unwrap().len(), 0);
        assert!(pending["poll_url"].as_str().unwrap().contains(&id));
        job.status = "completed".into();
        job.payment = "channel".into();
        job.data.push(ImageArtifact {
            index: 0,
            media_type: "image/png".into(),
            sha256: "ab".repeat(32),
            bytes: 1234,
            width: 512,
            height: 512,
        });
        let done = job.public_json(false);
        assert_eq!(done["status"], "completed");
        assert_eq!(done["data"][0]["url"], format!("/v1/images/{id}"));
        assert_eq!(done["gpn"]["payment"], "channel");
    }
}
