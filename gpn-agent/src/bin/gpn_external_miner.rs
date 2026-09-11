//! External GPU miner for GPN — wires the `gpn-agent` OpenCL Ethash miner to
//! the node's external-mining RPC (`gpn_getTemplate` / `gpn_submitWork`).
//!
//! This completes the **production mining architecture**: the node and the
//! miner are fully separable. This process talks to a lightweight node (which
//! needs no internal self-miner) purely over JSON-RPC, mines on its own GPU,
//! and submits the finished seal back over the wire.
//!
//! The actual mining loop now lives in `gpn_agent::mining` (a stoppable
//! engine), so the same code drives the agent's auto-switch `Mining` mode.
//! This binary is a thin CLI wrapper for standalone use / testing.
//!
//! Usage:
//!   gpn_external_miner [rpc_url] [device_index] [batch] [poll_ms]
//!
//! Defaults: rpc_url = http://127.0.0.1:9944, device_index = 0, batch = 1<<20, poll_ms = 2000

use gpn_agent::mining::{ExternalMiner, MiningConfig};

fn parse_u64(s: &str, default: u64) -> u64 {
    s.parse().unwrap_or(default)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let rpc_url = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "http://127.0.0.1:9944".to_string());
    let device_index: usize = parse_u64(args.get(2).map(|s| s.as_str()).unwrap_or("0"), 0) as usize;
    let batch: usize = parse_u64(
        args.get(3).map(|s| s.as_str()).unwrap_or("1048576"),
        1 << 20,
    ) as usize;
    let poll_ms: u64 = parse_u64(args.get(4).map(|s| s.as_str()).unwrap_or("2000"), 2000);

    println!("== GPN external GPU miner ==");
    println!("  rpc:          {rpc_url}");
    println!("  device index: {device_index}");
    println!("  batch:        {batch}");

    for line in gpn_agent::gpu::ethash_miner::EthashGpuMiner::device_summary() {
        println!("  device: {line}");
    }

    let cfg = MiningConfig {
        rpc_url,
        device_index,
        batch,
        poll_ms,
    };
    let miner = ExternalMiner::start(cfg).map_err(|e| format!("start miner: {e}"))?;
    println!("using GPU: {}", miner.device_name());
    println!("mining in background — press Ctrl-C to stop");

    // Block forever; the mining loop runs on its own thread.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
