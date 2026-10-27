//! gpn_gov_upgrade — **governance-driven runtime upgrade pipeline** (mainnet path).
//!
//! `gpn_upgrade_runtime` applies a forkless upgrade with
//! `Sudo.sudo(System.set_code(WASM))`. That is only correct on dev/testnet: the
//! mainnet `Sudo` key is an inert all-zeros placeholder (D2), so the *only*
//! origin that can reach `System.set_code` on mainnet is **Root**, and the only
//! way to get Root is `pallet-gpn-governance`:
//!
//! ```text
//! committee member A:  GpnGovernance.propose(System.set_code { code })   -> 1 approval
//! committee member B:  GpnGovernance.approve(<proposal hash>)            -> 2 approvals
//!   ... wait TimelockPeriod (17_280 blocks = 3 days @15 s) ...
//! anyone:              GpnGovernance.execute(<proposal hash>)            -> Root dispatch
//! ```
//!
//! This runtime has **no** `pallet-democracy`, `pallet-collective`,
//! `pallet-preimage` or `pallet-scheduler` (see `gpn-agent/src/gov.rs`), so
//! there are no referendum tracks, no technical-committee fast-track and no
//! hash-addressed preimages. The full WASM travels inside the `propose`
//! extrinsic and lives in `GpnGovernance::Proposals` until execution.
//!
//! Commands
//! --------
//! ```text
//!   surface                         governance surface: members, threshold, timelock, tracks
//!   stage    <wasm>                 validate an artifact, compute hashes, write a plan file
//!   propose  (--wasm|--plan)        propose System.set_code as a committee member
//!   approve  <hash>                 add this signer's approval
//!   status   [hash]                 pending proposals: approvals, timelock, readiness
//!   execute  <hash>                 dispatch as Root once threshold + timelock are met
//!   verify   (--plan|--expect-spec) poll until enacted; check specVersion + :code + health
//!   rollback (--to <wasm|plan>)     prepare/print (or submit) the rollback proposal
//! ```
//!
//! Global options: `--endpoint <ws-url>`, `--signer <uri>`, `--plan <file>`,
//! `--block-time <secs>`.
//!
//! Safety invariants this tool enforces:
//! * the artifact must carry a `runtime_version` section with
//!   `spec_name == <chain specName>` and `spec_version > <chain specVersion>`
//!   (exactly what `System.set_code` checks) — otherwise nothing is proposed;
//! * the proposal hash printed is `blake2_256(SCALE(RuntimeCall))`, the same id
//!   the pallet uses, so every member approves the *same* bytes;
//! * `verify` checks the runtime version **and** the `:code` hash, not just one;
//! * a write command never defaults the signer — you must name it explicitly;
//! * the node must expose `author_*` RPC (`--rpc-methods=unsafe`); `safe` mode
//!   silently makes every submission impossible.

use gpn_agent::chain::signer::{self, GpnConfig, Sr25519Signer};
use gpn_agent::gov::{self, Artifact, ArtifactProblem, GovSurface, ProposalEntry, VerifyState};
use serde::{Deserialize, Serialize};
use subxt::dynamic::Value;
use subxt::tx::Signer as _;
use subxt::OnlineClient;

const PALLET: &str = "GpnGovernance";
const DEFAULT_ENDPOINT: &str = "ws://127.0.0.1:9944";
const DEFAULT_BLOCK_TIME: u64 = 15;

// ###########################################################################
// CLI
// ###########################################################################

#[derive(Clone)]
struct Opts {
    command: String,
    args: Vec<String>,
    endpoint: String,
    signer_uri: Option<String>,
    plan: Option<String>,
    wasm: Option<String>,
    to: Option<String>,
    out: Option<String>,
    expect_spec: Option<u32>,
    expect_code_hash: Option<String>,
    track: String,
    call: String,
    block_time: u64,
    timeout_secs: u64,
    interval_secs: u64,
    ap_url: Option<String>,
    mode: String,
    dry_run: bool,
    force: bool,
    submit: bool,
    i_understand_risk: bool,
    json: bool,
}

fn print_help() {
    eprintln!(
        "gpn_gov_upgrade — governance-driven runtime upgrade (mainnet path)\n\
         \n\
         usage: gpn_gov_upgrade <command> [args] [options]\n\
         \n\
         commands:\n\
           surface                        governance surface (members, threshold, timelock, tracks)\n\
           stage <wasm>                   validate artifact, hash it, write a plan file\n\
           propose [--wasm <p>|--plan <p>]  propose System.set_code (committee member)\n\
           approve <proposal-hash>        approve a pending proposal (committee member)
           cancel <proposal-hash>         withdraw a pending proposal (needs the threshold)\n\
           status [proposal-hash]         show pending proposals and readiness\n\
           execute <proposal-hash>        execute as Root (anyone, after threshold+timelock)\n\
           verify [--plan <p>]            poll until enacted, then verify spec+code+health\n\
           rollback --to <wasm|plan>      prepare (or --submit) the rollback proposal\n\
         \n\
         options:\n\
           --endpoint <ws-url>      node RPC (default {DEFAULT_ENDPOINT})\n\
           --signer <uri>           sr25519 URI for write commands (e.g. //Alice, or a seed)\n\
           --plan <file>            plan file written by `stage`\n\
           --wasm <file>            artifact to propose (alternative to --plan)\n\
           --out <file>             where `stage` writes the plan\n\
           --expect-spec <n>        expected post-upgrade spec_version (verify)\n\
           --expect-code-hash <h>   expected :code blake2_256 (verify)\n\
           --track <name>           governance track (only `committee` exists)\n\
           --call <name>            set_code (default) | set_code_without_checks (emergency)\n\
           --mode <inline|preimage> proposal encoding (preimage needs the staged design)\n\
           --block-time <secs>      seconds per block for ETA display (default {DEFAULT_BLOCK_TIME})\n\
           --timeout <secs>         verify timeout (default 900)\n\
           --interval <secs>        verify poll interval (default 6)\n\
           --ap-url <url>           also check an access-point health endpoint (verify)\n\
           --dry-run                build/print the proposal without submitting\n\
           --force                  propose even if the checks fail (for dev rehearsal)\n\
           --submit                 actually submit (rollback)\n\
           --i-understand-risk      required for --call set_code_without_checks\n\
           --json                   machine-readable output where supported\n"
    );
}

fn parse_opts() -> Opts {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut o = Opts {
        command: String::new(),
        args: Vec::new(),
        endpoint: DEFAULT_ENDPOINT.to_string(),
        signer_uri: None,
        plan: None,
        wasm: None,
        to: None,
        out: None,
        expect_spec: None,
        expect_code_hash: None,
        track: gov::TRACK_COMMITTEE.to_string(),
        call: "set_code".to_string(),
        block_time: DEFAULT_BLOCK_TIME,
        timeout_secs: 900,
        interval_secs: 6,
        ap_url: None,
        mode: "inline".to_string(),
        dry_run: false,
        force: false,
        submit: false,
        i_understand_risk: false,
        json: false,
    };
    let mut positional = Vec::new();
    let mut it = raw.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--endpoint" => o.endpoint = it.next().unwrap_or(o.endpoint),
            "--signer" => o.signer_uri = it.next(),
            "--plan" => o.plan = it.next(),
            "--wasm" => o.wasm = it.next(),
            "--to" => o.to = it.next(),
            "--out" => o.out = it.next(),
            "--expect-spec" => {
                o.expect_spec = it.next().and_then(|v| v.parse().ok());
            }
            "--expect-code-hash" => o.expect_code_hash = it.next(),
            "--track" => o.track = it.next().unwrap_or(o.track),
            "--call" => o.call = it.next().unwrap_or(o.call),
            "--mode" => o.mode = it.next().unwrap_or(o.mode),
            "--block-time" => {
                o.block_time = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(DEFAULT_BLOCK_TIME)
            }
            "--timeout" => o.timeout_secs = it.next().and_then(|v| v.parse().ok()).unwrap_or(900),
            "--interval" => o.interval_secs = it.next().and_then(|v| v.parse().ok()).unwrap_or(6),
            "--ap-url" => o.ap_url = it.next(),
            "--dry-run" => o.dry_run = true,
            "--force" => o.force = true,
            "--submit" => o.submit = true,
            "--i-understand-risk" => o.i_understand_risk = true,
            "--json" => o.json = true,
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => positional.push(other.to_string()),
        }
    }
    o.command = positional.first().cloned().unwrap_or_default();
    o.args = positional.into_iter().skip(1).collect();
    o
}

// ###########################################################################
// Raw JSON-RPC (reads only — writes go through subxt so signing stays shared)
// ###########################################################################

struct Rpc {
    url: String,
    http: reqwest::Client,
}

impl Rpc {
    fn new(endpoint: &str) -> Self {
        Self {
            url: endpoint
                .replace("ws://", "http://")
                .replace("wss://", "https://"),
            http: reqwest::Client::new(),
        }
    }

    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
        let resp = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("{method}: cannot reach {}: {e}", self.url))?;
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("{method}: bad JSON: {e}"))?;
        if let Some(err) = v.get("error") {
            return Err(format!("{method}: RPC error {err}"));
        }
        Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null))
    }

    async fn storage(&self, key_hex: &str) -> Result<Option<Vec<u8>>, String> {
        let v = self
            .call("state_getStorage", serde_json::json!([key_hex]))
            .await?;
        match v.as_str() {
            None | Some("null") => Ok(None),
            Some(s) => {
                let s = s.trim_start_matches("0x");
                hex::decode(s)
                    .map(Some)
                    .map_err(|e| format!("state_getStorage: bad hex: {e}"))
            }
        }
    }

    async fn keys_paged(&self, prefix_hex: &str, count: u32) -> Result<Vec<String>, String> {
        let v = self
            .call(
                "state_getKeysPaged",
                serde_json::json!([
                    prefix_hex,
                    count,
                    serde_json::Value::Null,
                    serde_json::Value::Null
                ]),
            )
            .await?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn runtime_version(&self) -> Result<serde_json::Value, String> {
        self.call("state_getRuntimeVersion", serde_json::json!([]))
            .await
    }

    async fn head_number(&self) -> Result<u64, String> {
        let h = self.call("chain_getHeader", serde_json::json!([])).await?;
        h.get("number")
            .and_then(|n| n.as_str())
            .map(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16))
            .transpose()
            .map_err(|e| format!("chain_getHeader: bad number: {e}"))
            .map(|v| v.unwrap_or(0))
    }

    async fn chain_name(&self) -> String {
        self.call("system_chain", serde_json::json!([]))
            .await
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_default()
    }

    async fn health(&self) -> Option<serde_json::Value> {
        self.call("system_health", serde_json::json!([])).await.ok()
    }
}

#[derive(Clone, Debug)]
struct ChainState {
    spec_name: String,
    spec_version: u32,
    head: u64,
    name: String,
}

async fn chain_state(rpc: &Rpc) -> Result<ChainState, String> {
    let rv = rpc.runtime_version().await?;
    let spec_name = rv
        .get("specName")
        .and_then(|v| v.as_str())
        .ok_or("state_getRuntimeVersion: no specName")?
        .to_string();
    let spec_version = rv
        .get("specVersion")
        .and_then(|v| v.as_u64())
        .ok_or("state_getRuntimeVersion: no specVersion")? as u32;
    Ok(ChainState {
        spec_name,
        spec_version,
        head: rpc.head_number().await.unwrap_or(0),
        name: rpc.chain_name().await,
    })
}

fn ss58(account: &[u8]) -> String {
    if account.len() != 32 {
        return hex::encode(account);
    }
    let mut raw = [0u8; 32];
    raw.copy_from_slice(account);
    subxt::utils::AccountId32(raw).to_string()
}

async fn read_surface(rpc: &Rpc, client: &OnlineClient<GpnConfig>) -> Result<GovSurface, String> {
    let member_count = rpc
        .storage(&gov::storage_key_value(PALLET, "MemberCount"))
        .await?
        .and_then(|b| {
            if b.len() >= 4 {
                Some(u32::from_le_bytes(b[..4].try_into().unwrap()))
            } else {
                None
            }
        })
        .unwrap_or(0);
    let members = rpc
        .keys_paged(&gov::storage_prefix(PALLET, "Members"), 200)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|k| {
            let raw = hex::decode(k.trim_start_matches("0x")).ok()?;
            // 32-byte prefix + 16-byte blake2_128 + 32-byte account
            if raw.len() < 80 {
                return None;
            }
            Some(ss58(&raw[raw.len() - 32..]))
        })
        .collect::<Vec<_>>();
    Ok(GovSurface {
        member_count,
        approvals_required: const_u128(client, PALLET, "ApprovalsRequired").unwrap_or(0) as u32,
        timelock_blocks: const_u128(client, PALLET, "TimelockPeriod").unwrap_or(0) as u64,
        max_members: const_u128(client, PALLET, "MaxMembers").unwrap_or(0) as u32,
        members,
    })
}

fn const_u128(client: &OnlineClient<GpnConfig>, pallet: &str, name: &str) -> Option<u128> {
    let addr = subxt::dynamic::constant(pallet, name);
    let thunk = client.constants().at(&addr).ok()?;
    thunk.to_value().ok()?.as_u128()
}

/// Does the runtime metadata expose a pallet? Used to give a precise error for
/// `--mode preimage` and for anything else the chain might gain later.
fn has_pallet(client: &OnlineClient<GpnConfig>, name: &str) -> bool {
    client.metadata().pallet_by_name(name).is_some()
}

// ###########################################################################
// Plan file
// ###########################################################################

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Plan {
    schema: u32,
    artifact_path: String,
    encoded_len: usize,
    decoded_len: usize,
    compressed: bool,
    sha256: String,
    code_hash: String,
    spec_name: String,
    spec_version: u32,
    impl_version: u32,
    transaction_version: u32,
    system_version: u8,
    chain_name: String,
    chain_spec_version_at_stage: u32,
    track: String,
    call: String,
    mode: String,
    created_at_unix: u64,
}

impl Plan {
    fn from_artifact(art: &Artifact, cs: &ChainState, track: &str, call: &str, mode: &str) -> Plan {
        let v = art.version.clone().expect("checked before building plan");
        Plan {
            schema: 1,
            artifact_path: art.path.clone(),
            encoded_len: art.encoded_len,
            decoded_len: art.decoded_len,
            compressed: art.compressed,
            sha256: gov::hex0x(&art.sha256),
            code_hash: gov::hex0x(&art.code_hash),
            spec_name: v.spec_name,
            spec_version: v.spec_version,
            impl_version: v.impl_version,
            transaction_version: v.transaction_version,
            system_version: v.system_version,
            chain_name: cs.name.clone(),
            chain_spec_version_at_stage: cs.spec_version,
            track: track.to_string(),
            call: call.to_string(),
            mode: mode.to_string(),
            created_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }

    fn print(&self) {
        println!("== upgrade plan ==");
        println!(
            "  artifact         {} ({} bytes encoded)",
            self.artifact_path, self.encoded_len
        );
        println!("  sha256           {}", self.sha256);
        println!("  code hash        {}", self.code_hash);
        println!(
            "  runtime version  {} spec_version={} impl_version={} tx_version={}",
            self.spec_name, self.spec_version, self.impl_version, self.transaction_version
        );
        println!(
            "  chain            {} (spec_version {} when staged)",
            self.chain_name, self.chain_spec_version_at_stage
        );
        println!("  track / call     {} / {}", self.track, self.call);
    }
}

fn write_plan(plan: &Plan, out: Option<&str>) -> Result<String, String> {
    let path = match out {
        Some(p) => p.to_string(),
        None => {
            let dir = std::path::Path::new(".state/gov-upgrade");
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            dir.join(format!(
                "spec{}_staged{}.plan.json",
                plan.spec_version,
                &plan.code_hash[2..10]
            ))
            .to_string_lossy()
            .to_string()
        }
    };
    if let Some(parent) = std::path::Path::new(&path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
    }
    let json = serde_json::to_string_pretty(plan).map_err(|e| e.to_string())?;
    std::fs::write(&path, json + "\n").map_err(|e| format!("{path}: {e}"))?;
    Ok(path)
}

fn load_plan(path: &str) -> Result<Plan, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    serde_json::from_str(&s).map_err(|e| format!("{path}: not a plan file: {e}"))
}

// ###########################################################################
// Commands
// ###########################################################################

async fn cmd_surface(
    opts: &Opts,
    rpc: &Rpc,
    client: &OnlineClient<GpnConfig>,
) -> Result<(), String> {
    let cs = chain_state(rpc).await?;
    let surface = read_surface(rpc, client).await?;
    let tracks = [gov::TRACK_COMMITTEE];

    if opts.json {
        let out = serde_json::json!({
            "chain": cs.name,
            "spec_name": cs.spec_name,
            "spec_version": cs.spec_version,
            "head": cs.head,
            "member_count": surface.member_count,
            "approvals_required": surface.approvals_required,
            "timelock_blocks": surface.timelock_blocks,
            "max_members": surface.max_members,
            "members": surface.members,
            "tracks": tracks,
            "unavailable_tracks": gov::UNAVAILABLE_TRACKS.iter().map(|(n,w)| serde_json::json!({"name":n,"why":w})).collect::<Vec<_>>(),
            "root_origin_reachable_via": "GpnGovernance.execute (Root dispatch)",
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        return Ok(());
    }

    println!("== governance surface ==");
    println!(
        "  chain             {} (spec {} v{})",
        cs.name, cs.spec_name, cs.spec_version
    );
    println!("  head              block {}", cs.head);
    println!(
        "  committee         {} member(s), {} approval(s) required, max {}",
        surface.member_count, surface.approvals_required, surface.max_members
    );
    println!(
        "  timelock          {} blocks (~{} at {} s/block)",
        surface.timelock_blocks,
        gov::format_duration(surface.timelock_blocks, opts.block_time),
        opts.block_time
    );
    for (i, m) in surface.members.iter().enumerate() {
        println!("    [{i}] {m}");
    }
    if surface.members.is_empty() {
        println!("    (could not enumerate `Members` — check state_getKeysPaged support)");
    }
    println!();
    println!("  tracks available on this runtime:");
    println!(
        "    committee  -> propose System.set_code, {}/{} approvals, {} block timelock, execute as Root",
        surface.approvals_required, surface.member_count, surface.timelock_blocks
    );
    println!("  tracks NOT available (pallets absent from the runtime):");
    for (name, why) in gov::UNAVAILABLE_TRACKS {
        println!("    {name:<10} {why}");
    }
    println!();
    println!("  Root-reachable extrinsics are whatever the committee proposes; there is");
    println!("  no veto/cancel extrinsic — an approved proposal is either executed or left");
    println!("  pending (execution removes it before dispatch, even if the call fails).");
    println!("  Sudo on mainnet is the inert all-zeros key: it cannot be used.");
    Ok(())
}

async fn cmd_stage(
    opts: &Opts,
    rpc: &Rpc,
    client: &OnlineClient<GpnConfig>,
) -> Result<i32, String> {
    let wasm_path = opts
        .args
        .first()
        .cloned()
        .or_else(|| opts.wasm.clone())
        .ok_or("usage: stage <runtime.wasm>")?;
    let bytes = std::fs::read(&wasm_path).map_err(|e| format!("{wasm_path}: {e}"))?;
    let art = gov::inspect_artifact(&wasm_path, &bytes)?;
    println!("{}", art.summary());

    let cs = chain_state(rpc).await?;
    let surface = read_surface(rpc, client).await?;
    let track = gov::resolve_track(&opts.track, &surface)?;
    println!(
        "chain: {} ({} v{}, head {})   track: {}",
        cs.name, cs.spec_name, cs.spec_version, cs.head, track
    );

    let require_increase = opts.call == "set_code";
    let check =
        gov::check_artifact_for_chain(&art, &cs.spec_name, cs.spec_version, require_increase);
    if let Err(problem) = &check {
        eprintln!("❌ artifact rejected: {problem}");
        if !opts.force {
            eprintln!("   (--force skips this gate — only for rehearsals on a throwaway chain)");
            return Ok(3);
        }
        eprintln!("   --force given: continuing even though System.set_code would revert");
    }
    if let Some(expect) = opts.expect_spec {
        if art.version.as_ref().map(|v| v.spec_version) != Some(expect) {
            eprintln!(
                "❌ --expect-spec {} but the artifact says {:?}",
                expect,
                art.version.as_ref().map(|v| v.spec_version)
            );
            return Ok(3);
        }
    }

    let plan = Plan::from_artifact(&art, &cs, &opts.track, &opts.call, &opts.mode);
    plan.print();
    let path = write_plan(&plan, opts.out.as_deref())?;
    println!("\nplan written: {path}");
    if check.is_ok() {
        println!("next: gpn_gov_upgrade propose --plan {path} --signer <committee-uri>");
    }
    Ok(if check.is_ok() || opts.force { 0 } else { 3 })
}

/// Build the `System.set_code` (or `..._without_checks`) dynamic call value.
fn set_code_value(opts: &Opts, wasm: &[u8]) -> Result<Value, String> {
    let call = match opts.call.as_str() {
        "set_code" => "set_code",
        "set_code_without_checks" => "set_code_without_checks",
        other => return Err(format!("unknown --call '{other}'")),
    };
    Ok(Value::unnamed_variant(
        "System",
        vec![Value::unnamed_variant(call, vec![Value::from_bytes(wasm)])],
    ))
}

async fn cmd_propose(
    opts: &Opts,
    rpc: &Rpc,
    client: &OnlineClient<GpnConfig>,
) -> Result<i32, String> {
    // `preimage` mode submits `GpnGovernance.propose_preimage(hash, bytes)`:
    // the call bytes are stored under their blake2_256 hash and the proposal
    // carries only `{ hash, len }`. Requires the spec-42 (hardened) runtime.
    if opts.mode == "preimage" {
        let has_call = client
            .metadata()
            .pallet_by_name(PALLET)
            .map(|p| p.call_hash("propose_preimage").is_some())
            .unwrap_or(false);
        if !has_call {
            eprintln!("❌ --mode preimage is not available on this runtime:");
            eprintln!("   {PALLET}.propose_preimage is not in the runtime metadata.");
            eprintln!("   The chain is running the pre-hardening governance pallet.");
            eprintln!("   Use --mode inline (the proposal then carries the raw WASM), or");
            eprintln!("   apply the staged gov-hardening upgrade first");
            eprintln!("   (shared/artifacts/project7/staged/gov-hardening-v1.md).");
            return Ok(4);
        }
    } else if opts.mode != "inline" {
        return Err(format!("unknown --mode '{}'", opts.mode));
    }

    // Resolve the artifact.
    let (artifact_path, expect_code_hash) = match (&opts.plan, &opts.wasm) {
        (Some(p), _) => {
            let plan = load_plan(p)?;
            if plan.call != opts.call {
                eprintln!("❌ plan proposes {} but --call is {}", plan.call, opts.call);
                return Ok(3);
            }
            (plan.artifact_path, Some(plan.code_hash))
        }
        (None, Some(w)) => (w.clone(), None),
        (None, None) => return Err("usage: propose --plan <file> | --wasm <file>".into()),
    };
    let bytes = std::fs::read(&artifact_path).map_err(|e| format!("{artifact_path}: {e}"))?;
    let art = gov::inspect_artifact(&artifact_path, &bytes)?;
    if let Some(want) = expect_code_hash {
        if gov::hex0x(&art.code_hash) != want {
            eprintln!(
                "❌ artifact hashes to {} but the plan says {} — the file changed since staging",
                gov::hex0x(&art.code_hash),
                want
            );
            return Ok(3);
        }
    }
    println!("artifact: {}", art.summary());

    let cs = chain_state(rpc).await?;
    let surface = read_surface(rpc, client).await?;
    let track = gov::resolve_track(&opts.track, &surface)?;
    let require_increase = opts.call == "set_code";
    if let Err(problem) =
        gov::check_artifact_for_chain(&art, &cs.spec_name, cs.spec_version, require_increase)
    {
        eprintln!("❌ artifact rejected: {problem}");
        if !opts.force {
            return Ok(3);
        }
        eprintln!("   --force given: the on-chain dispatch will revert if the chain re-checks");
    }

    // The inner call, and its SCALE bytes → the proposal id.
    let inner = subxt::dynamic::tx(
        "System",
        opts.call.as_str(),
        vec![Value::from_bytes(&bytes)],
    );
    let call_bytes = client
        .tx()
        .call_data(&inner)
        .map_err(|e| format!("cannot encode {} call: {e}", opts.call))?;
    // Inline: the proposal id *is* blake2_256(call). Preimage: the bytes are
    // stored under blake2_256(call) and the proposal id is the domain-tagged
    // hash of (code_hash, len) — identical to
    // `GpnGovernance::preimage_proposal_id` on chain.
    let code_hash = gov::proposal_hash(&call_bytes);
    let preimage = opts.mode == "preimage";
    let proposal_id = if preimage {
        gov::preimage_proposal_id(code_hash, call_bytes.len() as u32)
    } else {
        code_hash
    };
    let hash_hex = gov::hex0x(&proposal_id);
    if preimage {
        println!(
            "preimage hash: {}  (blake2_256 of the {} call, {} bytes)",
            gov::hex0x(&code_hash),
            opts.call,
            call_bytes.len()
        );
        println!("proposal hash: {hash_hex}  (tagged hash of preimage + length)");
    } else {
        println!(
            "proposal hash: {hash_hex}  (blake2_256 of the {} call, {} bytes)",
            opts.call,
            call_bytes.len()
        );
    }
    println!("track: {track}");

    let existing = rpc
        .storage(&gov::storage_key_map_blake2_256(
            PALLET,
            "Proposals",
            &proposal_id,
        ))
        .await?;
    if existing.is_some() {
        println!("\nℹ️  this proposal already exists on chain (identical bytes) — nothing to do.");
        println!("   check it with: gpn_gov_upgrade status {hash_hex}");
        return Ok(0);
    }
    if opts.dry_run {
        println!("\n--dry-run: not submitting.");
        if preimage {
            println!(
                "would submit: {PALLET}.propose_preimage({{ hash: {}, len: {} }}, <{} bytes>)",
                gov::hex0x(&code_hash),
                call_bytes.len(),
                call_bytes.len()
            );
        } else {
            println!(
                "would submit: {PALLET}.propose(System.{} {{ code: <{} bytes> }})",
                opts.call,
                bytes.len()
            );
        }
        return Ok(0);
    }

    let signer_uri = require_signer(opts)?;
    let sg = Sr25519Signer::from_uri(&signer_uri).map_err(|e| e.to_string())?;
    let who = sg.account_id().to_string();
    if !surface.members.is_empty() && !surface.members.contains(&who) {
        eprintln!(
            "❌ {who} is not a committee member. Members: {:?}",
            surface.members
        );
        return Ok(3);
    }
    println!(
        "signing as {who} (this proposal will carry 1/{0} approvals)",
        surface.approvals_required
    );

    let outer = if preimage {
        subxt::dynamic::tx(
            PALLET,
            "propose_preimage",
            vec![Value::from_bytes(code_hash), Value::from_bytes(&call_bytes)],
        )
    } else {
        subxt::dynamic::tx(PALLET, "propose", vec![set_code_value(opts, &bytes)?])
    };
    let nonce = signer::current_nonce(client, &opts.endpoint, &sg.account_id()).await;
    println!(
        "submitting {PALLET}.{} [nonce {nonce}]…",
        if preimage {
            "propose_preimage"
        } else {
            "propose"
        }
    );
    signer::submit_signed(client, &outer, &sg, nonce)
        .await
        .map_err(|e| e.to_string())?;

    println!("\n✅ proposal created: {hash_hex}");
    println!(
        "   {}/{} approvals so far (you are 1)",
        1, surface.approvals_required
    );
    println!(
        "   execute no earlier than block {} (~{} after creation)",
        cs.head + surface.timelock_blocks,
        gov::format_duration(surface.timelock_blocks, opts.block_time)
    );
    println!("   next: gpn_gov_upgrade approve {hash_hex} --signer <other-member-uri>");
    Ok(0)
}

fn require_signer(opts: &Opts) -> Result<String, String> {
    opts.signer_uri.clone().ok_or_else(|| {
        "this command signs a transaction: pass --signer <uri> explicitly \
         (there is no default — mainnet has no hot dev key)"
            .to_string()
    })
}

async fn cmd_approve(
    opts: &Opts,
    rpc: &Rpc,
    client: &OnlineClient<GpnConfig>,
) -> Result<i32, String> {
    let hash_hex = opts
        .args
        .first()
        .cloned()
        .ok_or("usage: approve <proposal-hash>")?;
    let hash = gov::parse_hash(&hash_hex)?;
    let surface = read_surface(rpc, client).await?;
    let entry = rpc
        .storage(&gov::storage_key_map_blake2_256(PALLET, "Proposals", &hash))
        .await?
        .ok_or_else(|| {
            format!("no proposal {hash_hex} on chain (wrong hash, or already executed)")
        })?;
    let parsed = gov::parse_proposal_entry(&entry);
    match &parsed {
        Ok(p) => println!(
            "proposal {hash_hex}: {} approvals, created at block {}",
            p.approvals, p.created_at
        ),
        Err(e) => println!("proposal {hash_hex}: {e}"),
    }

    let signer_uri = require_signer(opts)?;
    let sg = Sr25519Signer::from_uri(&signer_uri).map_err(|e| e.to_string())?;
    let who = sg.account_id().to_string();
    if !surface.members.is_empty() && !surface.members.contains(&who) {
        eprintln!(
            "❌ {who} is not a committee member. Members: {:?}",
            surface.members
        );
        return Ok(3);
    }
    let call = subxt::dynamic::tx(PALLET, "approve", vec![Value::from_bytes(hash)]);
    let nonce = signer::current_nonce(client, &opts.endpoint, &sg.account_id()).await;
    println!("approving as {who} [nonce {nonce}]…");
    signer::submit_signed(client, &call, &sg, nonce)
        .await
        .map_err(|e| e.to_string())?;
    let after = rpc
        .storage(&gov::storage_key_map_blake2_256(PALLET, "Proposals", &hash))
        .await?
        .and_then(|b| gov::parse_proposal_entry(&b).ok())
        .map(|p| p.approvals)
        .unwrap_or(0);
    println!(
        "✅ approval recorded — now {}/{}",
        after, surface.approvals_required
    );
    if after >= surface.approvals_required {
        println!("   threshold met; `execute` unlocks after the timelock (see `status`).");
    }
    Ok(0)
}

/// `cancel <proposal-hash>` — withdraw a pending proposal. The pallet requires
/// the **same** approval threshold as execution (3 of 3 here), so a cancel is
/// signed once the collection is complete. No timelock applies to a cancel.
async fn cmd_cancel(
    opts: &Opts,
    rpc: &Rpc,
    client: &OnlineClient<GpnConfig>,
) -> Result<i32, String> {
    let hash_hex = opts
        .args
        .first()
        .cloned()
        .ok_or("usage: cancel <proposal-hash>")?;
    let hash = gov::parse_hash(&hash_hex)?;
    let surface = read_surface(rpc, client).await?;
    let entry = rpc
        .storage(&gov::storage_key_map_blake2_256(PALLET, "Proposals", &hash))
        .await?
        .ok_or_else(|| {
            format!("no proposal {hash_hex} on chain (wrong hash, or already executed)")
        })?;
    let parsed = gov::parse_proposal_entry(&entry)
        .map_err(|e| format!("cannot decode proposal {hash_hex}: {e}"))?;
    println!(
        "proposal {hash_hex}: {} approvals, created at block {}",
        parsed.approvals, parsed.created_at
    );
    if parsed.approvals < surface.approvals_required {
        eprintln!(
            "❌ cancel needs {}/{} approvals, this proposal has {}",
            surface.approvals_required, surface.approvals_required, parsed.approvals
        );
        return Ok(2);
    }

    let signer_uri = require_signer(opts)?;
    let sg = Sr25519Signer::from_uri(&signer_uri).map_err(|e| e.to_string())?;
    let who = sg.account_id().to_string();
    if !surface.members.is_empty() && !surface.members.contains(&who) {
        eprintln!(
            "❌ {who} is not a committee member. Members: {:?}",
            surface.members
        );
        return Ok(3);
    }
    let call = subxt::dynamic::tx(PALLET, "cancel", vec![Value::from_bytes(hash)]);
    let nonce = signer::current_nonce(client, &opts.endpoint, &sg.account_id()).await;
    println!("cancelling as {who} [nonce {nonce}]…");
    signer::submit_signed(client, &call, &sg, nonce)
        .await
        .map_err(|e| e.to_string())?;
    let still_there = rpc
        .storage(&gov::storage_key_map_blake2_256(PALLET, "Proposals", &hash))
        .await?;
    println!(
        "✅ proposal cancelled{}",
        if still_there.is_some() {
            " (still readable — re-check status)"
        } else {
            ", removed"
        }
    );
    Ok(0)
}

/// Enumerate every pending proposal `(id, raw entry)`.
///
/// `Proposals` is a `Blake2_256`-hashed map, so the proposal id is *not* in the
/// storage key (the key is `blake2_256(id)`). We therefore read each value and
/// recover the id from the stored call bytes.
async fn pending_proposals(rpc: &Rpc) -> Result<Vec<([u8; 32], Vec<u8>)>, String> {
    let prefix = gov::storage_prefix(PALLET, "Proposals");
    let keys = rpc.keys_paged(&prefix, 200).await?;
    let mut out = Vec::new();
    for k in keys {
        let Some(raw) = rpc.storage(&k).await? else {
            continue;
        };
        match gov::proposal_hash_from_entry(&raw) {
            Ok(h) => out.push((h, raw)),
            Err(e) => eprintln!("⚠️  skipping proposal {k}: {e}"),
        }
    }
    Ok(out)
}

async fn print_proposal(
    opts: &Opts,
    rpc: &Rpc,
    surface: &GovSurface,
    head: u64,
    hash: &[u8; 32],
    plan_hash: Option<[u8; 32]>,
) -> Result<(), String> {
    let hash_hex = gov::hex0x(hash);
    let raw = rpc
        .storage(&gov::storage_key_map_blake2_256(PALLET, "Proposals", hash))
        .await?
        .ok_or_else(|| format!("no proposal {hash_hex} on chain"))?;
    println!("proposal {hash_hex}");
    match gov::parse_proposal_entry(&raw) {
        Ok(p) => {
            println!(
                "  call         {} (pallet #{}, call #{})",
                p.call_name, p.pallet_index, p.call_index
            );
            if let Some(code) = &p.code {
                println!(
                    "  code         {} bytes, blake2 {}",
                    code.len(),
                    gov::hex0x(&blake2_hash(code))
                );
                match gov::inspect_artifact("<proposal>", code) {
                    Ok(a) => {
                        if let Some(v) = &a.version {
                            println!(
                                "  runtime      {} spec_version={} (decoded from the proposed WASM)",
                                v.spec_name, v.spec_version
                            );
                        } else {
                            println!("  runtime      <no runtime_version section>");
                        }
                    }
                    Err(e) => println!("  runtime      <cannot inspect: {e}>"),
                }
                if let Some(want) = plan_hash {
                    let got = blake2_hash(code);
                    if got == want {
                        println!("  matches      plan code hash ✓");
                    } else {
                        println!(
                            "  matches      ✗ plan expects {} — NOT the staged artifact",
                            gov::hex0x(&want)
                        );
                    }
                }
            }
            println!(
                "  approvals    {}/{}",
                p.approvals, surface.approvals_required
            );
            println!("  created      block {}", p.created_at);
            let remaining =
                gov::blocks_remaining(head, p.created_at as u64, surface.timelock_blocks);
            let exec_at = gov::execution_block(p.created_at as u64, surface.timelock_blocks);
            match gov::execute_readiness(
                head,
                p.created_at as u64,
                surface.timelock_blocks,
                p.approvals,
                surface.approvals_required,
            ) {
                Ok(()) => println!("  status       READY — execute now (block {head} ≥ {exec_at})"),
                Err(e) => println!(
                    "  status       {e} (executable at block {exec_at}, ~{} at {} s/block)",
                    gov::format_duration(remaining, opts.block_time),
                    opts.block_time
                ),
            }
        }
        Err(e) => {
            println!("  <cannot decode: {e}> (raw entry: {} bytes)", raw.len());
        }
    }
    println!();
    Ok(())
}

fn blake2_hash(b: &[u8]) -> [u8; 32] {
    gov::proposal_hash(b)
}

async fn cmd_status(
    opts: &Opts,
    rpc: &Rpc,
    client: &OnlineClient<GpnConfig>,
) -> Result<i32, String> {
    let cs = chain_state(rpc).await?;
    let surface = read_surface(rpc, client).await?;
    let plan_hash = match &opts.plan {
        Some(p) => Some(gov::parse_hash(&load_plan(p)?.code_hash)?),
        None => None,
    };
    let hashes: Vec<[u8; 32]> = match opts.args.first() {
        Some(h) => vec![gov::parse_hash(h)?],
        None => pending_proposals(rpc)
            .await?
            .into_iter()
            .map(|(h, _)| h)
            .collect(),
    };
    if hashes.is_empty() {
        println!(
            "no pending proposals on {} (head block {})",
            cs.name, cs.head
        );
        return Ok(0);
    }
    println!(
        "{} pending proposal(s) on {} (head {}, {} approval(s) required, {} block timelock)\n",
        hashes.len(),
        cs.name,
        cs.head,
        surface.approvals_required,
        surface.timelock_blocks
    );
    for h in &hashes {
        print_proposal(opts, rpc, &surface, cs.head, h, plan_hash).await?;
    }
    Ok(0)
}

async fn cmd_execute(
    opts: &Opts,
    rpc: &Rpc,
    client: &OnlineClient<GpnConfig>,
) -> Result<i32, String> {
    let hash_hex = opts
        .args
        .first()
        .cloned()
        .ok_or("usage: execute <proposal-hash>")?;
    let hash = gov::parse_hash(&hash_hex)?;
    let cs = chain_state(rpc).await?;
    let surface = read_surface(rpc, client).await?;
    let raw = rpc
        .storage(&gov::storage_key_map_blake2_256(PALLET, "Proposals", &hash))
        .await?
        .ok_or_else(|| format!("no proposal {hash_hex} on chain"))?;
    let entry: ProposalEntry = gov::parse_proposal_entry(&raw)
        .map_err(|e| format!("cannot decode proposal {hash_hex}: {e}"))?;
    match gov::execute_readiness(
        cs.head,
        entry.created_at as u64,
        surface.timelock_blocks,
        entry.approvals,
        surface.approvals_required,
    ) {
        Ok(()) => {}
        Err(e) => {
            // `--force` submits anyway, so the **pallet's** own guard is exercised
            // on chain (the tx is included and fails dispatch with the pallet
            // error, e.g. `TimelockNotElapsed`) instead of only the local check.
            if opts.force {
                eprintln!("⚠️  local readiness check failed: {e} — --force: submitting anyway");
            } else {
                eprintln!("❌ not executable yet: {e}");
                eprintln!("   re-check with: gpn_gov_upgrade status {hash_hex}");
                return Ok(2);
            }
        }
    }
    if let Some(code) = &entry.code {
        println!(
            "about to dispatch {} as Root: {} bytes, blake2 {}",
            entry.call_name,
            code.len(),
            gov::hex0x(&blake2_hash(code))
        );
    }

    let signer_uri = require_signer(opts)?;
    let sg = Sr25519Signer::from_uri(&signer_uri).map_err(|e| e.to_string())?;
    let call = subxt::dynamic::tx(PALLET, "execute", vec![Value::from_bytes(hash)]);
    let nonce = signer::current_nonce(client, &opts.endpoint, &sg.account_id()).await;
    println!("executing as {} [nonce {nonce}]…", sg.account_id());
    signer::submit_signed(client, &call, &sg, nonce)
        .await
        .map_err(|e| e.to_string())?;

    let gone = rpc
        .storage(&gov::storage_key_map_blake2_256(PALLET, "Proposals", &hash))
        .await?
        .is_none();
    println!(
        "✅ execute dispatched — proposal {}",
        if gone {
            "removed (dispatch succeeded)"
        } else {
            "still present (dispatch reverted; it was removed then re-added? check events)"
        }
    );
    println!(
        "   verify with: gpn_gov_upgrade verify --expect-code-hash {}",
        gov::hex0x(&entry.code_hash().unwrap_or([0u8; 32]))
    );
    Ok(0)
}

async fn cmd_verify(opts: &Opts, rpc: &Rpc) -> Result<i32, String> {
    let (expect_spec, expect_hash_opt) = match &opts.plan {
        Some(p) => {
            let plan = load_plan(p)?;
            (plan.spec_version, Some(gov::parse_hash(&plan.code_hash)?))
        }
        None => (
            opts.expect_spec
                .ok_or("verify needs --plan <file> or --expect-spec <n>")?,
            match &opts.expect_code_hash {
                Some(h) => Some(gov::parse_hash(h)?),
                None => None,
            },
        ),
    };
    let expect_hash = match expect_hash_opt {
        Some(h) => h,
        None => {
            return Err(
                "verify needs the expected :code hash (--plan, or --expect-code-hash). \
                 Without it a version bump alone cannot prove the right runtime landed."
                    .into(),
            )
        }
    };
    println!(
        "verifying spec_version ≥ {expect_spec} and :code == {}",
        gov::hex0x(&expect_hash)
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(opts.timeout_secs);
    let mut last_head = 0u64;
    let mut stalled_polls = 0u32;
    loop {
        let cs = chain_state(rpc).await?;
        let code = rpc.storage(gov::CODE_KEY_HEX).await?;
        let observed_hash = code.as_deref().map(blake2_hash);
        let state = gov::verify_outcome(expect_spec, expect_hash, cs.spec_version, observed_hash);
        let code_desc = match (&code, observed_hash) {
            (Some(b), Some(h)) => format!("{} bytes, blake2 {}", b.len(), gov::hex0x(&h)),
            _ => "<unreadable>".to_string(),
        };
        println!(
            "  head {:<10} spec_version {:<4} :code {}",
            cs.head, cs.spec_version, code_desc
        );
        match state {
            VerifyState::Ok => {
                let health = rpc.health().await;
                let (peers, syncing) = health
                    .as_ref()
                    .map(|h| {
                        (
                            h.get("peers").and_then(|v| v.as_u64()).unwrap_or(0),
                            h.get("isSyncing")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false),
                        )
                    })
                    .unwrap_or((0, false));
                println!("\n✅ upgrade enacted: spec_version {expect_spec}, code hash matches");
                println!("   peers {peers}, syncing {syncing}, head {}", cs.head);
                if let Some(ap) = &opts.ap_url {
                    check_ap(ap).await;
                }
                // Confirm blocks keep coming (a bad runtime upgrade stalls the chain).
                let h1 = rpc.head_number().await.unwrap_or(0);
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let h2 = rpc.head_number().await.unwrap_or(0);
                if h2 > h1 {
                    println!("   block production advancing ({h1} → {h2}) ✓");
                } else {
                    println!(
                        "⚠️  no new block in 3 s — watch the node (block time is 15 s target)"
                    );
                }
                return Ok(0);
            }
            VerifyState::Pending { .. } => {
                if cs.head == last_head {
                    stalled_polls += 1;
                } else {
                    stalled_polls = 0;
                }
                last_head = cs.head;
                if std::time::Instant::now() >= deadline {
                    eprintln!(
                        "\n❌ timed out after {}s waiting for enactment.",
                        opts.timeout_secs
                    );
                    eprintln!("   Possible causes:");
                    eprintln!("   • proposal still short of approvals or timelock (run `status`)");
                    eprintln!("   • execute reverted (check System::ExtrinsicFailed in the block)");
                    eprintln!(
                        "   • chain stopped producing blocks (stalled_polls={stalled_polls})"
                    );
                    return Ok(2);
                }
            }
            VerifyState::VersionMismatch { expected, observed } => {
                eprintln!(
                    "\n❌ :code matches the artifact but spec_version is {observed}, expected ≥ {expected}"
                );
                eprintln!("   The wrong artifact (or a stale rebuild) was enacted.");
                return Ok(3);
            }
            VerifyState::CodeMismatch { expected, observed } => {
                eprintln!("\n❌ spec_version moved but :code does not match");
                eprintln!("   expected {}", gov::hex0x(&expected));
                eprintln!(
                    "   observed {}",
                    observed.map(|h| gov::hex0x(&h)).unwrap_or_default()
                );
                eprintln!("   Another upgrade landed first. Re-stage against the new version.");
                return Ok(3);
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(opts.interval_secs)).await;
    }
}

async fn check_ap(url: &str) {
    let probe = async {
        let r = reqwest::Client::new()
            .get(url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .ok()?;
        Some(r.status().as_u16())
    };
    match probe.await {
        Some(code) => println!("   access point {url} → HTTP {code}"),
        None => println!("⚠️  access point {url} unreachable"),
    }
}

async fn cmd_rollback(
    opts: &Opts,
    rpc: &Rpc,
    client: &OnlineClient<GpnConfig>,
) -> Result<i32, String> {
    let target = opts
        .to
        .clone()
        .or_else(|| opts.wasm.clone())
        .or_else(|| opts.args.first().cloned())
        .ok_or("usage: rollback --to <wasm|plan>")?;
    let (path, plan_spec) = if target.ends_with(".plan.json") {
        let p = load_plan(&target)?;
        (p.artifact_path, Some(p.spec_version))
    } else {
        (target, None)
    };
    let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    let art = gov::inspect_artifact(&path, &bytes)?;
    let cs = chain_state(rpc).await?;
    println!("rollback target: {}", art.summary());
    println!("chain now: {} v{}", cs.spec_name, cs.spec_version);
    if let Some(s) = plan_spec {
        if art.version.as_ref().map(|v| v.spec_version) != Some(s) {
            println!(
                "⚠️  plan says spec {s} but the artifact says {:?}",
                art.version.as_ref().map(|v| v.spec_version)
            );
        }
    }

    let increase_ok = gov::check_artifact_for_chain(&art, &cs.spec_name, cs.spec_version, true);
    match &increase_ok {
        Ok(()) => {
            println!("\nthis artifact is a valid *forward* upgrade (spec_version > current),");
            println!("so the rollback is a normal proposal:");
            println!("  1) gpn_gov_upgrade propose --wasm {path} --signer <member-A>");
            println!("  2) gpn_gov_upgrade approve <hash> --signer <member-B>");
            println!(
                "  3) wait {} blocks (~{})",
                const_u128(client, PALLET, "TimelockPeriod").unwrap_or(0),
                gov::format_duration(
                    const_u128(client, PALLET, "TimelockPeriod").unwrap_or(0) as u64,
                    opts.block_time
                )
            );
            println!("  4) gpn_gov_upgrade execute <hash>   (anyone)");
            println!("  5) gpn_gov_upgrade verify --plan <plan>");
            if opts.submit {
                // Reuse the propose path with an explicit wasm.
                let mut sub = opts.clone();
                sub.wasm = Some(path.clone());
                sub.plan = None;
                return cmd_propose(&sub, rpc, client).await;
            }
            return Ok(0);
        }
        Err(ArtifactProblem::NotAnIncrease { .. }) => {}
        Err(e) => {
            eprintln!("❌ {e}");
            return Ok(3);
        }
    }

    // The honest case: rolling back to older code.
    println!("\n⚠️  {} ", increase_ok.unwrap_err());
    println!("There are exactly two honest rollback paths:");
    println!();
    println!("A) REBUILD the previous source with a higher spec_version (recommended).");
    println!("   `System.set_code` refuses a non-increasing spec_version, so a rollback is");
    println!("   always a *new* release of the old code:");
    println!(
        "     1. check out the previous runtime source, set spec_version = {}",
        cs.spec_version + 1
    );
    println!("     2. build the runtime WASM on its own (never the canonical genesis artifact)");
    println!("     3. gpn_gov_upgrade stage <wasm> && propose/approve/execute/verify");
    println!("   Clients downgrade cleanly: the code is the old behaviour, the version only");
    println!("   moves forward.");
    println!();
    println!("B) EMERGENCY: propose System.set_code_without_checks (root-only, skips the version");
    println!("   gate and the spec_name check). Use it only when the chain is visibly broken");
    println!("   and committee members have verified the artifact hash out-of-band:");
    println!("     gpn_gov_upgrade propose --wasm {path} --call set_code_without_checks \\");
    println!("        --i-understand-risk --signer <member-A>");
    println!("   The 3-day timelock still applies — this is not an instant switch-off.");
    println!();
    if opts.submit && opts.call == "set_code_without_checks" && opts.i_understand_risk {
        let mut sub = opts.clone();
        sub.wasm = Some(path.clone());
        sub.plan = None;
        return cmd_propose(&sub, rpc, client).await;
    }
    if opts.submit {
        eprintln!("❌ refusing to submit: for path B pass --call set_code_without_checks --i-understand-risk");
        return Ok(4);
    }
    Ok(0)
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let opts = parse_opts();
    if opts.command.is_empty() || opts.command == "help" {
        print_help();
        std::process::exit(if opts.command.is_empty() { 1 } else { 0 });
    }
    match run(&opts).await {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("❌ {e}");
            std::process::exit(1);
        }
    }
}

async fn run(opts: &Opts) -> Result<i32, String> {
    let rpc = Rpc::new(&opts.endpoint);
    let client = signer::connect(&opts.endpoint)
        .await
        .map_err(|e| format!("cannot connect to {}: {e}", opts.endpoint))?;
    match opts.command.as_str() {
        "surface" | "members" => {
            cmd_surface(opts, &rpc, &client).await?;
            Ok(0)
        }
        "stage" => cmd_stage(opts, &rpc, &client).await,
        "propose" => cmd_propose(opts, &rpc, &client).await,
        "approve" => cmd_approve(opts, &rpc, &client).await,
        "cancel" => cmd_cancel(opts, &rpc, &client).await,
        "status" => cmd_status(opts, &rpc, &client).await,
        "execute" => cmd_execute(opts, &rpc, &client).await,
        "verify" => cmd_verify(opts, &rpc).await,
        "rollback" => cmd_rollback(opts, &rpc, &client).await,
        other => {
            eprintln!("unknown command '{other}'");
            print_help();
            Ok(1)
        }
    }
}
