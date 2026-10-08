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
            stdout
                .lines()
                .find(|l| l.contains("CUDA Version"))
                .map(|l| l.split(':').next_back().unwrap_or("").trim().to_string())
        } else {
            None
        }
    }

    /// Compute a GPU challenge for attestation (placeholder)
    pub async fn compute_challenge(
        &self,
        challenge: &[u8; 32],
    ) -> Result<[u8; 32], Box<dyn std::error::Error>> {
        // In production: run actual GPU computation (matrix multiply, etc.)
        // For development: use SHA-256 as placeholder
        use sha2::{Digest, Sha256};
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
    let mut mm =
        matmul::GpuMatmul::new(device_index, 1024).map_err(|e| format!("open GPU matmul: {e}"))?;
    // 4 measured iters (1024³ GEMM ≈ 2.1 GFLOP each); warms up the kernel too.
    let (_elapsed, flops) = mm.run_many(4).map_err(|e| format!("GEMM benchmark: {e}"))?;
    // `run_many` returns raw FLOP/s (total_flops / elapsed, no /1e9). Convert to
    // true GFLOPS so the on-chain `compute_gflops` is in GFLOPS — matching the
    // attestation tier thresholds (Bronze≥100 / Silver≥1000 / Gold≥5000 GFLOPS)
    // and the marketplace `min_gflops` gate. (Previously this leaked raw FLOP/s
    // ≈ GFLOPS×1e9 on-chain, silently breaking the `min_gflops` match.)
    Ok((flops / 1_000_000_000.0).round() as u64)
}

pub fn hash_gpu_info(info: &GPUInfo) -> [u8; 32] {
    let serialized = serde_json::to_vec(info).unwrap_or_default();
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&serialized);
    hasher.finalize().into()
}

fn parse_mem(s: &str) -> u64 {
    s.replace("MiB", "")
        .replace("MiB/s", "")
        .trim()
        .parse()
        .unwrap_or(0)
}

// ── Minimum GPU capability gate (queue item 19) ─────────────────────────────
//
// GPN must be served by a *real* (discrete) GPU. A CPU/APU **integrated** GPU
// may only earn if it clears an objective FP32-TFLOPS floor. The authoritative
// compute metric is the *measured* GFLOPS attested on-chain; the figures here
// are an advisory best-effort hint for the heartbeat / routing gate:
//   gpu_class ∈ { "discrete", "integrated", "unknown" }
//   tflops    = estimated FP32 TFLOP/s (0.0 = unknown)
//
// `classify_gpu_name` / `estimate_tflops` are pure (unit-tested); the OS probes
// just feed them a model string (`nvidia-smi` / `lspci` on Linux,
// `Win32_VideoController` on Windows).

/// Classify a GPU model name as `"discrete"`, `"integrated"` or `"unknown"`.
///
/// Heuristic on vendor + model substring. Discrete: NVIDIA GeForce/Quadro/Tesla
/// (RTX/GTX), AMD Radeon RX/Pro/Instinct, Intel Arc. Integrated: Intel
/// UHD/HD/Iris graphics, AMD Radeon *Graphics* / Vega* APUs. Anything else
/// (incl. virtual/softwarerenders) stays `unknown` and must clear the TFLOPS
/// floor to be eligible.
pub fn classify_gpu_name(name: &str) -> &'static str {
    let n = name.to_ascii_lowercase();
    if n.is_empty() {
        return "unknown";
    }
    // Integrated first (some APU names also contain "radeon").
    if n.contains("intel") && (n.contains("uhd") || n.contains("hd graphics") || n.contains("iris")) {
        return "integrated";
    }
    if n.contains("radeon") && (n.contains("graphics") || n.contains("vega") || n.contains("660m") || n.contains("680m") || n.contains("780m") || n.contains("760m")) {
        return "integrated";
    }
    if n.contains("apple") && (n.contains("m1") || n.contains("m2") || n.contains("m3") || n.contains("m4")) {
        return "integrated";
    }
    if n.contains("tegra") || n.contains("mali") || n.contains("adreno") {
        return "integrated";
    }
    // Discrete.
    if n.contains("geforce")
        || n.contains("rtx")
        || n.contains("gtx")
        || n.contains("quadro")
        || n.contains("tesla")
        || n.contains(" radeon rx")
        || n.contains("radeon pro")
        || n.contains("instinct")
        || n.contains("arc a")
        || n.contains("arc b")
        || n.contains("data center gpu")
        || n.contains("nvidia a")
        || n.contains("nvidia h")
        || n.contains("nvidia l")
        || n.contains("nvidia t4")
        || n.contains("nvidia v100")
        || n.contains("nvidia p100")
    {
        return "discrete";
    }
    "unknown"
}

/// Rough datasheet **FP32** TFLOP/s for a GPU model name (0.0 = unknown).
///
/// Deliberately coarse — it exists only so the heartbeat/scheduler can reject
/// obvious integrated parts; the on-chain, challenge-backed `compute_gflops`
/// remains the authoritative figure. Matched by longest substring first.
pub fn estimate_tflops(name: &str) -> f64 {
    let n = name.to_ascii_lowercase();
    // (substring, FP32 TFLOP/s) — longest/most-specific first so e.g. "5090"
    // isn't shadowed by "50".
    const TABLE: &[(&str, f64)] = &[
        // NVIDIA RTX 50-series
        ("5090", 105.0), ("5080", 56.0), ("5070 ti", 44.0), ("5070", 30.0),
        ("5060", 18.0),
        // NVIDIA RTX 40-series
        ("4090", 82.6), ("4080", 48.7), ("4070 ti", 40.0), ("4070", 29.1),
        ("4060 ti", 22.0), ("4060", 15.1),
        // NVIDIA RTX 30-series
        ("3090", 35.6), ("3080 ti", 34.1), ("3080", 29.8), ("3070 ti", 21.7),
        ("3070", 20.3), ("3060 ti", 16.2), ("3060", 12.7),
        // NVIDIA RTX 20-series / GTX 16
        ("2080 ti", 13.4), ("2080", 10.1), ("2070", 7.5), ("2060", 6.5),
        ("1660 ti", 5.4), ("1660", 5.0), ("1650", 2.9),
        // Data-center NVIDIA
        ("h100", 67.0), ("a100", 19.5), ("l40", 90.0), ("v100", 15.7),
        ("t4", 8.1), ("p100", 9.3), ("a10", 31.2),
        // AMD Radeon RX
        ("7900", 61.0), ("7800", 37.0), ("7700", 29.0), ("7600", 17.0),
        ("6950", 23.0), ("6900", 23.0), ("6800", 16.2), ("6700", 13.4),
        ("6600", 8.9), ("5700", 7.9), ("5600", 6.4),
        // Intel Arc (discrete)
        ("arc a770", 19.2), ("arc a750", 17.2), ("arc a380", 3.9),
        // Integrated (APU) reference figures — never clear a ~3 TFLOPS floor.
        ("iris xe", 2.0), ("iris", 1.8), ("uhd", 0.5), ("hd graphics", 0.4),
        ("vega 8", 1.6), ("radeon graphics", 1.8), ("780m", 4.3), ("680m", 3.4),
    ];
    for (key, val) in TABLE {
        if n.contains(key) {
            return *val;
        }
    }
    0.0
}

/// Best-effort `(gpu_class, tflops)` probe for the local box.
///
/// Linux: `nvidia-smi` (a discrete NVIDIA ⇒ class `discrete`), else an `lspci`
/// VGA/3D scan classified by model name. Windows: `Win32_VideoController` via
/// PowerShell/CIM. Unknown hardware returns `("unknown", 0.0)` — the caller
/// decides whether that is acceptable (the routing gate fails closed on it when
/// a floor is set; the attestation path requires a measured floor anyway).
pub fn detect_gpu_capability() -> (String, f64) {
    #[cfg(windows)]
    {
        win_detect()
    }
    #[cfg(not(windows))]
    {
        unix_detect()
    }
}

#[cfg(not(windows))]
fn unix_detect() -> (String, f64) {
    // 1. A working `nvidia-smi` means a discrete NVIDIA card is present.
    if let Ok(o) = Command::new("nvidia-smi")
        .args(["--query-gpu=name", "--format=csv,noheader"])
        .output()
    {
        if o.status.success() {
            if let Some(line) = String::from_utf8_lossy(&o.stdout).lines().next() {
                let n = line.trim();
                if !n.is_empty() {
                    return ("discrete".to_string(), estimate_tflops(n));
                }
            }
        }
    }
    // 2. Fall back to an `lspci` display-controller scan.
    if let Ok(o) = Command::new("lspci").output() {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout);
            return scan_display_lines(s.lines());
        }
    }
    ("unknown".to_string(), 0.0)
}

#[cfg(windows)]
fn win_detect() -> (String, f64) {
    // `Win32_VideoController.Name` lists every video adapter (dGPU + iGPU).
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_VideoController | Select-Object -ExpandProperty Name",
        ])
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout);
            let names: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
            return scan_names(names.into_iter());
        }
    }
    ("unknown".to_string(), 0.0)
}

/// Classify raw `lspci` lines; keep the strongest (discrete beats integrated).
#[cfg(not(windows))]
fn scan_display_lines<'a>(lines: impl Iterator<Item = &'a str>) -> (String, f64) {
    let mut names: Vec<&str> = Vec::new();
    for line in lines {
        let l = line.to_ascii_lowercase();
        if l.contains("vga compatible controller")
            || l.contains("3d controller")
            || l.contains("display controller")
        {
            // `00:02.0 VGA compatible controller: Intel Corporation …`
            if let Some(name) = line.splitn(2, ':' ).nth(1) {
                names.push(name.trim());
            }
        }
    }
    scan_names(names.into_iter())
}

/// Fold a set of GPU model names into a single `(class, tflops)`: a discrete
/// adapter anywhere wins (a box with both a dGPU and an iGPU is still usable),
/// and we take the max TFLOPS seen.
fn scan_names<'a>(names: impl Iterator<Item = &'a str>) -> (String, f64) {
    let mut class = "unknown";
    let mut tflops = 0.0f64;
    for name in names {
        match classify_gpu_name(name) {
            "discrete" => {
                class = "discrete";
                tflops = tflops.max(estimate_tflops(name));
            }
            "integrated" => {
                if class != "discrete" {
                    class = "integrated";
                    tflops = tflops.max(estimate_tflops(name));
                }
            }
            _ => {}
        }
    }
    (class.to_string(), tflops)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_discrete_vs_integrated() {
        // Discrete.
        assert_eq!(classify_gpu_name("NVIDIA GeForce RTX 4060"), "discrete");
        assert_eq!(classify_gpu_name("NVIDIA GeForce RTX 5070 Ti"), "discrete");
        assert_eq!(classify_gpu_name("NVIDIA Quadro RTX 6000"), "discrete");
        assert_eq!(classify_gpu_name("AMD Radeon RX 7900 XTX"), "discrete");
        assert_eq!(classify_gpu_name("Intel Arc A770 Graphics"), "discrete");
        assert_eq!(classify_gpu_name("NVIDIA A100-SXM4-80GB"), "discrete");
        // Integrated (CPU/APU).
        assert_eq!(classify_gpu_name("Intel(R) UHD Graphics 630"), "integrated");
        assert_eq!(classify_gpu_name("Intel Iris Xe Graphics"), "integrated");
        assert_eq!(classify_gpu_name("AMD Radeon(TM) Graphics"), "integrated");
        assert_eq!(classify_gpu_name("AMD Radeon Vega 8 Graphics"), "integrated");
        assert_eq!(classify_gpu_name("Apple M2 Pro"), "integrated");
        // Unknown / nonsense.
        assert_eq!(classify_gpu_name(""), "unknown");
        assert_eq!(classify_gpu_name("Microsoft Basic Display Adapter"), "unknown");
    }

    #[test]
    fn tflops_estimates_are_ordered_and_igpus_stay_low() {
        // A 4090 out-rates a 4060; both are well above an iGPU.
        assert!(estimate_tflops("NVIDIA GeForce RTX 4090") > estimate_tflops("NVIDIA GeForce RTX 4060"));
        assert!(estimate_tflops("RTX 4060") >= 3.0, "discrete budget card clears a 3 TFLOPS floor");
        // Integrated parts never clear the ~3 TFLOPS floor.
        for igpu in ["Intel(R) UHD Graphics 630", "Intel Iris Xe Graphics", "AMD Radeon(TM) Graphics"] {
            assert!(estimate_tflops(igpu) < 3.0, "{igpu} must stay under the floor");
        }
        assert_eq!(estimate_tflops("Some Unknown Adapter"), 0.0);
    }

    #[test]
    fn scan_names_prefers_discrete_when_both_present() {
        // A laptop with an iGPU + a dGPU must classify as discrete.
        let (class, tflops) = scan_names(
            ["Intel(R) Iris Xe Graphics", "NVIDIA GeForce RTX 4060 Laptop GPU"].into_iter(),
        );
        assert_eq!(class, "discrete");
        assert!(tflops >= 3.0);
        // Integrated-only box (no discrete present).
        let (class2, tflops2) = scan_names(["Intel(R) UHD Graphics 620"].into_iter());
        assert_eq!(class2, "integrated");
        assert!(tflops2 < 3.0);
    }

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
