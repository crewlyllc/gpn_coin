//! gpn_llm_provider — the provider-side LLM executor for the GPN marketplace.
//!
//! Polls the chain for `LLMInference` jobs that are `Running` and assigned to
//! this signer, reads the off-chain prompt/model from the shared LLM store,
//! runs inference against the local ollama server (`:11434`), writes the
//! response back to the store, and settles the job on-chain:
//! `submit_result(sha256(response))` then `complete_job` (escrow release).
//!
//! This is the "bridge" between the chain (coordination/escrow) and the actual
//! model inference (off-chain). The chain never sees the prompt or response —
//! only the result hash.
//!
//! Usage:
//!   gpn_llm_provider [endpoint] [signer_uri] [poll_ms]
//! Env overrides: ENDPOINT, SIGNER_URI, POLL_MS, OLLAMA_URL.

use gpn_agent::chain::signer::Sr25519Signer;
use gpn_agent::chain::ChainClient;
use gpn_agent::llm::{get_job, run_inference, set_response, LlmStore};
use std::time::Duration;

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
        .unwrap_or(5000);

    let client = ChainClient::new(&endpoint).await?.with_signer(&signer_uri);
    let (me, addr) = {
        use subxt::tx::Signer as _;
        let s = Sr25519Signer::from_uri(&signer_uri)?;
        // get_job_state returns the provider as 0x-hex; the attestation lookup
        // parses an ss58/hex address string.
        let aid = s.account_id();
        let b: &[u8] = aid.as_ref();
        (format!("0x{}", hex::encode(b)), aid.to_string())
    };

    println!(
        "GPN LLM provider running — endpoint={} signer={} poll={}ms (inference via {})",
        endpoint,
        me,
        poll_ms,
        gpn_agent::llm::infer::default_ollama_url()
    );

    // Guaranteed baseline: every node ships a starter model set so the network
    // always has at least one usable LLM, even before any demand-driven pulls.
    let baseline = std::env::var("GPN_BASELINE_MODELS")
        .unwrap_or_else(|_| "llama3.2:1b,qwen2.5:0.5b".to_string());
    for m in baseline.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match ensure_model(m) {
            Ok(note) => println!("[provider] baseline: {note}"),
            Err(e) => eprintln!("[provider] baseline {m}: {e}"),
        }
    }

    loop {
        if let Err(e) = process_once(&client, &me, &addr).await {
            eprintln!("[provider] cycle error: {e}");
        }
        tokio::time::sleep(Duration::from_millis(poll_ms)).await;
    }
}

/// Resolve a requested model to a concrete one. `auto`/empty means "pick any
/// model this machine has" — choose the first from the local Ollama, falling
/// back to the guaranteed baseline model.
fn resolve_model(requested: &str) -> String {
    if !requested.is_empty() && requested != "auto" {
        return requested.to_string();
    }
    let base = gpn_agent::llm::infer::default_ollama_url();
    if let Ok(r) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(2500))
        .build()
        .and_then(|c| c.get(format!("{base}/api/tags")).send())
    {
        if let Ok(v) = r.json::<serde_json::Value>() {
            if let Some(name) = v["models"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|m| m["name"].as_str())
            {
                return name.to_string();
            }
        }
    }
    "llama3.2:1b".to_string()
}

fn assignment_store_path() -> String {
    std::env::var("GPN_ASSIGN_STORE").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.gpn/assignments.json")
    })
}

/// Persistent per-job inference-failure counter. We give up after a small
/// number of attempts so a mis-routed job (e.g. an image model like `sd-turbo`
/// that can never run as a chat completion) can't spin the provider forever.
fn failed_store_path() -> String {
    std::env::var("GPN_LLM_FAILED_STORE").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.gpn/llm-failed.json")
    })
}

/// Max inference attempts before the provider abandons a job.
const MAX_INFER_ATTEMPTS: u64 = 2;

fn fail_count(id: u64) -> u64 {
    std::fs::read_to_string(failed_store_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get(id.to_string()).and_then(|x| x.as_u64()))
        .unwrap_or(0)
}

fn record_failure(id: u64) -> u64 {
    let path = failed_store_path();
    let mut v: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let n = v.get(id.to_string()).and_then(|x| x.as_u64()).unwrap_or(0) + 1;
    if let Some(obj) = v.as_object_mut() {
        obj.insert(id.to_string(), serde_json::json!(n));
    }
    let _ = std::fs::write(&path, v.to_string());
    n
}

/// Should we (still) attempt inference for this job? False once we've hit the
/// attempt cap, or when the requested model is a known non-text kind.
fn should_attempt(id: u64, model: &str) -> bool {
    if fail_count(id) >= MAX_INFER_ATTEMPTS {
        return false;
    }
    // Fast-path reject: image/video/audio diffusion models can't serve chat.
    let m = model.to_ascii_lowercase();
    !(m.contains("sd-")
        || m.contains("diffusion")
        || m.contains("sdxl")
        || m.contains("flux")
        || m.contains("stable-diffusion"))
}

/// Job ids the access-point scheduler assigned to this provider (matched by
/// either the hex or ss58 form of our account), plus the requested model +
/// whether we must pull it on demand. Empty = no scheduler in play.
/// Handles both the legacy string form and the `{provider,model,pull}` object.
fn read_assigned(me: &str, addr: &str) -> Vec<(u64, String, bool)> {
    let path = assignment_store_path();
    let Ok(s) = std::fs::read_to_string(&path) else {
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
                    pv.get("provider").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    pv.get("model").and_then(|x| x.as_str()).unwrap_or("").to_string(),
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

/// Ensure `model` is present locally; if not, pull it from Ollama / HuggingFace.
/// Returns a human-readable note about what happened.
fn ensure_model(model: &str) -> Result<String, String> {
    if model.is_empty() || model == "auto" {
        return Ok("no specific model requested".into());
    }
    let base = gpn_agent::llm::infer::default_ollama_url();
    let cli = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3600))
        .build()
        .map_err(|e| e.to_string())?;
    // 1. already present?
    if let Ok(r) = cli.get(format!("{base}/api/tags")).send() {
        if let Ok(v) = r.json::<serde_json::Value>() {
            let have = v["models"]
                .as_array()
                .map(|a| a.iter().any(|m| {
                    let n = m["name"].as_str().unwrap_or("");
                    n == model || n.split(':').next() == Some(model)
                }))
                .unwrap_or(false);
            if have {
                return Ok(format!("model {model} already present"));
            }
        }
    }
    // 2. pull it on demand (Ollama registry). For a raw HF GGUF the operator
    //    should use `gpn_pull_model gguf <repo> <file>`, but Ollama-library
    //    models (and many HF repos) resolve via `ollama pull` directly.
    println!("[provider] model {model} missing — pulling on demand…");
    let r = cli
        .post(format!("{base}/api/pull"))
        .json(&serde_json::json!({"name": model, "stream": false}))
        .send()
        .map_err(|e| format!("pull {model} failed: {e}"))?;
    if r.status().is_success() {
        Ok(format!("pulled model {model} on demand"))
    } else {
        Err(format!("pull {model} http {}: {}", r.status(), r.text().unwrap_or_default()))
    }
}

/// True if the off-chain store says job `id` is an LLM (text) job. Unknown
/// jobs are treated as non-LLM so we never pull/handle a foreign kind.
fn is_llm_id(id: u64) -> bool {
    get_job(id).map(|j| j.kind == "llm").unwrap_or(false)
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
    for id in 0..next {
        if let Some((state, _price, _req, provider)) = client.get_job_state(id).await {
            if provider.as_deref() != Some(me) {
                continue;
            }
            if state == "AwaitingVerification" {
                match client.complete_job(id).await {
                    Ok(()) => {
                        println!("[provider] reconciled #{id} AwaitingVerification → Completed")
                    }
                    Err(e) => eprintln!("[provider] reconcile #{id} (complete) failed: {e}"),
                }
            } else if state == "Accepted" && is_llm_id(id) {
                // Capacity reclaim: an `Accepted` LLM job we own that was never
                // started is a zombie (its start tx never landed) permanently
                // holding one of our `MaxJobsPerProvider` slots → every later
                // accept fails `MaxProviderCapacity`. Since our claim loop only
                // *starts* `Pending` jobs, an `Accepted` job can never advance on
                // its own — drive it Accepted → Running → Completed to free the
                // slot (escrow is refunded by `complete_job`).
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

    // 0. Auto-claim: if an access-point scheduler has assigned us specific jobs,
    //    claim ONLY those; otherwise (standalone) claim any claimable LLM job.
    let assigned = read_assigned(me, addr);
    let targets: Vec<u64> = if assigned.is_empty() {
        let attested_gflops = client
            .get_attestation_record(addr)
            .await
            .map(|(_, f, _)| f)
            .unwrap_or(0);
        client
            .list_claimable_jobs(attested_gflops)
            .await
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    } else {
        // Model-aware: pull any requested-but-missing model BEFORE claiming.
        // Only LLM jobs: ignore image/video jobs the scheduler may have (mis)assigned,
        // otherwise we spam `ollama pull` for foreign models (e.g. sd-turbo) forever.
        // Also skip finished jobs and `auto`/empty requests (nothing concrete to pull).
        for (id, model, pull) in &assigned {
            let Some(job) = get_job(*id) else { continue };
            if job.kind != "llm" || job.status == "done" {
                continue;
            }
            if model.is_empty() || model == "auto" {
                continue;
            }
            let _ = pull;
            if let Err(e) = ensure_model(model) {
                eprintln!("[provider] job #{id}: {e}");
            }
        }
        assigned
            .iter()
            .filter(|(id, _, _)| is_llm_id(*id))
            .map(|(id, _, _)| *id)
            .collect()
    };
    for job_id in targets {
        if !get_job(job_id).map(|j| j.kind == "llm").unwrap_or(false) {
            continue;
        }
        // Only claim a Pending job — skip ones already accepted/started (avoids
        // the harmless-but-noisy `JobNotPending` error on re-claims).
        if let Some((state, _, _, _)) = client.get_job_state(job_id).await {
            if state != "Pending" {
                continue;
            }
        }
        println!("[provider] claiming LLM job #{job_id}");
        if let Err(e) = client.accept_job(job_id).await {
            eprintln!("[provider] accept #{job_id} failed: {e}");
            continue;
        }
        if let Err(e) = client.start_job(job_id).await {
            eprintln!("[provider] start #{job_id} failed: {e}");
        }
    }

    // 1. Run any Running LLM job assigned to this signer.
    let next = client.get_next_job_id().await;
    for id in 0..next {
        // Only jobs that have an off-chain LLM payload are LLM jobs.
        let job = match get_job(id) {
            Some(j) if j.kind == "llm" => j,
            _ => continue,
        };
        if let Some((state, _price, _req, provider)) = client.get_job_state(id).await {
            let is_mine = provider.as_deref() == Some(me);
            if state == "Running" && is_mine {
                if !should_attempt(id, &job.model) {
                    // Already failed too many times, or a foreign (non-text)
                    // model that can never run here — stop hammering it.
                    continue;
                }
                run_job(client, id, &job).await?;
            }
        }
    }
    Ok(())
}

/// A fresh random salt (hex). Uses time + a process counter + pid as entropy
/// (no external RNG crate); collisions are irrelevant for privacy here.
fn random_salt() -> String {
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let n = CTR.fetch_add(1, Ordering::Relaxed);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = Sha256::new();
    h.update(b"gpn-salt");
    h.update(t.to_le_bytes());
    h.update(n.to_le_bytes());
    h.update(std::process::id().to_le_bytes());
    let d = h.finalize();
    hex::encode(&d[..16])
}

/// sha256(salt || payload) — the on-chain commitment.
fn salted_commit(salt: &str, payload: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(salt.as_bytes());
    h.update(payload.as_bytes());
    let mut o = [0u8; 32];
    o.copy_from_slice(&h.finalize());
    o
}

async fn run_job(
    client: &ChainClient,
    id: u64,
    job: &gpn_agent::llm::LlmJob,
) -> Result<(), Box<dyn std::error::Error>> {
    let job = job.clone();
    let model = resolve_model(&job.model);
    let preview: String = job.prompt.chars().take(60).collect();
    println!(
        "[job {id}] running inference — model={} prompt=\"{}\"",
        model, preview
    );

    let _ = LlmStore::new().set_status(id, "running");
    // Yield the GPU to this job: drop the flag the node's miner checks, so block
    // mining parks (duty cycle) instead of contending with the inference.
    let yield_path = format!(
        "{}/.gpn/gpu-yield",
        std::env::var("HOME").unwrap_or_else(|_| ".".into())
    );
    let _ = std::fs::create_dir_all(
        std::path::Path::new(&yield_path).parent().unwrap_or(std::path::Path::new(".")),
    );
    let _ = std::fs::write(&yield_path, b"1");
    let result = run_inference(&model, &job.prompt, job.temperature, job.max_tokens).await;
    let _ = std::fs::remove_file(&yield_path);
    match result {
        Ok((text, hash)) => {
            // Salted commitment: on-chain value = sha256(salt ‖ answer). The salt is
            // random + kept OFF-chain, so the on-chain value is opaque to observers
            // but anyone holding the salt can verify the answer. (A bare
            // sha256(answer) would leak short answers by brute force.)
            let salt = random_salt();
            let commit = salted_commit(&salt, &text);
            let commit_hex = format!("0x{}", hex::encode(commit));
            let _ = set_response(id, &text, &commit_hex, Some(&salt));
            // Rough output-token meter for per-token billing (chars/4 heuristic).
            let est_tokens: u32 = ((text.chars().count() as u32) / 4).max(1);
            client.submit_result(id, commit, est_tokens).await?;
            client.complete_job(id).await?;
            println!(
                "[job {id}] done — response {} chars, salted commitment {:.16}… (salt off-chain)",
                text.len(),
                commit_hex
            );
            let _ = hash; // the bare hash stays off-chain too (informational)
        }
        Err(e) => {
            let _ = LlmStore::new().set_error(id, &e.to_string());
            let n = record_failure(id);
            eprintln!(
                "[job {id}] inference failed (attempt {n}/{MAX_INFER_ATTEMPTS}), giving up after cap: {e}"
            );
        }
    }
    Ok(())
}
