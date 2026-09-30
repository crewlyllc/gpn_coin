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
use std::path::PathBuf;
use std::time::Duration;

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

    loop {
        if let Err(e) = process_once(&client, &me).await {
            eprintln!("[provider] cycle error: {e}");
        }
        tokio::time::sleep(Duration::from_millis(poll_ms)).await;
    }
}

async fn process_once(client: &ChainClient, me: &str) -> Result<(), Box<dyn std::error::Error>> {
    let next = client.get_next_job_id().await;
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
    let out = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
        let out = std::process::Command::new("python3")
            .arg(&script)
            .arg(&req_file2)
            .output()
            .map_err(|e| format!("spawn python3: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() {
            return Err(format!("finetune failed: {}", stdout.trim()));
        }
        // transformers Trainer prints progress dicts to stdout; the final JSON
        // (our print) is the last non-empty line. Parse that.
        let last_json = stdout
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        serde_json::from_str(last_json.trim())
            .map_err(|e| format!("bad helper output: {e} ({stdout})"))
    })
    .await
    .map_err(|e| e.to_string())??;

    if out.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
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
    } else {
        let err = out["error"]
            .as_str()
            .unwrap_or("unknown finetune error")
            .to_string();
        let _ = LlmStore::new().set_error(id, &err);
        eprintln!("[job {id}] finetune failed: {err}");
    }
    let _ = std::fs::remove_file(&req_file);
    Ok(())
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
