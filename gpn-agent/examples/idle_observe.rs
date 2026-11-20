//! Item #210 observation harness: run the REAL `NvidiaProbe` and the REAL
//! `AutoIdle` state machine against the live GPU for a fixed duration and print
//! one line per classification change plus every action taken.
//!
//! This is not a simulation: the readings come from `nvidia-smi` through the
//! production probe, and the only injected value is the clock step.
//!
//! ```text
//! cargo run --example idle_observe -- --secs 3600 --idle-path /tmp/idle-observe.json
//! ```

use gpn_agent::idle_auto::{
    Action, AutoIdle, AutoIdleConfig, GpuLoadProbe, LoadReading, NvidiaProbe,
};
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn arg(name: &str, default: &str) -> String {
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == name {
            if let Some(v) = it.next() {
                return v;
            }
        }
    }
    default.to_string()
}

fn main() {
    let secs: u64 = arg("--secs", "3600").parse().unwrap_or(3600);
    let idle_path = arg("--idle-path", "/tmp/idle-observe.json");
    let tick: u64 = arg("--tick", "5").parse().unwrap_or(5);
    let cfg = AutoIdleConfig::from_env();
    let mut machine = AutoIdle::new(cfg.clone());
    let mut probe = NvidiaProbe::new(0);

    let start = SystemTime::now();
    let started = now();
    let mut last_verdict = "";
    let mut samples = 0u64;
    let mut readings: u64 = 0;
    let mut idle_seconds = 0u64;
    let mut last_t = started;

    println!(
        "[observe] start — {}s, tick {}s, util threshold {}%, min foreign VRAM {} MiB",
        secs, tick, cfg.util_percent, cfg.min_foreign_vram_mb
    );

    while started + secs > now() {
        let reading: Option<LoadReading> = probe.read();
        let t = now();
        let elapsed = t.saturating_sub(started);
        samples += 1;
        if reading.is_some() {
            readings += 1;
        }
        let verdict = reading
            .as_ref()
            .map(|r| r.workload(&cfg))
            .unwrap_or("unknown");
        if verdict != last_verdict {
            println!(
                "[observe] t={:>5}s verdict={:<17} {}",
                elapsed,
                verdict,
                reading
                    .as_ref()
                    .map(|r| r.evidence())
                    .unwrap_or_else(|| "no reading".into())
            );
            last_verdict = verdict;
        }
        let state = gpn_agent::idle::state_at(std::path::Path::new(&idle_path), t);
        match machine.step(reading.as_ref(), t, state.as_ref(), Some(4), false) {
            Action::None => {}
            Action::Enter {
                minutes,
                reason,
                evidence,
            } => {
                println!(
                    "[observe] t={:>5}s ACTION enter idle {} min — {} — {}",
                    elapsed, minutes, reason, evidence
                );
                let _ = gpn_agent::idle::start_at(
                    std::path::Path::new(&idle_path),
                    minutes,
                    &reason,
                    t,
                );
            }
            Action::Exit { why } => {
                println!("[observe] t={:>5}s ACTION resume — {}", elapsed, why);
                gpn_agent::idle::clear_at(std::path::Path::new(&idle_path));
            }
        }
        if machine.engaged() {
            idle_seconds = idle_seconds.saturating_add(t.saturating_sub(last_t));
        }
        last_t = t;
        std::thread::sleep(std::time::Duration::from_secs(tick));
    }

    let wall = SystemTime::now()
        .duration_since(start)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!(
        "[observe] done — wall {}s, samples {}, readings {}/{}, idle {}s, final verdict {}",
        wall, samples, readings, samples, idle_seconds, last_verdict
    );
    // Exit 3 when the observation itself was hollow (probe never read).
    if readings == 0 {
        std::process::exit(3);
    }
}
