//! argv contract (#W47): **every child-process flag the supervisor builds must
//! exist in the child binary that receives it.**
//!
//! Why this file exists: on 2026-11-08 the supervisor was handed an invented
//! `--reserved-peers` flag (the chain node's real name is `--reserved-nodes`).
//! Nothing failed at the point of the mistake — the flag was built, the child
//! was launched, and the damage only showed up as boxes that would not peer.
//! Two properties made that possible:
//!
//!   * the hand-rolled children (`gpn_provider_status`, the provider loops) use
//!     `opt(&args, "--x")`, so an unknown flag is *silently ignored* — there is
//!     no error to read anywhere; and
//!   * nothing in CI ever compared the supervisor's argv against the child.
//!
//! This test closes the second hole. It reads the supervisor's own argv
//! construction out of `src/bin/gpn_node.rs` (and `gpn_fleet`'s node launcher),
//! and checks every flag it finds against what the child actually accepts:
//!
//! - `node` → the substrate chain binary's `--help` (authoritative: the clap
//!   parser itself), falling back to the custom flags in
//!   `gpn-node/node/src/cli.rs` + the substrate run flags this supervisor is
//!   known to use, with a loud SKIP line if no chain binary can be found;
//! - `heartbeat` → the flags `gpn_provider_status` parses (`opt(&args, …)`);
//! - `provider` / `diffuse` / `ml` / `finetune` → they take **positional**
//!   arguments only, so the supervisor must pass them no flags at all.
//!
//! A flag added to the supervisor that no child implements therefore fails
//! `Build + unit tests (linux)` instead of costing another evening on a remote
//! box. The negative control at the bottom pins the exact incident: the real
//! chain flag `--reserved-nodes` is accepted, the invented `--reserved-peers`
//! is not.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

const SUPERVISOR_SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/bin/gpn_node.rs");
const FLEET_SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/bin/gpn_fleet.rs");
const HEARTBEAT_SRC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/bin/gpn_provider_status.rs"
);
const CHAIN_CLI_SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../gpn-node/node/src/cli.rs");

/// Substrate `RunCmd` flags this supervisor passes that are *not* defined in
/// this repo (they come from `sc-cli`). Only used when the chain binary itself
/// is unavailable — when the binary is found, its `--help` is the authority.
const SUBSTRATE_RUN_FLAGS: &[&str] = &[
    "--base-path",
    "--bootnodes",
    "--chain",
    "--node-key",
    "--node-key-file",
    "--reserved-nodes",
    "--rpc-port",
    "--sync",
    "--state-pruning",
    "--blocks-pruning",
    "--public-addr",
    // Present in `gpn-node --help` (substrate RunCmd) — verified 2026-11-11 by
    // running the binary, which logged "Running libp2p network backend" when
    // given it. It was missing from this list only because nothing used it
    // before the supervisor started pinning the backend.
    "--network-backend",
];

// ─────────────────────────────────────────────────────────────────────────────
// Tiny source scanning helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Every `"--flag"` string literal on one line. Deliberately conservative:
/// the literal must be fully quoted and lower-kebab, so `format!("--{a}")`
/// (the dynamic `--<author>` flag) and prose are both skipped.
fn flag_literals(line: &str) -> Vec<String> {
    let cs: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 2 < cs.len() {
        if cs[i] == '"' && cs[i + 1] == '-' && cs[i + 2] == '-' {
            let mut s = String::from("--");
            let mut j = i + 3;
            while j < cs.len() {
                let c = cs[j];
                if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                    s.push(c);
                    j += 1;
                } else {
                    break;
                }
            }
            if j < cs.len() && cs[j] == '"' && s.len() > 2 {
                out.push(s);
            }
        }
        i += 1;
    }
    out
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

fn label_of(line: &str) -> Option<String> {
    let t = line.trim_start();
    let rest = t.strip_prefix("label: \"")?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// flag sets per child label, read out of the supervisor's argv construction.
///
/// The supervisor builds each child's argv immediately above its
/// `specs.push(ChildSpec { label: "<name>" })`, so: collect `--flag` literals,
/// hand them to the next `label:` seen, and reset at each `argv = vec![`.
fn supervisor_child_flags() -> BTreeMap<String, BTreeSet<String>> {
    let src = read(SUPERVISOR_SRC);
    let mut map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut pending: BTreeSet<String> = BTreeSet::new();
    for line in src.lines() {
        if is_comment(line) {
            continue;
        }
        if line.contains("let mut argv = vec![") || line.contains("argv: vec![") {
            pending.clear();
        }
        if let Some(name) = label_of(line) {
            map.insert(name, std::mem::take(&mut pending));
            continue;
        }
        for f in flag_literals(line) {
            pending.insert(f);
        }
    }
    map
}

/// The flags `gpn_provider_status` actually parses (its whole CLI surface).
fn heartbeat_flags() -> BTreeSet<String> {
    let src = read(HEARTBEAT_SRC);
    let mut out = BTreeSet::new();
    for line in src.lines() {
        if is_comment(line) || !line.contains("opt(&args") {
            continue;
        }
        for f in flag_literals(line) {
            out.insert(f);
        }
    }
    out
}

/// Flags `gpn_fleet` passes when it launches the chain node (`start_node`).
fn fleet_node_flags() -> BTreeSet<String> {
    let src = read(FLEET_SRC);
    let mut out = BTreeSet::new();
    let mut inside = false;
    for line in src.lines() {
        if line.starts_with("fn start_node(") {
            inside = true;
            continue;
        }
        if inside && line.starts_with('}') {
            break;
        }
        if inside && !is_comment(line) {
            for f in flag_literals(line) {
                out.insert(f);
            }
        }
    }
    assert!(
        !out.is_empty(),
        "could not find any child flags in gpn_fleet::start_node — did the function move?"
    );
    out
}

/// Custom (non-substrate) flags accepted by the chain node, from its clap derive.
fn chain_custom_flags() -> BTreeSet<String> {
    let src = read(CHAIN_CLI_SRC);
    let mut out = BTreeSet::new();
    let mut pending_field: Option<String> = None;
    for line in src.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("pub ") {
            if let Some(name) = rest.split(':').next() {
                pending_field = Some(name.trim().replace('_', "-"));
            }
        }
        if t.starts_with("#[arg(") {
            if let Some(rest) = t.split("long = \"").nth(1) {
                if let Some(end) = rest.find('"') {
                    out.insert(format!("--{}", &rest[..end]));
                    continue;
                }
            }
            if t.contains("long") {
                if let Some(f) = pending_field.take() {
                    out.insert(format!("--{f}"));
                }
            }
        }
    }
    out
}

/// The executable name of the chain binary on this platform.
///
/// `gpn-node` on Unix, but `gpn-node.exe` on Windows — and getting this wrong is
/// not a cosmetic bug: until 2026-11-11 the probe below asked for the unsuffixed
/// name, so on the Windows runner it found nothing, fell through to the *pinned
/// flag list*, and the whole contract check silently degraded from "ask the
/// binary's own `--help`" to "compare against a hand-written constant". The
/// constant was missing `--network-backend`, the check still passed there while
/// failing on Linux, and a release was blocked by a test that was not measuring
/// what it claimed. Same family as the W47 incident this file exists to prevent.
fn chain_bin_names() -> Vec<String> {
    chain_bin_names_for(std::env::consts::EXE_SUFFIX)
}

/// The suffix is a parameter so both platforms' behaviour is testable *here*.
/// A test written only against `EXE_SUFFIX` proves nothing on Linux about
/// Windows — the `.exe` case has to be exercised explicitly.
fn chain_bin_names_for(suffix: &str) -> Vec<String> {
    let mut names = vec![format!("gpn-node{suffix}")];
    // Accept the other spelling too, so a cross-compiled/renamed checkout still
    // resolves instead of falling back to the weaker check.
    let other = if suffix.is_empty() {
        "gpn-node.exe".to_string()
    } else {
        "gpn-node".to_string()
    };
    if other != names[0] {
        names.push(other);
    }
    names
}

/// Every path worth probing for a built chain binary, in preference order.
fn chain_binary_candidates() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("GPN_CHAIN_BIN") {
        candidates.push(PathBuf::from(p));
    }
    let names = chain_bin_names();
    let mut dirs: Vec<PathBuf> = Vec::new();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest.parent().unwrap_or(manifest);
    for prof in ["release", "debug"] {
        dirs.push(repo.join(format!("target/{prof}")));
        dirs.push(repo.join(format!("gpn-node/target/{prof}")));
    }
    // CI builds the chain crate into a persistent cache dir of its own
    // (`CARGO_TARGET_DIR=/home/.../gpn-agent-target`, chain target next to it).
    if let Ok(td) = std::env::var("CARGO_TARGET_DIR") {
        let td = PathBuf::from(td);
        for prof in ["release", "debug"] {
            dirs.push(td.join(prof));
            if let Some(parent) = td.parent() {
                dirs.push(parent.join(format!("gpn-node-target/{prof}")));
            }
        }
    }
    for dir in dirs {
        for name in &names {
            candidates.push(dir.join(name));
        }
    }
    candidates
}

/// Locate a built chain binary (`gpn-node`, the substrate node).
fn find_chain_binary() -> Option<PathBuf> {
    chain_binary_candidates().into_iter().find(|p| p.is_file())
}

/// The chain binary's accepted flags. `Some(set)` from the binary's own
/// `--help` (authoritative), `None` when no binary is reachable.
fn chain_flags_from_binary(bin: &Path) -> Option<BTreeSet<String>> {
    let out = Command::new(bin).arg("--help").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    let mut flags = BTreeSet::new();
    for tok in text.split(|c: char| c.is_whitespace() || c == ',' || c == '=' || c == '<') {
        if let Some(name) = tok.trim_end_matches([')', ']']).strip_prefix("--") {
            let name = name.split(['<', '=', ' ']).next().unwrap_or("");
            let ok = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if ok {
                flags.insert(format!("--{name}"));
            }
        }
    }
    Some(flags)
}

/// The authoritative-or-fallback set of chain flags, plus how it was obtained.
fn chain_flags() -> (BTreeSet<String>, &'static str) {
    if let Some(bin) = find_chain_binary() {
        if let Some(flags) = chain_flags_from_binary(&bin) {
            return (flags, "chain binary --help");
        }
        eprintln!("[argv-contract] {} did not answer --help", bin.display());
    }
    eprintln!(
        "[argv-contract] SKIP: no chain binary found (probed `gpn-node{}` in the repo and \
         CARGO_TARGET_DIR target dirs) — grading node flags against \
         gpn-node/node/src/cli.rs + the pinned substrate run flags instead of --help. \
         This is the WEAK check: it cannot see a flag the binary really implements, \
         which is exactly how `--network-backend` blocked 0.3.43 on Windows (W130).",
        std::env::consts::EXE_SUFFIX
    );
    let mut flags = chain_custom_flags();
    flags.extend(SUBSTRATE_RUN_FLAGS.iter().map(|s| s.to_string()));
    (flags, "cli.rs + substrate list")
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn supervisor_argv_is_readable_and_complete() {
    let map = supervisor_child_flags();
    for child in ["node", "heartbeat", "provider", "diffuse", "ml", "finetune"] {
        assert!(
            map.contains_key(child),
            "supervisor child `{child}` not found in {SUPERVISOR_SRC} — \
             the argv scan broke (labels: {:?})",
            map.keys().collect::<Vec<_>>()
        );
    }
    assert!(
        map["node"].contains("--chain"),
        "expected `--chain` in the node child argv, got {:?}",
        map["node"]
    );
    assert!(
        !map["heartbeat"].is_empty(),
        "expected flags on the heartbeat child (it takes --listen/--gflops/…), got none"
    );
}

#[test]
fn chain_node_flags_exist_in_the_chain_binary() {
    let map = supervisor_child_flags();
    let (accepted, source) = chain_flags();
    let mut unknown: Vec<String> = Vec::new();
    for flag in &map["node"] {
        if !accepted.contains(flag) {
            unknown.push(flag.clone());
        }
    }
    assert!(
        unknown.is_empty(),
        "the supervisor passes flags the chain node does not implement: {unknown:?}\n\
         (checked against {source})\n\
         This is the #W47 defect class: an invented flag is accepted by the supervisor \
         and silently ignored (or fatal) in the child. Use the real name or add it to \
         gpn-node/node/src/cli.rs.",
    );

    // Same contract for the fleet supervisor, which launches the same binary.
    let fleet = fleet_node_flags();
    let bad: Vec<&String> = fleet.iter().filter(|f| !accepted.contains(*f)).collect();
    assert!(
        bad.is_empty(),
        "gpn_fleet passes flags the chain node does not implement: {bad:?} \
         (checked against {source})"
    );
}

#[test]
fn heartbeat_flags_exist_in_gpn_provider_status() {
    let map = supervisor_child_flags();
    let accepted = heartbeat_flags();
    assert!(
        accepted.contains("--listen") && accepted.contains("--capabilities"),
        "heartbeat flag scan looks wrong: {accepted:?}"
    );
    let unknown: Vec<&String> = map["heartbeat"]
        .iter()
        .filter(|f| !accepted.contains(*f))
        .collect();
    assert!(
        unknown.is_empty(),
        "the supervisor passes flags gpn_provider_status does not parse: {unknown:?}\n\
         It ignores unknown flags silently, so this would be invisible at runtime. \
         Parsed surface: {accepted:?}"
    );
}

#[test]
fn positional_provider_children_get_no_flags() {
    let map = supervisor_child_flags();
    for child in ["provider", "diffuse", "ml", "finetune"] {
        assert!(
            map[child].is_empty(),
            "`{child}` takes positional arguments only (endpoint, signer, poll-ms), \
             but the supervisor passes flags: {:?}. A flag there is swallowed as an \
             extra positional — the #W47 defect class.",
            map[child]
        );
    }
}

/// W130, pinned: the probe must ask for the platform's executable name.
///
/// `gpn-node.exe` is what CI's Windows runner actually writes; probing only the
/// unsuffixed name made the authoritative `--help` path unreachable there, so the
/// check silently graded the platform that was failing against a hand-written
/// list instead of the binary.
#[test]
fn chain_binary_probe_uses_the_platform_executable_name() {
    let names = chain_bin_names();
    let expected = format!("gpn-node{}", std::env::consts::EXE_SUFFIX);
    assert_eq!(
        names.first().map(String::as_str),
        Some(expected.as_str()),
        "the first thing probed must be `{expected}` (EXE_SUFFIX = {:?}), got {names:?}",
        std::env::consts::EXE_SUFFIX
    );
    assert!(
        names.len() >= 2 && names.iter().any(|n| n.ends_with(".exe")),
        "an `.exe` spelling must be among the candidates on every platform, got {names:?}"
    );
    // And it must be joined onto real target dirs, not just exist as a string.
    let cands = chain_binary_candidates();
    assert!(
        cands
            .iter()
            .any(|p| p.file_name() == Some(std::ffi::OsStr::new(expected.as_str()))),
        "no candidate path ends in `{expected}`: {cands:?}"
    );
}

/// Both platforms, exercised on whichever host runs the test.
#[test]
fn windows_probes_the_dot_exe_name_first() {
    let win = chain_bin_names_for(".exe");
    assert_eq!(
        win[0], "gpn-node.exe",
        "Windows must probe `gpn-node.exe` first"
    );
    assert!(
        win.contains(&"gpn-node".to_string()),
        "…and tolerate an unsuffixed binary: {win:?}"
    );
    let unix = chain_bin_names_for("");
    assert_eq!(unix[0], "gpn-node", "Unix must probe `gpn-node` first");
    assert!(
        unix.contains(&"gpn-node.exe".to_string()),
        "…and tolerate the `.exe` spelling: {unix:?}"
    );
    // The regression itself: the old, broken probe asked for the unsuffixed name
    // on Windows, so `gpn-node.exe` was never a candidate and the check degraded.
    assert_ne!(
        win[0], "gpn-node",
        "the W130 regression is back: Windows probes the unsuffixed name"
    );
}

/// The incident, pinned: the real chain flag is accepted, the invented one is not.
#[test]
fn negative_control_invented_flag_is_rejected() {
    let (accepted, source) = chain_flags();
    assert!(
        accepted.contains("--reserved-nodes"),
        "--reserved-nodes must be a known chain-node flag (checked against {source})"
    );
    assert!(
        !accepted.contains("--reserved-peers"),
        "the invented `--reserved-peers` must NOT be accepted — otherwise this gate \
         cannot fail and is worthless"
    );
}
