//! GPN Node — GPU-Native Network Substrate Node
//!
//! A Substrate-based blockchain node for the GPN network.
//! Supports GPU mining, GPU-attested staking, and a compute marketplace.

mod chain_spec;
mod cli;
mod command;
mod gpu_miner;
mod service;

fn main() -> sc_cli::Result<()> {
    command::run()
}
