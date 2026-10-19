//! gpn_diffusion_provider — provider-side image/video generation executor.
//!
//! Polls the chain for `ImageGeneration` / `VideoGeneration` jobs that are
//! `Running` and assigned to this signer, reads the prompt/params from the
//! off-chain store, shells out to the local diffusers helper (`gpn_diffuse.py`,
//! runs on the GPU via HuggingFace diffusers/CUDA), stores the produced
//! artifact + its sha256, and settles the job on-chain (`submit_result` then
//! `complete_job`). The chain only ever sees the artifact hash.
//!
//! Usage:
//!   gpn_diffusion_provider [endpoint] [signer_uri] [poll_ms]
//! Env: ENDPOINT, SIGNER_URI, POLL_MS, GPN_DIFFUSE_SCRIPT (path to gpn_diffuse.py)

use gpn_agent::chain::ChainClient;
use gpn_agent::llm::{get_job, LlmStore};
use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Job ids this process has already declared a **wedge** (helper hung past its
/// budget). Re-running one would just wedge again in a tight loop, burning the
/// GPU with no progress; the chain-side expiry sweep reclaims them instead.
/// In-memory only — a provider restart clears it (a restart is the known cure).
fn wedged_set() -> &'static Mutex<HashSet<u64>> {
    static WEDGED: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    WEDGED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn mark_wedged(id: u64) {
    wedged_set().lock().unwrap().insert(id);
}

fn is_wedged(id: u64) -> bool {
    wedged_set().lock().unwrap().contains(&id)
}

fn diffuse_script() -> PathBuf {
    if let Ok(p) = std::env::var("GPN_DIFFUSE_SCRIPT") {
        return PathBuf::from(p);
    }
    // Default: alongside this crate (repo gpn-agent dir).
    PathBuf::from("gpn_diffuse.py")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .or_else(|| std::env::var("ENDPOINT").ok())
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let signer_uri = args
        .get(2)
        .cloned()
        .or_else(|| std::env::var("SIGNER_URI").ok())
        .unwrap_or_else(|| "//Alice".to_string());
    let poll_ms: u64 = args
        .get(3)
        .and_then(|s| s.parse().ok())
        .or_else(|| std::env::var("POLL_MS").ok().and_then(|s| s.parse().ok()))
        .unwrap_or(4000);

    let client = ChainClient::new(&endpoint).await?.with_signer(&signer_uri);
    let (me, addr) = {
        use subxt::tx::Signer as _;
        let s = gpn_agent::chain::signer::Sr25519Signer::from_uri(&signer_uri)?;
        let aid = s.account_id();
        let b: &[u8] = aid.as_ref();
        (format!("0x{}", hex::encode(b)), aid.to_string())
    };

    println!(
        "GPN diffusion provider running — endpoint={} signer={} poll={}ms script={}",
        endpoint,
        me,
        poll_ms,
        diffuse_script().display()
    );

    // #46 Phase 1 — pull assignments from the AP (works behind NAT; see
    // gpn_agent::assign_pull). Push remains active unless GPN_ASSIGN_SOURCE=pull.
    if let Some(cfg) = gpn_agent::assign_pull::PullConfig::from_env(&me) {
        gpn_agent::assign_pull::spawn(cfg);
    }

    loop {
        if let Err(e) = process_once(&client, &me, &addr).await {
            eprintln!("[provider] cycle error: {e}");
        }
        tokio::time::sleep(Duration::from_millis(poll_ms)).await;
    }
}

fn is_gen(job: &gpn_agent::llm::LlmJob) -> bool {
    job.kind == "image" || job.kind == "video"
}

/// A state error meaning "the job already advanced / isn't in the state we
/// expected" — benign during reconcile (the owning provider or a prior cycle
/// got there first), so it must not be logged as an ERROR. This is the source
/// of the spurious `JobNotRunning [pallet=11 e=5]` log spam.
fn is_benign_state_err(s: &str) -> bool {
    s.contains("JobNotRunning") || s.contains("JobNotPending") || s.contains("JobNotAccepted")
}

/// How many of the most recent job ids the reconcile pass scans per cycle
/// (was `0..next` = every historical job, one RPC each, on EVERY poll).
const RECONCILE_WINDOW: u64 = 128;

/// Hard time budget for one generation. Beyond this the helper is considered
/// **wedged** (GPU contention, a stalled HF download, a CUDA hang) and is
/// killed, so a single bad job can never silence the provider forever — the
/// exact "silent wedge" this instrumentation detects. Override with
/// `GPN_DIFFUSE_TIMEOUT_SECS`; default 600 s for images, 1800 s for video.
fn diffuse_timeout(kind: &str) -> Duration {
    if let Ok(s) = std::env::var("GPN_DIFFUSE_TIMEOUT_SECS") {
        if let Ok(n) = s.parse::<u64>() {
            if n > 0 {
                return Duration::from_secs(n);
            }
        }
    }
    if kind == "video" {
        Duration::from_secs(1800)
    } else {
        Duration::from_secs(600)
    }
}

/// Progress-heartbeat interval while the generation helper runs. Emitting this
/// keeps the provider log from going *silent* during a long render, so a wedge
/// is visible as "still running at Ns" rather than dead air.
const HELPER_HEARTBEAT: Duration = Duration::from_secs(30);

/// Run a command under a hard watchdog, capturing stdout/stderr to temp files.
///
/// Returns `(stdout, stderr)` on success. On non-zero exit returns the helper's
/// stderr in `Err`. On timeout the child is **killed** and `Err("WEDGE: …")` is
/// returned so the caller logs the wedge and keeps serving instead of stalling.
fn run_with_watchdog(
    mut cmd: std::process::Command,
    timeout: Duration,
    label: &str,
) -> Result<(String, String), String> {
    let tag = format!("{}_{}", label.replace(' ', "_"), std::process::id());
    let out_path = gpn_agent::utils::temp_path(&format!("gpn_wd_{tag}.out"));
    let err_path = gpn_agent::utils::temp_path(&format!("gpn_wd_{tag}.err"));
    let out_f = std::fs::File::create(&out_path).map_err(|e| format!("tmp stdout: {e}"))?;
    let err_f = std::fs::File::create(&err_path).map_err(|e| format!("tmp stderr: {e}"))?;
    cmd.stdout(out_f).stderr(err_f);
    let mut child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;
    let pid = child.id();
    let start = std::time::Instant::now();
    let mut last_hb = start;
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let so = std::fs::read_to_string(&out_path).unwrap_or_default();
                let se = std::fs::read_to_string(&err_path).unwrap_or_default();
                break if status.success() {
                    Ok((so, se))
                } else {
                    Err(format!("helper exited {:?}: {}", status.code(), se.trim()))
                };
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(format!(
                        "WEDGE: {label} helper (pid {pid}) exceeded {}s — killed (GPU/download hang?)",
                        timeout.as_secs()
                    ));
                }
                if last_hb.elapsed() >= HELPER_HEARTBEAT {
                    eprintln!(
                        "[watchdog] {label} helper pid {pid} still running — elapsed={}s",
                        start.elapsed().as_secs()
                    );
                    last_hb = std::time::Instant::now();
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => {
                let _ = child.kill();
                break Err(format!("wait failed: {e}"));
            }
        }
    };
    let _ = std::fs::remove_file(&out_path);
    let _ = std::fs::remove_file(&err_path);
    result
}

fn assignment_store_path() -> String {
    std::env::var("GPN_ASSIGN_STORE").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.gpn/assignments.json")
    })
}

/// Job ids the scheduler assigned to us (hex or ss58), + the requested model +
/// whether we must pull it on demand. Handles string + `{provider,model,pull}`.
fn read_assigned(me: &str, addr: &str) -> Vec<(u64, String, bool)> {
    let Ok(s) = std::fs::read_to_string(assignment_store_path()) else {
        return vec![];
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else {
        return vec![];
    };
    let Some(m) = v.as_object() else {
        return vec![];
    };
    m.iter()
        .filter_map(|(k, pv)| {
            let id = k.parse::<u64>().ok()?;
            let (p, model, pull) = if let Some(s) = pv.as_str() {
                (s.to_string(), String::new(), false)
            } else {
                (
                    pv.get("provider")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    pv.get("model")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    pv.get("pull").and_then(|x| x.as_bool()).unwrap_or(false),
                )
            };
            if p == me || p == addr {
                Some((id, model, pull))
            } else {
                None
            }
        })
        .collect()
}

/// Ensure a diffusion model is present; download from HuggingFace if missing.
/// `model` is an HF repo id (e.g. `stabilityai/stable-diffusion-2-1`) or a local
/// path. Empty/`auto` = use whatever the helper defaults to.
fn ensure_model(model: &str) -> Result<String, String> {
    if model.is_empty() || model == "auto" {
        return Ok("no specific model requested".into());
    }
    if model.starts_with('/') || model.starts_with('.') {
        return Ok("local model path".into());
    }
    // Map short aliases (as advertised by gpn_diffuse.py) to real HF repo ids,
    // otherwise ensure_model would try to pull a non-existent repo (e.g. "sd-turbo")
    // and 401/404 every poll cycle.
    let model = match model {
        "sd-turbo" => "stabilityai/sd-turbo",
        "sd15" | "sd1.5" => "runwayml/stable-diffusion-v1-5",
        "ltx-video" => "Lightricks/LTX-Video",
        other => other,
    };
    // HF cache marker: ~/.cache/huggingface/hub/models--<org>--<name>
    let repo_dir = format!("models--{}", model.replace('/', "--"));
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let hub = std::env::var("HF_HOME")
        .map(|h| format!("{h}/hub"))
        .unwrap_or_else(|_| format!("{home}/.cache/huggingface/hub"));
    if std::path::Path::new(&format!("{hub}/{repo_dir}")).exists() {
        return Ok(format!("model {model} already cached"));
    }
    println!("[provider] diffusion model {model} missing — pulling from HuggingFace…");
    let out = std::process::Command::new("python3")
        .arg("-c")
        .arg(format!(
            "from huggingface_hub import snapshot_download; snapshot_download('{model}')"
        ))
        .output()
        .map_err(|e| format!("spawn python3: {e}"))?;
    if out.status.success() {
        Ok(format!("pulled diffusion model {model} from HuggingFace"))
    } else {
        Err(format!(
            "HF pull {model} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

async fn process_once(
    client: &ChainClient,
    me: &str,
    addr: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // -1. Reconcile: finalize any of our jobs left in `AwaitingVerification`.
    // `complete_job` is the provider's own finalization call (state
    // Running|AwaitingVerification → Completed). On a slow chain a
    // `submit_result`/`complete_job` tx can time out *after* the result was
    // recorded, stranding the job mid-state and permanently consuming one of
    // the provider's `MaxJobsPerProvider` slots (→ `MaxProviderCapacity` on
    // every later accept). Re-drive such jobs to Completed so capacity is
    // reclaimed. Kind-agnostic so it also clears zombies with no off-chain
    // payload entry.
    let next = client.get_next_job_id().await;
    for id in next.saturating_sub(RECONCILE_WINDOW)..next {
        if let Some((state, _price, _req, provider)) = client.get_job_state(id).await {
            if provider.as_deref() != Some(me) {
                continue;
            }
            if state == "AwaitingVerification" {
                // Kind-gate: skip jobs owned by the other provider (shared
                // signer account means `provider == me` is true for both kinds).
                // Unknown kinds are still reconciled so orphan zombies get freed.
                if !get_job(id).map(|j| is_gen(&j)).unwrap_or(true) {
                    continue;
                }
                match client.complete_job(id).await {
                    Ok(()) => {
                        println!("[provider] reconciled #{id} AwaitingVerification → Completed")
                    }
                    Err(e) if is_benign_state_err(&e.to_string()) => {
                        // Already finalized elsewhere — quiet, not an error.
                    }
                    Err(e) => eprintln!("[provider] reconcile #{id} (complete) failed: {e}"),
                }
            } else if state == "Accepted" && get_job(id).map(|j| is_gen(&j)).unwrap_or(false) {
                // Capacity reclaim: an `Accepted` gen job we own that was never
                // started is a zombie holding a `MaxJobsPerProvider` slot. Our
                // claim loop only starts `Pending` jobs, so it can never advance
                // on its own — drive it Accepted → Running → Completed to free
                // the slot (escrow is refunded by `complete_job`).
                if let Err(e) = client.start_job(id).await {
                    eprintln!("[provider] reconcile #{id} (start) failed: {e}");
                    continue;
                }
                match client.complete_job(id).await {
                    Ok(()) => println!(
                        "[provider] reconciled #{id} Accepted → Completed (capacity freed)"
                    ),
                    Err(e) => eprintln!("[provider] reconcile #{id} (complete) failed: {e}"),
                }
            }
        }
    }

    // 0. Auto-claim: if the scheduler assigned us specific jobs, claim ONLY those
    //    (pulling any missing model first); otherwise claim any Pending gen job.
    let assigned = read_assigned(me, addr);
    let assigned_ids: Vec<u64> = if assigned.is_empty() {
        vec![]
    } else {
        for (id, model, pull) in &assigned {
            let Some(job) = get_job(*id) else { continue };
            if !is_gen(&job) || job.status == "done" {
                continue;
            }
            if model.is_empty() || model == "auto" {
                continue;
            }
            let _ = pull;
            match ensure_model(model) {
                // Only report a pull that actually happened (avoid per-cycle noise).
                Ok(n) if n.starts_with("pulled") => println!("[provider] job #{id}: {n}"),
                Ok(_) => {}
                Err(e) => eprintln!("[provider] job #{id}: {e}"),
            }
        }
        assigned
            .iter()
            .filter(|(id, _, _)| get_job(*id).map(|j| is_gen(&j)).unwrap_or(false))
            .map(|(id, _, _)| *id)
            .collect()
    };

    let attested_gflops = client
        .get_attestation_record(addr)
        .await
        .map(|(_, f, _)| f)
        .unwrap_or(0);
    for (job_id, min_gflops) in client.list_claimable_jobs(attested_gflops).await {
        if !assigned_ids.is_empty() && !assigned_ids.contains(&job_id) {
            continue;
        }
        if !get_job(job_id).map(|j| is_gen(&j)).unwrap_or(false) {
            continue;
        }
        println!("[provider] claiming generation job #{job_id} (min_gflops={min_gflops})");
        if let Err(e) = client.accept_job(job_id).await {
            // JobNotPending means we (or a prior cycle) already accepted it; don't give
            // up — fall through and still drive it to Running, otherwise it wedges in
            // Accepted forever (accept→continue skipped start_job).
            let already = e.to_string().contains("JobNotPending");
            if !already {
                eprintln!("[provider] accept #{job_id} failed: {e}");
                continue;
            }
        }
        if let Err(e) = client.start_job(job_id).await {
            eprintln!("[provider] start #{job_id} failed: {e}");
        }
        // One GPU = one job: we have accepted this job — stop claiming others
        // until it has been run to completion.
        break;
    }

    // 1. Run any Running generation job assigned to this signer.
    let next = client.get_next_job_id().await;
    for id in 0..next {
        let job = match get_job(id) {
            Some(j) if is_gen(&j) => j,
            _ => continue,
        };
        if let Some((state, _price, _req, provider)) = client.get_job_state(id).await {
            if state == "Running" && provider.as_deref() == Some(me) {
                // Skip a job we already proved wedges this host — re-rendering it
                // only to kill it again would stall the GPU with no progress.
                if is_wedged(id) {
                    continue;
                }
                run_job(client, id, job).await?;
            }
        }
    }
    Ok(())
}

async fn run_job(
    client: &ChainClient,
    id: u64,
    job: gpn_agent::llm::LlmJob,
) -> Result<(), Box<dyn std::error::Error>> {
    // Item #22 build-order step 3: provider-side policy hook. Refuse hard-harm
    // categories (defense in depth over the AP's moderation) and any model this
    // operator does not serve BEFORE spending GPU time; settle on-chain so the
    // requestor's escrow refunds and the capacity slot frees.
    let policy = gpn_agent::provider_policy::ProviderPolicy::from_env();
    if let gpn_agent::provider_policy::Decision::Refuse(r) = policy.check(&job.prompt, &job.model) {
        let cat = r.category.unwrap_or("model_policy");
        eprintln!(
            "[job {id}] \u{26a0} policy refusal (category={cat}) \u{2014} not running: {}",
            r.reason
        );
        let _ = LlmStore::new().set_error(id, &format!("gpn_policy_violation: {cat}"));
        match client.fail_job(id, 2).await {
            Ok(()) => println!("[job {id}] refused by provider policy \u{2014} escrow refunded"),
            Err(e) => eprintln!("[job {id}] fail_job (policy) failed: {e}"),
        }
        return Ok(());
    }
    // Item #22 build-order step 4: report/ban path. Refuse a job whose on-chain
    // requestor is on this operator's denylist (shared with the AP).
    if let Some((_state, _price, Some(requestor), _prov)) = client.get_job_state(id).await {
        if let Some(r) = policy.requestor_refused(&requestor) {
            eprintln!(
                "[job {id}] \u{26a0} requestor denylist refusal \u{2014} not running: {}",
                r.reason
            );
            let _ = LlmStore::new().set_error(id, "gpn_policy_violation: requestor_denylist");
            match client.fail_job(id, 2).await {
                Ok(()) => println!("[job {id}] requestor banned \u{2014} escrow refunded"),
                Err(e) => eprintln!("[job {id}] fail_job (denylist) failed: {e}"),
            }
            return Ok(());
        }
    }
    println!(
        "[job {id}] {} generation — model={} prompt=\"{:.40}\"",
        job.kind, job.model, job.prompt
    );
    let _ = LlmStore::new().set_status(id, "running");

    // Artifact output path.
    let ext = if job.kind == "image" { "png" } else { "mp4" };
    let out_dir = std::env::var("GPN_ARTIFACTS").unwrap_or_else(|_| {
        gpn_agent::utils::temp_dir()
            .join("gpn_artifacts")
            .to_string_lossy()
            .to_string()
    });
    std::fs::create_dir_all(&out_dir)?;
    let out_path = format!("{}/job_{}.{}", out_dir, id, ext);

    // Resolve empty/"auto" to a concrete default so the diffusers helper never
    // receives the literal "auto" (invalid HF repo id) and wedges the job.
    let model = match job.model.as_str() {
        "" | "auto" => {
            if job.kind == "video" {
                "ltx-video"
            } else {
                "sd-turbo"
            }
        }
        m => m,
    };
    let req_json = json!({
        "kind": job.kind,
        "model": model,
        "prompt": job.prompt,
        "negative_prompt": job.negative_prompt,
        "width": job.width.unwrap_or(512),
        "height": job.height.unwrap_or(512),
        "steps": job.steps.unwrap_or(30),
        "seed": job.seed.unwrap_or(0),
        "frames": job.frames.unwrap_or(16),
        "fps": job.fps.unwrap_or(8),
        "output": out_path,
    });
    let req_file = gpn_agent::utils::temp_path(&format!("gpn_diffuse_req_{}.json", id));
    std::fs::write(&req_file, serde_json::to_string(&req_json)?)?;

    // Run the diffusers helper (blocking; do it off the async runtime).
    // Yield the GPU to this job for its GPU-contending render ONLY (one GPU = one
    // job): the co-located node miner FULLY STOPS while the flag is held and
    // resumes the instant the guard drops. The guard is scoped to the helper
    // invocation and released BEFORE the on-chain settlement below — settlement
    // needs fresh blocks, and holding the flag across a block-waiting
    // `submit_result` would starve block production on a single-authority chain
    // and deadlock the provider (chain halt). No GPU is used during settlement.
    let script = diffuse_script();
    let timeout = diffuse_timeout(&job.kind);
    let req_file2 = req_file.clone();
    let out_res = {
        let _yield = gpn_agent::gpu_yield::GpuYieldGuard::acquire();
        tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
            let mut cmd = std::process::Command::new("python3");
            cmd.arg(&script).arg(&req_file2);
            let (stdout, _stderr) = run_with_watchdog(cmd, timeout, &format!("job {id}"))?;
            serde_json::from_str(&stdout).map_err(|e| format!("bad helper output: {e} ({stdout})"))
        })
        .await
        .map_err(|e| e.to_string())?
    };
    // A wedged/timed-out helper is reported explicitly (never silent) and the
    // job is marked errored off-chain; on-chain it stays Running and is
    // reclaimed by the expiry sweep. The provider itself keeps serving.
    let out = match out_res {
        Ok(v) => v,
        Err(e) => {
            if e.contains("WEDGE") {
                eprintln!("[job {id}] ⚠ SILENT-WEDGE DETECTED — {e}");
                mark_wedged(id);
            } else {
                eprintln!("[job {id}] generation error: {e}");
            }
            let _ = LlmStore::new().set_error(id, &e);
            let _ = std::fs::remove_file(&req_file);
            return Ok(());
        }
    };

    if out.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        let artifact = out["artifact"].as_str().unwrap_or(&out_path).to_string();
        let media = out["media_type"]
            .as_str()
            .unwrap_or("application/octet-stream")
            .to_string();
        let hash = out["sha256"].as_str().unwrap_or("").to_string();
        let _ = LlmStore::new().complete_artifact(id, &artifact, &media, &hash);
        let mut hb = [0u8; 32];
        hb.copy_from_slice(&hex::decode(&hash)?);
        client.submit_result(id, hb, 0).await?;
        client.complete_job(id).await?;
        println!("[job {id}] done — artifact {artifact} ({media}), hash {hash:.16}…");
    } else {
        let err = out["error"]
            .as_str()
            .unwrap_or("unknown generation error")
            .to_string();
        let _ = LlmStore::new().set_error(id, &err);
        eprintln!("[job {id}] generation failed: {err}");
    }
    let _ = std::fs::remove_file(&req_file);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> std::process::Command {
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(script);
        c
    }

    #[test]
    fn watchdog_returns_output_on_success() {
        let (out, _err) = run_with_watchdog(sh("echo hello"), Duration::from_secs(10), "t_ok")
            .expect("should succeed");
        assert!(out.contains("hello"), "stdout was: {out}");
    }

    #[test]
    fn watchdog_kills_a_hung_helper_and_flags_wedge() {
        // A helper that never finishes must be killed at the budget, not hang forever.
        let start = std::time::Instant::now();
        let err = run_with_watchdog(sh("sleep 30"), Duration::from_secs(1), "t_wedge")
            .expect_err("expected a wedge error");
        assert!(err.contains("WEDGE"), "got: {err}");
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "watchdog did not return promptly (took {:?})",
            start.elapsed()
        );
    }

    #[test]
    fn watchdog_reports_nonzero_exit_without_calling_it_a_wedge() {
        let err = run_with_watchdog(
            sh("echo boom >&2; exit 3"),
            Duration::from_secs(10),
            "t_fail",
        )
        .expect_err("expected a failure");
        assert!(err.contains("boom"), "got: {err}");
        assert!(!err.contains("WEDGE"), "false wedge: {err}");
    }

    #[test]
    fn diffuse_timeout_video_is_at_least_image() {
        assert!(diffuse_timeout("video") >= diffuse_timeout("image"));
        assert!(diffuse_timeout("image") > Duration::from_secs(0));
    }

    #[test]
    fn wedged_marker_roundtrip() {
        assert!(!is_wedged(424242));
        mark_wedged(424242);
        assert!(is_wedged(424242));
    }
}
