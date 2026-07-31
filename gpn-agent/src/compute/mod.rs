//! Compute executor — runs real AI-compute workloads on the GPU.
//!
//! The headline auto-switch feature needs `Compute` mode to genuinely occupy
//! the GPU, exactly as mining does. The [`executor::ComputeExecutor`] engine
//! runs a dense float32 GEMM (the core primitive of neural inference) on a
//! dedicated thread, mirroring the mining engine, and is started/stopped by
//! [`crate::modes::ModeManager`].
//!
//! Production note: a full marketplace would pull Docker images and run
//! customer workloads in containers with GPU passthrough (bollard / nvidia
//! container runtime). This engine is the working substrate — real GPU
//! compute, measurable throughput — that the container orchestration layer
//! would drive. Docker is not required for the prototype.

pub mod executor;

pub use executor::{ComputeConfig, ComputeExecutor};
