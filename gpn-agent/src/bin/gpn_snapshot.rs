//! `gpn_snapshot` — fast node onboarding via **database snapshots** (GPN-TASKS #23).
//!
//! A fresh GPN node full-syncs every block *and* verifies its Ethash seal, so the
//! sync time grows with chain length (~123k blocks and climbing). The node's
//! native fast-sync modes do **not** work on this chain: `--sync fast` /
//! `fast-unsafe` burst (~270 blk/s) then stall early (the state proof needs a
//! *finalized* block and GPN has no finality gadget); `--sync warp` likewise
//! needs finality. So the reliable path is to hand a new node a **copy of an
//! already-synced RocksDB** and let it catch up only the last few blocks.
//!
//! This tool exports/imports that DB plus an auditable **manifest**:
//!
//! ```text
//!   gpn_snapshot info   [--rpc URL]                       # what a snapshot would record (node up)
//!   gpn_snapshot export --base-path <node dir> --out <file> [--chain-dir <dir>] [--meta <json>] [--allow-live]
//!   gpn_snapshot import --snapshot <file> --base-path <dir> [--force] [--pruning archive|none]
//!   gpn_snapshot verify --meta <snapshot.json> [--snapshot <file>]
//! ```
//!
//! ## Safety — STOP THE NODE FIRST
//! RocksDB must not be copied while a node is writing to it (you can capture a
//! torn / inconsistent DB). **Stop the node, then export** (a bounded
//! maintenance window of seconds). `export` detects a running `gpn-node` bound
//! to the same `--base-path` and refuses unless you pass either:
//!
//!   * `--freeze` — SIGSTOP the node, copy, SIGCONT (a **consistent hot copy**:
//!     the files cannot change while frozen, and unlike a stop it survives a
//!     supervisor like `gpn_fleet` restarting the node mid-copy); or
//!   * `--allow-live` — a best-effort, *unsafe* copy of a running DB (do not use
//!     it for a durable snapshot).
//!
//! Because a stopped node has no RPC, capture the block metadata while the node
//! is still up with `gpn_snapshot info --out meta.json`, then
//! `export --meta meta.json` after stopping. `export` will also read RPC
//! directly if it is reachable (e.g. a peer node).
//!
//! ## Trust
//! A snapshot is only as trustworthy as its source: importers should check the
//! manifest's `genesis_hash` against the chain they expect, and (for anything
//! security-sensitive) a recent-block hash against an independent node. The
//! archive carries a `sha256` in its manifest for integrity, not authenticity.
//!
//! `verify` closes that gap for the **public weekly snapshot**: it checks a
//! `snapshot.json` signed with the release ed25519 key (the same one embedded in
//! `gpn_node` for `releases.json`), and — with `--snapshot` — the downloaded
//! archive's exact size and sha256 against it. Installers use it to decide
//! whether the bytes are worth importing; an unsigned page scrape is never
//! trusted.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const DEFAULT_RPC: &str = "http://127.0.0.1:9944";
const MANIFEST_NAME: &str = "MANIFEST.json";

// ------------------------------------------------------------------ manifest

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct ArchiveInfo {
    file: String,
    format: String,
    bytes: u64,
    sha256: String,
    /// Files packed under `chains/`.
    file_count: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct Manifest {
    tool: String,
    format_version: u32,
    created_at: u64,
    created_at_iso: String,
    source_base_path: String,
    /// e.g. `gpn_testnet`
    chain_dir: String,
    /// archive-relative root, e.g. `chains/gpn_testnet/db`
    db_relative_path: String,
    best_block_number: Option<u64>,
    best_block_hash: Option<String>,
    genesis_hash: Option<String>,
    system_chain: Option<String>,
    system_version: Option<String>,
    system_name: Option<String>,
    spec_name: Option<String>,
    spec_version: Option<u64>,
    impl_version: Option<u64>,
    /// True if the export was a best-effort copy of a live node (unsafe).
    live_export: bool,
    /// Pruning mode of the source DB, when known (e.g. `"archive"`). Filled in
    /// from the `.gpn-pruning-archive` marker `gpn_node` drops next to a DB it
    /// created with archive pruning. On import this is re-materialised as the
    /// same marker so `gpn_node` knows the imported DB is safe for archive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pruning: Option<String>,
    archive: ArchiveInfo,
}

/// Marker file (kept in sync with `gpn_node.rs`) that records "this DB was
/// created with archive pruning" — the only thing that lets `gpn_node` safely
/// request `archive` on an *existing* DB.
const PRUNING_MARKER: &str = ".gpn-pruning-archive";

fn read_pruning_marker(base: &Path) -> Option<String> {
    if base.join(PRUNING_MARKER).is_file() {
        Some("archive".to_string())
    } else {
        None
    }
}

fn write_pruning_marker(base: &Path) {
    let _ = fs::create_dir_all(base);
    let _ = fs::write(base.join(PRUNING_MARKER), b"archive\n");
}

fn iso8601(ts: u64) -> String {
    // Minimal, dependency-free civil-time conversion (UTC).
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

// ------------------------------------------------------------------ rpc

fn rpc_call(
    url: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
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

/// Fill a manifest's chain metadata from a live node's JSON-RPC (best effort).
fn read_meta(rpc: &str, m: &mut Manifest) -> Result<(), String> {
    let header = rpc_call(rpc, "chain_getHeader", serde_json::json!([]))?;
    m.best_block_number = header.get("number").and_then(hex_u64);
    if let Some(parent) = header.get("parentHash").and_then(|x| x.as_str()) {
        // best hash is the header's own hash; get it explicitly
        let _ = parent;
    }
    m.best_block_hash = rpc_call(rpc, "chain_getBlockHash", serde_json::json!([]))
        .ok()
        .and_then(|v| v.as_str().map(String::from));
    m.genesis_hash = rpc_call(rpc, "chain_getBlockHash", serde_json::json!([0]))
        .ok()
        .and_then(|v| v.as_str().map(String::from));
    m.system_chain = rpc_call(rpc, "system_chain", serde_json::json!([]))
        .ok()
        .and_then(|v| v.as_str().map(String::from));
    m.system_name = rpc_call(rpc, "system_name", serde_json::json!([]))
        .ok()
        .and_then(|v| v.as_str().map(String::from));
    m.system_version = rpc_call(rpc, "system_version", serde_json::json!([]))
        .ok()
        .and_then(|v| v.as_str().map(String::from));
    if let Ok(rv) = rpc_call(rpc, "state_getRuntimeVersion", serde_json::json!([])) {
        m.spec_name = rv
            .get("specName")
            .and_then(|x| x.as_str())
            .map(String::from);
        m.spec_version = rv.get("specVersion").and_then(|x| x.as_u64());
        m.impl_version = rv.get("implVersion").and_then(|x| x.as_u64());
    }
    Ok(())
}

// ------------------------------------------------------------------ args

struct Args {
    cmd: String,
    base_path: Option<PathBuf>,
    out: Option<PathBuf>,
    snapshot: Option<PathBuf>,
    chain_dir: Option<String>,
    meta: Option<PathBuf>,
    rpc: String,
    allow_live: bool,
    freeze: bool,
    force: bool,
    /// Explicit pruning mode for `import`: `archive` or `none`. Overrides the
    /// manifest. When absent, import defaults to `archive` (see `import`).
    pruning: Option<String>,
    /// `checkpoint --stop-unit <unit>`: a systemd **user** unit to stop when the
    /// signed checkpoint says this node is on the wrong chain. Optional —
    /// without it the command only reports and sets the exit code.
    stop_unit: Option<String>,
    /// `heal --unit <unit>`: the systemd **user** unit that runs the node. It is
    /// stopped before the fork DB is set aside and started again after the
    /// import — required (`--apply` only), because a heal that leaves the unit
    /// down is a nicer-looking outage.
    unit: Option<String>,
    /// `heal --state <file>`: where the one-recovery-per-24 h record lives.
    /// Defaults to `<base-path>/last-recovery.json`.
    state: Option<PathBuf>,
    /// `heal --apply`: actually stop/rename/import/start. Without it the command
    /// only reports the plan (the safe default for a destructive repair).
    apply: bool,
    /// `heal --download-dir <dir>`: where to fetch the signed snapshot when
    /// `--snapshot` was not supplied.
    download_dir: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() {
        return Err(usage());
    }
    let mut cmd = String::new();
    let mut flags: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    let mut it = raw.into_iter().peekable();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--") {
            let (k, val) = match v.split_once('=') {
                Some((k, val)) => (k.to_string(), Some(val.to_string())),
                None => (v.to_string(), None),
            };
            // allow `--flag value`
            if val.is_none() {
                if let Some(next) = it.peek() {
                    if !next.starts_with("--") {
                        let v2 = it.next().unwrap();
                        flags.insert(k, Some(v2));
                        continue;
                    }
                }
            }
            flags.insert(k, val);
        } else if cmd.is_empty() {
            cmd = a;
        } else {
            return Err(format!("unexpected argument: {a}"));
        }
    }
    let get = |k: &str| flags.get(k).and_then(|v| v.clone());
    let has = |k: &str| flags.contains_key(k);
    let base_path = get("base-path").or_else(|| std::env::var("GPN_NODE_DATA").ok());
    Ok(Args {
        cmd,
        base_path: base_path.map(PathBuf::from),
        out: get("out").map(PathBuf::from),
        snapshot: get("snapshot").map(PathBuf::from),
        chain_dir: get("chain-dir"),
        meta: get("meta").map(PathBuf::from),
        rpc: get("rpc").unwrap_or_else(|| DEFAULT_RPC.to_string()),
        allow_live: has("allow-live"),
        freeze: has("freeze"),
        force: has("force"),
        pruning: get("pruning"),
        stop_unit: get("stop-unit"),
        unit: get("unit"),
        state: get("state").map(PathBuf::from),
        apply: has("apply"),
        download_dir: get("download-dir").map(PathBuf::from),
    })
}

fn usage() -> String {
    "usage:
  gpn_snapshot info   [--rpc URL] [--out meta.json]
  gpn_snapshot export --base-path <node dir> --out <file.tar.gz|.tar.zst> [--chain-dir <dir>] [--meta <json>] [--rpc URL] [--freeze] [--allow-live]
  gpn_snapshot import --snapshot <file> --base-path <dir> [--force] [--pruning archive|none]
  gpn_snapshot verify --meta <snapshot.json> [--snapshot <file>]
  gpn_snapshot checkpoint --meta <snapshot.json> [--rpc URL] [--stop-unit <unit>]
     compares this node's own hash at the signed checkpoint height;
     exit 0 = agree (or not there yet), exit 12 = WRONG CHAIN, exit 1 = could not decide
  gpn_snapshot heal --meta <snapshot.json> --base-path <dir> [--snapshot <file>] [--rpc URL]
                    [--unit <unit>] [--state <file>] [--download-dir <dir>] [--apply]
     self-heal a node whose hash at the signed checkpoint height is a fork:
     stop the unit, set the fork DB aside as chains-fork-<ts>, import the signed
     snapshot, start the unit again. At most one recovery per 24 h.
     exit 0 = nothing to do / healed, exit 12 = wrong chain (dry run: plan only),
     exit 13 = a recovery was already attempted in the last 24 h, exit 1 = error
".to_string()
}

// ------------------------------------------------------------------ fs helpers

fn find_chain_dir(base_path: &Path, want: Option<&str>) -> Result<String, String> {
    if let Some(w) = want {
        let p = base_path.join("chains").join(w).join("db");
        if p.is_dir() {
            return Ok(w.to_string());
        }
        return Err(format!("--chain-dir {w}: no db at {}", p.display()));
    }
    let chains = base_path.join("chains");
    let mut found = Vec::new();
    for e in fs::read_dir(&chains).map_err(|e| format!("read {}: {e}", chains.display()))? {
        let e = e.map_err(|e| e.to_string())?;
        if e.path().join("db").join("full").is_dir() {
            found.push(e.file_name().to_string_lossy().to_string());
        }
    }
    match found.len() {
        0 => Err(format!(
            "no chain DB found under {} (expected <base>/chains/<name>/db/full)",
            chains.display()
        )),
        1 => Ok(found.remove(0)),
        _ => Err(format!(
            "multiple chain DBs under {}: {} — pass --chain-dir",
            chains.display(),
            found.join(", ")
        )),
    }
}

/// Any `gpn-node` process whose cmdline references this base-path?
fn live_node_pid(base_path: &Path) -> Option<u32> {
    let base = base_path.to_string_lossy();
    let proc = Path::new("/proc");
    if !proc.is_dir() {
        return None; // non-Linux: rely on the RPC heuristic elsewhere
    }
    let entries = fs::read_dir(proc).ok()?;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Ok(cmd) = fs::read_to_string(e.path().join("cmdline")) {
            let cmd = cmd.replace('\0', " ");
            if cmd.contains("gpn-node") && cmd.contains(base.as_ref()) {
                return name.parse().ok();
            }
        }
    }
    None
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut f = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn human(bytes: u64) -> String {
    let mut b = bytes as f64;
    for u in ["B", "KiB", "MiB", "GiB", "TiB"] {
        if b < 1024.0 {
            return format!("{b:.1} {u}");
        }
        b /= 1024.0;
    }
    format!("{b:.1} PiB")
}

// ------------------------------------------------------------------ tar build

fn append_file<W: Write>(b: &mut tar::Builder<W>, path: &str, data: &[u8]) -> io::Result<()> {
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_mtime(0);
    h.set_cksum();
    b.append_data(&mut h, path, data)
}

/// Archive entries are named relative to `rel_base` (the node base-path), so
/// that `import` can extract them directly into a fresh base-path and reproduce
/// `chains/<dir>/db/...`.
fn build_tar<W: Write>(
    w: W,
    manifest_json: &[u8],
    db_src: &Path,
    rel_base: &Path,
) -> io::Result<(W, u64)> {
    let mut b = tar::Builder::new(w);
    b.follow_symlinks(false);
    append_file(&mut b, MANIFEST_NAME, manifest_json)?;
    let mut count = 0u64;
    for entry in walk(db_src)? {
        let rel = entry.strip_prefix(rel_base).unwrap_or(&entry);
        b.append_path_with_name(&entry, rel)?;
        count += 1;
    }
    let inner = b.into_inner()?;
    Ok((inner, count))
}

fn walk(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d)? {
            let e = e?;
            let p = e.path();
            let ft = e.file_type()?;
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

// ------------------------------------------------------------------ freeze

/// SIGSTOP/SIGCONT the running node around the copy: files can't change while
/// frozen, so the copy is consistent — and unlike a stop it survives a
/// supervisor (gpn_fleet) that would otherwise restart the node mid-copy.
/// SIGCONT is guaranteed on drop (including panics / early returns).
struct FreezeGuard {
    pid: Option<u32>,
}

fn send_sig(sig: &str, pid: u32) -> io::Result<()> {
    let st = Command::new("kill")
        .arg(format!("-{sig}"))
        .arg(pid.to_string())
        .status()?;
    if st.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("kill -{sig} {pid} failed")))
    }
}

impl FreezeGuard {
    fn new(pid: u32) -> Result<Self, String> {
        send_sig("STOP", pid)
            .map_err(|e| format!("freeze pid {pid}: {e} (--freeze needs a unix `kill`)"))?;
        Ok(Self { pid: Some(pid) })
    }
}

impl Drop for FreezeGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            if let Err(e) = send_sig("CONT", pid) {
                eprintln!("⚠️  failed to SIGCONT pid {pid}: {e} — it may stay frozen!");
            } else {
                println!("thawed node (pid {pid})");
            }
        }
    }
}

// ------------------------------------------------------------------ export

fn export(a: &Args) -> Result<(), String> {
    let base = a.base_path.as_ref().ok_or("export needs --base-path")?;
    let out = a.out.as_ref().ok_or("export needs --out")?;
    if a.cmd == "export" && out.starts_with(base) {
        return Err("--out must not live inside --base-path".into());
    }
    let chain_dir = find_chain_dir(base, a.chain_dir.as_deref())?;
    let db_src = base.join("chains").join(&chain_dir).join("db");

    // Safety: refuse a live node unless explicitly allowed.
    let mut live_export = false;
    let mut freeze_pid: Option<u32> = None;
    if let Some(pid) = live_node_pid(base) {
        if a.freeze {
            freeze_pid = Some(pid);
            live_export = true;
        } else if a.allow_live {
            eprintln!(
                "⚠️  --allow-live: copying a RUNNING node's DB (best-effort; may be inconsistent)"
            );
            live_export = true;
        } else {
            return Err(format!(
                "a gpn-node (pid {pid}) is writing to {} — STOP IT first, then export.\n\
                 (A live RocksDB copy can be torn/inconsistent. Re-run with --freeze for a\n\
                 hot-copy-safe SIGSTOP/SIGCONT snapshot, or --allow-live for a last-resort\n\
                 best-effort copy.)",
                base.display()
            ));
        }
    }

    let mut m = Manifest {
        tool: "gpn_snapshot".into(),
        format_version: 1,
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        source_base_path: base.display().to_string(),
        chain_dir: chain_dir.clone(),
        db_relative_path: format!("chains/{chain_dir}/db"),
        live_export,
        pruning: read_pruning_marker(base),
        ..Default::default()
    };
    m.created_at_iso = iso8601(m.created_at);

    // Metadata: --meta file wins, else try RPC.
    if let Some(p) = &a.meta {
        let txt = fs::read_to_string(p).map_err(|e| format!("read --meta {}: {e}", p.display()))?;
        let mm: Manifest = serde_json::from_str(&txt).map_err(|e| format!("parse --meta: {e}"))?;
        let keep = m.clone();
        m = mm;
        // retain export-time fields
        m.tool = keep.tool;
        m.format_version = keep.format_version;
        m.created_at = keep.created_at;
        m.created_at_iso = keep.created_at_iso;
        m.source_base_path = keep.source_base_path;
        m.chain_dir = keep.chain_dir;
        m.db_relative_path = keep.db_relative_path;
        m.live_export = keep.live_export;
        // Prefer export-time pruning detection over a (possibly absent) --meta value.
        if m.pruning.is_none() {
            m.pruning = keep.pruning;
        }
    } else if read_meta(&a.rpc, &mut m).is_ok() {
        println!("read best block from RPC {}", a.rpc);
    } else {
        eprintln!("⚠️  no RPC reachable and no --meta given: block metadata will be absent.");
        eprintln!("    Tip: while the node is UP, run `gpn_snapshot info --out meta.json` first.");
    }

    // Write the archive.
    println!("exporting {} -> {}", db_src.display(), out.display());
    let format = match out.extension().and_then(|s| s.to_str()) {
        Some("zst") => "tar.zst",
        _ => "tar.gz",
    };
    m.archive.format = format.to_string();
    m.archive.file = out
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let manifest_seed = serde_json::to_vec_pretty(&m).map_err(|e| e.to_string())?;

    // Freeze the live node (if requested) only for the duration of the copy.
    let _freeze = match freeze_pid {
        Some(pid) => {
            println!("❄️  freezing node (pid {pid}) for a consistent hot copy (SIGSTOP/SIGCONT)");
            Some(FreezeGuard::new(pid)?)
        }
        None => None,
    };

    if format == "tar.zst" {
        let mut child = Command::new("zstd")
            .args(["-q", "-T0", "-f", "-o"])
            .arg(out)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn zstd (needed for .tar.zst output): {e}"))?;
        let stdin = child.stdin.take().unwrap();
        let (stdin, count) =
            build_tar(stdin, &manifest_seed, &db_src, base).map_err(|e| format!("tar: {e}"))?;
        drop(stdin); // close the pipe so zstd flushes
        m.archive.file_count = count;
        let status = child.wait().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("zstd failed: {status}"));
        }
    } else {
        let file = File::create(out).map_err(|e| format!("create {}: {e}", out.display()))?;
        let enc = GzEncoder::new(file, Compression::default());
        let (enc, count) =
            build_tar(enc, &manifest_seed, &db_src, base).map_err(|e| format!("tar: {e}"))?;
        enc.finish().map_err(|e| format!("gzip finish: {e}"))?;
        m.archive.file_count = count;
    }

    m.archive.bytes = fs::metadata(out).map_err(|e| e.to_string())?.len();
    m.archive.sha256 = sha256_file(out).map_err(|e| e.to_string())?;
    // Re-write the sidecar with the final archive info.
    let sidecar = PathBuf::from(format!("{}.manifest.json", out.display()));
    fs::write(&sidecar, serde_json::to_vec_pretty(&m).unwrap()).map_err(|e| e.to_string())?;

    println!(
        "✅ snapshot written: {} ({}, {} files)",
        out.display(),
        human(m.archive.bytes),
        m.archive.file_count
    );
    println!(
        "   best block: #{} {}",
        m.best_block_number
            .map(|n| n.to_string())
            .unwrap_or("?".into()),
        m.best_block_hash.clone().unwrap_or_default()
    );
    println!(
        "   genesis:    {}",
        m.genesis_hash.clone().unwrap_or_default()
    );
    println!("   manifest:   {}", sidecar.display());
    println!("   sha256:     {}", m.archive.sha256);
    println!("\n   To onboard a new node:");
    println!(
        "     gpn_snapshot import --snapshot {} --base-path <new node dir>",
        out.display()
    );
    println!("     gpn-node --chain <chain> --base-path <new node dir> ...");
    Ok(())
}

// ------------------------------------------------------------------ import

fn import(a: &Args) -> Result<(), String> {
    let snap = a.snapshot.as_ref().ok_or("import needs --snapshot")?;
    let base = a.base_path.as_ref().ok_or("import needs --base-path")?;
    let is_zst = snap.extension().and_then(|s| s.to_str()) == Some("zst");

    fs::create_dir_all(base).map_err(|e| format!("create {}: {e}", base.display()))?;
    if !a.force && base.join("chains").exists() {
        return Err(format!(
            "{} already has a chains/ dir — refusing to overwrite (pass --force)",
            base.display()
        ));
    }

    println!("📦 importing {} -> {}", snap.display(), base.display());
    let local_sha = sha256_file(snap).ok();

    // Extract. MANIFEST.json is read (not written); chains/* is unpacked.
    let mut manifest: Option<Manifest> = None;
    let mut count = 0u64;

    let file = File::open(snap).map_err(|e| format!("open {}: {e}", snap.display()))?;
    if is_zst {
        let mut child = Command::new("zstd")
            .args(["-dc"])
            .arg(snap)
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn zstd: {e}"))?;
        let stdout = child.stdout.take().unwrap();
        extract(stdout, base, &mut manifest, &mut count)?;
        let _ = child.wait();
    } else {
        let dec = GzDecoder::new(file);
        extract(dec, base, &mut manifest, &mut count)?;
    }

    match &manifest {
        Some(m) => {
            println!("✅ imported {count} files");
            println!(
                "   source chain: {}  best #{} {}",
                m.chain_dir,
                m.best_block_number
                    .map(|n| n.to_string())
                    .unwrap_or("?".into()),
                m.best_block_hash.clone().unwrap_or_default()
            );
            println!(
                "   genesis:      {}",
                m.genesis_hash.clone().unwrap_or_default()
            );
            println!(
                "   exported:     {} ({})",
                m.created_at_iso,
                human(m.archive.bytes)
            );
            if let Some(sha) = &local_sha {
                let note = if !m.archive.sha256.is_empty() && &m.archive.sha256 != sha {
                    "  ⚠️  DOES NOT MATCH the manifest sha256!"
                } else {
                    ""
                };
                println!("   sha256:       {sha}{note}");
            }
            if m.live_export {
                println!("   ℹ️  source was a consistent hot copy (node frozen during copy).");
            }
            println!("\n   ⚠️  TRUST: a snapshot is only as trustworthy as its source. Verify the");
            println!("      genesis hash and (if security-sensitive) compare a recent block hash");
            println!(
                "      ({} best) against an independent node before relying on it.",
                m.best_block_number
                    .map(|n| n.to_string())
                    .unwrap_or("the".into())
            );
            println!("\n   Start it with the SAME chain spec:");
            println!(
                "     gpn-node --chain <chain> --base-path {} ...",
                base.display()
            );
        }
        None => println!("✅ imported {count} files (no embedded manifest)"),
    }

    // Record the source DB's pruning mode as our marker, so `gpn_node` knows an
    // imported *archive* DB is safe to run with `--state-pruning archive`
    // (without it, gpn_node conservatively leaves pruning alone).
    //
    // Resolution order:
    //   1. explicit `--pruning archive|none` always wins;
    //   2. else the manifest's own pruning field (a modern export records it);
    //   3. else DEFAULT TO ARCHIVE. A `gpn_snapshot` export produced by a
    //      modern `gpn-node` is archive (archive is our default pruning), and
    //      manifests baked before the marker feature existed have no pruning
    //      field at all — so an older snapshot would otherwise leave gpn_node
    //      blind and it would refuse to mark the imported DB. Defaulting to
    //      archive makes a pre-marker snapshot "just work".
    let explicit = a.pruning.as_deref();
    let manifest_mode = manifest.as_ref().and_then(|m| m.pruning.as_deref());
    let defaulted = explicit.is_none() && manifest_mode.is_none();
    let mode = effective_import_pruning(explicit, manifest_mode);
    match mode.as_str() {
        "archive" => {
            write_pruning_marker(base);
            if defaulted {
                println!(
                    "   pruning:      archive (no marker in the manifest \u{2014} assuming a modern \
                     gpn-node export; marked {} for gpn_node)",
                    PRUNING_MARKER
                );
            } else {
                println!(
                    "   pruning:      archive (marked {} for gpn_node)",
                    PRUNING_MARKER
                );
            }
        }
        "none" => {
            println!(
                "   pruning:      left as-is (no {} marker written \u{2014} gpn_node will not force archive)",
                PRUNING_MARKER
            );
        }
        other => {
            return Err(format!("--pruning {other}: expected 'archive' or 'none'"));
        }
    }
    Ok(())
}

/// Resolve the pruning mode an `import` should stamp, in priority order:
/// explicit `--pruning` value, else the manifest's `pruning` field, else the
/// **archive** default (a `gpn_snapshot` export from a modern `gpn-node` is
/// archive; older manifests carry no field). The returned value is
/// lower-cased/trimmed; validation happens in the caller.
fn effective_import_pruning(explicit: Option<&str>, manifest: Option<&str>) -> String {
    let norm = |s: &str| s.trim().to_ascii_lowercase();
    if let Some(e) = explicit {
        return norm(e);
    }
    if let Some(m) = manifest {
        return norm(m);
    }
    "archive".to_string()
}

fn extract<R: Read>(
    r: R,
    dest: &Path,
    manifest: &mut Option<Manifest>,
    count: &mut u64,
) -> Result<(), String> {
    let mut ar = tar::Archive::new(r);
    let entries = ar.entries().map_err(|e| format!("tar: {e}"))?;
    for e in entries {
        let mut e = e.map_err(|e| format!("tar entry: {e}"))?;
        let path = e.path().map_err(|e| e.to_string())?.into_owned();
        let ps = path.to_string_lossy().to_string();
        if ps == MANIFEST_NAME {
            let mut s = String::new();
            e.read_to_string(&mut s).map_err(|e| e.to_string())?;
            *manifest = serde_json::from_str(&s).ok();
            continue;
        }
        if !ps.starts_with("chains/") {
            eprintln!("   skipping unexpected entry: {ps}");
            continue;
        }
        e.unpack_in(dest).map_err(|e| format!("unpack {ps}: {e}"))?;
        *count += 1;
    }
    Ok(())
}

// ------------------------------------------------------------------ info

fn info(a: &Args) -> Result<(), String> {
    let chain_dir = match &a.base_path {
        Some(b) => find_chain_dir(b, a.chain_dir.as_deref()).unwrap_or_else(|_| "?".into()),
        None => "?".into(),
    };
    let mut m = Manifest {
        tool: "gpn_snapshot".into(),
        format_version: 1,
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        source_base_path: a
            .base_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        chain_dir,
        db_relative_path: String::new(),
        ..Default::default()
    };
    m.created_at_iso = iso8601(m.created_at);
    read_meta(&a.rpc, &mut m).map_err(|e| format!("RPC {e} (is the node up? pass --rpc)"))?;
    let json = serde_json::to_string_pretty(&m).map_err(|e| e.to_string())?;
    if let Some(out) = &a.out {
        fs::write(out, &json).map_err(|e| format!("write {}: {e}", out.display()))?;
        println!(
            "wrote {} (best #{} {})",
            out.display(),
            m.best_block_number
                .map(|n| n.to_string())
                .unwrap_or("?".into()),
            m.best_block_hash.clone().unwrap_or_default()
        );
        println!("Now stop the node and run: gpn_snapshot export --base-path <dir> --out <file> --meta {}", out.display());
    } else {
        println!("{json}");
    }
    Ok(())
}

// ------------------------------------------------------------------ verify

// `SnapshotMeta`, its signed canonical text, and the verifier all live in
// `gpn_agent::selfheal` now: the supervisors schedule the same check, and two
// copies of "what counts as a signed checkpoint" is exactly how the signer and
// verifier drift apart.
pub use gpn_agent::selfheal::{
    checkpoint_verdict, fork_dir_for, heal_decision, ledger_path, local_chain_state, read_recovery,
    set_unit, verdict_exit_code, write_recovery, CheckpointVerdict, HealDecision, NodeControl,
    RecoveryRecord, SnapshotMeta, SystemctlUnit, EXIT_HEAL_REFUSED, EXIT_WRONG_CHAIN,
    RECOVERY_STATE_NAME, RECOVERY_WINDOW_SECS, SNAPSHOT_CANONICAL_PREFIX,
    SNAPSHOT_CANONICAL_PREFIX_V1,
};
use gpn_agent::selfheal::{perform_recovery, plan_recovery, verify_snapshot_meta};

fn verify(a: &Args) -> Result<(), String> {
    let meta_path = a
        .meta
        .as_ref()
        .ok_or("verify needs --meta <snapshot.json>")?;
    let bytes = fs::read(meta_path).map_err(|e| format!("read {}: {e}", meta_path.display()))?;
    let m = verify_snapshot_meta(
        &bytes,
        gpn_agent::update::RELEASE_PUBKEY_HEX,
        a.snapshot.as_deref(),
    )?;
    println!("snapshot metadata: SIGNED and valid");
    println!("url={}", m.url);
    println!(
        "block={}",
        m.block.map(|b| b.to_string()).unwrap_or_else(|| "?".into())
    );
    println!("size={}", m.size);
    println!("sha256={}", m.sha256);
    if let Some(p) = a.snapshot.as_deref() {
        println!("file={} (sha256 + size verified)", p.display());
    }
    Ok(())
}

// -------------------------------------------------------------- checkpoint

/// `gpn_snapshot checkpoint` — the enforcement half of the signed checkpoint.
///
/// The signer (`scripts/snapshot-metadata.py`) and the verifier
/// (`SnapshotMeta::canonical_text`) already carry `genesis` + `block_hash` in
/// v2 metadata; this is the part that *compares them to this node*. It is
/// read-only apart from an optional `systemctl --user stop <unit>`.
///
/// Returns the exit code to use (0 = fine, 12 = wrong chain).
fn checkpoint(a: &Args) -> Result<i32, String> {
    let meta_path = a
        .meta
        .as_ref()
        .ok_or("checkpoint needs --meta <snapshot.json>")?;
    let bytes = fs::read(meta_path).map_err(|e| format!("read {}: {e}", meta_path.display()))?;
    let m = verify_snapshot_meta(&bytes, gpn_agent::update::RELEASE_PUBKEY_HEX, None)?;

    let is_v2 = m.format.trim() == SNAPSHOT_CANONICAL_PREFIX;
    let (block, block_hash) = match (is_v2, m.block) {
        (true, Some(b)) if !m.block_hash.trim().is_empty() => (b, m.block_hash.trim().to_string()),
        _ => {
            println!("checkpoint: v1 metadata — signed, but it carries no checkpoint; nothing to compare");
            return Ok(0);
        }
    };

    // Genesis first: it is the cheapest way to catch "this is a different
    // network", and a genesis mismatch can never be a transient.
    let local_genesis = rpc_call(&a.rpc, "chain_getBlockHash", serde_json::json!([0]))
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_ascii_lowercase()));
    if let Some(local) = &local_genesis {
        let want = m.genesis.trim().to_ascii_lowercase();
        if !want.is_empty() && *local != want {
            eprintln!("wrong chain (genesis): signed checkpoint is for genesis {want}, this node has {local}");
            stop_unit_if_requested(a);
            return Ok(EXIT_WRONG_CHAIN);
        }
    }

    let local_height = rpc_call(&a.rpc, "chain_getHeader", serde_json::json!([]))
        .ok()
        .and_then(|h| h.get("number").and_then(hex_u64));

    // Only ask for the hash when we could be at that height at all, so a fresh
    // node does not produce a spurious RPC error.
    let local_hash = match local_height {
        Some(h) if h >= block => rpc_call(&a.rpc, "chain_getBlockHash", serde_json::json!([block]))
            .ok()
            .and_then(|v| v.as_str().map(String::from)),
        _ => None,
    };

    let verdict = checkpoint_verdict(block, &block_hash, local_height, local_hash.as_deref());
    match &verdict {
        CheckpointVerdict::Match { block } => {
            println!("checkpoint: OK — local block {block} matches the signed hash");
        }
        CheckpointVerdict::NotYet {
            local_height: Some(h),
            block,
        } => {
            println!("checkpoint: not there yet — local height {h} < checkpoint {block}");
        }
        CheckpointVerdict::NotYet {
            local_height: None,
            block,
        } => {
            eprintln!(
                "checkpoint: could not read this node's height ({}) — no verdict, checkpoint {block}",
                a.rpc
            );
            return Ok(1);
        }
        CheckpointVerdict::WrongChain {
            block,
            expected,
            local,
        } => {
            eprintln!(
                "wrong chain: signed checkpoint block {block} is {expected}, this node has {local}"
            );
            eprintln!(
                "refusing to reorg onto a fork — stop the node and re-sync from a trusted source"
            );
            stop_unit_if_requested(a);
        }
    }
    Ok(verdict_exit_code(&verdict))
}

/// Stop the configured unit (best effort, and never fatal: the exit code is the
/// contract, the stop is a convenience for a supervisor that does not watch it).
fn stop_unit_if_requested(a: &Args) {
    let Some(unit) = a.stop_unit.as_deref() else {
        return;
    };
    match Command::new("systemctl")
        .args(["--user", "stop", unit])
        .status()
    {
        Ok(s) if s.success() => eprintln!("stopped {unit}"),
        Ok(s) => eprintln!("could not stop {unit} (systemctl exit {s})"),
        Err(e) => eprintln!("could not run systemctl to stop {unit}: {e}"),
    }
}

// ------------------------------------------------------------------ heal (#157)

/// `gpn_snapshot heal` — the recovery half of the signed checkpoint (#157).
///
/// Read-only unless `--apply`. The decision rules, the ledger and the recovery
/// sequence live in [`gpn_agent::selfheal`] — the same code the node supervisor
/// schedules every 5 minutes — so the manual path and the automatic path cannot
/// disagree. What is local to this binary is the *import*: `import()` above is
/// the only extractor in the tree.
///
/// With `--apply` the order is fixed and each step is predicated on the previous:
/// write the record (`completed: false`, consuming the 24 h window) → stop the
/// unit → rename `chains/` → import → mark complete → start the unit.
fn heal(a: &Args) -> Result<i32, String> {
    let meta_path = a.meta.as_ref().ok_or("heal needs --meta <snapshot.json>")?;
    let base = a.base_path.as_ref().ok_or("heal needs --base-path")?;
    let bytes = fs::read(meta_path).map_err(|e| format!("read {}: {e}", meta_path.display()))?;
    let m = verify_snapshot_meta(&bytes, gpn_agent::update::RELEASE_PUBKEY_HEX, None)?;

    if m.checkpoint().is_none() {
        println!("heal: v1 metadata — signed, but it carries no checkpoint; nothing to compare");
        return Ok(0);
    }
    let (block, block_hash) = m.checkpoint().expect("checked above");
    let block_hash = block_hash.to_string();

    let (height, local_hash, local_genesis) = local_chain_state(&a.rpc, block);

    // A genesis mismatch is a different network, not a fork: there is no correct
    // automatic repair for "this box is pointed at another chain".
    if let (Some(local), want) = (&local_genesis, m.genesis.trim().to_ascii_lowercase()) {
        if !want.is_empty() && *local != want {
            eprintln!(
                "heal: genesis mismatch — signed checkpoint is for {want}, this node has {local}; \
                 refusing to touch the DB (wrong network, not a fork)"
            );
            return Ok(EXIT_WRONG_CHAIN);
        }
    }

    let verdict = checkpoint_verdict(block, &block_hash, height, local_hash.as_deref());
    let now = gpn_agent::selfheal::now_unix();
    let state_path = a.state.clone().unwrap_or_else(|| ledger_path(base));
    let last = read_recovery(&state_path)?;
    let fork_dir = fork_dir_for(base, now);
    let decision = heal_decision(
        &verdict,
        last.as_ref(),
        now,
        RECOVERY_WINDOW_SECS,
        &fork_dir,
    );

    match decision {
        HealDecision::Nothing { reason } => {
            println!("heal: nothing to do — {reason}");
            Ok(0)
        }
        HealDecision::Refused { age_secs } => {
            let hours = age_secs / 3600;
            let rec = last.as_ref().expect("Refused implies a record");
            eprintln!(
                "heal: REFUSING — this node is still on a fork at block {block}, but a recovery was \
                 already attempted {hours}h ago ({}), max one per 24 h.",
                rec.recovered_at_iso
            );
            eprintln!(
                "      fork DB set aside at {}; snapshot was {}. Human decision needed: check \
                 whether the signed snapshot or this node's peers are the wrong ones, then delete \
                 {} to re-arm.",
                rec.fork_dir,
                rec.snapshot,
                state_path.display()
            );
            stop_unit_if_requested(a);
            Ok(EXIT_HEAL_REFUSED)
        }
        HealDecision::Recover { fork_dir } => {
            let (expected, local) = match &verdict {
                CheckpointVerdict::WrongChain {
                    expected, local, ..
                } => (expected.clone(), local.clone()),
                _ => unreachable!("Recover implies WrongChain"),
            };
            println!("heal: fork detected at block {block}");
            println!("      signed {expected}");
            println!("      local  {local}");
            println!(
                "      plan: stop unit -> move chains/ to {} -> import signed snapshot -> start unit",
                fork_dir.display()
            );

            if !a.apply {
                println!("heal: dry run — nothing changed. Re-run with --apply to heal.");
                return Ok(EXIT_WRONG_CHAIN);
            }

            let unit = a
                .unit
                .as_deref()
                .ok_or("heal --apply needs --unit <systemd user unit that runs the node>")?;

            // Resolve (and verify) the archive before touching anything.
            let download_dir = a.download_dir.clone().unwrap_or_else(std::env::temp_dir);
            let plan = plan_recovery(
                &m,
                &bytes,
                gpn_agent::update::RELEASE_PUBKEY_HEX,
                base,
                &state_path,
                a.snapshot.clone(),
                &download_dir,
                local,
                now,
                &|line| println!("{line}"),
            )?;
            let plan = gpn_agent::selfheal::RecoveryPlan {
                fork_dir: fork_dir.clone(),
                ..plan
            };

            let control = SystemctlUnit {
                unit: unit.to_string(),
            };
            perform_recovery(
                &plan,
                &control,
                &|snapshot, base| {
                    let import_args = Args {
                        cmd: "import".into(),
                        base_path: Some(base.to_path_buf()),
                        out: None,
                        snapshot: Some(snapshot.to_path_buf()),
                        chain_dir: None,
                        meta: None,
                        rpc: a.rpc.clone(),
                        allow_live: false,
                        freeze: false,
                        // No --force: if the rename above did not clear the DB,
                        // refusing is the correct outcome.
                        force: false,
                        pruning: a.pruning.clone(),
                        stop_unit: None,
                        unit: None,
                        state: None,
                        apply: false,
                        download_dir: None,
                    };
                    import(&import_args)
                },
                &|line| println!("{line}"),
            )?;
            Ok(0)
        }
    }
}

// ------------------------------------------------------------------ main

fn main() {
    let a = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    // `checkpoint` has its own exit-code contract (0 / 12), so it bypasses the
    // generic "any Err() = exit 1" path below.
    if a.cmd == "checkpoint" {
        match checkpoint(&a) {
            Ok(code) => std::process::exit(code),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
    // `heal` owns the 0 / 12 / 13 contract as well.
    if a.cmd == "heal" {
        match heal(&a) {
            Ok(code) => std::process::exit(code),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
    let r = match a.cmd.as_str() {
        "export" => export(&a),
        "import" => import(&a),
        "info" => info(&a),
        "verify" => verify(&a),
        "help" | "-h" | "--help" => {
            println!("{}", usage());
            Ok(())
        }
        other => Err(format!("unknown command '{other}'\n{}", usage())),
    };
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    // ── signed checkpoint (design §3 / §5 case 5) ───────────────────────────

    const H_A: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const H_B: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn a_matching_checkpoint_is_ok_and_exits_zero() {
        let v = checkpoint_verdict(272_654, H_A, Some(280_000), Some(H_A));
        assert_eq!(v, CheckpointVerdict::Match { block: 272_654 });
        assert_eq!(verdict_exit_code(&v), 0);
    }

    #[test]
    fn a_forked_hash_is_fatal_and_exits_twelve() {
        // The whole point of the item: a different block at the signed height is
        // a fork, and it must be fatal rather than a warning.
        let v = checkpoint_verdict(272_654, H_A, Some(280_000), Some(H_B));
        assert_eq!(
            v,
            CheckpointVerdict::WrongChain {
                block: 272_654,
                expected: H_A.into(),
                local: H_B.into(),
            }
        );
        assert_eq!(verdict_exit_code(&v), EXIT_WRONG_CHAIN);
        assert_eq!(EXIT_WRONG_CHAIN, 12);
    }

    #[test]
    fn below_the_checkpoint_is_not_yet_not_wrong_chain() {
        // A fresh node that simply has not reached the height must never be
        // accused of being on a fork.
        let v = checkpoint_verdict(272_654, H_A, Some(1_000), None);
        assert_eq!(
            v,
            CheckpointVerdict::NotYet {
                local_height: Some(1_000),
                block: 272_654
            }
        );
        assert_eq!(verdict_exit_code(&v), 0);
    }

    #[test]
    fn an_unreadable_local_value_is_never_a_verdict() {
        // Three-state rule (#141/#144): no data is not evidence of a fork.
        let v = checkpoint_verdict(272_654, H_A, Some(280_000), None);
        assert!(matches!(v, CheckpointVerdict::NotYet { .. }));
        assert_eq!(verdict_exit_code(&v), 0);
    }

    #[test]
    fn checkpoint_hash_comparison_ignores_case_and_whitespace() {
        let upper = format!("  {}  ", H_A.to_ascii_uppercase());
        let v = checkpoint_verdict(10, &upper, Some(20), Some(H_A));
        assert_eq!(v, CheckpointVerdict::Match { block: 10 });
    }

    #[test]
    fn a_v1_document_has_no_checkpoint_to_compare() {
        // v1 keeps verifying forever; it simply makes no chain claim, so the
        // command must say so instead of inventing a verdict.
        let sk = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let mut m = SnapshotMeta {
            format: SNAPSHOT_CANONICAL_PREFIX_V1.into(),
            url: "http://x/snap.tar.gz".into(),
            sha256: "aa".repeat(32),
            size: 10,
            block: Some(5),
            built: "now".into(),
            signing_key: hex::encode(sk.verifying_key().to_bytes()),
            signature: String::new(),
            genesis: String::new(),
            block_hash: String::new(),
        };
        use ed25519_dalek::Signer;
        m.signature = hex::encode(sk.sign(&m.canonical_text()).to_bytes());
        let json = serde_json::to_vec(&m).unwrap();
        let got = verify_snapshot_meta(&json, &hex::encode(sk.verifying_key().to_bytes()), None)
            .expect("v1 must stay verifiable");
        assert!(got.block_hash.trim().is_empty());
        assert_ne!(got.format.trim(), SNAPSHOT_CANONICAL_PREFIX);
    }

    #[test]
    fn import_pruning_defaults_to_archive_for_old_manifest() {
        // Pre-marker export: manifest has NO pruning field → assume archive.
        assert_eq!(effective_import_pruning(None, None), "archive");
    }

    #[test]
    fn import_pruning_honours_manifest_field() {
        // A manifest that DOES specify a mode is honoured (here: archive).
        assert_eq!(effective_import_pruning(None, Some("archive")), "archive");
        // ...including an explicit "none".
        assert_eq!(effective_import_pruning(None, Some("none")), "none");
    }

    #[test]
    fn import_pruning_explicit_flag_wins() {
        // `--pruning none` overrides a manifest that says archive.
        assert_eq!(
            effective_import_pruning(Some("none"), Some("archive")),
            "none"
        );
        // `--pruning archive` overrides a manifest that says none.
        assert_eq!(
            effective_import_pruning(Some("archive"), Some("none")),
            "archive"
        );
        // ...even when the manifest has no field.
        assert_eq!(effective_import_pruning(Some("none"), None), "none");
    }

    #[test]
    fn import_pruning_normalises_case_and_space() {
        assert_eq!(effective_import_pruning(Some(" Archive "), None), "archive");
        assert_eq!(effective_import_pruning(None, Some("NONE")), "none");
    }

    // ── signed snapshot metadata ────────────────────────────────────────────

    fn signed_meta(sk: &ed25519_dalek::SigningKey, size: u64) -> (Vec<u8>, [u8; 32]) {
        signed_meta_with(
            sk,
            size,
            &format!("0x{}", "11".repeat(32)),
            &format!("0x{}", "22".repeat(32)),
        )
    }

    fn signed_meta_with(
        sk: &ed25519_dalek::SigningKey,
        size: u64,
        genesis: &str,
        block_hash: &str,
    ) -> (Vec<u8>, [u8; 32]) {
        use ed25519_dalek::Signer;
        let pubkey = sk.verifying_key().to_bytes();
        let mut m = SnapshotMeta {
            format: SNAPSHOT_CANONICAL_PREFIX.into(),
            url: "https://example.invalid/snap.tar.gz".into(),
            sha256: "ab".repeat(32),
            size,
            block: Some(206_860),
            // v2 checkpoint: a signed doc must carry both, and both must be
            // 0x + 64 hex, or `verify` refuses it (see the shape check below).
            genesis: genesis.into(),
            block_hash: block_hash.into(),
            built: "2026-10-28T18:50:02Z".into(),
            signing_key: hex::encode(pubkey),
            signature: String::new(),
        };
        let sig = sk.sign(&m.canonical_text());
        m.signature = hex::encode(sig.to_bytes());
        (serde_json::to_vec(&m).unwrap(), pubkey)
    }

    #[test]
    fn a_v2_checkpoint_must_be_a_real_hash_even_when_correctly_signed() {
        // The shape check has to run *after* the signature, so a doc that is
        // genuinely signed but carries a half-filled checkpoint must still be
        // refused: a forged-looking pair is worse than no pair at all.
        let sk = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let pk = hex::encode(sk.verifying_key().to_bytes());
        let good_genesis = format!("0x{}", "11".repeat(32));
        let good_block = format!("0x{}", "22".repeat(32));
        for (genesis, block_hash) in [
            (String::new(), good_block.clone()),
            (good_genesis.clone(), String::new()),
            ("0xdeadbeef".to_string(), good_block.clone()),
            (good_genesis.clone(), "not-hex".to_string()),
        ] {
            let (json, _) = signed_meta_with(&sk, 1234, &genesis, &block_hash);
            let e = verify_snapshot_meta(&json, &pk, None).unwrap_err();
            assert!(
                e.contains("invalid genesis") || e.contains("invalid block_hash"),
                "expected a checkpoint shape refusal, got: {e}"
            );
        }
        // ...and the well-formed pair still verifies.
        let (json, _) = signed_meta(&sk, 1234);
        assert!(verify_snapshot_meta(&json, &pk, None).is_ok());
    }

    #[test]
    fn a_v1_document_still_verifies_forever() {
        // The v1 prefix is part of the signed text, so pre-v2 snapshots keep
        // verifying exactly as they did — the v2 fields must not leak into it.
        use ed25519_dalek::Signer;
        let sk = ed25519_dalek::SigningKey::from_bytes(&[4u8; 32]);
        let pk = hex::encode(sk.verifying_key().to_bytes());
        let mut m = SnapshotMeta {
            format: SNAPSHOT_CANONICAL_PREFIX_V1.into(),
            url: "https://example.invalid/old.tar.gz".into(),
            sha256: "cd".repeat(32),
            size: 4321,
            block: Some(123_456),
            genesis: String::new(),
            block_hash: String::new(),
            built: "2026-10-20T00:00:00Z".into(),
            signing_key: pk.clone(),
            signature: String::new(),
        };
        let sig = sk.sign(&m.canonical_text());
        m.signature = hex::encode(sig.to_bytes());
        let json = serde_json::to_vec(&m).unwrap();
        let got = verify_snapshot_meta(&json, &pk, None).unwrap();
        assert_eq!(got.size, 4321);
        assert_eq!(got.block, Some(123_456));
    }

    #[test]
    fn verify_accepts_a_correctly_signed_meta() {
        let sk = ed25519_dalek::SigningKey::from_bytes(&[3u8; 32]);
        let (json, pubkey) = signed_meta(&sk, 1234);
        let m = verify_snapshot_meta(&json, &hex::encode(pubkey), None).unwrap();
        assert_eq!(m.size, 1234);
        assert_eq!(m.block, Some(206_860));
    }

    #[test]
    fn verify_rejects_a_tampered_size_or_wrong_key() {
        let sk = ed25519_dalek::SigningKey::from_bytes(&[3u8; 32]);
        let (json, pubkey) = signed_meta(&sk, 1234);
        // Tamper with a signed field: the signature no longer covers the doc.
        let tampered = String::from_utf8(json.clone())
            .unwrap()
            .replace("\"size\":1234", "\"size\":9999");
        assert!(verify_snapshot_meta(tampered.as_bytes(), &hex::encode(pubkey), None).is_err());
        // A doc signed by someone else's key is refused outright.
        let other = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        assert!(
            verify_snapshot_meta(&json, &hex::encode(other.verifying_key().to_bytes()), None)
                .is_err()
        );
        // An unsigned doc is refused.
        let unsigned = String::from_utf8(json)
            .unwrap()
            .replace("\"signature\":", "\"signature\":\"\",\"x\":");
        assert!(verify_snapshot_meta(unsigned.as_bytes(), &hex::encode(pubkey), None).is_err());
    }

    #[test]
    fn verify_archive_size_and_hash_must_match() {
        let dir = std::env::temp_dir().join(format!("gpn-snap-verify-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("snap.bin");
        fs::write(&f, b"hello").unwrap();
        let sk = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]);
        let (json, pubkey) = signed_meta(&sk, 5);
        // Wrong digest (declared sha256 is not the file's) -> error.
        assert!(verify_snapshot_meta(&json, &hex::encode(pubkey), Some(&f)).is_err());
        // Correct digest + size -> ok.
        let mut m: SnapshotMeta = serde_json::from_slice(&json).unwrap();
        m.sha256 = sha256_file(&f).unwrap();
        use ed25519_dalek::Signer;
        m.signature = hex::encode(sk.sign(&m.canonical_text()).to_bytes());
        let good = serde_json::to_vec(&m).unwrap();
        assert!(verify_snapshot_meta(&good, &hex::encode(pubkey), Some(&f)).is_ok());
        // Size mismatch -> error.
        let mut m2 = m.clone();
        m2.size = 6;
        m2.signature = hex::encode(sk.sign(&m2.canonical_text()).to_bytes());
        let bad = serde_json::to_vec(&m2).unwrap();
        assert!(verify_snapshot_meta(&bad, &hex::encode(pubkey), Some(&f)).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    // ── self-heal (#157) ──────────────────────────────────────────

    fn wrong_chain() -> CheckpointVerdict {
        CheckpointVerdict::WrongChain {
            block: 272_654,
            expected: H_A.into(),
            local: H_B.into(),
        }
    }

    fn record_at(ts: u64) -> RecoveryRecord {
        RecoveryRecord {
            recovered_at: ts,
            recovered_at_iso: iso8601(ts),
            ..Default::default()
        }
    }

    #[test]
    fn heal_does_nothing_without_a_fork() {
        let fork = Path::new("/tmp/chains-fork-1");
        let now = 1_000_000u64;
        for (v, needle) in [
            (
                CheckpointVerdict::Match { block: 7 },
                "matches the signed checkpoint",
            ),
            (
                CheckpointVerdict::NotYet {
                    local_height: Some(3),
                    block: 7,
                },
                "behind the signed checkpoint",
            ),
        ] {
            match heal_decision(&v, None, now, RECOVERY_WINDOW_SECS, fork) {
                HealDecision::Nothing { reason } => assert!(reason.contains(needle), "{reason}"),
                other => panic!("expected Nothing, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_first_fork_is_recoverable_and_a_recent_one_is_not() {
        let fork = Path::new("/tmp/chains-fork-1");
        let now = 1_000_000u64;
        assert_eq!(
            heal_decision(&wrong_chain(), None, now, RECOVERY_WINDOW_SECS, fork),
            HealDecision::Recover {
                fork_dir: fork.to_path_buf()
            }
        );
        // 23 h later: still inside the window, refuse.
        let recent = record_at(now - 23 * 3600);
        assert_eq!(
            heal_decision(
                &wrong_chain(),
                Some(&recent),
                now,
                RECOVERY_WINDOW_SECS,
                fork
            ),
            HealDecision::Refused {
                age_secs: 23 * 3600
            }
        );
        // The 24 h boundary itself re-arms (>= window is a new attempt).
        let old = record_at(now - RECOVERY_WINDOW_SECS);
        assert!(matches!(
            heal_decision(&wrong_chain(), Some(&old), now, RECOVERY_WINDOW_SECS, fork),
            HealDecision::Recover { .. }
        ));
    }

    #[test]
    fn a_record_from_the_future_still_refuses() {
        // A clock that jumped backwards (or a hand-edited ledger) must not
        // unlock a second recovery on the same fork.
        let fork = Path::new("/tmp/chains-fork-1");
        let future = record_at(2_000_000);
        assert_eq!(
            heal_decision(
                &wrong_chain(),
                Some(&future),
                1_000_000,
                RECOVERY_WINDOW_SECS,
                fork
            ),
            HealDecision::Refused { age_secs: 0 }
        );
    }

    #[test]
    fn the_recovery_ledger_round_trips_and_fails_closed_when_corrupt() {
        let dir = std::env::temp_dir().join(format!("gpn-heal-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(RECOVERY_STATE_NAME);

        // Absent = never healed (the only case that licenses a recovery).
        assert_eq!(read_recovery(&path).unwrap(), None);

        let mut rec = record_at(1_700_000_000);
        rec.checkpoint_block = 272_654;
        rec.fork_dir = "/x/chains-fork-1".into();
        write_recovery(&path, &rec).unwrap();
        assert_eq!(read_recovery(&path).unwrap(), Some(rec.clone()));
        // Atomic write left no temp behind.
        assert!(!path.with_extension("tmp").exists());

        // Corrupt ledger = error (fail closed), never "no record".
        fs::write(&path, b"{not json").unwrap();
        assert!(read_recovery(&path).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fork_dir_is_never_reused() {
        let dir = std::env::temp_dir().join(format!("gpn-fork-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let first = fork_dir_for(&dir, 42);
        assert_eq!(first, dir.join("chains-fork-42"));
        fs::create_dir_all(&first).unwrap();
        // The earlier fork's DB is the only evidence of what we were on.
        assert_eq!(fork_dir_for(&dir, 42), dir.join("chains-fork-42-1"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_heal_refusal_code_is_distinct_from_the_fork_code() {
        assert_eq!(EXIT_HEAL_REFUSED, 13);
        assert_ne!(EXIT_HEAL_REFUSED, EXIT_WRONG_CHAIN);
        assert_eq!(RECOVERY_WINDOW_SECS, 86_400);
    }
}
