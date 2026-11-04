//! Process-wide serialisation for tests that touch the process environment.
//!
//! `std::env::set_var` is process-global while cargo runs `#[test]`s on many
//! threads, so a test that writes `HOME` or a `GPN_*` knob races every other
//! test that resolves a path or reads that knob. This is what made
//! `assign_pull::tests::poll_state_round_trips_and_feeds_the_dashboard` fail
//! ~3 times in 4 under `cargo test --lib`: `firstrun` briefly pointed `HOME` at
//! `/home/tester`, and the poll-state write landed outside the real `~/.gpn`.
//!
//! Every test that writes the environment takes [`lock_env`]; so does any test
//! that reads a variable another test writes. Keep them serialised through this
//! single lock rather than module-local ones, otherwise two modules can still
//! interleave.
//!
//! Poisoning is ignored on purpose: the lock only guards env mutation, so a test
//! that panicked mid-way must not cascade into unrelated failures.

use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard, OnceLock};

fn env_mutex() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Acquire the process-wide environment lock (poison-tolerant).
pub fn lock_env() -> MutexGuard<'static, ()> {
    env_mutex().lock().unwrap_or_else(|e| e.into_inner())
}

/// Run `f` with `vars` applied (`None` removes the variable), restoring the
/// previous values afterwards — including when `f` panics, because the restore
/// runs from `Drop` while the lock is still held.
pub fn with_env(vars: &[(&str, Option<OsString>)], f: impl FnOnce()) {
    struct Restore(Vec<(String, Option<OsString>)>);

    impl Drop for Restore {
        fn drop(&mut self) {
            for (k, v) in &self.0 {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    let _guard = lock_env();
    let saved: Vec<(String, Option<OsString>)> = vars
        .iter()
        .map(|(k, _)| ((*k).to_string(), std::env::var_os(k)))
        .collect();
    let restore = Restore(saved);

    for (k, v) in vars {
        match v {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    }

    f();
    drop(restore);
}
