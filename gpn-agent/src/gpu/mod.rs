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
        let o = Command::new("rocm-smi")
            .arg("--showdriverversion")
            .output()
            .ok()?;
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
    if n.contains("intel") && (n.contains("uhd") || n.contains("hd graphics") || n.contains("iris"))
    {
        return "integrated";
    }
    if n.contains("radeon")
        && (n.contains("graphics")
            || n.contains("vega")
            || n.contains("660m")
            || n.contains("680m")
            || n.contains("780m")
            || n.contains("760m"))
    {
        return "integrated";
    }
    if n.contains("apple")
        && (n.contains("m1") || n.contains("m2") || n.contains("m3") || n.contains("m4"))
    {
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
        ("5090", 105.0),
        ("5080", 56.0),
        ("5070 ti", 44.0),
        ("5070", 30.0),
        ("5060", 18.0),
        // NVIDIA RTX 40-series
        ("4090", 82.6),
        ("4080", 48.7),
        ("4070 ti", 40.0),
        ("4070", 29.1),
        ("4060 ti", 22.0),
        ("4060", 15.1),
        // NVIDIA RTX 30-series
        ("3090", 35.6),
        ("3080 ti", 34.1),
        ("3080", 29.8),
        ("3070 ti", 21.7),
        ("3070", 20.3),
        ("3060 ti", 16.2),
        ("3060", 12.7),
        // NVIDIA RTX 20-series / GTX 16
        ("2080 ti", 13.4),
        ("2080", 10.1),
        ("2070", 7.5),
        ("2060", 6.5),
        ("1660 ti", 5.4),
        ("1660", 5.0),
        ("1650", 2.9),
        // Data-center NVIDIA
        ("h100", 67.0),
        ("a100", 19.5),
        ("l40", 90.0),
        ("v100", 15.7),
        ("t4", 8.1),
        ("p100", 9.3),
        ("a10", 31.2),
        // AMD Radeon RX 7000-series (RDNA3) — specific SKU before the bare
        // family number so e.g. "7900 XTX" (61) isn't shadowed by "7900".
        ("7900 xtx", 61.0),
        ("7900 xt", 52.0),
        ("7900", 52.0),
        ("7800 xt", 37.0),
        ("7800", 37.0),
        ("7700 xt", 35.0),
        ("7700", 35.0),
        ("7600 xt", 22.0),
        ("7600", 21.7),
        // AMD Radeon RX 6000-series (RDNA2)
        ("6950 xt", 23.0),
        ("6950", 23.0),
        ("6900 xt", 23.0),
        ("6900", 23.0),
        ("6800 xt", 20.7),
        ("6800", 20.7),
        ("6700 xt", 13.2),
        ("6700", 13.2),
        ("6650 xt", 11.4),
        ("6600 xt", 10.6),
        ("6600", 8.9),
        // AMD Radeon RX 5000-series (RDNA1)
        ("5700 xt", 9.8),
        ("5700", 7.9),
        ("5600 xt", 7.2),
        ("5600", 6.4),
        // AMD Vega (discrete) + Radeon Pro / Instinct (workstation dGPU)
        ("vega 64", 12.6),
        ("vega 56", 10.5),
        ("radeon pro", 20.0),
        ("instinct mi250", 45.3),
        ("instinct mi210", 22.6),
        ("instinct mi100", 23.1),
        ("instinct", 23.0),
        // Intel Arc (discrete)
        ("arc a770", 19.2),
        ("arc a750", 17.2),
        ("arc a380", 3.9),
        // Integrated (APU) reference figures — never clear a ~3 TFLOPS floor.
        ("iris xe", 2.0),
        ("iris", 1.8),
        ("uhd", 0.5),
        ("hd graphics", 0.4),
        ("vega 11", 1.8),
        ("vega 10", 1.7),
        ("vega 8", 1.6),
        ("vega 7", 1.5),
        ("radeon 890m", 4.3),
        ("radeon 880m", 4.0),
        ("radeon 780m", 4.3),
        ("radeon 760m", 3.4),
        ("radeon 680m", 3.4),
        ("radeon 660m", 1.7),
        ("radeon graphics", 1.8),
        ("780m", 4.3),
        ("680m", 3.4),
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
        after.split([',', '"']).next().unwrap_or("")
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

/// Split one CSV line into fields, honouring double-quoted fields (so a
/// `DriverDesc` containing a comma stays one cell) and `""` escapes.
fn split_csv_fields(line: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes => {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            }
            '"' => in_quotes = true,
            ',' if !in_quotes => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}

/// Parse a byte/second count that may be written as a plain integer or, for a
/// few Windows drivers, in float/exponent form (`8.589934592E+09`). Returns
/// `None` for `N/A`, empty, non-numeric or non-positive values.
fn parse_size_number(s: &str) -> Option<u64> {
    let s = s.trim().trim_matches('"').trim();
    if s.is_empty() || s.eq_ignore_ascii_case("n/a") || s.eq_ignore_ascii_case("null") {
        return None;
    }
    if let Ok(v) = s.parse::<u64>() {
        return (v > 0).then_some(v);
    }
    let f: f64 = s.parse().ok()?;
    if !f.is_finite() || f <= 0.0 {
        return None;
    }
    let v = f as u64;
    (v > 0).then_some(v)
}

/// Parse `wmic path win32_VideoController get name,AdapterRAM /format:csv`
/// output (or the equivalent `Get-CimInstance … | ConvertTo-Csv` form) into
/// `(name, vram_mb)` pairs. Header-order agnostic (CSV columns are located by
/// name). **Caveat:** Windows exposes `AdapterRAM` as a **uint32**, so it
/// saturates at 4,294,967,295 bytes = **4095 MiB**: a 12 GB or 24 GB card
/// reports ~4 GB here. This is a *last-resort* source only — prefer
/// `nvidia-smi`/`rocm-smi` ([`parse_nvidia_smi_vram_mb`]) or the registry
/// `HardwareInformation.qwMemorySize` ([`parse_registry_qwmemorysize_mb`]).
pub fn parse_wmic_videocontroller(out: &str) -> Vec<(String, u64)> {
    let mut rows: Vec<(String, u64)> = Vec::new();
    let mut name_idx: Option<usize> = None;
    let mut ram_idx: Option<usize> = None;
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let cells = split_csv_fields(line);
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
        let ram_bytes: u64 = cells
            .get(ri)
            .and_then(|s| parse_size_number(s))
            .unwrap_or(0);
        rows.push((name.clone(), ram_bytes / (1024 * 1024)));
    }
    rows
}

/// Largest plausible single-GPU VRAM, in MiB (1 TiB). Anything above this is a
/// wrapped/garbage registry value, not a real card.
const VRAM_SANITY_MAX_MB: u64 = 1024 * 1024;

/// Parse `nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits`
/// (or the ROCm `rocm-smi --showmeminfo vram` view, via
/// [`parse_rocm_smi_vram_mb`]) into the **largest** GPU's total VRAM in MiB.
/// The value is a true 64-bit figure — no 4 GiB cap. `N/A`, empty and
/// non-numeric lines (a driver that cannot report) are skipped.
pub fn parse_nvidia_smi_vram_mb(out: &str) -> Option<u64> {
    let mut best: Option<u64> = None;
    for line in out.lines() {
        // One row per GPU; take the biggest (a box may mix sizes).
        let Some(mb) = parse_size_number(line) else {
            continue;
        };
        if mb > VRAM_SANITY_MAX_MB {
            continue;
        }
        best = Some(best.map_or(mb, |b: u64| b.max(mb)));
    }
    best
}

/// Parse the PowerShell CSV for the display-class registry key
/// `…\Control\Class\{4d36e968-…}\0*` with columns
/// `DriverDesc, HardwareInformation.qwMemorySize` into the largest **discrete**
/// adapter's VRAM in MiB.
///
/// `qwMemorySize` is a **REG_QWORD** (64-bit), so unlike `AdapterRAM` it reports
/// 12 GiB / 24 GiB / 48 GiB cards correctly. Rows whose `DriverDesc`
/// classifies as integrated (APU/iGPU share system RAM and report a bogus small
/// value) are dropped, as are zero, `N/A` and implausible (>1 TiB) values.
pub fn parse_registry_qwmemorysize_mb(out: &str) -> Option<u64> {
    let mut desc_idx: Option<usize> = None;
    let mut size_idx: Option<usize> = None;
    let mut best: Option<u64> = None;
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let cells = split_csv_fields(line);
        if size_idx.is_none() {
            for (i, c) in cells.iter().enumerate() {
                let cl = c.to_ascii_lowercase();
                if cl == "driverdesc" {
                    desc_idx = Some(i);
                } else if cl.contains("qwmemorysize") {
                    size_idx = Some(i);
                }
            }
            if size_idx.is_some() {
                continue; // header consumed
            }
        }
        let Some(si) = size_idx else { continue };
        let Some(bytes) = cells.get(si).and_then(|s| parse_size_number(s)) else {
            continue;
        };
        let mb = bytes / (1024 * 1024);
        if mb == 0 || mb > VRAM_SANITY_MAX_MB {
            continue;
        }
        // Ignore iGPUs: they report a BIOS-reserved slice, not real VRAM.
        if let Some(di) = desc_idx {
            if let Some(desc) = cells.get(di) {
                if classify_gpu_name(desc) == "integrated" {
                    continue;
                }
            }
        }
        best = Some(best.map_or(mb, |b: u64| b.max(mb)));
    }
    best
}

/// Where a Windows VRAM figure came from. Ordered by trustworthiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VramSource {
    /// `nvidia-smi --query-gpu=memory.total` — true 64-bit value.
    NvidiaSmi,
    /// `rocm-smi --showmeminfo vram` — true 64-bit value.
    RocmSmi,
    /// Registry `HardwareInformation.qwMemorySize` — 64-bit, per adapter.
    Registry,
    /// `Win32_VideoController.AdapterRAM` — **uint32, caps at 4095 MiB**.
    AdapterRam,
    /// Nothing usable was found.
    None,
}

/// Result of a VRAM probe: the figure plus where it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VramProbe {
    pub total_mb: u64,
    pub source: VramSource,
}

/// Message emitted when only the 32-bit `AdapterRAM` field was usable.
pub const ADAPTERRAM_CAP_WARNING: &str = "[gpu] VRAM from AdapterRAM (32-bit, capped at 4095 MiB) — value may understate the real total; models larger than 4 GB may be wrongly excluded";

/// Message emitted when nothing at all could be detected.
pub const VRAM_ZERO_WARNING: &str = "[gpu] VRAM detection returned 0 MiB (no nvidia-smi, rocm-smi, registry qwMemorySize or AdapterRAM value) — VRAM is NOT advertised and the VRAM model gate is disabled, so oversized models will not be blocked";

impl VramProbe {
    pub fn unknown() -> Self {
        Self {
            total_mb: 0,
            source: VramSource::None,
        }
    }

    /// True when the figure came from the capped 32-bit field.
    pub fn is_capped(&self) -> bool {
        self.total_mb > 0 && self.source == VramSource::AdapterRam
    }

    /// Emit the operator-facing warning for a degraded probe (capped or zero).
    /// Deduped per process so a heartbeat loop does not spam the log.
    pub fn warn_if_degraded(&self) {
        use std::sync::Once;
        static CAPPED: Once = Once::new();
        static ZERO: Once = Once::new();
        if self.is_capped() {
            CAPPED.call_once(|| log::warn!("{}", ADAPTERRAM_CAP_WARNING));
        } else if self.total_mb == 0 {
            ZERO.call_once(|| log::warn!("{}", VRAM_ZERO_WARNING));
        }
    }
}

/// Windows detection order, first success wins: `nvidia-smi` → `rocm-smi` →
/// registry `qwMemorySize` → `AdapterRAM`. Each input is the *already
/// validated* (`> 0`) figure from that source for the **largest** adapter.
///
/// Split out as a pure function so the ordering is unit-testable on Linux CI,
/// where the `cfg(windows)` probes cannot run.
pub fn choose_windows_vram(
    nvidia_smi_mb: Option<u64>,
    rocm_smi_mb: Option<u64>,
    registry_mb: Option<u64>,
    adapterram_mb: Option<u64>,
) -> VramProbe {
    let pick = |mb: Option<u64>, source: VramSource| {
        mb.filter(|v| *v > 0)
            .map(|total_mb| VramProbe { total_mb, source })
    };
    pick(nvidia_smi_mb, VramSource::NvidiaSmi)
        .or_else(|| pick(rocm_smi_mb, VramSource::RocmSmi))
        .or_else(|| pick(registry_mb, VramSource::Registry))
        .or_else(|| pick(adapterram_mb, VramSource::AdapterRam))
        .unwrap_or_else(VramProbe::unknown)
}

/// PowerShell: per-adapter 64-bit VRAM from the display driver class key.
#[cfg(windows)]
const PS_QW_MEMORY_SIZE: &str = "Get-ItemProperty -Path 'HKLM:\\SYSTEM\\CurrentControlSet\\Control\\Class\\{4d36e968-e325-11ce-bfc1-08002be10318}\\0*' -ErrorAction SilentlyContinue | Where-Object { $_.'HardwareInformation.qwMemorySize' -ne $null } | Select-Object DriverDesc, 'HardwareInformation.qwMemorySize' | ConvertTo-Csv -NoTypeInformation";

/// PowerShell: adapter names only (`AdapterRAM` deliberately not requested —
/// classification never needs it, and the field is 32-bit capped).
#[cfg(windows)]
const PS_ADAPTER_NAMES: &str =
    "Get-CimInstance Win32_VideoController | Select-Object Name | ConvertTo-Csv -NoTypeInformation";

#[cfg(windows)]
fn run_powershell(script: &str) -> Option<String> {
    let o = Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()
        .ok()?;
    if !o.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&o.stdout).to_string())
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

/// Best-effort total VRAM (MiB) of the local GPU, vendor-agnostic, plus the
/// source it came from. Detection order (first success wins, largest GPU wins):
///
/// 1. `nvidia-smi --query-gpu=memory.total` — true 64-bit value.
/// 2. `rocm-smi --showmeminfo vram` — true 64-bit value (AMD).
/// 3. Windows only: registry `HardwareInformation.qwMemorySize` (REG_QWORD,
///    64-bit) per display adapter, iGPUs skipped.
/// 4. Windows only: `Win32_VideoController.AdapterRAM` — **uint32, caps at
///    4095 MiB**, used only as a last resort and always flagged as capped.
///
/// `total_mb == 0` means nothing could be detected (the heartbeat then treats
/// VRAM as "not advertised"). Degraded probes are logged, never silent.
pub fn detect_vram() -> VramProbe {
    #[cfg(not(windows))]
    let probe = {
        let nv = Command::new("nvidia-smi")
            .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| parse_nvidia_smi_vram_mb(&String::from_utf8_lossy(&o.stdout)));
        let amd = Command::new("rocm-smi")
            .args(["--showmeminfo", "vram"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| parse_rocm_smi_vram_mb(&String::from_utf8_lossy(&o.stdout)));
        choose_windows_vram(nv, amd, None, None)
    };
    #[cfg(windows)]
    let probe = {
        let nv = Command::new("nvidia-smi")
            .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| parse_nvidia_smi_vram_mb(&String::from_utf8_lossy(&o.stdout)));
        let amd = Command::new("rocm-smi")
            .args(["--showmeminfo", "vram"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| parse_rocm_smi_vram_mb(&String::from_utf8_lossy(&o.stdout)));
        // 64-bit per-adapter value; the only trustworthy Windows source when no
        // vendor CLI is installed.
        let reg =
            run_powershell(PS_QW_MEMORY_SIZE).and_then(|s| parse_registry_qwmemorysize_mb(&s));
        // Last resort: 32-bit, caps at 4095 MiB.
        let cached = run_powershell("Get-CimInstance Win32_VideoController | Select-Object Name,AdapterRAM | ConvertTo-Csv -NoTypeInformation")
            .and_then(|s| {
                parse_wmic_videocontroller(&s)
                    .into_iter()
                    .map(|(_, mb)| mb)
                    .filter(|mb| *mb > 0)
                    .max()
            });
        choose_windows_vram(nv, amd, reg, cached)
    };
    probe.warn_if_degraded();
    probe
}

/// Best-effort total VRAM in MiB. See [`detect_vram`] for the detection order;
/// `0` means unknown (the heartbeat treats 0 as "not advertised").
pub fn detect_vram_mb() -> u64 {
    detect_vram().total_mb
}

/// Adapter names reported by Windows (`Win32_VideoController`), used only for
/// class/TFLOPS classification. Deliberately **does not** request `AdapterRAM`:
/// that field is 32-bit capped and must never be paired with a name as if it
/// were the card's real VRAM.
#[cfg(windows)]
fn win_adapter_names() -> Vec<String> {
    if let Some(s) = run_powershell(PS_ADAPTER_NAMES) {
        let rows: Vec<String> = parse_wmic_videocontroller(&s)
            .into_iter()
            .map(|(n, _)| n)
            .filter(|n| !n.is_empty())
            .collect();
        if !rows.is_empty() {
            return rows;
        }
        // CIM name-only CSV has no AdapterRAM column, so parse the plain CSV.
        let names: Vec<String> = s
            .lines()
            .skip(1)
            .filter_map(|l| split_csv_fields(l).into_iter().next())
            .filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case("name"))
            .collect();
        if !names.is_empty() {
            return names;
        }
    }
    // Fallback: the classic `wmic` CSV form (older Windows without CIM).
    let o = Command::new("wmic")
        .args([
            "path",
            "win32_VideoController",
            "get",
            "name",
            "/format:csv",
        ])
        .output();
    if let Ok(o) = o {
        if o.status.success() {
            let names: Vec<String> = String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| split_csv_fields(l).into_iter().last())
                .filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case("Name"))
                .collect();
            if !names.is_empty() {
                return names;
            }
        }
    }
    Vec::new()
}

#[cfg(windows)]
fn win_detect() -> (String, f64) {
    // `Win32_VideoController` lists every video adapter (dGPU + iGPU). Only the
    // names are needed here — the VRAM figure comes from `detect_vram()`, which
    // does not rely on the 32-bit-capped `AdapterRAM` field.
    let names = win_adapter_names();
    if !names.is_empty() {
        return scan_names(names.iter().map(|n| n.as_str()));
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
            if let Some(name) = line.split_once(':').map(|x| x.1) {
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
        assert_eq!(
            classify_gpu_name("AMD Radeon Vega 8 Graphics"),
            "integrated"
        );
        assert_eq!(classify_gpu_name("Apple M2 Pro"), "integrated");
        // Unknown / nonsense.
        assert_eq!(classify_gpu_name(""), "unknown");
        assert_eq!(
            classify_gpu_name("Microsoft Basic Display Adapter"),
            "unknown"
        );
    }

    #[test]
    fn tflops_estimates_are_ordered_and_igpus_stay_low() {
        // A 4090 out-rates a 4060; both are well above an iGPU.
        assert!(
            estimate_tflops("NVIDIA GeForce RTX 4090") > estimate_tflops("NVIDIA GeForce RTX 4060")
        );
        assert!(
            estimate_tflops("RTX 4060") >= 3.0,
            "discrete budget card clears a 3 TFLOPS floor"
        );
        // Integrated parts never clear the ~3 TFLOPS floor.
        for igpu in [
            "Intel(R) UHD Graphics 630",
            "Intel Iris Xe Graphics",
            "AMD Radeon(TM) Graphics",
        ] {
            assert!(
                estimate_tflops(igpu) < 3.0,
                "{igpu} must stay under the floor"
            );
        }
        assert_eq!(estimate_tflops("Some Unknown Adapter"), 0.0);
    }

    #[test]
    fn scan_names_prefers_discrete_when_both_present() {
        // A laptop with an iGPU + a dGPU must classify as discrete.
        let (class, tflops) = scan_names(
            [
                "Intel(R) Iris Xe Graphics",
                "NVIDIA GeForce RTX 4060 Laptop GPU",
            ]
            .into_iter(),
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
            assert!(
                estimate_tflops(name) < 5.0,
                "{name} is an iGPU (low TFLOPS)"
            );
        }
        for apu in ["AMD Radeon(TM) Graphics", "AMD Radeon Vega 8 Graphics"] {
            assert!(
                estimate_tflops(apu) < 3.0,
                "{apu} must stay under the floor"
            );
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
        let json =
            r#"{"card0": {"Card series": "Navi 32 [Radeon RX 7800 XT]", "Card model": "0x747e"}}"#;
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

    // ── Windows VRAM detection order (4095 MiB AdapterRAM cap fix) ───────────

    #[test]
    fn nvidia_smi_vram_parses_true_values_above_4gb() {
        // The whole point: 32-bit-era 4095 is NOT a ceiling here.
        for (out, want) in [
            ("4096\n", 4096u64),
            ("8192\n", 8192),
            ("12288\n", 12288),
            ("24576\n", 24576),
            (" 24576 \r\n", 24576),
        ] {
            assert_eq!(parse_nvidia_smi_vram_mb(out), Some(want), "{out:?}");
        }
        // Multi-GPU: the largest card wins.
        assert_eq!(parse_nvidia_smi_vram_mb("8192\n24576\n"), Some(24576));
        assert_eq!(parse_nvidia_smi_vram_mb("24576\n8192\n"), Some(24576));
    }

    #[test]
    fn nvidia_smi_vram_ignores_garbage_and_zeros() {
        assert_eq!(parse_nvidia_smi_vram_mb(""), None);
        assert_eq!(parse_nvidia_smi_vram_mb("N/A\n"), None);
        assert_eq!(parse_nvidia_smi_vram_mb("[N/A]\n"), None);
        assert_eq!(parse_nvidia_smi_vram_mb("0\n"), None);
        assert_eq!(parse_nvidia_smi_vram_mb("unknown\n\n  \n"), None);
        // A mixed stream keeps only the usable rows.
        assert_eq!(parse_nvidia_smi_vram_mb("N/A\n12288\n"), Some(12288));
    }

    #[test]
    fn registry_qwmemorysize_handles_12gb_and_24gb() {
        // PowerShell `Select-Object DriverDesc,'HardwareInformation.qwMemorySize'`
        // piped through ConvertTo-Csv.
        let csv = "\"DriverDesc\",\"HardwareInformation.qwMemorySize\"\n\
\"NVIDIA GeForce RTX 3060\",\"12884901888\"\n\
\"Intel(R) UHD Graphics 630\",\"134217728\"\n";
        // 12884901888 B = 12288 MiB (12 GiB) — impossible via AdapterRAM.
        assert_eq!(parse_registry_qwmemorysize_mb(csv), Some(12288));

        let csv24 = "\"DriverDesc\",\"HardwareInformation.qwMemorySize\"\n\
\"NVIDIA GeForce RTX 4090\",\"25769803776\"\n";
        // 25769803776 B = 24576 MiB (24 GiB).
        assert_eq!(parse_registry_qwmemorysize_mb(csv24), Some(24576));
    }

    #[test]
    fn registry_qwmemorysize_skips_igpus_zeros_and_garbage() {
        // iGPU-only values must not be reported for the box.
        let igpu = "\"DriverDesc\",\"HardwareInformation.qwMemorySize\"\n\
\"Intel(R) Iris Xe Graphics\",\"1073741824\"\n\
\"AMD Radeon(TM) Graphics\",\"536870912\"\n";
        assert_eq!(parse_registry_qwmemorysize_mb(igpu), None);
        // dGPU + iGPU → the dGPU wins.
        let mixed = "\"DriverDesc\",\"HardwareInformation.qwMemorySize\"\n\
\"Intel(R) UHD Graphics 630\",\"134217728\"\n\
\"AMD Radeon RX 7800 XT\",\"17179869184\"\n";
        assert_eq!(parse_registry_qwmemorysize_mb(mixed), Some(16384));
        // Zero / N/A / empty / non-numeric rows are ignored.
        let junk = "\"DriverDesc\",\"HardwareInformation.qwMemorySize\"\n\
\"Microsoft Basic Display Adapter\",\"0\"\n\
\"NVIDIA GeForce RTX 4060\",\"N/A\"\n\
\"Some Adapter\",\"\"\n\
\"Other Adapter\",\"not-a-number\"\n";
        assert_eq!(parse_registry_qwmemorysize_mb(junk), None);
        // No header at all → nothing parsed (fail closed, no blind guess).
        assert_eq!(parse_registry_qwmemorysize_mb("garbage line"), None);
        // Comma inside DriverDesc must not shift the numeric column.
        let comma = "\"DriverDesc\",\"HardwareInformation.qwMemorySize\"\n\
\"NVIDIA GeForce RTX 4090, OEM\",\"25769803776\"\n";
        assert_eq!(parse_registry_qwmemorysize_mb(comma), Some(24576));
    }

    #[test]
    fn adapterram_is_flagged_capped_and_is_last_resort() {
        let csv = "\"Name\",\"AdapterRAM\"\n\"NVIDIA GeForce RTX 4090\",\"4294967295\"\n";
        let rows = parse_wmic_videocontroller(csv);
        let mb = rows.iter().map(|(_, m)| *m).max();
        let probe = choose_windows_vram(None, None, None, mb);
        assert_eq!(probe.total_mb, 4095);
        assert_eq!(probe.source, VramSource::AdapterRam);
        assert!(probe.is_capped(), "AdapterRAM-sourced VRAM must be flagged");
        // Sanity: the exact operator-facing line.
        assert_eq!(
            ADAPTERRAM_CAP_WARNING,
            "[gpu] VRAM from AdapterRAM (32-bit, capped at 4095 MiB) — value may understate the real total; models larger than 4 GB may be wrongly excluded"
        );
    }

    #[test]
    fn detection_order_prefers_nvidia_smi_then_registry_then_adapterram() {
        // nvidia-smi beats everything (true 24 GB, not the capped 4095).
        let p = choose_windows_vram(Some(24576), Some(12288), Some(24576), Some(4095));
        assert_eq!((p.total_mb, p.source), (24576, VramSource::NvidiaSmi));
        assert!(!p.is_capped());
        // rocm-smi beats the Windows-only paths.
        let p = choose_windows_vram(None, Some(16384), Some(24576), Some(4095));
        assert_eq!((p.total_mb, p.source), (16384, VramSource::RocmSmi));
        // No vendor CLI → registry (64-bit) beats AdapterRAM.
        let p = choose_windows_vram(None, None, Some(12288), Some(4095));
        assert_eq!((p.total_mb, p.source), (12288, VramSource::Registry));
        assert!(!p.is_capped());
        // Nothing else → AdapterRAM, capped.
        let p = choose_windows_vram(None, None, None, Some(2048));
        assert_eq!((p.total_mb, p.source), (2048, VramSource::AdapterRam));
        assert!(p.is_capped());
        // Zero is never a "success" — it must not mask a missing probe.
        let p = choose_windows_vram(None, None, Some(0), Some(0));
        assert_eq!(p, VramProbe::unknown());
        assert_eq!(p.source, VramSource::None);
        assert_eq!(p.total_mb, 0);
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
