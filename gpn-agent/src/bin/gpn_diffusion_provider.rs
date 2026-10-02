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

/// Path to the GPU duty-cycle yield flag the local miner watches.
fn gpu_yield_path() -> String {
    format!(
        "{}/.gpn/gpu-yield",
        std::env::var("HOME").unwrap_or_else(|_| ".".into())
    )
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
    let Some(m) = v.as_object() else { return vec![] };
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
            if p == me || p == addr { Some((id, model, pull)) } else { None }
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
    // Yield the GPU: drop the same flag the node miner checks so block mining
    // parks (duty cycle) instead of contending with the diffusion run.
    let yield_path = gpu_yield_path();
    let _ = std::fs::create_dir_all(
        std::path::Path::new(&yield_path).parent().unwrap_or(std::path::Path::new(".")),
    );
    let _ = std::fs::write(&yield_path, b"1");
    let script = diffuse_script();
    let req_file2 = req_file.clone();
    let out_res = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
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
    .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&yield_path);
    let out = out_res?;

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
