//! Mode manager — handles transitions between Mining, Compute, and StakeOnly
//! modes. The headline feature: switching to `Mining` actually **starts the
//! real GPU mining loop** (`ExternalMiner`), and switching away **stops it and
//! releases the GPU** for compute/idle.

use crate::compute::{ComputeConfig, ComputeExecutor};
use crate::mining::{ExternalMiner, MiningConfig};
use crate::{AgentState, Mode};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

pub struct ModeManager {
    state: Arc<RwLock<AgentState>>,
    /// The running external mining engine, if any. Mutex because start/stop
    /// are called from `&self` and the handle is not Clone.
    miner: Mutex<Option<ExternalMiner>>,
    /// Config for spawning the miner (node RPC URL, GPU index).
    mining_cfg: MiningConfig,
    /// The running compute engine (real GPU AI workload), if any — the
    /// compute-side counterpart to `miner`. Mutex for the same reason.
    compute: Mutex<Option<ComputeExecutor>>,
    /// Config for spawning the compute engine (GPU index, GEMM size).
    compute_cfg: ComputeConfig,
}

impl ModeManager {
    pub fn new(
        state: Arc<RwLock<AgentState>>,
        mining_cfg: MiningConfig,
        compute_cfg: ComputeConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            state,
            miner: Mutex::new(None),
            mining_cfg,
            compute: Mutex::new(None),
            compute_cfg,
        })
    }

    /// Whether the mining engine is currently running.
    pub fn is_mining(&self) -> bool {
        self.miner
            .lock()
            .unwrap()
            .as_ref()
            .map(|m| m.is_running())
            .unwrap_or(false)
    }

    /// Whether the compute engine (real GPU AI workload) is currently running.
    pub fn is_computing(&self) -> bool {
        self.compute
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.is_running())
            .unwrap_or(false)
    }

    /// Latest measured compute throughput in GFLOPS (0.0 if not computing).
    pub fn compute_gflops(&self) -> f64 {
        self.compute
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.gflops())
            .unwrap_or(0.0)
    }

    /// Force-entry a mode on boot, regardless of the current state flag.
    /// Unlike [`switch_to`](Self::switch_to), this does not short-circuit when
    /// `AgentState.current_mode` already equals `mode` — it ensures the
    /// target mode's resources (mining engine / compute engine) are actually
    /// started.
    pub async fn boot(&self, mode: Mode) -> Result<(), Box<dyn std::error::Error>> {
        log::info!("Booting into {:?} mode", mode);
        match mode {
            Mode::Mining => self.start_miner()?,
            Mode::Compute => {
                self.stop_miner();
                self.start_compute()?;
            }
            Mode::StakeOnly => {
                self.stop_miner();
                self.stop_compute();
            }
        }
        Ok(())
    }

    /// Switch to a new operating mode.
    pub async fn switch_to(&self, new_mode: Mode) -> Result<(), Box<dyn std::error::Error>> {
        let current = { self.state.read().await.current_mode };

        if current == new_mode {
            log::debug!("Already in {:?} mode, no switch needed", new_mode);
            return Ok(());
        }

        log::info!("🔄 Transitioning: {:?} → {:?}", current, new_mode);

        // Phase 1: Stop current mode (releases GPU resources as needed).
        self.stop_mode(current).await?;

        // Phase 2: Cleanup / prepare resources for the new mode.
        self.cleanup(current, new_mode).await?;

        // Phase 3: Start the new mode (may spawn the mining loop).
        self.start_mode(new_mode).await?;

        // Persist the new mode so the manager stays the authority on state.
        self.state.write().await.current_mode = new_mode;

        log::info!("✅ Transition complete: {:?} → {:?}", current, new_mode);
        Ok(())
    }

    async fn stop_mode(&self, mode: Mode) -> Result<(), Box<dyn std::error::Error>> {
        match mode {
            Mode::Mining => {
                log::info!("Stopping mining engine...");
                self.stop_miner();
            }
            Mode::Compute => {
                log::info!("Stopping compute engine...");
                // Cancel the running AI workload and release the GPU so
                // mining/idle can take over.
                self.stop_compute();
            }
            Mode::StakeOnly => {
                log::info!("Already idle (stake-only mode)");
            }
        }
        Ok(())
    }

    async fn cleanup(&self, from: Mode, to: Mode) -> Result<(), Box<dyn std::error::Error>> {
        if from == to {
            return Ok(());
        }
        match (from, to) {
            (Mode::Mining, Mode::Compute) | (Mode::Mining, Mode::StakeOnly) => {
                // The miner was already stopped in stop_mode, which releases
                // the DAG from VRAM. Confirm.
                if self.is_mining() {
                    log::warn!("mining engine still running after stop — forcing");
                    self.stop_miner();
                }
                log::info!("GPU DAG released — VRAM free for compute/idle");
            }
            (Mode::Compute, Mode::Mining) | (Mode::StakeOnly, Mode::Mining) => {
                // start_mode will build the DAG; nothing to pre-clean.
                log::info!("GPU will build Ethash DAG on mining start (30-120s)");
            }
            (Mode::Compute, Mode::StakeOnly) | (Mode::StakeOnly, Mode::Compute) => {
                log::info!("Freeing GPU resources for target mode");
            }
            _ => {
                log::info!("No cleanup needed for this transition");
            }
        }
        Ok(())
    }

    async fn start_mode(&self, mode: Mode) -> Result<(), Box<dyn std::error::Error>> {
        match mode {
            Mode::Mining => {
                log::info!("Starting Ethash mining engine on GPU...");
                self.start_miner()?;
            }
            Mode::Compute => {
                log::info!("Starting compute engine (real GPU AI workload)...");
                self.start_compute()?;
            }
            Mode::StakeOnly => {
                log::info!("GPU idle, staking rewards continue...");
                self.stop_compute();
            }
        }
        Ok(())
    }

    /// Start the external mining engine (build DAG + spawn loop). Idempotent:
    /// if already mining, this is a no-op.
    fn start_miner(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut guard = self.miner.lock().unwrap();
        if let Some(m) = guard.as_ref() {
            if m.is_running() {
                log::debug!("mining engine already running");
                return Ok(());
            }
        }
        let miner = ExternalMiner::start(self.mining_cfg.clone())
            .map_err(|e| format!("start mining engine: {e}"))?;
        log::info!("mining engine started on {}", miner.device_name());
        *guard = Some(miner);
        Ok(())
    }

    /// Stop the external mining engine (cancel loop + release DAG). Idempotent.
    fn stop_miner(&self) {
        let mut guard = self.miner.lock().unwrap();
        if let Some(mut m) = guard.take() {
            m.stop();
        }
    }

    /// Start the compute engine (spawn real GPU AI workload). Idempotent.
    fn start_compute(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut guard = self.compute.lock().unwrap();
        if let Some(c) = guard.as_ref() {
            if c.is_running() {
                log::debug!("compute engine already running");
                return Ok(());
            }
        }
        let compute = ComputeExecutor::start(self.compute_cfg.clone())
            .map_err(|e| format!("start compute engine: {e}"))?;
        log::info!("compute engine started on {}", compute.device_name());
        *guard = Some(compute);
        Ok(())
    }

    /// Stop the compute engine (cancel workload + release GPU). Idempotent.
    fn stop_compute(&self) {
        let mut guard = self.compute.lock().unwrap();
        if let Some(mut c) = guard.take() {
            c.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentState, Mode};

    fn test_state() -> Arc<RwLock<AgentState>> {
        Arc::new(RwLock::new(AgentState {
            current_mode: Mode::StakeOnly,
            gpu_info: crate::gpu::GPUInfo {
                model_name: "test".into(),
                driver_version: "0".into(),
                vram_total_bytes: 8 << 30,
                vram_used_mb: 0,
                temperature_celsius: 40.0,
                gpu_utilization_percent: 0,
                current_hashrate_mhs: 0.0,
                compute_capability: "8.0".into(),
                cuda_version: None,
                gpu_count: 1,
            },
            account: crate::chain::AccountInfo {
                address: "x".into(),
                balance: 0.0,
                nonce: 0,
            },
            last_profitability: None,
            is_running: true,
        }))
    }

    /// Switch-to-same-mode is a no-op and does not start the miner.
    #[tokio::test]
    async fn switch_same_mode_is_noop() {
        let state = test_state();
        let mgr = ModeManager::new(
            state.clone(),
            MiningConfig::new("http://127.0.0.1:9"),
            ComputeConfig::new(0),
        )
        .unwrap();
        mgr.switch_to(Mode::StakeOnly).await.unwrap();
        // Same mode: no engine started.
        assert!(!mgr.is_mining());
        assert!(!mgr.is_computing());
    }

    /// The auto-switch core, compute side: Mining->Compute must START the real
    /// GPU AI workload, and Compute->Mining must STOP it (releasing the GPU).
    /// Requires a GPU with an OpenCL runtime.
    #[tokio::test]
    #[ignore]
    async fn mode_switch_starts_and_stops_compute() {
        let state = test_state();
        let mgr = ModeManager::new(
            state.clone(),
            MiningConfig::new("http://127.0.0.1:9"),
            ComputeConfig::new(0),
        )
        .unwrap();

        // Start mining.
        mgr.switch_to(Mode::Mining).await.unwrap();
        assert!(
            mgr.is_mining(),
            "mining engine should run after switch_to(Mining)"
        );
        assert!(!mgr.is_computing(), "compute must not run while mining");

        // Switch to compute -> mining stops, real GPU AI workload starts.
        mgr.switch_to(Mode::Compute).await.unwrap();
        assert!(
            !mgr.is_mining(),
            "mining engine should stop after switch_to(Compute)"
        );
        assert!(
            mgr.is_computing(),
            "compute engine should run after switch_to(Compute)"
        );

        // Switch back to mining -> compute stops, mining restarts.
        mgr.switch_to(Mode::Mining).await.unwrap();
        assert!(
            mgr.is_mining(),
            "mining should restart after switch_to(Mining)"
        );
        assert!(
            !mgr.is_computing(),
            "compute must stop after switch_to(Mining)"
        );

        // Clean shutdown.
        mgr.stop_miner();
        mgr.stop_compute();
    }

    /// The auto-switch core: Mining->Compute must STOP the real GPU mining
    /// engine (releasing the DAG), and Compute->Mining must RESTART it.
    /// Requires a GPU with an OpenCL runtime (epoch-0 DAG build).
    #[tokio::test]
    #[ignore]
    async fn mode_switch_starts_and_stops_miner() {
        let state = test_state();
        let mgr = ModeManager::new(
            state.clone(),
            MiningConfig::new("http://127.0.0.1:9"),
            ComputeConfig::new(0),
        )
        .unwrap();

        // Start mining.
        mgr.switch_to(Mode::Mining).await.unwrap();
        assert!(
            mgr.is_mining(),
            "mining engine should be running after switch_to(Mining)"
        );

        // Switch away -> engine must stop (GPU freed for compute).
        mgr.switch_to(Mode::Compute).await.unwrap();
        assert!(
            !mgr.is_mining(),
            "mining engine should be stopped after switch_to(Compute)"
        );

        // Switch back -> engine restarts.
        mgr.switch_to(Mode::Mining).await.unwrap();
        assert!(
            mgr.is_mining(),
            "mining engine should restart after switch_to(Mining)"
        );

        // Clean shutdown.
        mgr.stop_miner();
    }
}
