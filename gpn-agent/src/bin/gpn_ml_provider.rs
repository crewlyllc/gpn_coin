//! gpn_ml_provider — provider-side generic-ML-inference executor.
//!
//! Direct counterpart to `gpn_llm_provider` (chat) and `gpn_diffusion_provider`
//! (image/video): it gives the `MLInference` job type a *real* executor. Polls
//! the chain for `MLInference` jobs that are `Running` and assigned to this
//! signer, reads the off-chain input/model/task from the shared store, runs a
//! genuine model inference on the local ollama backend (raw completion via
//! `/api/generate`, or a vector via `/api/embeddings`), stores the result and
//! its sha256, and settles on-chain (`submit_result` then `complete_job`).
//!
//! The chain never sees the input or the result — only the hash.
//!
//! Usage:
//!   gpn_ml_provider [endpoint] [signer_uri] [poll_ms]
//! Env: ENDPOINT, SIGNER_URI, POLL_MS, OLLAMA_URL, GPN_ML_MODEL

use gpn_agent::chain::ChainClient;
use gpn_agent::llm::{normalize_ml_task, run_ml_inference, LlmJob, LlmStore, get_job};
use std::time::Duration;

/// How many trailing jobs to inspect for self-claim each pass. A full
/// `0..next` scan every poll would peg the RPC once the ledger grows; ML jobs
/// are only ever newly created, so a bounded window is enough.
const CLAIM_WINDOW: u64 = 64;

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
        "GPN ML-inference provider running — endpoint={} signer={} poll={}ms ollama={}",
        endpoint,
        me,
        poll_ms,
        gpn_agent::llm::infer::default_ollama_url()
    );

    loop {
        if let Err(e) = process_once(&client, &me).await {
            eprintln!("[provider] cycle error: {e}");
        }
        tokio::time::sleep(Duration::from_millis(poll_ms)).await;
    }
}

async fn process_once(client: &ChainClient, me: &str) -> Result<(), Box<dyn std::error::Error>> {
    let next = client.get_next_job_id().await;

    // 0. Self-claim Pending MLInference jobs. There is no AP/scheduler routing
    //    path for `MLInference` yet, so this provider is the only consumer: it
    //    accepts + starts any recent Pending ml job that has an off-chain
    //    payload here. `accept_job` is gated on-chain by the min_gflops
    //    attestation check, so an under-provisioned provider is rejected there.
    for id in next.saturating_sub(CLAIM_WINDOW)..next {
        if !is_ml_job(id) {
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

    // 1. Run ML jobs we own.
    for id in 0..next {
        let job = match get_job(id) {
            Some(j) if j.kind == "ml" => j,
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

fn is_ml_job(id: u64) -> bool {
    get_job(id).map(|j| j.kind == "ml").unwrap_or(false)
}

/// Resolve the requested model to one ollama actually has installed; fall back
/// to any installed model (preferring `llama3.2:1b`) so a job whose model isn't
/// present still COMPLETES instead of orphaning forever and saturating the
/// on-chain `MaxActiveJobs` cap.
fn resolve_model(requested: &str) -> String {
    if let Ok(forced) = std::env::var("GPN_ML_MODEL") {
        if !forced.trim().is_empty() {
            return forced;
        }
    }
    let base = gpn_agent::llm::infer::default_ollama_url();
    let installed: Vec<String> = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(2500))
        .build()
        .and_then(|c| c.get(format!("{base}/api/tags")).send())
        .ok()
        .and_then(|r| r.json::<serde_json::Value>().ok())
        .and_then(|v| {
            v["models"].as_array().map(|a| {
                a.iter()
                    .filter_map(|m| m["name"].as_str().map(String::from))
                    .collect()
            })
        })
        .unwrap_or_default();
    let req = requested.trim();
    if !req.is_empty() && req != "auto" {
        if let Some(exact) = installed.iter().find(|n| n.as_str() == req) {
            return exact.clone();
        }
        if let Some(tag) = installed.iter().find(|n| n.split(':').next() == Some(req)) {
            return tag.clone();
        }
        // Not installed: hand it to ollama anyway (it may resolve an alias).
        return req.to_string();
    }
    if installed.iter().any(|n| n == "llama3.2:1b") {
        return "llama3.2:1b".to_string();
    }
    installed
        .into_iter()
        .next()
        .unwrap_or_else(|| "llama3.2:1b".to_string())
}

async fn run_job(
    client: &ChainClient,
    id: u64,
    job: LlmJob,
) -> Result<(), Box<dyn std::error::Error>> {
    let task = normalize_ml_task(job.task.as_deref());
    let model = resolve_model(&job.model);
    println!(
        "[job {id}] ML inference — task={task} model={} (requested {}) input_len={}",
        model,
        job.model,
        job.prompt.len()
    );
    let _ = LlmStore::new().set_status(id, "running");

    match run_ml_inference(&model, &job.prompt, task, job.temperature, job.max_tokens).await {
        Ok((result_json, hash)) => {
            let _ = LlmStore::new().complete(id, &result_json, &hash, None);
            let mut hb = [0u8; 32];
            hb.copy_from_slice(&hex::decode(&hash)?);
            client.submit_result(id, hb, 0).await?;
            client.complete_job(id).await?;
            println!("[job {id}] done — task={task} hash {:.16}…", hash);
        }
        Err(e) => {
            let err = e.to_string();
            let _ = LlmStore::new().set_error(id, &err);
            eprintln!("[job {id}] ML inference failed: {err}");
        }
    }
    Ok(())
}
