//! Public speech (text-to-speech) jobs: a durable, pollable record of one
//! `POST /v1/audio/speech` request. Item #202.
//!
//! Deliberately the same shape as [`crate::image_jobs`], for the same reason:
//! synthesis is seconds of GPU (measured 6.7 s for 7.55 s of audio on a 4060,
//! plus 15 s warm model load), and a cold engine loads for minutes, so holding
//! the connection collides with the Worker's 110 s upstream limit. The AP
//! answers `202` with an id and a `poll_url`, and this module is the state that
//! poll reads.
//!
//! ## What is stored, and what is not
//!
//! A job record (`<id>.json`) and one MP3 per artifact (`<id>-<index>.mp3`)
//! under `$GPN_AUDIO_DIR` (default `~/.gpn/audio`). The **reference clip is
//! never stored here** — owner decision 2026-11-18: per-job fetch, no
//! retention. The clip lives only in the request that carried it and in the
//! provider's temp file for the duration of the render, which deletes it.
//!
//! Ids are 128-bit capability URLs, validated before they touch the filesystem.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;

pub use crate::image_jobs::{base64_encode, now_secs, public_url, valid_id, DEFAULT_TTL_SECS};

/// One produced clip.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AudioArtifact {
    /// Position in the caller's sequence (0-based; audio is 1 artifact today).
    pub index: u32,
    pub media_type: String,
    pub sha256: String,
    pub bytes: u64,
    /// Measured duration of the produced audio — the billing unit (owner
    /// decision: priced on actual output, like images).
    pub seconds: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioJob {
    pub id: String,
    pub object: String,
    /// `pending` | `completed` | `failed`.
    pub status: String,
    pub created: u64,
    pub updated: u64,
    pub expires_at: u64,
    pub model: String,
    /// The text to speak, exactly as submitted.
    pub input: String,
    /// Language tag handed to the engine.
    pub language: String,
    /// The consent attestation the caller gave. Kept so the record shows the
    /// request was consented, never the clip itself.
    pub consent: bool,
    /// sha256 of the reference clip — proves which clip was used, without
    /// keeping the clip.
    pub voice_sha256: String,
    /// Ceiling on billable output seconds (also the price's basis).
    pub max_seconds: u32,
    /// `channel` (off-chain signed settlement) or `not_charged`.
    pub payment: String,
    pub channel_id: Option<u64>,
    pub job_id: Option<u64>,
    pub price_gpn: f64,
    pub estimated_seconds: f64,
    #[serde(default)]
    pub data: Vec<AudioArtifact>,
    pub error: Option<String>,
}

impl AudioJob {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        input: &str,
        model: &str,
        language: &str,
        voice_sha256: &str,
        max_seconds: u32,
        estimated_seconds: f64,
        price_gpn: f64,
    ) -> Self {
        let now = now_secs();
        Self {
            id,
            object: "audio.speech".to_string(),
            status: "pending".to_string(),
            created: now,
            updated: now,
            expires_at: now + ttl_secs(),
            model: model.to_string(),
            input: input.to_string(),
            language: language.to_string(),
            consent: true,
            voice_sha256: voice_sha256.to_string(),
            max_seconds,
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
            "voice_sha256": self.voice_sha256,
            "max_seconds": self.max_seconds,
            "data": [],
            "poll_url": public_url(&format!("/v1/audio/speech/{}", self.id)),
            "expires_at": self.expires_at,
            "gpn": {
                "payment": self.payment,
                "price_gpn": self.price_gpn,
                "estimated_seconds": (self.estimated_seconds * 100.0).round() / 100.0,
            }
        })
    }

    pub fn public_json(&self) -> serde_json::Value {
        let data: Vec<serde_json::Value> = self
            .data
            .iter()
            .map(|a| {
                serde_json::json!({
                    "url": audio_url(&self.id, a.index),
                    "media_type": a.media_type,
                    "sha256": a.sha256,
                    "bytes": a.bytes,
                    "seconds": (a.seconds * 1000.0).round() / 1000.0,
                })
            })
            .collect();
        serde_json::json!({
            "id": self.id,
            "object": self.object,
            "created": self.created,
            "status": self.status,
            "model": self.model,
            "language": self.language,
            "voice_sha256": self.voice_sha256,
            "max_seconds": self.max_seconds,
            "data": data,
            "expires_at": self.expires_at,
            "gpn": {
                "payment": self.payment,
                "channel_id": self.channel_id,
                "job_id": self.job_id,
                "price_gpn": self.price_gpn,
                "estimated_seconds": (self.estimated_seconds * 100.0).round() / 100.0,
                "billing_unit": "output_seconds",
            },
            "error": self.error,
        })
    }
}

/// Stable, Worker-proxyable URL for one clip.
pub fn audio_url(id: &str, index: u32) -> String {
    let path = if index == 0 {
        format!("/v1/audio/{id}")
    } else {
        format!("/v1/audio/{id}/{index}")
    };
    public_url(&path)
}

pub fn ttl_secs() -> u64 {
    std::env::var("GPN_AUDIO_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n >= 60)
        .unwrap_or(DEFAULT_TTL_SECS)
}

pub fn audio_dir() -> PathBuf {
    if let Ok(d) = std::env::var("GPN_AUDIO_DIR") {
        if !d.trim().is_empty() {
            return PathBuf::from(d);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".gpn").join("audio")
}

fn record_path(id: &str) -> Option<PathBuf> {
    valid_id(id).then(|| audio_dir().join(format!("{id}.json")))
}

pub fn save(job: &AudioJob) -> io::Result<()> {
    let Some(p) = record_path(&job.id) else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad audio id"));
    };
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(job)?)?;
    std::fs::rename(&tmp, &p)
}

pub fn get(id: &str) -> Option<AudioJob> {
    let p = record_path(id)?;
    let raw = std::fs::read(p).ok()?;
    let job: AudioJob = serde_json::from_slice(&raw).ok()?;
    if job.expires_at <= now_secs() {
        return None;
    }
    Some(job)
}

pub fn update<F: FnOnce(&mut AudioJob)>(id: &str, f: F) -> Option<AudioJob> {
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
    audio_dir().join(format!("{id}-{index}.mp3"))
}

/// Persist one produced clip atomically. The caller validates the bytes
/// (MP3 frame sync / sha256); this only writes.
pub fn put_artifact(id: &str, index: u32, bytes: &[u8]) -> io::Result<PathBuf> {
    if !valid_id(id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad audio id"));
    }
    let p = artifact_path(id, index);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("mp3.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &p)?;
    Ok(p)
}

/// Delete expired records and their clips. Returns `(records, files)` removed.
pub fn gc() -> (usize, usize) {
    let dir = audio_dir();
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
                .and_then(|b| serde_json::from_slice::<AudioJob>(&b).ok())
                .map(|j| j.expires_at <= now)
                .unwrap_or(true);
            if expired {
                if std::fs::remove_file(&p).is_ok() {
                    records += 1;
                }
                for i in 0..4 {
                    if std::fs::remove_file(artifact_path(id, i)).is_ok() {
                        files += 1;
                    }
                }
            }
        } else if name.ends_with(".tmp") || name.ends_with(".wav") {
            // A leftover reference clip is a retention failure: the clip must
            // never outlive the render. Remove it unconditionally.
            let _ = std::fs::remove_file(&p);
        }
    }
    (records, files)
}

/// True when `bytes` looks like an MP3 stream (ID3 tag or an MPEG frame sync).
///
/// The engine writes MP3 (owner decision); checking the container before
/// publishing is what keeps a truncated/failed render from being served as a
/// successful clip.
pub fn is_mp3(bytes: &[u8]) -> bool {
    if bytes.len() < 4 {
        return false;
    }
    if &bytes[..3] == b"ID3" {
        return true;
    }
    // MPEG audio frame sync: 11 set bits, then a non-reserved layer/version.
    bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0 && (bytes[1] & 0x06) != 0x00
}

/// Decode standard base64 (with or without padding). Hand-rolled so this crate
/// keeps its dependency list; matches [`crate::image_jobs::base64_encode`].
pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for c in s.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let Some(v) = val(c) else {
            return Err(format!("invalid base64 character '{}'", c as char));
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if out.is_empty() {
        return Err("empty base64 payload".to_string());
    }
    Ok(out)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// The owner's accepted reference-clip window, shared by the AP (so a caller is
/// refused before anything is charged) and the provider (so a wire-level clip
/// is refused before `accept`).
pub const MIN_REF_SECS: f64 = 5.0;
pub const MAX_REF_SECS: f64 = 15.0;

/// The AP's sync-mode rule, mirroring [`crate::image_jobs::sync_refusal`].
pub fn sync_refusal(estimated_seconds: f64) -> Option<String> {
    const SYNC_BUDGET_SECS: f64 = 60.0;
    if estimated_seconds > SYNC_BUDGET_SECS {
        Some(format!(
            "sync mode refused: this clip is estimated at {estimated_seconds:.1}s on the anchor card, \
             above the {SYNC_BUDGET_SECS:.0}s synchronous budget (the proxy times out at 110s). \
             Submit without \"sync\" and poll poll_url."
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_and_rejects_junk() {
        let raw: Vec<u8> = (0..300u32).map(|i| (i % 251) as u8).collect();
        let enc = base64_encode(&raw);
        assert_eq!(base64_decode(&enc).unwrap(), raw);
        // Unpadded input is accepted (URL-safe alphabets omit it).
        assert_eq!(base64_decode("QUJD").unwrap(), b"ABC");
        assert!(base64_decode("!!!").is_err());
        assert!(base64_decode("").is_err());
    }

    #[test]
    fn mp3_magic_is_checked() {
        assert!(is_mp3(b"ID3\x04\x00\x00\x00\x00\x00\x00"));
        assert!(is_mp3(&[0xFF, 0xFB, 0x90, 0x64]));
        assert!(!is_mp3(b"\x89PNG\r\n\x1a\n"));
        assert!(!is_mp3(b"ID"));
        assert!(!is_mp3(&[0xFF, 0xF0, 0x00, 0x00]));
    }

    #[test]
    fn records_persist_and_expire() {
        let dir = std::env::temp_dir().join(format!("gpn-audio-test-{}", std::process::id()));
        std::env::set_var("GPN_AUDIO_DIR", &dir);
        let id = crate::image_jobs::new_id("hello");
        let job = AudioJob::new(
            id.clone(),
            "hello",
            "xtts-v2",
            "en",
            "abc123",
            20,
            12.0,
            0.02,
        );
        save(&job).unwrap();
        let back = get(&id).unwrap();
        assert_eq!(back.input, "hello");
        assert_eq!(back.data.len(), 0);
        assert_eq!(back.object, "audio.speech");
        update(&id, |j| {
            j.status = "completed".into();
            j.data.push(AudioArtifact {
                index: 0,
                media_type: "audio/mpeg".into(),
                sha256: "dead".into(),
                bytes: 4,
                seconds: 7.5,
            });
        })
        .unwrap();
        let done = get(&id).unwrap();
        assert_eq!(done.status, "completed");
        assert_eq!(done.public_json()["data"][0]["seconds"], 7.5);
        // The clip is served through the record's advertised URL, not inline.
        assert!(done.public_json()["data"][0]["url"]
            .as_str()
            .unwrap()
            .contains(&id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_budget_refuses_slow_clips() {
        assert!(sync_refusal(10.0).is_none());
        assert!(sync_refusal(120.0).is_some());
    }
}
