//! Self-heal on chain divergence (#157) — **the schedule half**.
//!
//! `design-self-heal-divergence.md` shipped the *decision* (`heal_decision`) and
//! the *recovery sequence* (`gpn_snapshot heal --apply`) on 2026-11-10, and said
//! plainly what was missing:
//!
//! > Not in this change: wiring into a schedule (a `gpn-heal.timer` / the
//! > supervisor) — deliberately left to the owner's call.
//!
//! Nothing ever called it. A node that landed on a fork kept serving and sealing
//! it, got banned by its peers, reconnected, and repeated that forever: box 1 sat
//! 17 000 blocks down its own branch on 2026-11-13 and could not reorg, because
//! ordinary sync cannot rewrite that depth (see `docs/goal-vs-implementation.md`
//! Part 4, finding #1).
//!
//! This module is the piece that was missing. It owns:
//!
//! * **fetching and verifying** the signed `snapshot.json` (unsigned or
//!   unverifiable ⇒ do nothing, and say why — a peer's opinion is never a
//!   reference);
//! * **observing** this node's height / hash at the checkpoint height / genesis
//!   over its own JSON-RPC, so divergence is detected from a *verified* signed
//!   checkpoint and never from gossip;
//! * **deciding** (`heal_decision`, unchanged rules: match / behind / v1 / wrong
//!   network ⇒ nothing; a fork with no recovery in the last 24 h ⇒ recover);
//! * **recovering** through a [`NodeControl`], so the same sequence runs whether
//!   the node is a systemd unit (`gpn_snapshot heal --apply`, Linux) or a child
//!   process of a supervisor (`gpn_node`, which is what a Windows box is).
//!
//! ## Safety properties (each is a test below)
//!
//! * A **genesis mismatch is not a fork** — it is a different network, and no
//!   automatic repair is correct. Never recovers, never touches the DB.
//! * **Unknown local state is not a verdict** — an RPC that cannot be read, a
//!   height below the checkpoint, or v1 metadata ⇒ `Nothing`.
//! * **Unverifiable metadata ⇒ `Nothing`.** A forged or edited `snapshot.json`,
//!   a key that is not the one built into this node, or a document that cannot be
//!   fetched at all never schedules a recovery.
//! * **One recovery per 24 h**, and a corrupt ledger refuses rather than licensing
//!   a loop. The window is written *before* anything destructive.
//! * **Never stop the node while a job is being served.** The GPU yield flag is
//!   the cross-process "a job owns the GPU" signal (`gpu_yield::yield_flag_path`);
//!   while it exists the check reports `busy` and tries again next tick. A
//!   mid-seal block is not a hazard *because recovery stops the node first* and
//!   only then moves `chains/` — the abandoned branch goes with it.
//!
//! ## What it deliberately does not do
//!
//! It does not import RocksDB itself: the archive extraction lives in
//! `gpn_snapshot import` and moving it here would duplicate the one piece of
//! this system that can silently produce a half-imported DB. The caller passes an
//! import closure (`gpn_snapshot` calls its own `import()`, `gpn_node` runs
//! `gpn_snapshot import` as a subprocess next to itself).

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Exit code for "this node is on the wrong chain" (design §"Exit codes").
pub const EXIT_WRONG_CHAIN: i32 = 12;
/// Exit code for "a recovery was already attempted inside the window".
pub const EXIT_HEAL_REFUSED: i32 = 13;

/// At most one automatic recovery per window, then stop and explain.
pub const RECOVERY_WINDOW_SECS: u64 = 24 * 60 * 60;
/// Ledger filename, next to the node's `chains/` (same default as the CLI).
pub const RECOVERY_STATE_NAME: &str = "last-recovery.json";

/// How often a supervisor runs the divergence check (design: "at startup and
/// every 5 minutes").
pub const CHECK_INTERVAL_SECS: u64 = 300;

/// The public signed snapshot document. The same URL `install.sh` verifies and
/// `scripts/snapshot-weekly.sh` publishes (`crewlyllc/gpn_coin`, rolling tag).
pub const DEFAULT_SNAPSHOT_META_URL: &str =
    "https://github.com/crewlyllc/gpn_coin/releases/download/snapshot-latest/snapshot.json";

pub const SNAPSHOT_CANONICAL_PREFIX_V1: &str = "gpn-snapshot-v1";
pub const SNAPSHOT_CANONICAL_PREFIX: &str = "gpn-snapshot-v2";

/// A logging sink. The supervisors each have their own line discipline
/// (`[watchdog] …`, `[self-heal] …`), so the module composes text and the caller
/// decides where it goes.
pub type Log<'a> = &'a dyn Fn(&str);

// ------------------------------------------------------------------ metadata

/// Signed metadata for the public weekly snapshot (`snapshot.json`).
///
/// Field order in [`SnapshotMeta::canonical_text`] is the signed order and must
/// not change without changing `scripts/snapshot-metadata.py` in the same commit.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SnapshotMeta {
    #[serde(default)]
    pub format: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub block: Option<u64>,
    /// v2: the genesis this snapshot belongs to.
    #[serde(default)]
    pub genesis: String,
    /// v2: the block hash AT `block` — the only way to tell a canonical snapshot
    /// from one taken on a fork that shares the genesis.
    #[serde(default)]
    pub block_hash: String,
    #[serde(default)]
    pub built: String,
    #[serde(default)]
    pub signing_key: String,
    #[serde(default)]
    pub signature: String,
}

impl SnapshotMeta {
    /// The exact bytes signed. v1 documents keep verifying forever (the prefix
    /// is part of the signed text); only v2 adds the two checkpoint lines.
    pub fn canonical_text(&self) -> Vec<u8> {
        let is_v2 = self.format.trim() == SNAPSHOT_CANONICAL_PREFIX;
        let mut out = String::new();
        if is_v2 {
            out.push_str(&format!(
                "{SNAPSHOT_CANONICAL_PREFIX}\nurl={}\nsha256={}\nsize={}\nblock={}\ngenesis={}\nblock_hash={}\nbuilt={}\nsigning_key={}\n",
                self.url.trim(),
                self.sha256.trim().to_ascii_lowercase(),
                self.size,
                self.block.map(|b| b.to_string()).unwrap_or_default(),
                self.genesis.trim().to_ascii_lowercase(),
                self.block_hash.trim().to_ascii_lowercase(),
                self.built.trim(),
                self.signing_key.trim().to_ascii_lowercase(),
            ));
        } else {
            out.push_str(&format!(
                "{SNAPSHOT_CANONICAL_PREFIX_V1}\nurl={}\nsha256={}\nsize={}\nblock={}\nbuilt={}\nsigning_key={}\n",
                self.url.trim(),
                self.sha256.trim().to_ascii_lowercase(),
                self.size,
                self.block.map(|b| b.to_string()).unwrap_or_default(),
                self.built.trim(),
                self.signing_key.trim().to_ascii_lowercase(),
            ));
        }
        out.into_bytes()
    }

    /// The signed checkpoint, if this document carries one.
    pub fn checkpoint(&self) -> Option<(u64, &str)> {
        if self.format.trim() != SNAPSHOT_CANONICAL_PREFIX {
            return None;
        }
        let block = self.block?;
        let hash = self.block_hash.trim();
        if hash.is_empty() {
            return None;
        }
        Some((block, hash))
    }
}

/// sha256 of a file, hex lower-case.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Verify a `snapshot.json` against `pubkey_hex`; optionally also check a
/// downloaded archive's exact size and sha256 against it.
pub fn verify_snapshot_meta(
    meta_bytes: &[u8],
    pubkey_hex: &str,
    archive: Option<&Path>,
) -> Result<SnapshotMeta, String> {
    let m: SnapshotMeta =
        serde_json::from_slice(meta_bytes).map_err(|e| format!("snapshot.json: {e}"))?;
    if m.signature.trim().is_empty() || m.signing_key.trim().is_empty() {
        return Err("snapshot.json is UNSIGNED — refusing to trust it".into());
    }
    let embedded = pubkey_hex.trim().to_ascii_lowercase();
    if m.signing_key.trim().to_ascii_lowercase() != embedded {
        return Err(format!(
            "snapshot.json was signed by a different key ({}) than the one built into this node ({embedded})",
            m.signing_key.trim()
        ));
    }
    let key = hex::decode(&embedded).map_err(|e| format!("embedded key: {e}"))?;
    let key_arr: [u8; 32] = key
        .as_slice()
        .try_into()
        .map_err(|_| "embedded key is not 32 bytes".to_string())?;
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&key_arr)
        .map_err(|e| format!("verifying key: {e}"))?;
    let sig_raw = hex::decode(m.signature.trim()).map_err(|e| format!("signature hex: {e}"))?;
    let sig_arr: [u8; 64] = sig_raw
        .as_slice()
        .try_into()
        .map_err(|_| "signature is not 64 bytes".to_string())?;
    let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);
    use ed25519_dalek::Verifier;
    vk.verify(&m.canonical_text(), &sig)
        .map_err(|_| "snapshot.json signature does NOT verify (edited or forged)".to_string())?;

    if m.sha256.trim().len() != 64 || !m.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("snapshot.json has no valid sha256".into());
    }
    if m.size == 0 {
        return Err("snapshot.json has no size".into());
    }
    // v2 checkpoint shape: if either field is present, both must be, and both
    // must be 0x-hex. A half-filled checkpoint is worse than none -- it looks
    // authoritative and cannot be compared.
    let v2 = m.format.trim() == SNAPSHOT_CANONICAL_PREFIX;
    if v2 {
        for (name, v, want) in [
            ("genesis", m.genesis.trim(), 32usize),
            ("block_hash", m.block_hash.trim(), 32usize),
        ] {
            let hexpart = v.strip_prefix("0x").unwrap_or("");
            if hexpart.len() != want * 2 || !hexpart.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!(
                    "snapshot.json v2 has an invalid {name} (want 0x + 64 hex): {v}"
                ));
            }
        }
        if m.block.is_none() {
            return Err("snapshot.json v2 declares a checkpoint without a block height".into());
        }
    }

    if let Some(path) = archive {
        let got_len = fs::metadata(path)
            .map_err(|e| format!("stat {}: {e}", path.display()))?
            .len();
        if got_len != m.size {
            return Err(format!(
                "size mismatch for {}: expected {} bytes, got {got_len}",
                path.display(),
                m.size
            ));
        }
        let got_sha = sha256_file(path).map_err(|e| format!("hash {}: {e}", path.display()))?;
        if got_sha != m.sha256.trim().to_ascii_lowercase() {
            return Err(format!(
                "sha256 mismatch for {}: expected {}, got {got_sha}",
                path.display(),
                m.sha256.trim()
            ));
        }
    }
    Ok(m)
}

/// Fetch the signed document from a URL or read it from a path. Bounded: a
/// checkpoint document is a few hundred bytes, so a 200 KiB cap is generous and
/// stops a hostile mirror from feeding us a megabyte to parse.
pub fn load_meta_bytes(src: &str) -> Result<Vec<u8>, String> {
    const CAP: u64 = 200 * 1024;
    if src.starts_with("http://") || src.starts_with("https://") {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .get(src)
            .send()
            .map_err(|e| format!("GET {src}: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("GET {src}: HTTP {}", resp.status()));
        }
        let mut buf = Vec::new();
        resp.take(CAP + 1)
            .read_to_end(&mut buf)
            .map_err(|e| format!("read {src}: {e}"))?;
        if buf.len() as u64 > CAP {
            return Err(format!("GET {src}: document larger than {CAP} bytes"));
        }
        Ok(buf)
    } else {
        fs::read(src).map_err(|e| format!("read {src}: {e}"))
    }
}

// ------------------------------------------------------------------ verdict

/// What the local chain says about a signed checkpoint (design §3).
///
/// `NotYet` exists so an *unknown* never becomes a verdict — a failed read is
/// not evidence of a fork, and a fork must never be inferred from missing data.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum CheckpointVerdict {
    /// We do not have the checkpoint height yet (fresh node, or still syncing).
    NotYet {
        local_height: Option<u64>,
        block: u64,
    },
    /// Our hash at the checkpoint height is the signed one.
    Match { block: u64 },
    /// Our hash at that height is a different block: a fork. Fatal.
    WrongChain {
        block: u64,
        expected: String,
        local: String,
    },
}

impl CheckpointVerdict {
    pub fn slug(&self) -> &'static str {
        match self {
            CheckpointVerdict::NotYet { .. } => "not-yet",
            CheckpointVerdict::Match { .. } => "match",
            CheckpointVerdict::WrongChain { .. } => "wrong-chain",
        }
    }
}

/// Map a verdict to the process exit code (0 / 12).
pub fn verdict_exit_code(v: &CheckpointVerdict) -> i32 {
    match v {
        CheckpointVerdict::WrongChain { .. } => EXIT_WRONG_CHAIN,
        _ => 0,
    }
}

/// Pure comparison of a signed checkpoint against what the local node reports.
///
/// `local_hash_at` is `None` when the local node does not have that height (or
/// the block hash could not be read) — that is `NotYet`, never a mismatch.
pub fn checkpoint_verdict(
    checkpoint_block: u64,
    checkpoint_hash: &str,
    local_height: Option<u64>,
    local_hash_at: Option<&str>,
) -> CheckpointVerdict {
    match (local_height, local_hash_at) {
        (Some(h), _) if h < checkpoint_block => CheckpointVerdict::NotYet {
            local_height: Some(h),
            block: checkpoint_block,
        },
        (_, None) => CheckpointVerdict::NotYet {
            local_height,
            block: checkpoint_block,
        },
        (_, Some(local)) => {
            let expected = checkpoint_hash.trim().to_ascii_lowercase();
            let got = local.trim().to_ascii_lowercase();
            if expected == got {
                CheckpointVerdict::Match {
                    block: checkpoint_block,
                }
            } else {
                CheckpointVerdict::WrongChain {
                    block: checkpoint_block,
                    expected,
                    local: got,
                }
            }
        }
    }
}

// ------------------------------------------------------------------ ledger

/// Durable record of the last self-heal. Written **before** anything destructive
/// so a crash mid-repair still consumes the window (never a loop).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryRecord {
    pub recovered_at: u64,
    pub recovered_at_iso: String,
    pub checkpoint_block: u64,
    pub checkpoint_hash: String,
    pub local_hash: String,
    /// Where the forked DB was set aside (`<base>/chains-fork-<ts>`).
    pub fork_dir: String,
    pub snapshot: String,
    /// False while the import is in flight; the refusal rule ignores this flag on
    /// purpose — a half-finished recovery still consumes the window.
    pub completed: bool,
    /// Short human line: what happened, or why it stopped.
    pub detail: String,
}

/// What `heal` decided to do. Pure, so every branch is a test rather than a
/// comment: the destructive path must be reachable only from `Recover`.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum HealDecision {
    /// No fork: matching, behind the checkpoint, or a v1 doc with no claim.
    Nothing { reason: String },
    /// A recovery was already attempted inside the window — stop and explain.
    Refused { age_secs: u64 },
    /// A fork with no recent attempt: set `fork_dir` aside and import.
    Recover { fork_dir: PathBuf },
}

pub fn heal_decision(
    verdict: &CheckpointVerdict,
    last: Option<&RecoveryRecord>,
    now: u64,
    window_secs: u64,
    fork_dir: &Path,
) -> HealDecision {
    match verdict {
        CheckpointVerdict::Match { block } => HealDecision::Nothing {
            reason: format!("local block {block} matches the signed checkpoint"),
        },
        CheckpointVerdict::NotYet { .. } => HealDecision::Nothing {
            reason: "this node is behind the signed checkpoint (not a fork)".into(),
        },
        CheckpointVerdict::WrongChain { .. } => match last {
            Some(rec) => {
                // A clock that jumped backwards must not unlock a second heal.
                let age = now.saturating_sub(rec.recovered_at);
                if age < window_secs {
                    HealDecision::Refused { age_secs: age }
                } else {
                    HealDecision::Recover {
                        fork_dir: fork_dir.to_path_buf(),
                    }
                }
            }
            None => HealDecision::Recover {
                fork_dir: fork_dir.to_path_buf(),
            },
        },
    }
}

/// Read the recovery record. `Ok(None)` = never healed. A file that exists but
/// cannot be read or parsed is an **error**, not "no record": a corrupt ledger
/// must fail closed, or it would license an unbounded repair loop.
pub fn read_recovery(path: &Path) -> Result<Option<RecoveryRecord>, String> {
    match fs::read(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("{} is not a valid recovery record: {e}", path.display())),
    }
}

/// Atomic write (temp + rename): a torn ledger is worse than none.
pub fn write_recovery(path: &Path, rec: &RecoveryRecord) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let body = serde_json::to_vec_pretty(rec).map_err(|e| format!("serialise record: {e}"))?;
    fs::write(&tmp, &body).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", tmp.display()))?;
    Ok(())
}

/// A free `chains-fork-<ts>` path: never overwrite an earlier fork's DB — it is
/// the only evidence of what this node was on.
pub fn fork_dir_for(base: &Path, stamp: u64) -> PathBuf {
    let mut candidate = base.join(format!("chains-fork-{stamp}"));
    let mut n = 1u32;
    while candidate.exists() {
        candidate = base.join(format!("chains-fork-{stamp}-{n}"));
        n += 1;
    }
    candidate
}

/// Where the ledger lives: `<base>/last-recovery.json` unless the caller
/// overrides it (the CLI's `--state`).
pub fn ledger_path(base: &Path) -> PathBuf {
    base.join(RECOVERY_STATE_NAME)
}

pub fn iso8601(ts: u64) -> String {
    let days = (ts / 86_400) as i64;
    let secs = (ts % 86_400) as i64;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Howard Hinnant's days-from-civil, inverted (days since 1970-01-01 -> y/m/d).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ------------------------------------------------------------------ remote state

/// One JSON-RPC call against the node's own HTTP endpoint (`--rpc-port`).
pub fn rpc_call(
    url: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
    let resp: serde_json::Value = client
        .post(url)
        .json(&body)
        .send()
        .map_err(|e| format!("{method}: {e}"))?
        .json()
        .map_err(|e| format!("{method}: bad json: {e}"))?;
    if let Some(err) = resp.get("error") {
        return Err(format!("{method}: rpc error {err}"));
    }
    Ok(resp
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

fn hex_u64(v: &serde_json::Value) -> Option<u64> {
    v.as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
}

/// This node's height, its hash at `block`, and its genesis — the same
/// three-state read `gpn_snapshot checkpoint` does, factored out so the
/// supervisor and the CLI cannot disagree about what "unreadable" means.
pub fn local_chain_state(rpc: &str, block: u64) -> (Option<u64>, Option<String>, Option<String>) {
    let genesis = rpc_call(rpc, "chain_getBlockHash", serde_json::json!([0]))
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_ascii_lowercase()));
    let height = rpc_call(rpc, "chain_getHeader", serde_json::json!([]))
        .ok()
        .and_then(|h| h.get("number").and_then(hex_u64));
    let hash = match height {
        Some(h) if h >= block => rpc_call(rpc, "chain_getBlockHash", serde_json::json!([block]))
            .ok()
            .and_then(|v| v.as_str().map(String::from)),
        _ => None,
    };
    (height, hash, genesis)
}

/// Everything one check observed, in the form the log line and the decision
/// both need.
#[derive(Debug, Clone)]
pub struct Observation {
    pub block: u64,
    pub checkpoint_hash: String,
    pub local_height: Option<u64>,
    pub local_hash: Option<String>,
    pub local_genesis: Option<String>,
    pub expected_genesis: String,
    pub verdict: CheckpointVerdict,
}

impl Observation {
    /// `Some((local, want))` when this node is on a different network.
    pub fn genesis_mismatch(&self) -> Option<(String, String)> {
        let local = self.local_genesis.as_ref()?;
        let want = self.expected_genesis.trim().to_ascii_lowercase();
        if want.is_empty() || *local == want {
            None
        } else {
            Some((local.clone(), want))
        }
    }

    /// The INFO line an operator reads to know the check ran.
    pub fn check_line(&self) -> String {
        let local = self
            .local_hash
            .as_deref()
            .map(short_hash)
            .unwrap_or_else(|| "unreadable".to_string());
        format!(
            "divergence check: local {} @ #{} vs signed checkpoint {} @ #{}",
            local,
            self.local_height
                .map(|h| h.to_string())
                .unwrap_or_else(|| "?".into()),
            short_hash(&self.checkpoint_hash),
            self.block
        )
    }

    /// The INFO line that says what it decided, naming both hashes and the
    /// verdict. One line, so `grep divergence` is the whole audit trail.
    pub fn verdict_line(&self, what: &str) -> String {
        let local = self
            .local_hash
            .as_deref()
            .unwrap_or("unreadable")
            .to_string();
        format!(
            "divergence verdict: {} (local {local} vs checkpoint {} at #{}) — {what}",
            self.verdict.slug(),
            self.checkpoint_hash,
            self.block
        )
    }
}

pub fn short_hash(h: &str) -> String {
    let s = h.trim_start_matches("0x");
    format!("0x{}", s.chars().take(8).collect::<String>())
}

/// Build the observation from one metadata document + one RPC read.
pub fn observe(meta: &SnapshotMeta, rpc: &str) -> Result<Observation, String> {
    let (block, hash) = meta
        .checkpoint()
        .ok_or("metadata carries no checkpoint (v1)")?;
    let block_hash = hash.to_string();
    let (height, local_hash, local_genesis) = local_chain_state(rpc, block);
    let verdict = checkpoint_verdict(block, &block_hash, height, local_hash.as_deref());
    Ok(Observation {
        block,
        checkpoint_hash: block_hash,
        local_height: height,
        local_hash,
        local_genesis,
        expected_genesis: meta.genesis.clone(),
        verdict,
    })
}

// ------------------------------------------------------------------ the check

pub struct CheckConfig {
    /// The node's own JSON-RPC (`http://127.0.0.1:<rpc-port>`).
    pub rpc: String,
    /// Node base path (`chains/` lives here; the ledger defaults next to it).
    pub base: PathBuf,
    /// `snapshot.json` — URL or path.
    pub meta_src: String,
    /// The release key compiled into this build.
    pub pubkey_hex: String,
    /// Override for the ledger path (CLI `--state`).
    pub ledger: Option<PathBuf>,
}

/// What one check concluded. `observation`/`decision` are `None` when the
/// metadata could not be trusted at all — in which case nothing is done and the
/// log says exactly why.
#[derive(Debug)]
pub struct CheckOutcome {
    pub observation: Option<Observation>,
    pub decision: Option<HealDecision>,
}

impl CheckOutcome {
    /// Only this outcome may touch a DB.
    pub fn recovery(&self) -> Option<(&Observation, &PathBuf)> {
        match (&self.observation, &self.decision) {
            (Some(obs), Some(HealDecision::Recover { fork_dir })) => Some((obs, fork_dir)),
            _ => None,
        }
    }
}

/// Run the design's check: fetch → verify → observe → decide, logging both the
/// check and the verdict. Read-only; recovery is the caller's next step.
pub fn check(cfg: &CheckConfig, now: u64, log: Log) -> CheckOutcome {
    let none = CheckOutcome {
        observation: None,
        decision: None,
    };

    let bytes = match load_meta_bytes(&cfg.meta_src) {
        Ok(b) => b,
        Err(e) => {
            log(&format!(
                "divergence check: no verdict — cannot fetch the signed checkpoint ({e})"
            ));
            return none;
        }
    };
    let meta = match verify_snapshot_meta(&bytes, &cfg.pubkey_hex, None) {
        Ok(m) => m,
        Err(e) => {
            log(&format!(
                "divergence check: no verdict — signed metadata does not verify ({e}); \
                 a peer's opinion is not a reference, so nothing is done"
            ));
            return none;
        }
    };
    let obs = match observe(&meta, &cfg.rpc) {
        Ok(o) => o,
        Err(e) => {
            log(&format!(
                "divergence check: no verdict — {e} (nothing to compare, nothing done)"
            ));
            return none;
        }
    };
    log(&obs.check_line());

    // Genesis first: a different network is not a fork, and no automatic
    // repair is correct. This is exit-12 territory, never a wipe.
    if let Some((local, want)) = obs.genesis_mismatch() {
        let reason = format!(
            "different NETWORK, not a fork: genesis {local} vs signed checkpoint {want} — \
             DB untouched (no automatic repair is correct for a wrong --chain)"
        );
        log(&obs.verdict_line(&reason));
        return CheckOutcome {
            observation: Some(obs),
            decision: Some(HealDecision::Nothing { reason }),
        };
    }

    let ledger = cfg.ledger.clone().unwrap_or_else(|| ledger_path(&cfg.base));
    let last = match read_recovery(&ledger) {
        Ok(r) => r,
        Err(e) => {
            // Fail closed: a ledger we cannot read must not license a repair.
            log(&format!(
                "divergence check: no verdict — recovery ledger is unreadable ({e}); \
                 refusing to touch the DB"
            ));
            return CheckOutcome {
                observation: Some(obs),
                decision: None,
            };
        }
    };

    let fork_dir = fork_dir_for(&cfg.base, now);
    let decision = heal_decision(
        &obs.verdict,
        last.as_ref(),
        now,
        RECOVERY_WINDOW_SECS,
        &fork_dir,
    );
    let what = match &decision {
        HealDecision::Nothing { reason } => reason.clone(),
        HealDecision::Refused { age_secs } => format!(
            "a recovery was already attempted {}h ago (max one per 24 h) — NEEDS A HUMAN",
            age_secs / 3600
        ),
        HealDecision::Recover { fork_dir } => {
            format!("fork confirmed against the signed checkpoint; heal scheduled, forked DB will move to {}", fork_dir.display())
        }
    };
    log(&obs.verdict_line(&what));
    CheckOutcome {
        observation: Some(obs),
        decision: Some(decision),
    }
}

/// Is a GPU job being served right now? The yield flag is set for the whole GPU
/// section of a job *by the provider that holds the lock* (`gpu_yield`), so this
/// is the cross-process answer, not a guess. Recovery must never stop the node
/// while this is true.
pub fn job_in_flight() -> bool {
    crate::gpu_yield::yield_flag_path().exists()
}

// ------------------------------------------------------------------ recovery

/// Who stops and starts the node. systemd on Linux; the supervisor that owns the
/// child process on Windows (and under `gpn_fleet`/`gpn_node`).
pub trait NodeControl {
    /// Stop the node and do not return until it is gone (recovery renames a live
    /// RocksDB otherwise, which corrupts it).
    fn stop(&self) -> Result<(), String>;
    /// Start it again.
    fn start(&self) -> Result<(), String>;
}

/// The systemd half of the CLI's `--apply`.
#[derive(Debug, Clone)]
pub struct SystemctlUnit {
    pub unit: String,
}

impl NodeControl for SystemctlUnit {
    fn stop(&self) -> Result<(), String> {
        set_unit(&self.unit, "stop")
    }
    fn start(&self) -> Result<(), String> {
        set_unit(&self.unit, "start")
    }
}

/// Stop (or start) a systemd user unit and treat a failure as fatal — the
/// opposite of the read-only convenience stop.
pub fn set_unit(unit: &str, verb: &str) -> Result<(), String> {
    let st = std::process::Command::new("systemctl")
        .args(["--user", verb, unit])
        .status()
        .map_err(|e| format!("systemctl --user {verb} {unit}: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("systemctl --user {verb} {unit} exited {st}"))
    }
}

/// Download the archive named by the signed document.
pub fn download_snapshot(url: &str, dest: &Path) -> Result<(), String> {
    if url.trim().is_empty() {
        return Err("snapshot.json carries no url — pass an archive path".into());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;
    let mut resp = client
        .get(url)
        .send()
        .map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET {url}: HTTP {}", resp.status()));
    }
    let part = dest.with_extension("part");
    {
        let mut f = File::create(&part).map_err(|e| format!("create {}: {e}", part.display()))?;
        io::copy(&mut resp, &mut f).map_err(|e| format!("download {url}: {e}"))?;
        f.sync_all()
            .map_err(|e| format!("sync {}: {e}", part.display()))?;
    }
    fs::rename(&part, dest).map_err(|e| format!("rename {}: {e}", dest.display()))?;
    Ok(())
}

/// Everything the destructive half needs. Built by [`plan_recovery`], which is
/// where the "resolve the archive before touching anything" step lives.
pub struct RecoveryPlan {
    pub base: PathBuf,
    pub ledger: PathBuf,
    pub fork_dir: PathBuf,
    pub snapshot: PathBuf,
    pub checkpoint_block: u64,
    pub checkpoint_hash: String,
    pub local_hash: String,
    pub now: u64,
}

/// Resolve (and verify) the archive for a decided recovery. Downloads only when
/// no local archive was supplied. Returns the plan — nothing has been touched
/// yet, so every failure here leaves a running node running.
pub fn plan_recovery(
    meta: &SnapshotMeta,
    meta_bytes: &[u8],
    pubkey_hex: &str,
    base: &Path,
    ledger: &Path,
    snapshot: Option<PathBuf>,
    download_dir: &Path,
    local_hash: String,
    now: u64,
    log: Log,
) -> Result<RecoveryPlan, String> {
    let (block, hash) = meta
        .checkpoint()
        .ok_or("metadata carries no checkpoint — nothing to heal toward")?;
    let snapshot = match snapshot {
        Some(p) => p,
        None => {
            let name = meta
                .url
                .rsplit('/')
                .find(|s| s.contains('.'))
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("gpn-snapshot-{block}.tar.gz"));
            let dest = download_dir.join(name);
            log(&format!(
                "self-heal: downloading {} -> {}",
                meta.url,
                dest.display()
            ));
            download_snapshot(&meta.url, &dest)?;
            dest
        }
    };
    // Trust the bytes before we trust the import (size + sha256 vs the signed
    // document, the same check a fresh installer does).
    verify_snapshot_meta(meta_bytes, pubkey_hex, Some(&snapshot))?;
    Ok(RecoveryPlan {
        base: base.to_path_buf(),
        ledger: ledger.to_path_buf(),
        fork_dir: fork_dir_for(base, now),
        snapshot,
        checkpoint_block: block,
        checkpoint_hash: hash.to_string(),
        local_hash,
        now,
    })
}

/// The recovery sequence (design §"Recovery sequence"), unchanged in order:
///
/// 1. stop the node (fatal if it fails, and nothing is written: a stop that
///    failed has not touched the DB and must not consume the window);
/// 2. write the record with `completed: false` — the window is consumed *before*
///    the destructive step, so a crash mid-repair can never loop;
/// 3. `chains/` → `chains-fork-<ts>` (rename, never delete, never reuse);
/// 4. import the archive (caller's closure — no duplicate extractor);
/// 5. mark the record complete and start the node.
///
/// If the import fails the node is left **stopped** and the record says why: a
/// node that comes back on the fork looks healthier than it is.
pub fn perform_recovery(
    plan: &RecoveryPlan,
    control: &dyn NodeControl,
    import: &dyn Fn(&Path, &Path) -> Result<(), String>,
    log: Log,
) -> Result<(), String> {
    let mut rec = RecoveryRecord {
        recovered_at: plan.now,
        recovered_at_iso: iso8601(plan.now),
        checkpoint_block: plan.checkpoint_block,
        checkpoint_hash: plan.checkpoint_hash.clone(),
        local_hash: plan.local_hash.clone(),
        fork_dir: plan.fork_dir.display().to_string(),
        snapshot: plan.snapshot.display().to_string(),
        completed: false,
        detail: "started".into(),
    };

    // Stop FIRST. Nothing has been touched yet, so a stop that fails must not
    // consume the 24 h window: the node is still serving its (wrong) chain and
    // the next tick may well succeed (a job finishing, a slow shutdown). The
    // window is written below, before the first destructive step.
    control.stop()?;

    // From here the window is consumed even if the process dies mid-repair.
    write_recovery(&plan.ledger, &rec)?;

    let chains = plan.base.join("chains");
    if chains.exists() {
        fs::rename(&chains, &plan.fork_dir).map_err(|e| {
            format!(
                "move {} -> {}: {e}",
                chains.display(),
                plan.fork_dir.display()
            )
        })?;
        log(&format!(
            "self-heal: moved forked DB to {}",
            plan.fork_dir.display()
        ));
    } else {
        log(&format!(
            "self-heal: no chains/ dir at {} — importing into a fresh layout",
            chains.display()
        ));
    }

    if let Err(e) = import(&plan.snapshot, &plan.base) {
        rec.detail = format!("import failed: {e}");
        let _ = write_recovery(&plan.ledger, &rec);
        log(&format!(
            "self-heal: import FAILED ({e}). Node left STOPPED; forked DB is at {}. \
             Not restarting — it would come back on the fork.",
            plan.fork_dir.display()
        ));
        return Err(e);
    }

    rec.completed = true;
    rec.detail = "healed: forked DB set aside and signed snapshot imported".into();
    write_recovery(&plan.ledger, &rec)?;
    control.start()?;
    log(&format!(
        "self-heal: done — imported signed snapshot from block {}; node started; \
         fork DB kept at {} (delete when the node is verified healthy)",
        plan.checkpoint_block,
        plan.fork_dir.display()
    ));
    Ok(())
}

/// Wall-clock seconds, saturating (a clock before 1970 is not a reason to panic).
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;

    const GENESIS: &str = "0xb8e4636a00000000000000000000000000000000000000000000000000000001";
    const OTHER_GENESIS: &str =
        "0xdeadbeef00000000000000000000000000000000000000000000000000000002";
    const CANON: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const FORK: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn signed(
        sk: &ed25519_dalek::SigningKey,
        block: u64,
        block_hash: &str,
        genesis: &str,
    ) -> (Vec<u8>, [u8; 32]) {
        let mut m = SnapshotMeta {
            format: SNAPSHOT_CANONICAL_PREFIX.into(),
            url: "https://example.invalid/snap.tar.gz".into(),
            sha256: "ab".repeat(32),
            size: 1234,
            block: Some(block),
            genesis: genesis.into(),
            block_hash: block_hash.into(),
            built: "2026-11-13T00:00:00Z".into(),
            signing_key: hex::encode(sk.verifying_key().to_bytes()),
            signature: String::new(),
        };
        m.signature = hex::encode(sk.sign(&m.canonical_text()).to_bytes());
        (
            serde_json::to_vec(&m).unwrap(),
            sk.verifying_key().to_bytes(),
        )
    }

    fn record_at(ts: u64) -> RecoveryRecord {
        RecoveryRecord {
            recovered_at: ts,
            recovered_at_iso: iso8601(ts),
            ..Default::default()
        }
    }

    /// A one-shot JSON-RPC stub: answers exactly the three calls `observe` makes
    /// (genesis, header, block hash) with canned values. Runs in a thread and
    /// returns its URL.
    fn rpc_stub(height: u64, hash_at_checkpoint: &str) -> String {
        use std::io::{BufRead, BufReader};
        use std::net::TcpListener;
        let hash_at_checkpoint = hash_at_checkpoint.to_string();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            loop {
                let Ok((mut s, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(s.try_clone().unwrap());
                // Read headers until the blank line, then exactly `len` body bytes.
                // (Header order is not ours to assume: `content-length` may come
                // before or after `host`.)
                let mut len = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = line
                        .to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().to_string())
                    {
                        len = v.parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; len];
                use std::io::{Read as _, Write as _};
                if reader.read_exact(&mut body).is_err() {
                    continue;
                }
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
                let params = req.get("params").cloned().unwrap_or_default();
                let result = match method {
                    "chain_getBlockHash" => {
                        let idx = params.get(0).and_then(|v| v.as_u64());
                        match idx {
                            Some(0) => serde_json::json!(GENESIS),
                            _ => serde_json::json!(hash_at_checkpoint),
                        }
                    }
                    "chain_getHeader" => serde_json::json!({"number": format!("0x{height:x}")}),
                    _ => serde_json::Value::Null,
                };
                let resp = serde_json::json!({"jsonrpc":"2.0","id":1,"result":result}).to_string();
                let _ = s.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{resp}",
                        resp.len()
                    )
                    .as_bytes(),
                );
                let _ = s.flush();
            }
        });
        url
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gpn-selfheal-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn cfg(dir: &Path, rpc: String, meta_src: String, pk: &[u8; 32]) -> CheckConfig {
        CheckConfig {
            rpc,
            base: dir.to_path_buf(),
            meta_src,
            pubkey_hex: hex::encode(pk),
            ledger: None,
        }
    }

    fn log_sink() -> (std::sync::Arc<std::sync::Mutex<Vec<String>>>, impl Fn(&str)) {
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let l = lines.clone();
        let f = move |s: &str| l.lock().unwrap().push(s.to_string());
        (lines, f)
    }

    // ── (a) a fork schedules a heal ─────────────────────────────────────────

    #[test]
    fn a_node_whose_hash_differs_at_the_checkpoint_schedules_a_heal() {
        let dir = tmpdir("fork");
        let sk = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let (json, pk) = signed(&sk, 297_000, CANON, GENESIS);
        let meta_path = dir.join("snapshot.json");
        fs::write(&meta_path, &json).unwrap();
        let rpc = rpc_stub(297_100, FORK);
        let (lines, log) = log_sink();
        let out = check(
            &cfg(&dir, rpc, meta_path.display().to_string(), &pk),
            now_unix(),
            &log,
        );
        let (obs, fork_dir) = out.recovery().expect("a fork must schedule a recovery");
        assert_eq!(obs.verdict.slug(), "wrong-chain");
        assert_eq!(obs.local_hash.as_deref(), Some(FORK));
        assert_eq!(obs.checkpoint_hash.trim(), CANON);
        assert!(fork_dir.starts_with(&dir));
        // Both required INFO lines, naming local hash / checkpoint hash / verdict.
        let lines = lines.lock().unwrap();
        assert!(
            lines.iter().any(|l| l.starts_with("divergence check:")
                && l.contains("#297100")
                && l.contains("#297000")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("divergence verdict: wrong-chain")
                    && l.contains(FORK)
                    && l.contains(CANON)),
            "{lines:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    // ── (b) a matching node does not ────────────────────────────────────────

    #[test]
    fn a_node_whose_hash_matches_the_checkpoint_does_nothing() {
        let dir = tmpdir("match");
        let sk = ed25519_dalek::SigningKey::from_bytes(&[8u8; 32]);
        let (json, pk) = signed(&sk, 297_000, CANON, GENESIS);
        let meta_path = dir.join("snapshot.json");
        fs::write(&meta_path, &json).unwrap();
        let rpc = rpc_stub(297_050, CANON);
        let (lines, log) = log_sink();
        let out = check(
            &cfg(&dir, rpc, meta_path.display().to_string(), &pk),
            now_unix(),
            &log,
        );
        assert!(out.recovery().is_none(), "a matching chain must not heal");
        match out.decision {
            Some(HealDecision::Nothing { .. }) => {}
            other => panic!("expected Nothing, got {other:?}"),
        }
        assert!(lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("divergence verdict: match")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn being_behind_the_checkpoint_is_not_a_fork() {
        let dir = tmpdir("behind");
        let sk = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let (json, pk) = signed(&sk, 297_000, CANON, GENESIS);
        let meta_path = dir.join("snapshot.json");
        fs::write(&meta_path, &json).unwrap();
        let rpc = rpc_stub(280_610, FORK); // box 1's shape: behind AND forked later
        let (_, log) = log_sink();
        let out = check(
            &cfg(&dir, rpc, meta_path.display().to_string(), &pk),
            now_unix(),
            &log,
        );
        assert!(out.recovery().is_none(), "behind is not forked");
        let _ = fs::remove_dir_all(&dir);
    }

    // ── (c) unverifiable metadata never triggers one ────────────────────────

    #[test]
    fn unverifiable_metadata_never_schedules_a_heal() {
        let dir = tmpdir("unsigned");
        let sk = ed25519_dalek::SigningKey::from_bytes(&[10u8; 32]);
        let (json, pk) = signed(&sk, 297_000, CANON, GENESIS);
        let meta_path = dir.join("snapshot.json");

        // (1) tampered: the bytes no longer match the signature
        let tampered = String::from_utf8(json.clone())
            .unwrap()
            .replace("\"block\":297000", "\"block\":297001");
        fs::write(&meta_path, tampered).unwrap();
        let rpc = rpc_stub(297_100, FORK);
        let (lines, log) = log_sink();
        let out = check(
            &cfg(&dir, rpc.clone(), meta_path.display().to_string(), &pk),
            now_unix(),
            &log,
        );
        assert!(out.recovery().is_none());
        assert!(out.decision.is_none(), "no verdict from unverified bytes");

        // (2) signed by a key we do not trust
        let other = ed25519_dalek::SigningKey::from_bytes(&[11u8; 32]);
        let (foreign, _) = signed(&other, 297_000, CANON, GENESIS);
        fs::write(&meta_path, foreign).unwrap();
        let out = check(
            &cfg(&dir, rpc.clone(), meta_path.display().to_string(), &pk),
            now_unix(),
            &log,
        );
        assert!(out.recovery().is_none());

        // (3) not fetchable at all
        let out = check(
            &cfg(
                &dir,
                rpc,
                "https://127.0.0.1:1/snapshot.json".to_string(),
                &pk,
            ),
            now_unix(),
            &log,
        );
        assert!(out.recovery().is_none());
        assert!(lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("no verdict")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_different_genesis_is_never_repaired() {
        let dir = tmpdir("net");
        let sk = ed25519_dalek::SigningKey::from_bytes(&[12u8; 32]);
        let (json, pk) = signed(&sk, 297_000, CANON, OTHER_GENESIS);
        let meta_path = dir.join("snapshot.json");
        fs::write(&meta_path, &json).unwrap();
        let rpc = rpc_stub(297_100, FORK);
        let (lines, log) = log_sink();
        let out = check(
            &cfg(&dir, rpc, meta_path.display().to_string(), &pk),
            now_unix(),
            &log,
        );
        assert!(out.recovery().is_none(), "wrong network is not a fork");
        assert!(lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("different NETWORK")));
        let _ = fs::remove_dir_all(&dir);
    }

    // ── (d) the ledger blocks a second heal within 24 h ─────────────────────

    #[test]
    fn the_ledger_blocks_a_second_heal_within_24_hours() {
        let dir = tmpdir("ledger");
        let sk = ed25519_dalek::SigningKey::from_bytes(&[13u8; 32]);
        let (json, pk) = signed(&sk, 297_000, CANON, GENESIS);
        let meta_path = dir.join("snapshot.json");
        fs::write(&meta_path, &json).unwrap();
        let rpc = rpc_stub(297_100, FORK);
        let now = now_unix();
        write_recovery(&ledger_path(&dir), &record_at(now - 3600)).unwrap();
        let (lines, log) = log_sink();
        let out = check(
            &cfg(&dir, rpc.clone(), meta_path.display().to_string(), &pk),
            now,
            &log,
        );
        assert!(out.recovery().is_none(), "must not heal twice in 24 h");
        match out.decision {
            Some(HealDecision::Refused { age_secs }) => assert_eq!(age_secs, 3600),
            other => panic!("expected Refused, got {other:?}"),
        }
        assert!(lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("max one per 24 h")));

        // ...and exactly 24 h later it is allowed again.
        let out = check(
            &cfg(&dir, rpc, meta_path.display().to_string(), &pk),
            now - 3600 + RECOVERY_WINDOW_SECS,
            &log,
        );
        assert!(out.recovery().is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_ledger_fails_closed() {
        let dir = tmpdir("corrupt");
        fs::write(ledger_path(&dir), b"{not json").unwrap();
        let sk = ed25519_dalek::SigningKey::from_bytes(&[14u8; 32]);
        let (json, pk) = signed(&sk, 297_000, CANON, GENESIS);
        let meta_path = dir.join("snapshot.json");
        fs::write(&meta_path, &json).unwrap();
        let rpc = rpc_stub(297_100, FORK);
        let (_, log) = log_sink();
        let out = check(
            &cfg(&dir, rpc, meta_path.display().to_string(), &pk),
            now_unix(),
            &log,
        );
        assert!(out.recovery().is_none(), "unreadable ledger must refuse");
        let _ = fs::remove_dir_all(&dir);
    }

    // ── the destructive sequence ────────────────────────────────────────────

    struct Recorder {
        stopped: std::sync::Mutex<Vec<&'static str>>,
    }
    impl Recorder {
        fn new() -> Self {
            Self {
                stopped: std::sync::Mutex::new(Vec::new()),
            }
        }
    }
    impl NodeControl for Recorder {
        fn stop(&self) -> Result<(), String> {
            self.stopped.lock().unwrap().push("stop");
            Ok(())
        }
        fn start(&self) -> Result<(), String> {
            self.stopped.lock().unwrap().push("start");
            Ok(())
        }
    }

    #[test]
    fn recovery_stops_first_then_moves_the_fork_aside_then_imports() {
        let dir = tmpdir("perform");
        let chains = dir.join("chains");
        fs::create_dir_all(chains.join("gpn_testnet")).unwrap();
        fs::write(chains.join("gpn_testnet").join("db"), b"forked").unwrap();
        let ledger = ledger_path(&dir);
        let plan = RecoveryPlan {
            base: dir.clone(),
            ledger: ledger.clone(),
            fork_dir: fork_dir_for(&dir, 1_700_000_000),
            snapshot: dir.join("snap.tar.gz"),
            checkpoint_block: 297_000,
            checkpoint_hash: CANON.into(),
            local_hash: FORK.into(),
            now: 1_700_000_000,
        };
        let control = Recorder::new();
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let ev = events.clone();
        let (_, log) = log_sink();
        perform_recovery(
            &plan,
            &control,
            &move |_snap: &Path, base: &Path| {
                // The node must already be stopped and chains/ already moved.
                assert!(!base.join("chains").exists(), "chains/ must be gone");
                ev.lock().unwrap().push("import".into());
                Ok(())
            },
            &log,
        )
        .unwrap();
        assert_eq!(*control.stopped.lock().unwrap(), vec!["stop", "start"]);
        assert_eq!(*events.lock().unwrap(), vec!["import".to_string()]);
        assert!(plan.fork_dir.join("gpn_testnet/db").exists());
        let rec = read_recovery(&ledger).unwrap().unwrap();
        assert!(rec.completed);
        assert_eq!(rec.checkpoint_block, 297_000);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_stop_writes_no_ledger_and_touches_nothing() {
        // Box 1, 2026-11-13: `taskkill` without /F left the node alive, the
        // recovery refused to move a live DB — correctly — but had already
        // written the ledger, which would have locked the box out of healing
        // for 24 h over an attempt that touched nothing.
        let dir = tmpdir("stopfail");
        let chains = dir.join("chains");
        fs::create_dir_all(chains.join("gpn_testnet")).unwrap();
        let ledger = ledger_path(&dir);
        let plan = RecoveryPlan {
            base: dir.clone(),
            ledger: ledger.clone(),
            fork_dir: fork_dir_for(&dir, 7),
            snapshot: dir.join("snap.tar.gz"),
            checkpoint_block: 297_000,
            checkpoint_hash: CANON.into(),
            local_hash: FORK.into(),
            now: 7,
        };
        struct Stuck;
        impl NodeControl for Stuck {
            fn stop(&self) -> Result<(), String> {
                Err("node child did not stop within 60s".into())
            }
            fn start(&self) -> Result<(), String> {
                panic!("must not start what was never stopped")
            }
        }
        let (_, log) = log_sink();
        assert!(perform_recovery(&plan, &Stuck, &|_, _| Ok(()), &log).is_err());
        assert!(
            read_recovery(&ledger).unwrap().is_none(),
            "no ledger on a failed stop"
        );
        assert!(chains.join("gpn_testnet").exists(), "DB untouched");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_import_leaves_the_node_stopped_and_the_window_consumed() {
        let dir = tmpdir("importfail");
        let chains = dir.join("chains");
        fs::create_dir_all(&chains).unwrap();
        let ledger = ledger_path(&dir);
        let plan = RecoveryPlan {
            base: dir.clone(),
            ledger: ledger.clone(),
            fork_dir: fork_dir_for(&dir, 42),
            snapshot: dir.join("snap.tar.gz"),
            checkpoint_block: 297_000,
            checkpoint_hash: CANON.into(),
            local_hash: FORK.into(),
            now: 42,
        };
        let control = Recorder::new();
        let (_, log) = log_sink();
        let err =
            perform_recovery(&plan, &control, &|_, _| Err("bad archive".into()), &log).unwrap_err();
        assert!(err.contains("bad archive"));
        // Started is never called: coming back on the fork looks healthier.
        assert_eq!(*control.stopped.lock().unwrap(), vec!["stop"]);
        let rec = read_recovery(&ledger).unwrap().unwrap();
        assert!(!rec.completed);
        assert!(rec.detail.contains("import failed"));
        let _ = fs::remove_dir_all(&dir);
    }
}
