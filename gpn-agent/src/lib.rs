//! GPN Node Agent library.
//!
//! Provides the GPU monitor, OpenCL Ethash miner, mode manager, chain client,
//! profitability calculator, and compute executor used by the agent binary.

pub mod abuse;
pub mod ap_token;
pub mod assign_pull;
pub mod attest_idle;
pub mod chain;
pub mod chainspec;
pub mod cli;
pub mod compute;
pub mod compute_unit;
pub mod config;
pub mod disk;
pub mod firstrun;
pub mod gov;
pub mod gpu;
pub mod gpu_yield;
pub mod gui;
pub mod hub_ap;
pub mod hub_channel;
pub mod idle;
pub mod idle_auto;
pub mod image_channel;
pub mod image_jobs;
pub mod jobkind;
pub mod join;
pub mod llm;
pub mod mining;
pub mod models;
pub mod moderation;
pub mod modes;
pub mod net;
pub mod network_registry;
pub mod nodestatus;
pub mod ollama;
pub mod pricing;
pub mod profitability;
pub mod provider_policy;
pub mod provider_watchdog;
pub mod referral;
pub mod routing;
pub mod seedlist;
pub mod settle_policy;
pub mod strict;
pub mod update;
pub mod utils;

// Re-export the scripted feed for the headless simulator binary.
pub use profitability::scripted::ScriptedFeed;

use std::sync::Arc;
use tokio::sync::RwLock;

/// Operating mode for the GPU.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// GPU mining (Ethash PoW)
    Mining,
    /// Serving compute jobs
    Compute,
    /// Idle GPU, only staking rewards
    StakeOnly,
}

/// Agent state shared across all components.
#[derive(Clone)]
pub struct AgentState {
    /// Current operating mode
    pub current_mode: Mode,
    /// GPU hardware info
    pub gpu_info: gpu::GPUInfo,
    /// Wallet/account info
    pub account: chain::AccountInfo,
    /// Last profitability analysis
    pub last_profitability: Option<profitability::ProfitAnalysis>,
    /// Whether agent is running
    pub is_running: bool,
}

/// Shared agent state (RwLock-wrapped) used by sub-components.
pub type SharedState = Arc<RwLock<AgentState>>;
