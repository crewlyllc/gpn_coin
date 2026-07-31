/// Compute job executor — manages Docker-based AI inference/training jobs

use crate::config::AgentConfig;
use std::collections::HashMap;

/// A running compute job
pub struct RunningJob {
    pub job_id: [u8; 32],
    pub container_id: String,
    pub started_at: std::time::Instant,
    pub gpu_index: u8,
}

/// Docker executor for compute jobs
pub struct DockerExecutor {
    config: AgentConfig,
    active_jobs: HashMap<[u8; 32], RunningJob>,
}

impl DockerExecutor {
    pub fn new(config: &AgentConfig) -> Self {
        Self {
            config: config.clone(),
            active_jobs: HashMap::new(),
        }
    }

    /// Start a compute job in a Docker container
    pub async fn start_job(
        &mut self,
        job_id: [u8; 32],
        docker_image: &str,
        entrypoint: &str,
        _args: &[String],
        gpu_index: u8,
    ) -> Result<RunningJob, Box<dyn std::error::Error>> {
        log::info!(
            "Starting compute job: image={}, entrypoint={}, gpu={}",
            docker_image, entrypoint, gpu_index
        );

        // In production: use bollard (Docker API) to:
        // 1. Pull image
        // 2. Create container with GPU passthrough
        // 3. Set resource limits
        // 4. Start container
        // 5. Monitor execution

        let job = RunningJob {
            job_id,
            container_id: format!("gpn-job-{:02x}{:02x}{:02x}{:02x}", job_id[0], job_id[1], job_id[2], job_id[3]),
            started_at: std::time::Instant::now(),
            gpu_index,
        };

        self.active_jobs.insert(job_id, RunningJob {
            job_id,
            container_id: String::new(),
            started_at: std::time::Instant::now(),
            gpu_index,
        });

        Ok(job)
    }

    /// Stop a running compute job
    pub async fn stop_job(&mut self, job_id: &[u8; 32]) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(job) = self.active_jobs.remove(job_id) {
            log::info!("Stopping compute job container: {}", job.container_id);
            // docker stop + docker rm
        }
        Ok(())
    }

    /// Get number of active jobs
    pub fn active_job_count(&self) -> usize {
        self.active_jobs.len()
    }

    /// Check if we can accept more jobs
    pub fn can_accept_jobs(&self) -> bool {
        self.active_jobs.len() < self.config.max_concurrent_jobs as usize
    }
}
