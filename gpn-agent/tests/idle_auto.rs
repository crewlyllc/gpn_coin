//! End-to-end tests for auto-idle with an **injectable probe** — no GPU, no
//! chain, no sleeping for minutes.
//!
//! These drive the real sampler thread ([`idle_auto::spawn`]) and assert on the
//! real `idle.json` that the miner and every provider read. That is the whole
//! contract: auto-idle must produce exactly the same state a manual click does.

use gpn_agent::idle;
use gpn_agent::idle_auto::{
    self, AutoIdleConfig, AutoIdleShared, GpuLoadProbe, LoadReading, RunOptions,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn tmp(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("gpn-idle-auto-it-{tag}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("idle.json")
}

fn busy() -> LoadReading {
    LoadReading {
        util_percent: Some(91),
        vram_used_mb: Some(6000),
        vram_total_mb: Some(8188),
        lock_held: false,
        gpn_vram_mb: 1120,
        foreign_vram_mb: 2600,
        foreign_pids: 2,
    }
}

fn quiet() -> LoadReading {
    LoadReading {
        util_percent: Some(2),
        vram_used_mb: Some(1200),
        vram_total_mb: Some(8188),
        lock_held: false,
        gpn_vram_mb: 1120,
        foreign_vram_mb: 80,
        foreign_pids: 1,
    }
}

/// A probe the test can flip between "operator is gaming" and "quiet".
struct FakeProbe(Arc<Mutex<LoadReading>>);

impl GpuLoadProbe for FakeProbe {
    fn read(&mut self) -> Option<LoadReading> {
        Some(self.0.lock().unwrap().clone())
    }
}

struct Harness {
    idle_path: std::path::PathBuf,
    reading: Arc<Mutex<LoadReading>>,
    shared: Arc<AutoIdleShared>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Harness {
    fn start(tag: &str, minutes: u64) -> Self {
        let idle_path = tmp(tag);
        let reading = Arc::new(Mutex::new(busy()));
        let cfg = AutoIdleConfig {
            util_percent: 40,
            busy_samples: 2,
            quiet_samples: 2,
            minutes,
            cooldown_secs: 2,
            busy_sample_secs: 1,
            quiet_sample_secs: 1,
            min_foreign_vram_mb: 512,
        };
        let shared = Arc::new(AutoIdleShared::new(cfg.clone(), true, tmp(&format!("{tag}-toggle"))));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = idle_auto::spawn(RunOptions {
            idle_path: idle_path.clone(),
            force: false,
            peers: Arc::new(|| Some(3)),
            probe: Box::new(FakeProbe(reading.clone())),
            tick_secs: 1,
            stop: stop.clone(),
            shared: shared.clone(),
        });
        Self {
            idle_path,
            reading,
            shared,
            stop,
            handle: Some(handle),
        }
    }

    fn set(&self, r: LoadReading) {
        *self.reading.lock().unwrap() = r;
    }

    /// The claim gate every provider consults: idle.json absent ⇒ allowed.
    fn claims(&self) -> bool {
        idle::state_at(&self.idle_path, idle::now_unix()).is_none()
    }

    fn state(&self) -> Option<idle::IdleState> {
        idle::read_state_at(&self.idle_path)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = std::fs::remove_file(&self.idle_path);
    }
}

fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    f()
}

/// The whole point: sustained foreign load → a real `idle.json` with an `auto:`
/// reason; load gone → the file is removed and providers may claim again.
#[test]
fn sustained_foreign_load_idles_and_quiet_resumes() {
    let h = Harness::start("cycle", 30);
    assert!(
        wait_until(Duration::from_secs(8), || h.state().is_some()),
        "sustained foreign load must enter idle"
    );
    let st = h.state().unwrap();
    assert!(
        st.reason.starts_with("auto:"),
        "reason must mark it auto: {}",
        st.reason
    );
    assert!(st.reason.contains("foreign GPU load 91%"), "{}", st.reason);
    assert!(!h.claims(), "providers must not claim");
    assert!(h.shared.status_json(idle::now_unix())["engaged"].as_bool().unwrap());

    // The operator's game exits: the node resumes by itself.
    h.set(quiet());
    assert!(
        wait_until(Duration::from_secs(10), || h.state().is_none()),
        "quiet GPU must auto-resume (idle.json removed)"
    );
    assert!(
        h.claims(),
        "providers may claim again"
    );
    assert!(!h.shared.status_json(idle::now_unix())["engaged"].as_bool().unwrap());
}

/// A single busy sample (a shader compile, a compositor blip) must not park
/// the node — the probe has to stay loaded for the whole debounce window.
#[test]
fn a_single_spike_does_not_idle() {
    let h = Harness::start("spike", 30);
    // Quiet immediately: the first sample may be busy, but never two.
    std::thread::sleep(Duration::from_millis(300));
    h.set(quiet());
    std::thread::sleep(Duration::from_secs(4));
    assert!(h.state().is_none(), "one spike must never write idle.json");
}

/// The dashboard toggle is a real off switch: enabled=false stops the sampler
/// from reacting even to sustained foreign load, and re-enabling resumes it.
#[test]
fn toggle_off_prevents_auto_idle() {
    let h = Harness::start("toggle", 30);
    h.shared.set_enabled(false);
    std::thread::sleep(Duration::from_secs(5));
    assert!(
        h.state().is_none(),
        "auto-idle must be inert while the toggle is off"
    );
    h.shared.set_enabled(true);
    assert!(
        wait_until(Duration::from_secs(8), || h.state().is_some()),
        "re-enabling must let it engage"
    );
}

/// A manual idle already in force is left alone: auto-idle never rewrites the
/// operator's reason or cuts their window short.
#[test]
fn a_manual_idle_is_untouched() {
    let h = Harness::start("manual", 30);
    let now = idle::now_unix();
    idle::start_at(&h.idle_path, 60, "gaming", now).unwrap();
    std::thread::sleep(Duration::from_secs(5));
    let st = h.state().expect("manual idle must survive");
    assert_eq!(st.reason, "gaming");
    assert_eq!(st.until, now + 3_600);
}

/// A manual resume while auto-idle is active is respected, and the cooldown
/// keeps auto-idle from immediately re-engaging.
#[test]
fn manual_resume_wins_and_cools_down() {
    let h = Harness::start("manual-resume", 30);
    assert!(wait_until(Duration::from_secs(8), || h.state().is_some()));

    // The operator presses "Resume now" while the game is still running.
    idle::clear_at(&h.idle_path);
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        h.state().is_none(),
        "a manual resume must not be undone immediately"
    );
    // Cooldown is 2 s in this harness; for its duration, sustained foreign load
    // must not re-engage.
    std::thread::sleep(Duration::from_millis(800));
    assert!(h.state().is_none(), "cooldown must hold");
}
