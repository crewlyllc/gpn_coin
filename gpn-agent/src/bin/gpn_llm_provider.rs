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

    loop {
        if let Err(e) = process_once(&client, &me, &addr).await {
            eprintln!("[provider] cycle error: {e}");
        }
        tokio::time::sleep(Duration::from_millis(poll_ms)).await;
    }
}

fn assignment_store_path() -> String {
    std::env::var("GPN_ASSIGN_STORE").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.gpn/assignments.json")
    })
}

/// Job ids the access-point scheduler assigned to this provider (matched by
/// either the hex or ss58 form of our account). Empty = no scheduler in play.
fn read_assigned(me: &str, addr: &str) -> Vec<u64> {
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
        .filter(|(_, pv)| {
            let p = pv.as_str().unwrap_or("");
            p == me || p == addr
        })
        .filter_map(|(k, _)| k.parse::<u64>().ok())
        .collect()
}

async fn process_once(
    client: &ChainClient,
    me: &str,
    addr: &str,
) -> Result<(), Box<dyn std::error::Error>> {
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
        assigned
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
                run_job(client, id, &job).await?;
            }
        }
    }
    Ok(())
}

async fn run_job(
    client: &ChainClient,
    id: u64,
    job: &gpn_agent::llm::LlmJob,
) -> Result<(), Box<dyn std::error::Error>> {
    let job = job.clone();
    let preview: String = job.prompt.chars().take(60).collect();
    println!(
        "[job {id}] running inference — model={} prompt=\"{}\"",
        job.model, preview
    );

    let _ = LlmStore::new().set_status(id, "running");
    match run_inference(&job.model, &job.prompt, job.temperature, job.max_tokens).await {
        Ok((text, hash)) => {
            let _ = set_response(id, &text, &hash);
            let mut hb = [0u8; 32];
            hb.copy_from_slice(&hex::decode(&hash)?);
            client.submit_result(id, hb).await?;
            client.complete_job(id).await?;
            println!(
                "[job {id}] done — response {} chars, result_hash {:.16}…",
                text.len(),
                hash
            );
        }
        Err(e) => {
            let _ = LlmStore::new().set_error(id, &e.to_string());
            eprintln!("[job {id}] inference failed: {e}");
        }
    }
    Ok(())
}
