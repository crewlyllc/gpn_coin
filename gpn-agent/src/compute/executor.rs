//! Compute executor — a *stoppable* engine that runs a **real AI-compute
//! workload** on the GPU, mirroring the `ExternalMiner` mining engine.
//!
//! This is the compute-side leg of the headline **auto-switch** feature: when
//! the agent switches to `Compute` mode, this engine starts and genuinely
//! occupies the GPU with a dense float32 GEMM (the core primitive of neural
//! inference). When it switches away, the engine stops and releases the GPU
//! back to mining / idle.
//!
//! Like the miner, the workload runs on a dedicated OS thread (the OpenCL
//! handles are moved into the thread at spawn). The engine reports measured
//! throughput (GFLOPS) so the agent can prove real compute is being served.

use crate::gpu::matmul::GpuMatmul;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Configuration for the compute engine.
#[derive(Clone, Debug)]
pub struct ComputeConfig {
    /// OpenCL device index (0 = first GPU).
    pub device_index: usize,
    /// GEMM size N (NxNxN matmul). Default 2048 (~8.6 GFLOP / matmul).
    pub matrix_size: usize,
    /// Matmuls per reporting batch (controls log cadence).
    pub batch: usize,
}

impl ComputeConfig {
    pub fn new(device_index: usize) -> Self {
        Self {
            device_index,
            matrix_size: 2048,
            batch: 4,
        }
    }
}

/// Handle to a running compute loop. Dropping the handle does **not** stop the
/// job — call [`ComputeExecutor::stop`] to cancel it and release the GPU.
pub struct ComputeExecutor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    device_name: String,
    /// Latest measured GFLOPS, updated by the loop; read for diagnostics.
    gflops: Arc<std::sync::Mutex<f64>>,
}

impl ComputeExecutor {
    /// Open the GPU, allocate matrices, and start running matmuls on a
    /// background thread. Returns immediately; compute proceeds in background.
    pub fn start(cfg: ComputeConfig) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let gflops = Arc::new(std::sync::Mutex::new(0.0f64));

        // Fail fast on a missing GPU before spawning the thread.
        let mut mm = GpuMatmul::new(cfg.device_index, cfg.matrix_size)
            .map_err(|e| format!("open GPU matmul: {e}"))?;
        let device_name = mm.device_name();

        // Warm-up + self-check: one matmul + readback so a broken kernel is
        // caught at start, not mid-session.
        mm.run_once().map_err(|e| format!("warm-up matmul: {e}"))?;

        let stop_clone = stop.clone();
        let gflops_clone = gflops.clone();
        let thread = std::thread::Builder::new()
            .name("gpn-compute".into())
            .spawn(move || {
                compute_loop(mm, cfg, stop_clone, gflops_clone);
            })
            .map_err(|e| format!("spawn compute thread: {e}"))?;

        Ok(Self {
            stop,
            thread: Some(thread),
            device_name,
            gflops,
        })
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// True if the loop is still running.
    pub fn is_running(&self) -> bool {
        self.thread
            .as_ref()
            .map(|t| !t.is_finished())
            .unwrap_or(false)
    }

    /// Latest measured GFLOPS (0.0 until the first batch completes).
    pub fn gflops(&self) -> f64 {
        *self.gflops.lock().unwrap()
    }

    /// Cancel the compute loop and wait for the thread to exit, releasing the
    /// GPU. Bounded wait (~5s) so a stuck kernel cannot block forever.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !t.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(25));
            }
            if t.is_finished() {
                let _ = t.join();
                log::info!("compute engine stopped — GPU released for mining/idle");
            } else {
                log::warn!("compute engine did not stop within 5s — detaching (GPU may stay busy)");
            }
        }
    }
}

/// The compute loop body. Runs on the dedicated thread: repeatedly executes a
/// batch of matmuls, reports GFLOPS, and checks the stop flag so cancellation
/// is responsive.
fn compute_loop(
    mut mm: GpuMatmul,
    cfg: ComputeConfig,
    stop: Arc<AtomicBool>,
    gflops: Arc<std::sync::Mutex<f64>>,
) {
    let n = mm.n();
    let flops_per_matmul = 2.0 * (n as f64).powi(3);
    let mut total_flops: f64 = 0.0;
    let session_start = Instant::now();
    let mut report_start = Instant::now();

    log::info!(
        "compute loop started: {}x{} GEMM on {}, serving AI inference jobs",
        n, n, mm.device_name()
    );

    while !stop.load(Ordering::SeqCst) {
        let (elapsed, _) = match mm.run_many(cfg.batch) {
            Ok(v) => v,
            Err(e) => {
                log::error!("matmul error: {e}");
                // Small interruptible backoff then retry.
                if wait_interruptible(&stop, 500) {
                    break;
                }
                continue;
            }
        };
        total_flops += flops_per_matmul * cfg.batch as f64;

        // Report throughput roughly every 2s.
        if report_start.elapsed().as_secs() >= 2 {
            let g = total_flops / session_start.elapsed().as_secs_f64();
            *gflops.lock().unwrap() = g;
            log::info!("  [compute] {:.2} GFLOPS ({} matmuls, {}s)", g, cfg.batch, elapsed.as_millis());
            report_start = Instant::now();
        }
    }

    log::info!("compute loop exited after serving {} GFLOP", total_flops / 1e9);
}

/// Sleep for `ms` ms, but return early (true) if `stop` becomes set.
fn wait_interruptible(stop: &AtomicBool, ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        if stop.load(Ordering::SeqCst) {
            return true;
        }
        let rem = deadline.saturating_duration_since(Instant::now()).as_millis() as u64;
        std::thread::sleep(Duration::from_millis(rem.min(10).max(1)));
    }
    false
}
