/// Utility functions for the GPU node agent

/// Cross-platform temporary directory for GPN scratch/artifact files.
///
/// Uses `std::env::temp_dir()` (honours `TMPDIR` / `TMP` / `TEMP`), falling
/// back to the OS default. Prefer this over hard-coded `/tmp`, which does not
/// exist on Windows.
pub fn temp_dir() -> std::path::PathBuf {
    std::env::temp_dir()
}

/// Path to a GPN temp file, e.g. `gpn_temp("gpn_ft_req_5.json")`.
pub fn temp_path(name: &str) -> std::path::PathBuf {
    temp_dir().join(name)
}

pub mod crypto {
    use sha2::{Digest, Sha256};

    /// Sign a message with the node's private key
    pub fn sign_message(_message: &[u8], _private_key: &[u8]) -> Result<[u8; 64], String> {
        // In production: use ed25519 or sr25519 signing from sp-core
        Ok([0u8; 64])
    }

    /// Verify a signature
    pub fn verify_signature(_message: &[u8], signature: &[u8; 64], _public_key: &[u8]) -> bool {
        // In production: use sp-core verification
        !signature.iter().all(|&b| b == 0)
    }

    /// Hash bytes to 32-byte digest
    pub fn hash_bytes(data: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hasher.finalize().into()
    }
}

pub mod metrics {
    /// Simple metrics collector (Prometheus-compatible)
    pub struct MetricsCollector {
        pub mode_switches: u64,
        pub jobs_completed: u64,
        pub attestations_submitted: u64,
        pub mining_hashes_computed: u64,
        pub uptime_seconds: u64,
    }

    impl Default for MetricsCollector {
        fn default() -> Self {
            Self::new()
        }
    }

    impl MetricsCollector {
        pub fn new() -> Self {
            Self {
                mode_switches: 0,
                jobs_completed: 0,
                attestations_submitted: 0,
                mining_hashes_computed: 0,
                uptime_seconds: 0,
            }
        }

        pub fn record_mode_switch(&mut self) {
            self.mode_switches += 1;
        }

        pub fn record_job_completed(&mut self) {
            self.jobs_completed += 1;
        }

        pub fn record_attestation(&mut self) {
            self.attestations_submitted += 1;
        }

        pub fn to_prometheus(&self) -> String {
            format!(
                r#"# HELP gpn_mode_switches_total Total mode switches
# TYPE gpn_mode_switches_total counter
gpn_mode_switches_total {}
# HELP gpn_jobs_completed_total Total compute jobs completed
# TYPE gpn_jobs_completed_total counter
gpn_jobs_completed_total {}
# HELP gpn_attestations_submitted_total Total attestations submitted
# TYPE gpn_attestations_submitted_total counter
gpn_attestations_submitted_total {}
# HELP gpn_uptime_seconds Agent uptime
# TYPE gpn_uptime_seconds gauge
gpn_uptime_seconds {}
"#,
                self.mode_switches,
                self.jobs_completed,
                self.attestations_submitted,
                self.uptime_seconds,
            )
        }
    }
}
