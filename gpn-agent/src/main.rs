//! # GPU Node Agent
//!
//! The gpu-node-agent runs alongside each GPU provider's node.
//! It monitors GPU hardware, manages mining/compute/staking modes,
//! generates attestation proofs, and implements auto-switching logic.
//!
//! ## Modes
//! - **Mining**: Runs Ethash PoW miner on GPU
//! - **Compute**: Serves AI inference/training jobs via Docker
//! - **Stake Only**: GPU idle, only staking rewards
//! - **Auto**: Dynamically switches based on profitability
//!
//! ## Architecture
//! ```text
//! gpu-node-agent
//! ├── GPU Monitor (nvidia-smi / rocm-smi)
//! ├── Mining Engine (Ethash ethminer process)
//! ├── Compute Executor (Docker / Ray.io)
//! ├── Attestation Generator (challenge-response)
//! └── Profitability Calculator (auto-switch decision)
//! ```

use gpn_agent::{
    chain, compute::ComputeConfig, config, gpu, mining::MiningConfig, modes, profitability,
    AgentState, Mode,
};
use sha2::Digest;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Main agent struct
pub struct GPUMiningNodeAgent {
    config: config::AgentConfig,
    state: Arc<RwLock<AgentState>>,
    chain_client: chain::ChainClient,
    gpu_monitor: gpu::GPUMonitor,
    mode_manager: modes::ModeManager,
    profitability_calc: profitability::ProfitabilityCalculator,
}

impl GPUMiningNodeAgent {
    /// Create a new agent from a config file path.
    pub async fn new(config_path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        Self::with_config(config::AgentConfig::from_file(config_path)?).await
    }

    /// Create a new agent from an already-loaded configuration.
    pub async fn with_config(
        config: config::AgentConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let chain_client = chain::ChainClient::new(&config.chain_endpoint)
            .await?
            .with_signer(&config.signer_uri);
        let gpu_monitor = gpu::GPUMonitor::new()?;
        let account = chain_client
            .get_account_info(&config.account_address)
            .await?;

        let state = Arc::new(RwLock::new(AgentState {
            current_mode: Mode::Mining,
            gpu_info: gpu_monitor.get_gpu_info()?,
            account,
            last_profitability: None,
            is_running: true,
        }));

        let mode_manager = modes::ModeManager::new(
            state.clone(),
            MiningConfig {
                rpc_url: config.mining_rpc_url.clone(),
                device_index: config.mining_device_index,
                batch: 1 << 20,
                poll_ms: 2000,
                signer_uri: config.signer_uri.clone(),
            },
            ComputeConfig {
                device_index: config.compute_device_index,
                matrix_size: config.compute_matrix_size,
                batch: config.compute_batch,
            },
        )?;

        let profitability_calc = profitability::ProfitabilityCalculator::new(
            config.clone(),
            chain_client.clone(),
            gpu_monitor.clone(),
        );

        Ok(Self {
            config,
            state,
            chain_client,
            gpu_monitor,
            mode_manager,
            profitability_calc,
        })
    }

    /// Main agent loop
    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        log::info!("GPU Node Agent starting in {:?} mode", self.config.mode);

        // Enter the configured mode on boot so mining actually launches.
        let boot_mode = match self.config.mode.as_str() {
            "compute" => Mode::Compute,
            "stake_only" => Mode::StakeOnly,
            "mine" => Mode::Mining,
            // "auto" and anything else start mining, then let the
            // profitability oracle switch as needed.
            _ => Mode::Mining,
        };
        self.mode_manager.boot(boot_mode).await?;
        {
            let mut state = self.state.write().await;
            state.current_mode = boot_mode;
        }
        log::info!(
            "Booted into {:?} mode (mining={}, computing={})",
            boot_mode,
            self.mode_manager.is_mining(),
            self.mode_manager.is_computing()
        );

        // Periodic operations
        let mut attestation_interval = tokio::time::interval(std::time::Duration::from_secs(
            self.config.attestation_interval_secs,
        ));
        let mut profitability_interval = tokio::time::interval(std::time::Duration::from_secs(
            self.config.profitability_check_interval_secs,
        ));
        let mut health_check_interval = tokio::time::interval(
            std::time::Duration::from_secs(60), // Every minute
        );

        loop {
            tokio::select! {
                _ = attestation_interval.tick() => {
                    if let Err(e) = self.perform_attestation().await {
                        log::error!("Attestation failed: {}", e);
                    }
                }
                _ = profitability_interval.tick() => {
                    if self.config.mode == "auto" {
                        if let Err(e) = self.check_profitability().await {
                            log::error!("Profitability check failed: {}", e);
                        }
                        // Close the loop: when the oracle says compute demand is
                        // high (Compute is the current/recommended mode), actually
                        // claim & serve marketplace jobs on-chain instead of just
                        // running a background GEMM.
                        let serving = self.state.read().await.current_mode == Mode::Compute;
                        if serving {
                            if let Err(e) = self.claim_and_serve_jobs().await {
                                log::error!("Marketplace job serving failed: {}", e);
                            }
                        }
                    }
                }
                _ = health_check_interval.tick() => {
                    self.perform_health_check().await;
                }
            }

            // Check if agent should stop
            if !self.state.read().await.is_running {
                break;
            }
        }

        Ok(())
    }

    /// Perform GPU attestation challenge-response
    async fn perform_attestation(&self) -> Result<(), Box<dyn std::error::Error>> {
        log::debug!("Fetching attestation challenge...");

        // The pallet requires the provider to have staked before attesting
        // (`GPUAttestation::deposit_stake`, `MinAttestationStake` = 1,000 GPN).
        // Deposit once up-front so a fresh provider can attest on first boot;
        // a subsequent duplicate deposit fails with `AlreadyAttested` and is
        // ignored (attestation is what really matters).
        let stake = self.config.min_attestation_stake;
        if stake > 0 {
            match self.chain_client.deposit_stake(stake).await {
                Ok(()) => log::info!("Provider stake ensured ({} GPN)", stake),
                Err(e) => log::warn!("Stake deposit skipped: {} (proceeding to attest)", e),
            }
        }

        // Get current challenge from chain
        let challenge = self.chain_client.get_attestation_challenge().await?;

        // Run challenge on GPU
        log::info!("Computing attestation proof on GPU...");
        let result = self.gpu_monitor.compute_challenge(&challenge).await?;

        // Build GPU info hash
        let gpu_info = self.state.read().await.gpu_info.clone();
        let gpu_info_hash = gpu::hash_gpu_info(&gpu_info);
        let gpu_count = 1u8; // TODO: read actual GPU count from telemetry

        // Measure real compute throughput (GFLOPS) on the GPU — this is the
        // on-chain attestable metric proving real AI-compute capability.
        //
        // Item 5: never let a failed measurement become a SILENT 0. A 0 here is
        // written on-chain as `compute_gflops = 0`, which makes the provider
        // ineligible for every `min_gflops > 0` job (and puts it in the lowest
        // attestation tier) — so say exactly why it happened.
        let gflops = match gpu::measure_gflops(self.config.mining_device_index) {
            Ok(g) => g,
            Err(e) => {
                log::warn!(
                    "⚠ GPU compute measurement FAILED ({e}) — attesting gflops=0. This provider \
                     will only be routed jobs with min_gflops=0 and will not reach a paid \
                     attestation tier. Fix the OpenCL/CUDA device (device index \
                     {}) or set GFLOPS explicitly.",
                    self.config.mining_device_index
                );
                0
            }
        };
        if gflops == 0 {
            log::warn!(
                "⚠ measured 0 GFLOPS — the GEMM benchmark returned no throughput (GPU busy, \
                 driver mismatch, or OpenCL runtime missing)"
            );
        }
        log::info!("Measured GPU compute: {} GFLOPS (FP32)", gflops);

        // Submit attestation to chain
        self.chain_client
            .submit_attestation(result, gpu_info_hash, gpu_count, gflops)
            .await?;

        log::info!("GPU attestation submitted successfully");
        Ok(())
    }

    /// Check profitability and switch modes if needed (auto mode)
    async fn check_profitability(&self) -> Result<(), Box<dyn std::error::Error>> {
        let analysis = self.profitability_calc.analyze().await?;

        // Store analysis
        {
            let mut state = self.state.write().await;
            state.last_profitability = Some(analysis.clone());
        }

        log::info!(
            "Profitability: Mining=${:.4}/hr, Compute=${:.4}/hr, Staking=${:.4}/hr",
            analysis.mining_revenue_per_hour,
            analysis.compute_revenue_per_hour,
            analysis.staking_revenue_per_hour,
        );

        // Decide if switch is needed
        let current_mode = self.state.read().await.current_mode;
        let recommended = analysis.recommended_mode();

        if recommended != current_mode {
            log::info!("🔄 Switching mode: {:?} → {:?}", current_mode, recommended);

            // Check hysteresis threshold
            if self
                .profitability_calc
                .should_switch(&analysis, current_mode)
            {
                self.mode_manager.switch_to(recommended).await?;

                let mut state = self.state.write().await;
                state.current_mode = recommended;
                log::info!("✅ Mode switch complete: now {:?}", recommended);
            }
        }

        Ok(())
    }

    /// Close the loop: when the agent is serving AI compute (auto mode picked
    /// Compute, or compute demand is high), actually CLAIM pending marketplace
    /// jobs on-chain and serve them with the real GPU — accept -> start -> run
    /// a real GEMM -> submit_result -> complete (payout). This is what ties the
    /// profitability oracle -> marketplace -> agent together end-to-end.
    ///
    /// Safe to call repeatedly: it only claims jobs this provider can satisfy
    /// and that are still Pending, and it skips if not registered/attested.
    async fn claim_and_serve_jobs(&self) -> Result<usize, Box<dyn std::error::Error>> {
        // Resolve the signer's address (the account that signs marketplace txs).
        use subxt::tx::Signer as _;
        let signer = chain::signer::Sr25519Signer::from_uri(&self.config.signer_uri)?;
        let addr = signer.account_id().to_string();

        // 1. Provider must be registered. Register idempotently if not.
        if !self.chain_client.is_provider_registered(&addr).await {
            log::info!("registering as marketplace provider ({}...)", &addr[..10]);
            // Advertise the attested compute tier so the marketplace can match us.
            let attested = self.chain_client.get_attestation_record(&addr).await;
            let (gpus, gflops) = attested.map(|(g, f, _)| (g, f)).unwrap_or((1, 0));
            self.chain_client
                .register_provider(8_000, 75, gpus, 0, gflops)
                .await?;
        }

        // 2. Provider must be attested to take gated jobs.
        let attested_gflops = self
            .chain_client
            .get_attestation_record(&addr)
            .await
            .map(|(_, f, _)| f)
            .unwrap_or(0);

        // 3. Find pending jobs we can actually satisfy.
        let claimable = self.chain_client.list_claimable_jobs(attested_gflops).await;
        if attested_gflops == 0 {
            // Item 5: gflops=0 silently narrows the marketplace to `min_gflops=0`
            // jobs only. Say it once, with the reason, instead of looking idle.
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                log::warn!(
                    "attested compute is 0 GFLOPS — every marketplace job with min_gflops > 0 \
                     will be skipped by list_claimable_jobs. Re-run attestation once the GPU \
                     measurement works (see the 'GPU compute measurement FAILED' line above)."
                );
            });
        }
        if claimable.is_empty() {
            log::debug!(
                "no claimable marketplace jobs (attested {} GFLOPS)",
                attested_gflops
            );
            return Ok(0);
        }
        log::info!(
            "{} claimable job(s) found (attested {} GFLOPS)",
            claimable.len(),
            attested_gflops
        );

        let mut served = 0usize;
        for (job_id, min_gflops) in claimable {
            log::info!("serving job #{} (min_gflops={})", job_id, min_gflops);
            // accept -> start
            self.chain_client.accept_job(job_id).await?;
            log::info!("  accepted job #{}", job_id);
            self.chain_client.start_job(job_id).await?;
            log::info!("  started job #{}", job_id);

            // Run a REAL dense GEMM on the GPU and hash its output -> result_hash.
            let n = self.config.compute_matrix_size;
            let mut mm = crate::gpu::matmul::GpuMatmul::new(self.config.compute_device_index, n)
                .map_err(|e| format!("open GPU matmul: {e}"))?;
            let (elapsed, _gflops) = mm.run_many(1).map_err(|e| format!("matmul: {e}"))?;
            let out = mm.read_result().map_err(|e| format!("read result: {e}"))?;
            let mut hasher = sha2::Sha256::new();
            for chunk in out.chunks(4096) {
                let bytes: Vec<u8> = chunk.iter().flat_map(|f| f.to_le_bytes()).collect();
                hasher.update(&bytes);
            }
            let result_hash: [u8; 32] = hasher.finalize().into();
            log::info!(
                "  GPU GEMM done in {}ms -> result_hash 0x{}...",
                elapsed.as_millis(),
                hex::encode(&result_hash[..4])
            );

            // submit + complete (payout to provider from escrow)
            self.chain_client
                .submit_result(job_id, result_hash, 0)
                .await?;
            log::info!("  result submitted for job #{}", job_id);
            self.chain_client.complete_job(job_id).await?;
            log::info!("✅ job #{} completed & paid out", job_id);
            served += 1;
        }

        Ok(served)
    }

    /// Health check — report GPU status and mode
    async fn perform_health_check(&self) {
        let state = self.state.read().await;
        log::info!(
            "📊 [Health] Mode={:?} GPU={}°C VRAM={}MB/{:?} Hashes={:.1}MH/s Compute={:.1}GFLOPS",
            state.current_mode,
            state.gpu_info.temperature_celsius,
            state.gpu_info.vram_used_mb,
            state.gpu_info.vram_total_bytes / (1024 * 1024),
            state.gpu_info.current_hashrate_mhs,
            self.mode_manager.compute_gflops(),
        );
    }
}

/// The config path this bin was asked to use.
///
/// `gpn-agent` is the **legacy** single-purpose miner/compute agent: its only
/// argument is an optional TOML config path. It does no other argument
/// parsing, which used to make a probe flag (`--version`, `--help`, a typo)
/// silently become a *path*: `AgentConfig::from_file()` returns defaults for a
/// missing path, so `./gpn-agent --version` started a full GPU miner instead of
/// printing anything (a 5 h / 100 % CPU orphan was found that way on
/// 2026-10-20). Flags are now answered or rejected before this is reached.
const DEFAULT_CONFIG_PATH: &str = "config/default.toml";

fn config_path_from(argv: &[String]) -> String {
    argv.get(1)
        .cloned()
        .unwrap_or_else(|| DEFAULT_CONFIG_PATH.to_string())
}

/// Returns the offending argument when `argv[1]` still looks like a flag.
fn unexpected_flag(argv: &[String]) -> Option<&str> {
    argv.get(1)
        .map(|s| s.as_str())
        .filter(|s| s.starts_with('-'))
}

fn print_legacy_help() {
    println!(
        "gpn-agent {} (legacy GPU miner/compute agent)",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!("Usage:");
    println!("  gpn-agent [CONFIG.toml]   start mining/compute (default config/default.toml)");
    println!("  gpn-agent --version       print the version and exit");
    println!("  gpn-agent --help          print this help and exit");
    println!();
    println!("Most operators want `gpn_node` (one command: node + heartbeat + provider).");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    env_logger::init();

    let argv: Vec<String> = std::env::args().collect();

    // Informational flags FIRST: they must never reach `from_file()` (which
    // treats any string as a path and silently falls back to defaults).
    match gpn_agent::firstrun::info_request(&argv) {
        Some(gpn_agent::firstrun::InfoRequest::Version) => {
            println!("gpn-agent {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(gpn_agent::firstrun::InfoRequest::Help) => {
            print_legacy_help();
            return Ok(());
        }
        None => {}
    }

    if let Some(flag) = unexpected_flag(&argv) {
        eprintln!("gpn-agent: unexpected option `{flag}`");
        eprintln!("usage: gpn-agent [CONFIG.toml] | --version | --help");
        std::process::exit(2);
    }

    let config_path = config_path_from(&argv);

    // An explicitly supplied path must exist (a typo is reported, not
    // silently replaced by defaults); only the built-in default may be absent.
    let config = if argv.len() > 1 {
        config::AgentConfig::from_file(&config_path)?
    } else {
        config::AgentConfig::from_file_or_default(&config_path)?
    };

    let agent = GPUMiningNodeAgent::with_config(config).await?;
    agent.run().await?;

    Ok(())
}

#[cfg(test)]
mod startup_args_tests {
    use super::{config_path_from, unexpected_flag};

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_flag_is_never_taken_as_a_config_path() {
        // No argument: the built-in default (unchanged behaviour).
        assert_eq!(
            config_path_from(&argv(&["gpn-agent"])),
            "config/default.toml"
        );
        // A real path is used verbatim.
        assert_eq!(
            config_path_from(&argv(&["gpn-agent", "/etc/gpn.toml"])),
            "/etc/gpn.toml"
        );
        // Every flag-shaped argv[1] is caught before it can become a path —
        // this is the class that started a miner on `--version`.
        for flag in [
            "--version",
            "-V",
            "--help",
            "-h",
            "--setup",
            "-x",
            "--versio",
        ] {
            assert_eq!(
                unexpected_flag(&argv(&["gpn-agent", flag])),
                Some(flag),
                "{flag}"
            );
        }
        assert_eq!(unexpected_flag(&argv(&["gpn-agent"])), None);
        assert_eq!(
            unexpected_flag(&argv(&["gpn-agent", "cfg/agent.toml"])),
            None
        );
    }
}
