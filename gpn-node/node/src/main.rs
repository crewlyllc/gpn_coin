//! GPN Node — GPU-Native Network Substrate Node
//!
//! A Substrate-based blockchain node for the GPN network.
//! Supports GPU mining, GPU-attested staking, and a compute marketplace.

// The one fatal-exit path is shared verbatim with the supervisor crate
// (`gpn-agent/src/exit_report.rs`), so both binaries write the same
// `~/.gpn/last-exit.json` line and print the same boxed block. It is a plain
// `#[path]` module — no dependency on gpn-agent, no duplicated format.
#[path = "../../../gpn-agent/src/exit_report.rs"]
mod exit_report;

mod block_serving;
mod chain_spec;
mod chain_state;
mod challenge;
mod cli;
mod clock;
mod command;
mod dialability;
mod discovery;
mod fork_tolerance;
mod gpu_miner;
mod lan_discovery;
mod network_registry;
mod peer_exchange;
mod peer_priority;
mod portmap;
mod rpc;
mod service;

fn main() {
    if let Err(e) = command::run() {
        // 0xC0000135 (missing VC++ runtime) never reaches this line: the loader
        // kills the process before `main`. The Windows launcher reports that
        // case from the exit status it observes.
        let detail = format!("the GPN chain node could not start: {e}");
        let kind = exit_report::kind_for_text(&detail);
        exit_report::fatal(kind, detail, exit_report::default_fix(kind));
    }
}
