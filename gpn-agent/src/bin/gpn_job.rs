//! gpn_job — client-side AI job submitter.
//!
//! Signs the `create_job` transaction with **your own wallet** and submits it
//! straight to the chain, then asks an access point to ROUTE it to a provider.
//! The access point never holds your key and never pays — it only schedules.
//!
//! Usage:
//!   gpn_job --wallet <seed> --prompt "..." [--model llama3.2:1b]
//!           [--price 10] [--min-gflops 150] [--job-type LLMInference]
//!           [--endpoint ws://127.0.0.1:9944] [--scheduler http://127.0.0.1:8092]
//! Env: GPN_WALLET, GPN_ENDPOINT, GPN_SCHEDULER

use gpn_agent::chain::ChainClient;

fn opt(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let wallet = opt(&args, "--wallet")
        .or_else(|| std::env::var("GPN_WALLET").ok())
        .ok_or("set --wallet <secret-seed> (your wallet)")?;
    let prompt = opt(&args, "--prompt").ok_or("set --prompt \"...\"")?;
    let price_gpn: f64 = opt(&args, "--price")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10.0);
    let min_gflops: u64 = opt(&args, "--min-gflops")
        .and_then(|s| s.parse().ok())
        .unwrap_or(150);
    let model_arg = opt(&args, "--model");
    // Item #76: the kind comes from the shared classifier, so a generation model
    // submitted WITHOUT `--job-type` (the default used to be LLMInference) is
    // still a generation job — on chain, in the off-chain payload, and at the
    // provider. Deriving it from `--job-type` alone created an `LLMInference`
    // job with an LLM payload for `sdxl-turbo`, which the diffusion provider
    // would not claim and the LLM provider could not run: `Running` forever.
    let kind = match gpn_agent::jobkind::admit(
        opt(&args, "--job-type").as_deref(),
        model_arg.as_deref().unwrap_or(""),
    ) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("❌ {}", e.message());
            std::process::exit(2);
        }
    };
    let job_type = kind.onchain_name().to_string();
    // Generative (image/video) params — only used when the job is a render.
    let gen_kind: Option<&'static str> = if kind.is_generation() {
        Some(kind.payload_kind())
    } else {
        None
    };
    let negative = opt(&args, "--negative-prompt");
    let width: u32 = opt(&args, "--width")
        .and_then(|s| s.parse().ok())
        .unwrap_or(512);
    let height: u32 = opt(&args, "--height")
        .and_then(|s| s.parse().ok())
        .unwrap_or(512);
    let steps: u32 = opt(&args, "--steps")
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    let seed: Option<i64> = opt(&args, "--seed").and_then(|s| s.parse().ok());
    let frames: u32 = opt(&args, "--frames")
        .and_then(|s| s.parse().ok())
        .unwrap_or(16);
    let fps: u32 = opt(&args, "--fps")
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    // Diffusion model default differs from the LLM fallback.
    let model = match (gen_kind, model_arg) {
        (Some(_), None) => "sd-turbo".to_string(),
        (_, Some(m)) => m,
        (None, None) => "llama3.2:1b".to_string(),
    };
    let endpoint = opt(&args, "--endpoint")
        .or_else(|| std::env::var("GPN_ENDPOINT").ok())
        .unwrap_or_else(|| "ws://127.0.0.1:9944".into());
    let scheduler = opt(&args, "--scheduler")
        .or_else(|| std::env::var("GPN_SCHEDULER").ok())
        .unwrap_or_else(|| "http://127.0.0.1:8092".into());

    // Sign + submit WITH THE CLIENT'S WALLET (not a server hot key).
    let client = ChainClient::new(&endpoint).await?.with_signer(&wallet);

    let workload_hash = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(prompt.as_bytes());
        h.update(model.as_bytes());
        let mut o = [0u8; 32];
        o.copy_from_slice(&h.finalize());
        o
    };
    println!("== gpn_job ==");
    println!("  endpoint:  {endpoint}");
    println!("  scheduler: {scheduler}");
    println!(
        "  prompt:    \"{}\"  model={model}  price={price_gpn} GPN",
        prompt
    );

    client
        .create_job(
            8192,
            75,
            1,
            0,
            min_gflops,
            &job_type,
            "Spot",
            (price_gpn * 1e12) as u128,
            0,
            0,
            1000,
            workload_hash,
        )
        .await?;
    let id = client.get_next_job_id().await.saturating_sub(1);
    println!("  ✅ job #{id} created + escrowed from YOUR wallet");

    // Store the off-chain prompt/model so the provider can read it.
    // The payload kind must agree with the kind that was written on chain and
    // with the provider that will claim it (item #76).
    let job = match kind {
        k if k.is_generation() => gpn_agent::llm::LlmJob::new_gen(
            k.payload_kind(),
            &model,
            &prompt,
            negative.as_deref(),
            width,
            height,
            steps,
            seed,
            frames,
            fps,
        ),
        gpn_agent::jobkind::JobKind::Ml => gpn_agent::llm::LlmJob::new_ml(
            &model,
            &prompt,
            gpn_agent::llm::normalize_ml_task(opt(&args, "--task").as_deref()),
            0.7,
            256,
        ),
        _ => gpn_agent::llm::LlmJob::new_llm(&model, &prompt, 0.7, 256),
    };
    let _ = gpn_agent::llm::LlmStore::new().put(id, job);

    // Ask the access point to route it to the best provider.
    match reqwest::blocking::Client::new()
        .post(format!("{scheduler}/route/{id}"))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .and_then(|r| r.text())
    {
        Ok(t) => println!("  ✅ routed: {t}"),
        Err(e) => println!("  ⚠️ routing request failed ({e}); the job is on-chain and a provider may still claim it"),
    }

    println!("  poll: curl {scheduler}/job/{id}");
    Ok(())
}
