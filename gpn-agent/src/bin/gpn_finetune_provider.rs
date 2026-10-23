//! gpn_finetune_provider — provider-side on-network fine-tuning executor.
//!
//! Polls the chain for `FineTuning` jobs that are `Running` and assigned to this
//! signer, reads the dataset/model/params from the off-chain store, shells out
//! to the local LoRA fine-tuner (`gpn_finetune.py`, HF transformers + PEFT on
//! CUDA), stores the produced adapter artifact + its sha256, and settles the job
//! on-chain (`submit_result` then `complete_job`). The chain only sees the hash.
//!
//! Usage:
//!   gpn_finetune_provider [endpoint] [signer_uri] [poll_ms]
//! Env: ENDPOINT, SIGNER_URI, POLL_MS, GPN_FINETUNE_SCRIPT

use gpn_agent::chain::ChainClient;
use gpn_agent::llm::{get_job, LlmStore};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// A fine-tune that fails must never leave the job `Running` on chain with the
/// escrow locked — that is the #78 class in the one provider bin that still had
/// it: the old code wrote the error to the off-chain store only, so the chain
/// kept the job `Running` (and this provider, seeing `Running` + itself as the
/// provider, re-ran the whole training job every poll until the on-chain
/// deadline). Two shapes of failure, two treatments:
///
/// * the helper **ran and reported** `ok:false` — a verdict about this job's
///   dataset/model, so retrying cannot change it: fail on chain immediately;
/// * the helper **could not run at all** (missing script, wrong python, no
///   verdict, unparsable output) — an environment problem worth a bounded
///   number of retries before the job is failed instead of burning GPU forever.
const MAX_FT_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtFailure {
    /// The helper ran and returned a verdict (`ok:false`).
    Reported,
    /// No verdict at all: spawn/exit/output problem.
    Raw,
}

/// Whether this failure should be settled on chain now (`true`) or retried.
/// `Reported` is always terminal for the job; `Raw` is terminal once the bounded
/// budget is spent. The budget is floored at 1 so a misconfigured `0` cannot
/// turn every job into a single-attempt failure.
pub fn should_fail_now(kind: FtFailure, attempt: u32, max_attempts: u32) -> bool {
    match kind {
        FtFailure::Reported => true,
        FtFailure::Raw => attempt >= max_attempts.max(1),
    }
}

/// Parse the fine-tuner's verdict out of its output.
///
/// `transformers`' `Trainer` prints progress dicts to stdout, so the verdict is
/// the **last non-empty line** — and it is a verdict even when the process
/// exited non-zero (the helper reports budget/data errors through JSON). Only
/// when nothing on the output is a JSON object carrying `ok` is the run treated
/// as having produced no verdict at all.
pub fn helper_verdict(
    stdout: &str,
    exit_ok: bool,
    stderr: &str,
) -> Result<serde_json::Value, String> {
    let last = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    match serde_json::from_str::<serde_json::Value>(last.trim()) {
        Ok(v) if v.get("ok").is_some() => Ok(v),
        other => {
            let tail = |s: &str| {
                let t: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
                t[t.len().saturating_sub(3)..].join(" | ")
            };
            Err(format!(
                "finetune helper produced no verdict (exit_ok={exit_ok}, parse={:?}): stdout[{}] stderr[{}]",
                other.err().map(|e| e.to_string()),
                tail(stdout),
                tail(stderr)
            ))
        }
    }
}

fn ft_script() -> PathBuf {
    if let Ok(p) = std::env::var("GPN_FINETUNE_SCRIPT") {
        return PathBuf::from(p);
    }
    PathBuf::from("gpn_finetune.py")
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
    let me = {
        use subxt::tx::Signer as _;
        let s = gpn_agent::chain::signer::Sr25519Signer::from_uri(&signer_uri)?;
        let aid = s.account_id();
        let b: &[u8] = aid.as_ref();
        format!("0x{}", hex::encode(b))
    };

    println!(
        "GPN finetune provider running — endpoint={} signer={} poll={}ms script={}",
        endpoint,
        me,
        poll_ms,
        ft_script().display()
    );

    // #46 Phase 1 — pull assignments from the AP (works behind NAT; see
    // gpn_agent::assign_pull). Push remains active unless GPN_ASSIGN_SOURCE=pull.
    if let Some(cfg) = gpn_agent::assign_pull::PullConfig::from_env(&me) {
        gpn_agent::assign_pull::spawn(cfg);
    }

    loop {
        // IDLE gate: while the operator idles the node we take no new job.
        // The pass below claims and runs in one go, so a job already started
        // finishes normally; what stops is the *next* claim.
        if !gpn_agent::idle::claims_allowed() {
            tokio::time::sleep(Duration::from_millis(poll_ms.max(1000))).await;
            continue;
        }
        if let Err(e) = process_once(&client, &me).await {
            eprintln!("[provider] cycle error: {e}");
        }
        tokio::time::sleep(Duration::from_millis(poll_ms)).await;
    }
}

/// How many trailing jobs to consider for self-claim each pass. A full
/// `0..next` scan every poll pegs the RPC once the ledger grows (same lesson as
/// the LLM provider), and fine-tune jobs are only ever newly created.
const CLAIM_WINDOW: u64 = 64;

async fn process_once(client: &ChainClient, me: &str) -> Result<(), Box<dyn std::error::Error>> {
    let next = client.get_next_job_id().await;

    // 0. Self-claim Pending fine-tune jobs. There is no AP/scheduler routing
    //    path for `FineTuning` yet, so this provider is the only consumer: it
    //    accepts + starts any recent Pending finetune job that has an off-chain
    //    payload here. `accept_job` is gated on-chain by the min_gflops
    //    attestation check, so an under-provisioned provider is rejected there.
    for id in next.saturating_sub(CLAIM_WINDOW)..next {
        let is_ft = get_job(id).map(|j| j.kind == "finetune").unwrap_or(false);
        if !is_ft {
            continue;
        }
        if let Some((state, _price, _req, provider)) = client.get_job_state(id).await {
            if state == "Pending" && provider.is_none() {
                if let Err(e) = client.accept_job(id).await {
                    eprintln!("[provider] accept #{id} failed: {e}");
                    continue;
                }
                if let Err(e) = client.start_job(id).await {
                    eprintln!("[provider] start #{id} failed: {e}");
                }
            }
        }
    }

    for id in 0..next {
        let job = match get_job(id) {
            Some(j) if j.kind == "finetune" => j,
            _ => continue,
        };
        if let Some((state, _price, _req, provider)) = client.get_job_state(id).await {
            if state == "Running" && provider.as_deref() == Some(me) {
                run_job(client, id, job).await?;
            }
        }
    }
    Ok(())
}

/// Per-job attempt counter for `Raw` (no-verdict) failures. In-process only:
/// a restart gets a fresh budget, which is intended — a restart is exactly when
/// the environment may have been fixed.
fn attempt_counters() -> &'static Mutex<HashMap<u64, u32>> {
    static COUNTERS: OnceLock<Mutex<HashMap<u64, u32>>> = OnceLock::new();
    COUNTERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn bump_attempt(id: u64) -> u32 {
    let mut m = attempt_counters().lock().unwrap_or_else(|e| e.into_inner());
    let n = m.entry(id).or_insert(0);
    *n += 1;
    *n
}

fn clear_attempts(id: u64) {
    attempt_counters()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id);
}

/// Settle an unpayable fine-tune on chain so the escrow refunds and the
/// capacity slot frees now instead of at the deadline. Reason 5 = the provider
/// cannot produce a result for this job (same code the LLM/diffusion providers
/// use for "owns it, provably cannot run it").
async fn fail_on_chain(client: &ChainClient, id: u64, why: &str) {
    match client.fail_job(id, 5).await {
        Ok(()) => println!(
            "[job {id}] failed on-chain ({why}) — escrow refunded, slot freed"
        ),
        Err(e) => eprintln!(
            "[job {id}] fail_job after '{why}' did not land: {e} (the on-chain deadline path still refunds)"
        ),
    }
}

async fn run_job(
    client: &ChainClient,
    id: u64,
    job: gpn_agent::llm::LlmJob,
) -> Result<(), Box<dyn std::error::Error>> {
    let dataset = job.dataset.unwrap_or_default();
    let n_examples = dataset.matches("\"prompt\"").count();
    println!(
        "[job {id}] fine-tune — model={} examples={} lora_r={} epochs={}",
        job.model,
        n_examples,
        job.lora_r.unwrap_or(16),
        job.epochs.unwrap_or(2)
    );
    let _ = LlmStore::new().set_status(id, "running");

    let out_dir = gpn_agent::utils::temp_dir()
        .join(format!("gpn_artifacts/job_{id}_ft"))
        .to_string_lossy()
        .to_string();
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir)?;

    let req_json = json!({
        "model": job.model,
        "dataset": serde_json::from_str::<serde_json::Value>(&dataset)
            .unwrap_or_else(|_| serde_json::json!([])),
        "output": out_dir,
        "lora_r": job.lora_r.unwrap_or(16),
        "epochs": job.epochs.unwrap_or(2),
    });
    let req_file = gpn_agent::utils::temp_path(&format!("gpn_ft_req_{}.json", id));
    std::fs::write(&req_file, serde_json::to_string(&req_json)?)?;

    let script = ft_script();
    let req_file2 = req_file.clone();
    let outcome = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
        let child = std::process::Command::new("python3")
            .arg(&script)
            .arg(&req_file2)
            .output()
            .map_err(|e| format!("spawn python3: {e}"))?;
        let stdout = String::from_utf8_lossy(&child.stdout);
        let stderr = String::from_utf8_lossy(&child.stderr);
        helper_verdict(&stdout, child.status.success(), &stderr)
    })
    .await;

    // Normalise the three failure limbs into (kind, message): a helper verdict,
    // a helper that produced no verdict, or the blocking task panicking.
    let (kind, message, out) = match outcome {
        Ok(Ok(v)) if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) => {
            (None, String::new(), Some(v))
        }
        Ok(Ok(v)) => {
            let err = v
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown finetune error")
                .to_string();
            (Some(FtFailure::Reported), err, None)
        }
        Ok(Err(msg)) => (Some(FtFailure::Raw), msg, None),
        Err(join) => (
            Some(FtFailure::Raw),
            format!("finetune task failed: {join}"),
            None,
        ),
    };

    if let Some(kind) = kind {
        // Never leave a failed tune `Running` on chain with the escrow locked.
        let _ = LlmStore::new().set_error(id, &message);
        let attempt = bump_attempt(id);
        if should_fail_now(kind, attempt, MAX_FT_ATTEMPTS) {
            eprintln!(
                "[job {id}] fine-tune {} (attempt {attempt}/{MAX_FT_ATTEMPTS}): {message}",
                if kind == FtFailure::Reported {
                    "reported an error — not retrying"
                } else {
                    "could not run — out of retries"
                }
            );
            fail_on_chain(client, id, "the fine-tune cannot produce a result").await;
            // Only a terminal failure clears the budget; a retry must keep
            // counting or the job is retried forever (the very defect here).
            clear_attempts(id);
        } else {
            eprintln!(
                "[job {id}] fine-tune could not run (attempt {attempt}/{MAX_FT_ATTEMPTS}): {message} — retrying on the next pass"
            );
        }
        let _ = std::fs::remove_file(&req_file);
        return Ok(());
    }

    let out = out.expect("a successful verdict produced a json body");
    clear_attempts(id);

    {
        let artifact = out["artifact"].as_str().unwrap_or("").to_string();
        let media = out["media_type"]
            .as_str()
            .unwrap_or("application/zip")
            .to_string();
        let hash = out["sha256"].as_str().unwrap_or("").to_string();
        let loss = out["loss"].as_f64().unwrap_or(0.0);
        let _ = LlmStore::new().complete_artifact(id, &artifact, &media, &hash);
        let mut hb = [0u8; 32];
        hb.copy_from_slice(&hex::decode(&hash)?);
        client.submit_result(id, hb, 0).await?;
        client.complete_job(id).await?;
        println!("[job {id}] done — adapter {artifact} loss={loss:.3} hash {hash:.16}…");
        // Auto-register the tuned model for serving (tuned-<jobid>): merge LoRA
        // -> GGUF -> ollama create, detached so the poll loop isn't blocked.
        spawn_servemodel(id, &job.model, &out_dir);
    }
    let _ = std::fs::remove_file(&req_file);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reported_helper_error_is_terminal_on_the_first_attempt() {
        assert!(should_fail_now(FtFailure::Reported, 1, MAX_FT_ATTEMPTS));
        assert!(should_fail_now(FtFailure::Reported, 99, MAX_FT_ATTEMPTS));
    }

    #[test]
    fn a_helper_that_could_not_run_is_bounded_then_terminal() {
        assert!(!should_fail_now(FtFailure::Raw, 1, MAX_FT_ATTEMPTS));
        assert!(!should_fail_now(FtFailure::Raw, 2, MAX_FT_ATTEMPTS));
        assert!(should_fail_now(FtFailure::Raw, 3, MAX_FT_ATTEMPTS));
        // A zero budget must not make every job single-shot.
        assert!(!should_fail_now(FtFailure::Raw, 0, 0));
        assert!(should_fail_now(FtFailure::Raw, 1, 0));
    }

    #[test]
    fn a_retry_keeps_its_count_and_only_a_terminal_failure_clears_it() {
        // The counter is the only thing that makes the retry bounded: if a
        // retry path cleared it, attempt would read 1/3 forever (observed
        // live on job #6532 before this was fixed).
        clear_attempts(9001);
        let mut seen = Vec::new();
        for _ in 0..MAX_FT_ATTEMPTS {
            let n = bump_attempt(9001);
            seen.push(n);
            if should_fail_now(FtFailure::Raw, n, MAX_FT_ATTEMPTS) {
                clear_attempts(9001);
            }
        }
        assert_eq!(seen, vec![1, 2, 3]);
        assert_eq!(bump_attempt(9001), 1, "a terminal failure frees the slot");
        clear_attempts(9001);
    }

    #[test]
    fn the_verdict_is_the_last_json_line_even_after_trainer_progress_noise() {
        let stdout = concat!(
            "{'loss': 2.1, 'epoch': 0.5}\n",
            "{'loss': 2.0, 'epoch': 1.0}\n",
            "{\"ok\": false, \"error\": \"dataset has no completions\"}\n"
        );
        let v = helper_verdict(stdout, false, "").expect("a verdict was printed");
        assert_eq!(v["ok"], serde_json::json!(false));
        assert_eq!(v["error"], serde_json::json!("dataset has no completions"));
    }

    #[test]
    fn a_run_with_no_verdict_reports_the_tails_and_is_not_a_failure_verdict() {
        let v = helper_verdict(
            "starting…\ntraceback (most recent call last)\n",
            false,
            "CUDA OOM",
        );
        let msg = v.expect_err("no verdict must be an error, not ok:false");
        assert!(msg.contains("no verdict"), "{msg}");
        assert!(msg.contains("traceback"), "{msg}");
        assert!(msg.contains("CUDA OOM"), "{msg}");
    }

    #[test]
    fn a_parsed_json_line_without_an_ok_field_is_no_verdict() {
        assert!(helper_verdict("{\"loss\": 1.9}\n", true, "").is_err());
        assert!(helper_verdict("", true, "").is_err());
    }

    #[test]
    fn attempt_counters_advance_and_clear() {
        clear_attempts(4242);
        assert_eq!(bump_attempt(4242), 1);
        assert_eq!(bump_attempt(4242), 2);
        clear_attempts(4242);
        assert_eq!(bump_attempt(4242), 1);
        clear_attempts(4242);
    }
}

/// Spawn gpn_servemodel.py (detached) to register the tuned adapter as an
/// ollama model named `tuned-<job_id>`. Uses the same training template so the
/// served model answers in the format it was fine-tuned on.
fn spawn_servemodel(job_id: u64, base_model: &str, adapter_dir: &str) {
    let script =
        std::env::var("GPN_SERVEMODEL_SCRIPT").unwrap_or_else(|_| "gpn_servemodel.py".to_string());
    let req = json!({
        "base_model": base_model,
        "adapter_dir": adapter_dir,
        "name": format!("tuned-{}", job_id),
    });
    let req_file = gpn_agent::utils::temp_path(&format!("gpn_serve_req_{}.json", job_id));
    if let Ok(js) = serde_json::to_string(&req) {
        if let Err(e) = std::fs::write(&req_file, js) {
            eprintln!("[job {job_id}] servemodel: cannot write req: {e}");
            return;
        }
    } else {
        eprintln!("[job {job_id}] servemodel: serialization failed");
        return;
    }
    let script2 = script.clone();
    let req_file2 = req_file.clone();
    let name = format!("tuned-{job_id}");
    std::thread::spawn(move || {
        let out = std::process::Command::new("python3")
            .arg(&script2)
            .arg(&req_file2)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn();
        match out {
            Ok(mut child) => {
                // Detach: wait briefly; if still running, leave it (it will finish on its own).
                let _ = child.wait();
                println!(
                    "[job {job_id}] servemodel registration finished — model {name} (if ok:true)"
                );
            }
            Err(e) => eprintln!("[job {job_id}] servemodel spawn failed: {e}"),
        }
        let _ = std::fs::remove_file(&req_file2);
    });
}
