/// Mode manager — handles transitions between Mining, Compute, and StakeOnly modes

use crate::{AgentState, Mode};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct ModeManager {
    target_mode: String,
    state: Arc<RwLock<AgentState>>,
}

impl ModeManager {
    pub fn new(target_mode: String, state: Arc<RwLock<AgentState>>) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self { target_mode, state })
    }

    /// Switch to a new operating mode
    pub async fn switch_to(&self, new_mode: Mode) -> Result<(), Box<dyn std::error::Error>> {
        let current = { self.state.read().await.current_mode };

        if current == new_mode {
            log::debug!("Already in {:?} mode, no switch needed", new_mode);
            return Ok(());
        }

        log::info!("🔄 Transitioning: {:?} → {:?}", current, new_mode);

        // Phase 1: Stop current mode
        self.stop_mode(current).await?;

        // Phase 2: Cleanup and resource release
        self.cleanup(current, new_mode).await?;

        // Phase 3: Start new mode
        self.start_mode(new_mode).await?;

        log::info!("✅ Transition complete: {:?} → {:?}", current, new_mode);
        Ok(())
    }

    async fn stop_mode(&self, mode: Mode) -> Result<(), Box<dyn std::error::Error>> {
        match mode {
            Mode::Mining => {
                log::info!("Stopping mining engine...");
                // Kill ethminer process
                // Save mining progress
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            Mode::Compute => {
                log::info!("Stopping compute executor...");
                // Stop accepting new jobs
                // Wait for running jobs (with timeout)
                // Kill Docker containers
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
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
            (Mode::Mining, Mode::Compute) => {
                // Release DAG from VRAM
                log::info!("Releasing Ethash DAG from GPU memory...");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            (Mode::Compute, Mode::Mining) => {
                // Generate Ethash DAG for current epoch
                log::info!("Generating Ethash DAG (this may take 30-120s)...");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            (Mode::Mining, Mode::StakeOnly) | (Mode::Compute, Mode::StakeOnly) => {
                log::info!("Freeing GPU resources for idle mode");
            }
            (Mode::StakeOnly, Mode::Mining) => {
                log::info!("Generating Ethash DAG for mining...");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            (Mode::StakeOnly, Mode::Compute) => {
                log::info!("Preparing Docker environment for compute...");
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
                log::info!("Starting Ethash mining engine...");
                // Spawn ethminer process
                // Set GPU to mining mode
            }
            Mode::Compute => {
                log::info!("Starting compute executor...");
                // Start Docker daemon
                // Register as available provider on chain
                // Begin polling for jobs
            }
            Mode::StakeOnly => {
                log::info!("GPU idle, staking rewards continue...");
                // GPU goes idle
                // Attestation continues
            }
        }
        Ok(())
    }
}
