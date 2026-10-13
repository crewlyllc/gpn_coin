//! GPU yield flag (one GPU = one job).
//!
//! While a provider runs an AI job it holds this file flag for the job's WHOLE
//! duration; the co-located node's miner watches the same path and **fully stops**
//! mining (searches no nonces) for as long as the flag is present, resuming the
//! instant it clears. This dedicates the GPU to the job instead of duty-cycling
//! against block production.
//!
//! Acquire with `GpuYieldGuard::acquire()`: the flag is written on construction
//! and cleared on `Drop`, so it is held on **every** return path (accept →
//! complete/fail/error, including early `?`/`return`). The node reads the same
//! path via `$GPN_GPU_YIELD` or `~/.gpn/gpu-yield`.

use std::path::{Path, PathBuf};

/// Path of the yield flag the node's miner watches (env-aware, matching the
/// node's `gpu_yield_requested()`): `$GPN_GPU_YIELD` if set, else `~/.gpn/gpu-yield`.
pub fn yield_flag_path() -> PathBuf {
    if let Ok(p) = std::env::var("GPN_GPU_YIELD") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".gpn/gpu-yield")
}

/// RAII hold on the GPU yield flag: writes it on creation, clears it on drop.
pub struct GpuYieldGuard {
    path: PathBuf,
}

impl GpuYieldGuard {
    /// Set the flag and hold it until this guard drops (job duration).
    pub fn acquire() -> Self {
        let path = yield_flag_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, b"1");
        Self { path }
    }
}

impl Drop for GpuYieldGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
