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
use std::path::PathBuf;
use std::time::Duration;

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
    let me = {
        use subxt::tx::Signer as _;
        let s = gpn_agent::chain::signer::Sr25519Signer::from_uri(&signer_uri)?;
        let aid = s.account_id();
        let b: &[u8] = aid.as_ref();
        format!("0x{}", hex::encode(b))
    };

    println!(
        "GPN diffusion provider running — endpoint={} signer={} poll={}ms script={}",
        endpoint,
        me,
        poll_ms,
        diffuse_script().display()
    );

    loop {
        if let Err(e) = process_once(&client, &me).await {
            eprintln!("[provider] cycle error: {e}");
        }
        tokio::time::sleep(Duration::from_millis(poll_ms)).await;
    }
}

fn is_gen(job: &gpn_agent::llm::LlmJob) -> bool {
    job.kind == "image" || job.kind == "video"
}

async fn process_once(client: &ChainClient, me: &str) -> Result<(), Box<dyn std::error::Error>> {
    let next = client.get_next_job_id().await;
    for id in 0..next {
        let job = match get_job(id) {
            Some(j) if is_gen(&j) => j,
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

    let req_json = json!({
        "kind": job.kind,
        "model": job.model,
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
    let script = diffuse_script();
    let req_file2 = req_file.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
        let out = std::process::Command::new("python3")
            .arg(&script)
            .arg(&req_file2)
            .output()
            .map_err(|e| format!("spawn python3: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() {
            return Err(format!("diffusers failed: {}", stdout.trim()));
        }
        serde_json::from_str(&stdout).map_err(|e| format!("bad helper output: {e} ({stdout})"))
    })
    .await
    .map_err(|e| e.to_string())??;

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
        client.submit_result(id, hb).await?;
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
