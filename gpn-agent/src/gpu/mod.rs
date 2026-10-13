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
        // Try nvidia-smi first, fall back to ROCm (AMD).
        if let Ok(info) = Self::query_nvidia_smi() {
            return Ok(info);
        }
        if let Ok(info) = Self::query_rocm_smi() {
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

    /// AMD telemetry via `rocm-smi` (the ROCm/OpenCL vendor CLI). Model +
    /// total VRAM are authoritative; the instantaneous metrics (temp/util) are
    /// left at 0 when unavailable rather than invented.
    fn query_rocm_smi() -> Result<GPUInfo, Box<dyn std::error::Error>> {
        let name_out = Command::new("rocm-smi").arg("--showproductname").output()?;
        if !name_out.status.success() {
            return Err("rocm-smi failed or not found".into());
        }
        let name = parse_rocm_smi_name(&String::from_utf8_lossy(&name_out.stdout))
            .ok_or("rocm-smi: no AMD GPU product name")?;
        let vram_mb = Command::new("rocm-smi")
            .args(["--showmeminfo", "vram"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| parse_rocm_smi_vram_mb(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or(0);
        Ok(GPUInfo {
            model_name: name,
            driver_version: Self::get_rocm_version().unwrap_or_else(|| "rocm".to_string()),
            vram_total_bytes: vram_mb * 1024 * 1024,
            vram_used_mb: 0,
            temperature_celsius: 0.0,
            gpu_utilization_percent: 0,
            current_hashrate_mhs: 0.0,
            compute_capability: String::new(),
            cuda_version: None,
            gpu_count: 1,
        })
    }

    /// ROCm driver version via `rocm-smi --showdriverversion`, if present.
    fn get_rocm_version() -> Option<String> {
        let o = Command::new("rocm-smi").arg("--showdriverversion").output().ok()?;
        if !o.status.success() {
            return None;
        }
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .find(|l| l.to_ascii_lowercase().contains("driver version"))
            .and_then(last_colon_value)
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
        // AMD Radeon RX 7000-series (RDNA3) — specific SKU before the bare
        // family number so e.g. "7900 XTX" (61) isn't shadowed by "7900".
        ("7900 xtx", 61.0), ("7900 xt", 52.0), ("7900", 52.0),
        ("7800 xt", 37.0), ("7800", 37.0),
        ("7700 xt", 35.0), ("7700", 35.0),
        ("7600 xt", 22.0), ("7600", 21.7),
        // AMD Radeon RX 6000-series (RDNA2)
        ("6950 xt", 23.0), ("6950", 23.0), ("6900 xt", 23.0), ("6900", 23.0),
        ("6800 xt", 20.7), ("6800", 20.7),
        ("6700 xt", 13.2), ("6700", 13.2),
        ("6650 xt", 11.4), ("6600 xt", 10.6), ("6600", 8.9),
        // AMD Radeon RX 5000-series (RDNA1)
        ("5700 xt", 9.8), ("5700", 7.9), ("5600 xt", 7.2), ("5600", 6.4),
        // AMD Vega (discrete) + Radeon Pro / Instinct (workstation dGPU)
        ("vega 64", 12.6), ("vega 56", 10.5), ("radeon pro", 20.0),
        ("instinct mi250", 45.3), ("instinct mi210", 22.6), ("instinct mi100", 23.1),
        ("instinct", 23.0),
        // Intel Arc (discrete)
        ("arc a770", 19.2), ("arc a750", 17.2), ("arc a380", 3.9),
        // Integrated (APU) reference figures — never clear a ~3 TFLOPS floor.
        ("iris xe", 2.0), ("iris", 1.8), ("uhd", 0.5), ("hd graphics", 0.4),
        ("vega 11", 1.8), ("vega 10", 1.7), ("vega 8", 1.6), ("vega 7", 1.5),
        ("radeon 890m", 4.3), ("radeon 880m", 4.0), ("radeon 780m", 4.3), ("radeon 760m", 3.4),
        ("radeon 680m", 3.4), ("radeon 660m", 1.7), ("radeon graphics", 1.8),
        ("780m", 4.3), ("680m", 3.4),
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
    // 2. AMD: `rocm-smi` (installed with the ROCm/OpenCL runtime) reports the
    //    product name directly — the authoritative AMD model string.
    if let Some(name) = rocm_smi_name() {
        return (classify_gpu_name(&name).to_string(), estimate_tflops(&name));
    }
    // 3. Fall back to an `lspci` display-controller scan (covers AMD + Intel
    //    integrated APUs even when neither vendor CLI is installed).
    if let Ok(o) = Command::new("lspci").output() {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout);
            return scan_display_lines(s.lines());
        }
    }
    ("unknown".to_string(), 0.0)
}

#[cfg(not(windows))]
fn rocm_smi_name() -> Option<String> {
    let o = Command::new("rocm-smi")
        .args(["--showproductname"])
        .output()
        .ok()?;
    if !o.status.success() {
        return None;
    }
    parse_rocm_smi_name(&String::from_utf8_lossy(&o.stdout))
}

/// Extract the value that immediately follows `marker` on a line. Handles the
/// classic `… : Card series: <value>` form and the JSON `"Card series": "<value>"`
/// form (value ends at the next `,` or quote), skipping an optional `:` and
/// interleaved `(B)`-style tokens.
fn value_after_marker(line: &str, marker: &str) -> Option<String> {
    let idx = line.to_ascii_lowercase().find(marker)? + marker.len();
    let rest = &line[idx..];
    // The value starts after the next ':' (classic and JSON both use one).
    let after = match rest.find(':') {
        Some(c) => &rest[c + 1..],
        None => rest,
    };
    let after = after.trim_start();
    let val = if let Some(s) = after.strip_prefix('"') {
        s.split('"').next().unwrap_or("")
    } else {
        after.split(|c: char| c == ',' || c == '"').next().unwrap_or("")
    };
    let v = val.trim();
    if v.is_empty() {
        None
    } else {
        Some(v.to_string())
    }
}

/// Parse `rocm-smi --showproductname` output into a GPU model string.
///
/// Handles both the classic line format
/// `GPU[0] : Card series: Navi 31 [Radeon RX 7900 XTX]`
/// and the newer `--json` form `"Card series": "Navi 31 [Radeon RX 7900 XTX]"`.
/// Prefers *Card series* (human name) over *Card model* (a raw PCI id).
pub fn parse_rocm_smi_name(out: &str) -> Option<String> {
    let mut model: Option<String> = None;
    for line in out.lines() {
        if line.to_ascii_lowercase().contains("card series") {
            if let Some(v) = value_after_marker(line, "card series") {
                return Some(v);
            }
        } else if line.to_ascii_lowercase().contains("card model") && model.is_none() {
            model = value_after_marker(line, "card model");
        }
    }
    model
}

/// Parse `rocm-smi --showmeminfo vram` output into total VRAM MiB.
///
/// Classic: `GPU[0] : VRAM Total Memory (B): 25753026560`; newer JSON:
/// `"VRAM Total Memory (B)": 25753026560`. (`VRAM Total Used Memory` is ignored.)
pub fn parse_rocm_smi_vram_mb(out: &str) -> Option<u64> {
    for line in out.lines() {
        if line.to_ascii_lowercase().contains("vram total memory") {
            if let Some(v) = value_after_marker(line, "vram total memory") {
                let bytes: u64 = v.trim().trim_end_matches(',').parse().ok()?;
                return Some(bytes / (1024 * 1024));
            }
        }
    }
    None
}

/// Parse `wmic path win32_VideoController get name,AdapterRAM /format:csv`
/// output (or the equivalent `Get-CimInstance … | ConvertTo-Csv` form) into
/// `(name, vram_mb)` pairs. Header-order agnostic (CSV columns are located by
/// name). **Caveat:** Windows exposes `AdapterRAM` as a 32-bit field, so cards
/// with >4 GiB report a capped/wrapped value here — the Linux `rocm-smi` path
/// (or a `--vram-mb` override) gives the true figure.
pub fn parse_wmic_videocontroller(out: &str) -> Vec<(String, u64)> {
    let mut rows: Vec<(String, u64)> = Vec::new();
    let mut name_idx: Option<usize> = None;
    let mut ram_idx: Option<usize> = None;
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let cells: Vec<String> = line
            .split(',')
            .map(|c| c.trim().trim_matches('"').trim().to_string())
            .collect();
        if name_idx.is_none() {
            // Header row: locate the columns by name.
            for (i, c) in cells.iter().enumerate() {
                let cl = c.to_ascii_lowercase();
                if cl == "name" {
                    name_idx = Some(i);
                } else if cl.contains("adapterram") {
                    ram_idx = Some(i);
                }
            }
            if name_idx.is_some() {
                continue;
            }
        }
        let (Some(ni), Some(ri)) = (name_idx, ram_idx) else {
            continue;
        };
        let Some(name) = cells.get(ni) else { continue };
        if name.is_empty() {
            continue;
        }
        let ram_bytes: u64 = cells.get(ri).and_then(|s| s.parse().ok()).unwrap_or(0);
        rows.push((name.clone(), ram_bytes / (1024 * 1024)));
    }
    rows
}

/// The value after the final `:` on a line, with surrounding quotes/commas
/// stripped (works for both the classic and the JSON `rocm-smi` forms).
fn last_colon_value(line: &str) -> Option<String> {
    let v = line
        .rsplit(':')
        .next()?
        .trim()
        .trim_matches('"')
        .trim_end_matches(',')
        .trim();
    if v.is_empty() {
        None
    } else {
        Some(v.to_string())
    }
}

/// Best-effort total VRAM (MiB) of the local discrete GPU, vendor-agnostic.
/// NVIDIA via `nvidia-smi`; AMD via `rocm-smi`; Windows via `Win32_VideoController`
/// (4 GiB-capped). `0` = unknown (caller decides; the heartbeat treats 0 as
/// "not advertised").
pub fn detect_vram_mb() -> u64 {
    #[cfg(not(windows))]
    {
        if let Ok(o) = Command::new("nvidia-smi")
            .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
            .output()
        {
            if o.status.success() {
                if let Some(l) = String::from_utf8_lossy(&o.stdout).lines().next() {
                    if let Ok(mb) = l.trim().parse::<u64>() {
                        if mb > 0 {
                            return mb;
                        }
                    }
                }
            }
        }
        if let Ok(o) = Command::new("rocm-smi")
            .args(["--showmeminfo", "vram"])
            .output()
        {
            if o.status.success() {
                if let Some(mb) = parse_rocm_smi_vram_mb(&String::from_utf8_lossy(&o.stdout)) {
                    if mb > 0 {
                        return mb;
                    }
                }
            }
        }
    }
    #[cfg(windows)]
    {
        if let Ok(o) = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Get-CimInstance Win32_VideoController | Select-Object Name,AdapterRAM | ConvertTo-Csv -NoTypeInformation",
            ])
            .output()
        {
            if o.status.success() {
                return parse_wmic_videocontroller(&String::from_utf8_lossy(&o.stdout))
                    .into_iter()
                    .map(|(_, mb)| mb)
                    .max()
                    .unwrap_or(0);
            }
        }
    }
    0
}

#[cfg(windows)]
fn win_detect() -> (String, f64) {
    // `Win32_VideoController` lists every video adapter (dGPU + iGPU); request
    // Name + AdapterRAM as CSV so we can also read the (4 GiB-capped) VRAM.
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_VideoController | Select-Object Name,AdapterRAM | ConvertTo-Csv -NoTypeInformation",
        ])
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            let v = parse_wmic_videocontroller(&String::from_utf8_lossy(&o.stdout));
            if !v.is_empty() {
                return scan_names(v.iter().map(|(n, _)| n.as_str()));
            }
        }
    }
    // Fallback: the classic `wmic` CSV form (older Windows without CIM).
    let out = Command::new("wmic")
        .args([
            "path",
            "win32_VideoController",
            "get",
            "name,AdapterRAM",
            "/format:csv",
        ])
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            let v = parse_wmic_videocontroller(&String::from_utf8_lossy(&o.stdout));
            if !v.is_empty() {
                return scan_names(v.iter().map(|(n, _)| n.as_str()));
            }
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

    // ── AMD support (queue item #27) ────────────────────────────────────────

    #[test]
    fn amd_name_maps_to_class_and_tflops() {
        // Discrete AMD dGPUs → "discrete" + datasheet FP32 TFLOPS.
        let discrete = [
            ("AMD Radeon RX 7900 XTX", 61.0),
            ("AMD Radeon RX 7900 XT", 52.0),
            ("AMD Radeon RX 7800 XT", 37.0),
            ("AMD Radeon RX 7700 XT", 35.0),
            ("AMD Radeon RX 7600", 21.7),
            ("AMD Radeon RX 6900 XT", 23.0),
            ("AMD Radeon RX 6800 XT", 20.7),
            ("AMD Radeon RX 6700 XT", 13.2),
            ("AMD Radeon RX 6600", 8.9),
            ("AMD Radeon Pro W6800", 20.7),
            ("AMD Instinct MI250X", 45.3),
        ];
        for (name, tf) in discrete {
            assert_eq!(classify_gpu_name(name), "discrete", "{name}");
            assert!(
                (estimate_tflops(name) - tf).abs() < 0.05,
                "{name} → {} (want {tf})",
                estimate_tflops(name)
            );
        }
        // Integrated AMD APUs → "integrated". Older/Basic APUs sit well under
        // the 3 TFLOPS floor; a modern RDNA3 iGPU (Radeon 780M ≈4.3) is still
        // low but can clear it — the gate is `discrete OR tflops≥floor`.
        let integrated = [
            "AMD Radeon(TM) Graphics",
            "AMD Radeon Vega 8 Graphics",
            "AMD Radeon Graphics (Ryzen 7000)",
            "AMD Radeon 780M",
            "AMD Radeon 680M",
        ];
        for name in integrated {
            assert_eq!(classify_gpu_name(name), "integrated", "{name}");
            assert!(estimate_tflops(name) < 5.0, "{name} is an iGPU (low TFLOPS)");
        }
        for apu in ["AMD Radeon(TM) Graphics", "AMD Radeon Vega 8 Graphics"] {
            assert!(estimate_tflops(apu) < 3.0, "{apu} must stay under the floor");
        }
        // an unknown AMD-ish string stays unknown (must clear the floor to earn).
        assert_eq!(classify_gpu_name("AMD Something"), "unknown");
    }

    #[test]
    fn amd_discrete_clears_floor_but_amdgpu_igpu_does_not() {
        // The min-GPU gate (#19) `discrete OR tflops≥floor`: an AMD dGPU is
        // admitted by the class flag alone; an AMD APU is excluded by both.
        let (class, tf) = scan_names(["AMD Radeon RX 7900 XTX"].into_iter());
        assert_eq!(class, "discrete");
        assert!(tf >= 3.0);
        let (class, tf) = scan_names(["AMD Radeon Graphics (Ryzen 7000)"].into_iter());
        assert_eq!(class, "integrated");
        assert!(tf < 3.0);
    }

    #[test]
    fn parse_rocm_smi_name_reads_card_series() {
        // Classic line format.
        let classic = "============================ ROCm System Management Interface ============================\n\
GPU[0]\t\t: Card series: \t\tNavi 31 [Radeon RX 7900 XTX]\n\
GPU[0]\t\t: Card model: \t\t0x744c\n\
GPU[0]\t\t: Card vendor: \t\tAdvanced Micro Devices, Inc. [AMD/ATI]\n";
        assert_eq!(
            parse_rocm_smi_name(classic).as_deref(),
            Some("Navi 31 [Radeon RX 7900 XTX]")
        );
        // Newer JSON form.
        let json = r#"{"card0": {"Card series": "Navi 32 [Radeon RX 7800 XT]", "Card model": "0x747e"}}"#;
        assert_eq!(
            parse_rocm_smi_name(json).as_deref(),
            Some("Navi 32 [Radeon RX 7800 XT]")
        );
        // Only a model id available.
        let model_only = "GPU[0]: Card model: 0x67df";
        assert_eq!(parse_rocm_smi_name(model_only).as_deref(), Some("0x67df"));
        assert_eq!(parse_rocm_smi_name("no gpu here"), None);
    }

    #[test]
    fn parse_rocm_smi_vram_reads_total_mib() {
        let classic = "GPU[0]\t\t: VRAM Total Memory (B): 25753026560\n\
GPU[0]\t\t: VRAM Total Used Memory (B): 12345678\n";
        // 25753026560 / 1048576 = 24560 MiB.
        assert_eq!(parse_rocm_smi_vram_mb(classic), Some(24560));
        let json = r#"{"card0": {"VRAM Total Memory (B)": "17163091968"}}"#;
        assert_eq!(parse_rocm_smi_vram_mb(json), Some(16368));
        assert_eq!(parse_rocm_smi_vram_mb("nothing"), None);
    }

    #[test]
    fn parse_wmic_videocontroller_reads_name_and_ram() {
        // wmic /format:csv (Node,AdapterRAM,Name — alphabetical).
        let csv = "Node,AdapterRAM,Name\n\
DESKTOP-ABC,4294967295,AMD Radeon RX 7900 XTX\n\
DESKTOP-ABC,2147483648,AMD Radeon(TM) Graphics\n";
        let v = parse_wmic_videocontroller(csv);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].0, "AMD Radeon RX 7900 XTX");
        assert_eq!(v[0].1, 4095); // 4294967295 B ≈ 4095 MiB (32-bit cap)
        assert_eq!(v[1].0, "AMD Radeon(TM) Graphics");
        assert_eq!(v[1].1, 2048);
        // Get-CimInstance | ConvertTo-Csv order (Name,AdapterRAM).
        let cim = "\"Name\",\"AdapterRAM\"\n\"NVIDIA GeForce RTX 4060\",\"8589934592\"\n";
        let v = parse_wmic_videocontroller(cim);
        assert_eq!(v, vec![("NVIDIA GeForce RTX 4060".to_string(), 8192)]);
        // Empty / header-only input yields no rows.
        assert!(parse_wmic_videocontroller("Node,AdapterRAM,Name\n").is_empty());
    }

    #[test]
    fn wmic_names_fold_into_a_capability() {
        // A Windows AMD box (dGPU + iGPU) → discrete, AMD dGPU TFLOPS.
        let csv = "Node,AdapterRAM,Name\n\
PC,4294967295,AMD Radeon RX 7800 XT\n\
PC,2147483648,AMD Radeon(TM) Graphics\n";
        let rows = parse_wmic_videocontroller(csv);
        let (class, tflops) = scan_names(rows.iter().map(|(n, _)| n.as_str()));
        assert_eq!(class, "discrete");
        assert!((tflops - 37.0).abs() < 0.05);
    }
}
