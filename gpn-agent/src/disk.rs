//! Disk-headroom reporting (item #43).
//!
//! 2026-10-19: the root filesystem hit 100% (Rust `target/debug` caches = 217 GB)
//! and the ONLY symptom was a chain halt — the node mined a block but could not
//! persist its header (ENOSPC), leaving an unfetchable best-chain hash. Nothing
//! warned before writes started failing. This module gives the heartbeat (and the
//! supervisor) a cheap, dependency-free way to report free space so an operator /
//! monitor can alert at <10% instead of discovering it after a halt.
//!
//! `0` means "unknown" (the probe failed) — deliberately NOT reported as full, so
//! a platform whose probe is unavailable never looks like an outage.

/// Free/total space of the filesystem holding a path, in MiB (0 = unknown).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiskSpace {
    pub free_mb: u64,
    pub total_mb: u64,
}

impl DiskSpace {
    /// Free percentage (integer, rounded down). `None` when the probe failed.
    pub fn free_percent(&self) -> Option<u64> {
        if self.total_mb == 0 {
            None
        } else {
            Some(self.free_mb.saturating_mul(100) / self.total_mb)
        }
    }

    pub fn is_known(&self) -> bool {
        self.total_mb > 0
    }
}

/// Space of the filesystem holding `path` (default `.` when empty).
pub fn disk_space(path: &str) -> DiskSpace {
    let path = if path.is_empty() { "." } else { path };
    if cfg!(windows) {
        windows_disk_space(path)
    } else {
        unix_disk_space(path)
    }
}

fn unix_disk_space(path: &str) -> DiskSpace {
    // `-P` = POSIX output (one line per fs, never wrapped), `-k` = 1024-byte blocks.
    let out = std::process::Command::new("df")
        .arg("-Pk")
        .arg(path)
        .output();
    match out {
        Ok(o) if o.status.success() => {
            parse_df_output(&String::from_utf8_lossy(&o.stdout)).unwrap_or_default()
        }
        _ => DiskSpace::default(),
    }
}

/// Parse `df -Pk` output into MiB. Pure so it is unit-testable.
pub fn parse_df_output(out: &str) -> Option<DiskSpace> {
    // Filesystem 1024-blocks Used Available Capacity Mounted-on
    // Take the LAST non-empty line: the header is line 1 and a path with spaces
    // can only affect the final field, never the numeric ones.
    let line = out.lines().rfind(|l| !l.trim().is_empty())?;
    let f: Vec<&str> = line.split_whitespace().collect();
    if f.len() < 4 {
        return None;
    }
    let total_kb: u64 = f[1].parse().ok()?;
    let free_kb: u64 = f[3].parse().ok()?;
    Some(DiskSpace {
        total_mb: total_kb / 1024,
        free_mb: free_kb / 1024,
    })
}

fn windows_disk_space(path: &str) -> DiskSpace {
    // Get-PSDrive covers the drive of the given path; fall back to the system drive.
    let drive = path
        .chars()
        .next()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_string())
        .unwrap_or_else(|| "C".into());
    let script = format!(
        "$d=Get-PSDrive -Name '{drive}'; Write-Output ($d.Free); Write-Output ($d.Used+$d.Free)"
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            parse_windows_free_total(&String::from_utf8_lossy(&o.stdout)).unwrap_or_default()
        }
        _ => DiskSpace::default(),
    }
}

/// Parse the two lines printed above (`free bytes`, `total bytes`) into MiB.
pub fn parse_windows_free_total(out: &str) -> Option<DiskSpace> {
    let nums: Vec<u64> = out
        .lines()
        .filter_map(|l| l.trim().parse::<u64>().ok())
        .collect();
    if nums.len() < 2 {
        return None;
    }
    Some(DiskSpace {
        free_mb: nums[0] / (1024 * 1024),
        total_mb: nums[1] / (1024 * 1024),
    })
}

/// Default low-space threshold (percent free) used by the heartbeat warning.
pub const LOW_DISK_PERCENT: u64 = 10;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_posix_df_output() {
        let out = "Filesystem     1024-blocks      Used Available Capacity Mounted on\n\
                   /dev/sdb3        460143344 208000000 230000000      48% /\n";
        let d = parse_df_output(out).expect("parsed");
        assert_eq!(d.total_mb, 460143344 / 1024);
        assert_eq!(d.free_mb, 230000000 / 1024);
        assert_eq!(d.free_percent(), Some(49));
        assert!(d.is_known());
    }

    #[test]
    fn parses_windows_free_total_and_rejects_garbage() {
        let d = parse_windows_free_total("107374182400\n536870912000\n").unwrap();
        assert_eq!(d.free_mb, 102400);
        assert_eq!(d.total_mb, 512000);
        assert!(parse_windows_free_total("nope\n").is_none());
        assert!(parse_df_output("").is_none());
        assert!(parse_df_output("Filesystem 1024-blocks\n").is_none());
    }

    #[test]
    fn unknown_probe_is_zero_and_never_looks_full() {
        let d = DiskSpace::default();
        assert!(!d.is_known());
        assert_eq!(d.free_percent(), None);
    }

    #[test]
    fn real_probe_returns_a_sane_filesystem() {
        // Lives on the host's real root FS; skip only if `df` is unavailable.
        let d = disk_space(".");
        if d.is_known() {
            assert!(d.free_mb <= d.total_mb);
            assert!(d.free_percent().unwrap() <= 100);
        }
    }
}
