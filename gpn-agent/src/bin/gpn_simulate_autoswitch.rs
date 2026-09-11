//! Headless auto-switch simulator.
//!
//! Proves the headline feature — **profitability-driven GPU auto-switching** —
//! without needing a live node or GPU. It runs a scripted market timeline
//! through the *same* pure decision function (`decide_switch`) the live agent
//! uses, and prints the mode the GPU would be in at each step, plus every
//! switch that fires.
//!
//! This is the deterministic counterpart to the live `auto` mode: it shows
//! that a rising compute price flips the GPU to Compute, and a fading one
//! flips it back to Mining — with hysteresis preventing flapping.
//!
//! Usage:
//!   gpn_simulate_autoswitch [hysteresis_percent] [start_mode]
//!   (start_mode: mining | compute | stake_only; default mining)

use gpn_agent::{Mode, ScriptedFeed};

fn main() {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let hysteresis: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let start_mode = match args.get(2).map(|s| s.as_str()) {
        Some("compute") => Mode::Compute,
        Some("stake_only") => Mode::StakeOnly,
        _ => Mode::Mining,
    };

    println!("== GPN auto-switch simulator ==");
    println!("  start mode:     {start_mode:?}");
    println!("  hysteresis:     {hysteresis}%");
    println!();

    let feed = ScriptedFeed::demand_spike_scenario();
    let trace = feed.simulate(start_mode, hysteresis);

    let mut current = start_mode;
    for (i, (mode, step, decided)) in trace.iter().enumerate() {
        let arrow = if *mode != current {
            "  ◀── SWITCH"
        } else {
            ""
        };
        let decided_note = match decided {
            Some(m) => format!("(would switch to {m:?})"),
            None => String::new(),
        };
        let mode_str = format!("{mode:?}");
        println!(
            "step {i:02} | mode={mode_str:<10} | {:<42} | mining=${:.2} compute=${:.2} staking=${:.2} {}{}",
            step.label,
            step.mining_usd_hr,
            step.compute_usd_hr,
            step.staking_usd_hr,
            decided_note,
            arrow,
        );
        current = *mode;
    }

    println!();
    println!("Final mode: {current:?}");
}
