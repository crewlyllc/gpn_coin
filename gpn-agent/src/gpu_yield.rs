//! One GPU = one job: a **cross-process** GPU lock plus the miner yield flag.
//!
//! Two independent mechanisms, deliberately:
//!
//! 1. **Mutual exclusion (this module, the lock).** Every GPU-using provider
//!    (`gpn_llm_provider`, `gpn_diffusion_provider`, …) takes an exclusive
//!    advisory lock on `~/.gpn/gpu.lock` (`$GPN_GPU_LOCK`) for the *whole* GPU
//!    section of a job. A second job — in another thread **or another process**
//!    — blocks and waits its turn instead of running concurrently. Before
//!    2026-10-22 this guard only wrote a flag file, which is not exclusion at
//!    all: `gpn_llm_provider` and `gpn_diffusion_provider` both "held" it and
//!    fought over VRAM, so a chat in flight made image generation exit 1
//!    (`helper exited Some(1)`) and stretched a 73 ms chat to 22 s.
//!
//!    - Unix: `flock(LOCK_EX|LOCK_NB)` on the lock file, polled until the
//!      timeout. `flock` is tied to the open file description, so it excludes
//!      other processes *and* other opens in this process.
//!    - Windows: `LockFileEx` with `LOCKFILE_EXCLUSIVE_LOCK |
//!      LOCKFILE_FAIL_IMMEDIATELY`, same polling/timeout semantics.
//!
//! 2. **Miner yield (unchanged).** While the lock is held we also write
//!    `~/.gpn/gpu-yield` (`$GPN_GPU_YIELD`). The co-located node's miner watches
//!    that path and fully stops mining for as long as it exists. The node must
//!    NEVER take the lock — it only reads the flag — otherwise provider and
//!    miner would deadlock against each other. `gpu_yield_requested()` in
//!    `gpn-node/node/src/service.rs` is that reader; do not "fix" it.
//!
//! Timeout: `$GPN_GPU_LOCK_TIMEOUT_SECS` (default 900 s). On timeout acquire()
//! returns `Err(GpuBusyError)`, which providers report as a job failure
//! ("GPU busy: another job held the GPU for Xs") — waiting forever is worse
//! than failing crisply.
//!
//! Release order on `Drop` (RAII, so it happens on `?`/`return`/panic): clear
//! the yield flag FIRST (mining may resume), then release the lock (the next
//! job may start and re-set the flag). Both are cleared on every path.
//!
//! Note: the lock file handle is opened by `std`, i.e. close-on-exec, so a
//! spawned helper (`python3 gpn_diffuse.py`) never inherits the lock.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Path of the yield flag the node's miner watches (env-aware, matching the
/// node's `gpu_yield_requested()`): `$GPN_GPU_YIELD` if set, else `~/.gpn/gpu-yield`.
pub fn yield_flag_path() -> PathBuf {
    if let Ok(p) = std::env::var("GPN_GPU_YIELD") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".gpn/gpu-yield")
}

/// Path of the cross-process GPU lock: `$GPN_GPU_LOCK` if set, else `~/.gpn/gpu.lock`.
pub fn lock_path() -> PathBuf {
    if let Ok(p) = std::env::var("GPN_GPU_LOCK") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".gpn/gpu.lock")
}

/// How long to wait for the GPU before failing the job.
/// `$GPN_GPU_LOCK_TIMEOUT_SECS`, default 900 s.
pub fn lock_timeout() -> Duration {
    std::env::var("GPN_GPU_LOCK_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(900))
}

/// The GPU stayed busy for longer than the configured timeout.
#[derive(Debug, Clone)]
pub struct GpuBusyError {
    pub waited: Duration,
    /// Best-effort description of the holder (`pid=… since=…`) if readable.
    pub holder: Option<String>,
}

impl std::fmt::Display for GpuBusyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "GPU busy: another job held the GPU for {}s (waited {}s",
            self.waited.as_secs(),
            self.waited.as_secs()
        )?;
        match &self.holder {
            Some(h) => write!(f, ", holder: {h})"),
            None => write!(f, ")"),
        }
    }
}

impl std::error::Error for GpuBusyError {}

/// RAII hold on the GPU: exclusive cross-process lock + the miner yield flag.
#[derive(Debug)]
pub struct GpuYieldGuard {
    flag: PathBuf,
    /// Kept alive for the lifetime of the guard; closing releases the lock.
    _lock: File,
    lock_path: PathBuf,
    /// How long we waited to get the lock (0 if uncontended).
    pub acquired_after: Duration,
}

impl GpuYieldGuard {
    /// Take the GPU, waiting up to `$GPN_GPU_LOCK_TIMEOUT_SECS` (default 900 s).
    pub fn acquire() -> Result<Self, GpuBusyError> {
        Self::acquire_with_timeout(lock_timeout())
    }

    /// Take the GPU **without waiting**: `None` means another job (in this
    /// process or another) owns it right now.
    ///
    /// This is the probe the attestation gate uses (queue item #93): an
    /// attestation measurement must be taken on an *idle* device, so "who owns
    /// the GPU" is answered by trying to take it rather than by inspecting a
    /// flag. On success the caller holds real exclusion — which is what makes
    /// the following measurement uncontended for its whole duration, not just
    /// at the instant of the check.
    pub fn try_acquire() -> Option<Self> {
        Self::acquire_with_timeout(Duration::ZERO).ok()
    }

    /// Take the GPU, waiting at most `timeout`. Blocks the calling thread while
    /// waiting — call it from a blocking context (a job thread, or
    /// `spawn_blocking`), never from an async task that must stay responsive.
    pub fn acquire_with_timeout(timeout: Duration) -> Result<Self, GpuBusyError> {
        let lock_path = lock_path();
        let flag = yield_flag_path();
        if let Some(parent) = lock_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
        {
            Ok(f) => f,
            Err(e) => {
                // No lock file means we cannot guarantee exclusion. Fail closed
                // rather than running concurrently: a busy GPU is a soft error,
                // two jobs on one GPU is a hardware error.
                return Err(GpuBusyError {
                    waited: Duration::ZERO,
                    holder: Some(format!("cannot open lock {}: {e}", lock_path.display())),
                });
            }
        };

        let started = Instant::now();
        let mut announced = false;
        let mut backoff = Duration::from_millis(25);
        loop {
            match try_lock_exclusive(&file) {
                Ok(true) => break,
                Ok(false) => {}
                Err(e) => {
                    return Err(GpuBusyError {
                        waited: started.elapsed(),
                        holder: Some(format!("lock error on {}: {e}", lock_path.display())),
                    });
                }
            }
            let waited = started.elapsed();
            if waited >= timeout {
                return Err(GpuBusyError {
                    waited,
                    holder: read_holder(&lock_path),
                });
            }
            if !announced {
                announced = true;
                // Best-effort only: the holder line is written by the current
                // owner AFTER it acquires, so it can name the previous holder.
                let holder = read_holder(&lock_path)
                    .map(|h| format!(", lock file last written by {h}"))
                    .unwrap_or_default();
                eprintln!(
                    "[gpu-lock] waiting for {} (timeout {}s{holder})",
                    lock_path.display(),
                    timeout.as_secs()
                );
            }
            std::thread::sleep(
                backoff
                    .min(timeout.saturating_sub(waited))
                    .max(Duration::from_millis(1)),
            );
            backoff = (backoff * 2).min(Duration::from_millis(250));
        }

        let acquired_after = started.elapsed();
        // Publish holder info for the next waiter's log line. Best-effort: the
        // lock (not this content) is what excludes.
        let mut f = &file;
        let _ = f.set_len(0);
        let _ = f.seek(SeekFrom::Start(0));
        let _ = writeln!(f, "pid={} since={}", std::process::id(), now_ms());
        let _ = f.flush();

        if acquired_after >= Duration::from_millis(20) {
            eprintln!("[gpu-lock] acquired after {}ms", acquired_after.as_millis());
        }

        // Miner yield flag: written only while we own the GPU, cleared on Drop.
        if let Some(parent) = flag.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&flag, b"1") {
            eprintln!("[gpu-yield] acquire write {} failed: {e}", flag.display());
        }

        Ok(Self {
            flag,
            _lock: file,
            lock_path,
            acquired_after,
        })
    }

    /// Path of the held lock file (diagnostics / tests).
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }
}

impl Drop for GpuYieldGuard {
    fn drop(&mut self) {
        // Order matters: let the miner resume only after this job's GPU work is
        // done, and release the lock only after the flag is gone. The lock is
        // released by the `File` drop that follows this body (flock/Windows
        // locks are released on close).
        let _ = std::fs::remove_file(&self.flag);
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Is somebody else holding the GPU lock right now?
///
/// Used by auto-idle (`idle_auto.rs`): a job of *ours* is expected load, not
/// foreign load. This is a **read-only probe** — unlike [`GpuYieldGuard::acquire`]
/// it never writes the yield flag, so asking the question cannot park the
/// miner. The lock we take here (when free) is released immediately by dropping
/// the handle.
pub fn lock_is_held() -> bool {
    let path = lock_path();
    let Ok(f) = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
    else {
        // No lock file means nothing holds it (and no provider could either).
        return false;
    };
    match try_lock_exclusive(&f) {
        // Free: we took it for an instant; the drop of `f` releases it.
        Ok(true) => false,
        // Held by a provider (same or another process).
        Ok(false) => true,
        // Unknown I/O state: do not claim a job is running.
        Err(_) => false,
    }
}

/// Read the holder line written by the lock owner (diagnostics only).
fn read_holder(path: &Path) -> Option<String> {
    let mut s = String::new();
    let mut f = File::open(path).ok()?;
    f.read_to_string(&mut s).ok()?;
    let line = s.trim();
    if line.is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}

#[cfg(unix)]
fn try_lock_exclusive(f: &File) -> std::io::Result<bool> {
    use std::os::unix::io::AsRawFd;
    loop {
        let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(true);
        }
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            // EAGAIN and EWOULDBLOCK are the same value on Linux; match both
            // spellings for other unices.
            Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => return Ok(false),
            Some(libc::EINTR) => continue,
            _ => return Err(e),
        }
    }
}

#[cfg(windows)]
fn try_lock_exclusive(f: &File) -> std::io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
    let h = f.as_raw_handle() as HANDLE;
    let ok = unsafe {
        LockFileEx(
            h,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            u32::MAX,
            u32::MAX,
            &mut ov,
        )
    };
    if ok != 0 {
        return Ok(true);
    }
    let e = std::io::Error::last_os_error();
    if e.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        Ok(false)
    } else {
        Err(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};

    /// These tests mutate process-global env vars, so they must not run in
    /// parallel with each other (cargo test's default is a thread per test).
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    struct EnvGuard {
        yield_flag: PathBuf,
        lock: PathBuf,
    }

    impl EnvGuard {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "gpn-gpu-lock-test-{}-{}-{}",
                tag,
                std::process::id(),
                now_ms()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let yield_flag = dir.join("gpu-yield");
            let lock = dir.join("gpu.lock");
            std::env::set_var("GPN_GPU_YIELD", &yield_flag);
            std::env::set_var("GPN_GPU_LOCK", &lock);
            Self { yield_flag, lock }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var("GPN_GPU_YIELD");
            std::env::remove_var("GPN_GPU_LOCK");
            let _ = std::fs::remove_dir_all(self.lock.parent().unwrap());
        }
    }

    /// Second acquirer in the same process must block until the first drops,
    /// and the two GPU sections must never overlap.
    #[test]
    fn second_thread_waits_for_first_and_sections_do_not_overlap() {
        let _serial = serial();
        let _env = EnvGuard::new("threads");
        let in_section = Arc::new(AtomicUsize::new(0));
        let max_in_section = Arc::new(AtomicUsize::new(0));
        let order = Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));

        let run = |name: &'static str, hold: Duration| {
            let in_section = in_section.clone();
            let max_in_section = max_in_section.clone();
            let order = order.clone();
            std::thread::spawn(move || {
                let g = GpuYieldGuard::acquire_with_timeout(Duration::from_secs(10)).unwrap();
                let now = in_section.fetch_add(1, Ordering::SeqCst) + 1;
                max_in_section.fetch_max(now, Ordering::SeqCst);
                order.lock().unwrap().push(name);
                std::thread::sleep(hold);
                in_section.fetch_sub(1, Ordering::SeqCst);
                drop(g);
            })
        };

        let a = run("first", Duration::from_millis(400));
        std::thread::sleep(Duration::from_millis(50));
        let b = run("second", Duration::from_millis(50));
        a.join().unwrap();
        b.join().unwrap();

        assert_eq!(
            max_in_section.load(Ordering::SeqCst),
            1,
            "two GPU sections overlapped"
        );
        assert_eq!(
            *order.lock().unwrap(),
            vec!["first", "second"],
            "second acquirer did not wait its turn"
        );
    }

    /// The yield flag exists while the lock is held and is gone afterwards.
    #[test]
    fn flag_present_while_held_and_removed_after_drop() {
        let _serial = serial();
        let env = EnvGuard::new("flag");
        assert!(!env.yield_flag.exists());
        {
            let g = GpuYieldGuard::acquire_with_timeout(Duration::from_secs(2)).unwrap();
            assert!(env.yield_flag.exists(), "miner flag not written");
            drop(g);
        }
        assert!(!env.yield_flag.exists(), "miner flag left behind");
        // Lock itself is reusable afterwards.
        let _g = GpuYieldGuard::acquire_with_timeout(Duration::from_secs(2)).unwrap();
    }

    /// A waiter that exceeds its timeout gets a clear error instead of hanging.
    #[test]
    fn timeout_returns_error_instead_of_blocking_forever() {
        let _serial = serial();
        let _env = EnvGuard::new("timeout");
        let held = GpuYieldGuard::acquire_with_timeout(Duration::from_secs(2)).unwrap();
        let t0 = Instant::now();
        let err = GpuYieldGuard::acquire_with_timeout(Duration::from_millis(300)).unwrap_err();
        let waited = t0.elapsed();
        assert!(waited >= Duration::from_millis(250), "returned too early");
        assert!(
            waited < Duration::from_secs(5),
            "did not honour the timeout"
        );
        let msg = err.to_string();
        assert!(msg.contains("GPU busy"), "unexpected message: {msg}");
        drop(held);
    }

    /// Two processes: a child process holds the GPU, the parent must block
    /// until the child exits. Proves exclusion is cross-process, not just
    /// cross-thread. (The child is this same test binary, filtered to this
    /// function and told to act as the holder.)
    #[test]
    fn cross_process_exclusion() {
        if std::env::var("GPN_GPU_LOCK_CHILD").is_ok() {
            let g = GpuYieldGuard::acquire_with_timeout(Duration::from_secs(10)).unwrap();
            println!("CHILD_HELD");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_millis(700));
            drop(g);
            println!("CHILD_RELEASED");
            std::io::stdout().flush().unwrap();
            return;
        }

        let _serial = serial();
        let _env = EnvGuard::new("procs");
        let exe = std::env::current_exe().unwrap();
        let mut child = std::process::Command::new(exe)
            .args([
                "gpu_yield::tests::cross_process_exclusion",
                "--exact",
                "--nocapture",
            ])
            .env("GPN_GPU_LOCK_CHILD", "1")
            .env("GPN_GPU_LOCK", lock_path())
            .env("GPN_GPU_YIELD", yield_flag_path())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        // Wait for the child to say it holds the GPU.
        let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        let mut held = false;
        for _ in 0..20 {
            line.clear();
            use std::io::BufRead;
            if out.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            if line.contains("CHILD_HELD") {
                held = true;
                break;
            }
        }
        assert!(held, "child never acquired the GPU lock");
        assert!(
            yield_flag_path().exists(),
            "child did not publish the miner flag"
        );

        // Parent must wait for the child (which holds ~700 ms).
        let t0 = Instant::now();
        let g = GpuYieldGuard::acquire_with_timeout(Duration::from_secs(10)).unwrap();
        let waited = t0.elapsed();
        assert!(
            waited >= Duration::from_millis(400),
            "parent acquired while the child held the GPU (waited {waited:?})"
        );
        eprintln!(
            "[test] parent waited {waited:?} for the child-process lock (internal {:?})",
            g.acquired_after
        );
        drop(g);
        let _ = child.wait();
    }

    /// Miner-side contract: the node reads the flag path with plain `exists()`
    /// and never takes the lock. Taking the lock must not be required for the
    /// node's check to work, and the flag must be readable as a plain file.
    #[test]
    fn miner_sees_plain_flag_file_not_the_lock() {
        let _serial = serial();
        let env = EnvGuard::new("miner");
        let g = GpuYieldGuard::acquire_with_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(std::fs::read_to_string(&env.yield_flag).unwrap(), "1");
        // The lock file is a different path; the node never touches it.
        assert_ne!(env.yield_flag, env.lock);
        let _ = std::fs::read(&env.lock).unwrap();
        drop(g);
        assert!(!env.yield_flag.exists());
    }
}
