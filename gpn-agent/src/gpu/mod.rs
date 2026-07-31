/// GPU monitoring module
/// Interfaces with nvidia-smi or rocm-smi to get GPU telemetry

use serde::{Deserialize, Serialize};
use std::process::Command;

pub mod ethash_miner;
pub mod matmul;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GPUInfo {
    pub model_name: String,
    pub driver_version: String,
    pub vram_total_bytes: u64,
    pub vram_used_mb: u64,
    pub temperature_celsius: f32,
    pub gpu_utilization_percent: u8,
    pub current_hashrate_mhs: f64,
    pub compute_capability: String,
    pub cuda_version: Option<String>,
    pub gpu_count: u8,
}

#[derive(Clone)]
pub struct GPUMonitor;

impl GPUMonitor {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self)
    }

    pub fn get_gpu_info(&self) -> Result<GPUInfo, Box<dyn std::error::Error>> {
        // Try nvidia-smi first, fall back to ROCm
        if let Ok(info) = Self::query_nvidia_smi() {
            return Ok(info);
        }

        // Fallback: return default GPU info
        Ok(GPUInfo {
            model_name: "Unknown GPU".to_string(),
            driver_version: "unknown".to_string(),
            vram_total_bytes: 8 * 1024 * 1024 * 1024, // 8GB default
            vram_used_mb: 512,
            temperature_celsius: 50.0,
            gpu_utilization_percent: 0,
            current_hashrate_mhs: 0.0,
            compute_capability: "8.0".to_string(),
            cuda_version: None,
            gpu_count: 1,
        })
    }

    fn query_nvidia_smi() -> Result<GPUInfo, Box<dyn std::error::Error>> {
        let output = Command::new("nvidia-smi")
            .args(["--query-gpu=name,driver_version,memory.total,memory.used,temperature.gpu,utilization.gpu,compute_cap",
                   "--format=csv,noheader,nounits"])
            .output()?;

        if !output.status.success() {
            return Err("nvidia-smi failed or not found".into());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.lines().next().ok_or("No GPU data")?;
        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();

        Ok(GPUInfo {
            model_name: parts.first().unwrap_or(&"Unknown").to_string(),
            driver_version: parts.get(1).unwrap_or(&"unknown").to_string(),
            vram_total_bytes: parse_mem(parts.get(2).unwrap_or(&"0")) * 1024 * 1024,
            vram_used_mb: parse_mem(parts.get(3).unwrap_or(&"0")),
            temperature_celsius: parts.get(4).unwrap_or(&"50").parse()?,
            gpu_utilization_percent: parts.get(5).unwrap_or(&"0").parse()?,
            current_hashrate_mhs: 0.0,
            compute_capability: parts.get(6).unwrap_or(&"8.0").to_string(),
            cuda_version: Self::get_cuda_version(),
            gpu_count: stdout.lines().count() as u8,
        })
    }

    fn get_cuda_version() -> Option<String> {
        if let Ok(output) = Command::new("nvidia-smi").arg("--version").output() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout.lines().find(|l| l.contains("CUDA Version"))
                .map(|l| l.split(':').last().unwrap_or("").trim().to_string())
        } else {
            None
        }
    }

    /// Compute a GPU challenge for attestation (placeholder)
    pub async fn compute_challenge(&self, challenge: &[u8; 32]) -> Result<[u8; 32], Box<dyn std::error::Error>> {
        // In production: run actual GPU computation (matrix multiply, etc.)
        // For development: use SHA-256 as placeholder
        use sha2::{Sha256, Digest};
        let mut hasher = Sha256::new();
        hasher.update(challenge);
        let result = hasher.finalize();
        Ok(result.into())
    }
}

/// Measure real FP32 compute throughput (GFLOPS) with a short dense-GEMM
/// benchmark on the given OpenCL device. This is the number submitted on-chain
/// as the **attestable compute metric** (`AttestationRecord::compute_gflops`),
/// tying the AI-compute leg to the marketplace's `min_gflops` requirement.
pub fn measure_gflops(device_index: usize) -> Result<u64, String> {
    let mut mm = matmul::GpuMatmul::new(device_index, 1024)
        .map_err(|e| format!("open GPU matmul: {e}"))?;
    // 4 measured iters (1024³ GEMM ≈ 2.1 GFLOP each); warms up the kernel too.
    let (_elapsed, gflops) = mm.run_many(4).map_err(|e| format!("GEMM benchmark: {e}"))?;
    Ok(gflops.round() as u64)
}

pub fn hash_gpu_info(info: &GPUInfo) -> [u8; 32] {
    let serialized = serde_json::to_vec(info).unwrap_or_default();
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(&serialized);
    hasher.finalize().into()
}

fn parse_mem(s: &str) -> u64 {
    s.replace("MiB", "").replace("MiB/s", "").trim().parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_gpu_info() {
        let info = GPUInfo {
            model_name: "NVIDIA RTX 4090".into(),
            driver_version: "535.154.05".into(),
            vram_total_bytes: 24_000_000_000,
            vram_used_mb: 1024,
            temperature_celsius: 65.0,
            gpu_utilization_percent: 95,
            current_hashrate_mhs: 120.5,
            compute_capability: "8.9".into(),
            cuda_version: Some("12.2".into()),
            gpu_count: 1,
        };
        let hash = hash_gpu_info(&info);
        assert_eq!(hash.len(), 32);
    }
}
