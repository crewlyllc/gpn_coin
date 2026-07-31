//! Real GPU AI-compute primitive: dense float32 GEMM via OpenCL.
//!
//! This is the compute-side counterpart to `ethash_miner`: it opens the GPU
//! through the same runtime-loaded OpenCL path (no CUDA toolchain / C headers
//! required), compiles `matmul_kernel.cl` at runtime, and runs a real
//! `C = A * B` matrix multiply — the core primitive of neural-network
//! inference (MLP / attention linear layers). The `ComputeExecutor` runs this
//! repeatedly on a dedicated thread so that "Compute" mode genuinely occupies
//! the GPU, exactly as the mining engine does.

use opencl3::command_queue::CommandQueue;
use opencl3::context::Context;
use opencl3::device::{get_device_ids, Device, CL_DEVICE_TYPE_ALL, CL_DEVICE_TYPE_GPU};
use opencl3::kernel::Kernel;
use opencl3::memory::{Buffer, CL_MEM_COPY_HOST_PTR, CL_MEM_READ_WRITE};
use opencl3::platform::get_platforms;
use opencl3::program::Program;
use std::os::raw::c_void;

const KERNEL_SRC: &str = include_str!("matmul_kernel.cl");
const TILE: usize = 16;

/// A GPU-bound dense matmul (C = A*B, NxNxN float32).
pub struct GpuMatmul {
    device: Device,
    queue: CommandQueue,
    kernel: Kernel,
    // Kept alive for the lifetime of the struct: they own the cl_mem that the
    // kernel's set_arg references. Never read directly.
    #[allow(dead_code)]
    a: Buffer<f32>,
    #[allow(dead_code)]
    b: Buffer<f32>,
    c: Buffer<f32>,
    n: usize,
}

impl GpuMatmul {
    /// Open the GPU at `device_index` (0 = first) and allocate NxN buffers.
    pub fn new(device_index: usize, n: usize) -> Result<Self, String> {
        let platforms = get_platforms().map_err(|e| format!("no OpenCL platforms: {e}"))?;
        let mut gpus: Vec<Device> = Vec::new();
        for p in &platforms {
            let devices = get_device_ids(p.id(), CL_DEVICE_TYPE_ALL).unwrap_or_default();
            for did in devices {
                let d = Device::new(did);
                if (d.dev_type().unwrap_or(0) & CL_DEVICE_TYPE_GPU) != 0 {
                    gpus.push(d);
                }
            }
        }
        if gpus.is_empty() {
            return Err("no OpenCL GPU device found".into());
        }
        let device = gpus
            .get(device_index)
            .cloned()
            .ok_or_else(|| format!("GPU index {device_index} out of range ({} GPU(s))", gpus.len()))?;
        let gpu_name = device.name().unwrap_or_default();

        let context = Context::from_device(&device).map_err(|e| format!("context: {e}"))?;
        let queue = CommandQueue::create_default(&context, 0).map_err(|e| format!("queue: {e}"))?;

        let mut program = Program::create_from_source(&context, KERNEL_SRC)
            .map_err(|e| format!("create program: {e}"))?;
        program
            .build(&[device.id()], "")
            .map_err(|e| {
                let log = program.get_build_log(device.id()).unwrap_or_default();
                format!("matmul kernel build failed: {e}\n--- build log ---\n{log}")
            })?;
        let kernel = Kernel::create(&program, "gpn_sgemm").map_err(|e| format!("sgemm kernel: {e}"))?;

        let elems = n * n;
        let a = unsafe {
            Buffer::<f32>::create(&context, CL_MEM_READ_WRITE | CL_MEM_COPY_HOST_PTR, elems, Self::fill(n, 1.0f32).as_ptr() as *mut c_void)
        }
        .map_err(|e| format!("A buffer: {e}"))?;
        let b = unsafe {
            Buffer::<f32>::create(&context, CL_MEM_READ_WRITE | CL_MEM_COPY_HOST_PTR, elems, Self::fill(n, 2.0f32).as_ptr() as *mut c_void)
        }
        .map_err(|e| format!("B buffer: {e}"))?;
        let c = unsafe { Buffer::<f32>::create(&context, CL_MEM_READ_WRITE, elems, std::ptr::null_mut()) }
            .map_err(|e| format!("C buffer: {e}"))?;

        unsafe {
            kernel.set_arg(0, &(n as u32)).map_err(|e| format!("arg N: {e}"))?;
            kernel.set_arg(1, &a).map_err(|e| format!("arg A: {e}"))?;
            kernel.set_arg(2, &b).map_err(|e| format!("arg B: {e}"))?;
            kernel.set_arg(3, &c).map_err(|e| format!("arg C: {e}"))?;
        }

        log::info!("GPU matmul ready on {gpu_name} (N={n}, {} MiB matrices)", elems * 4 / (1024 * 1024));
        Ok(Self { device, queue, kernel, a, b, c, n })
    }

    pub fn device_name(&self) -> String {
        self.device.name().unwrap_or_default()
    }

    pub fn n(&self) -> usize {
        self.n
    }

    /// Fill an NxN buffer with the constant `v` (row i gets scaled so results
    /// are non-trivial).
    fn fill(n: usize, v: f32) -> Vec<f32> {
        let mut out = vec![0f32; n * n];
        for i in 0..n {
            for j in 0..n {
                out[i * n + j] = v * (1.0 + (j % 7) as f32 * 0.001);
            }
        }
        out
    }

    /// Run one matmul on the GPU and return elapsed time (seconds). The
    /// result is left in the `C` buffer; for benchmarking we don't read it
    /// back each iteration.
    pub fn run_once(&mut self) -> Result<std::time::Duration, String> {
        let gws: [usize; 2] = [self.n, self.n];
        let lws: [usize; 2] = [TILE, TILE];
        let start = std::time::Instant::now();
        unsafe {
            self.queue
                .enqueue_nd_range_kernel(self.kernel.get(), 2, std::ptr::null(), gws.as_ptr(), lws.as_ptr(), &[])
                .map_err(|e| format!("ndrange: {e}"))?;
        }
        self.queue.finish().map_err(|e| format!("finish: {e}"))?;
        Ok(start.elapsed())
    }

    /// Run `iters` matmuls back to back, returning (total_elapsed, avg_flops).
    /// FLOPs per matmul = 2 * N^3.
    pub fn run_many(&mut self, iters: usize) -> Result<(std::time::Duration, f64), String> {
        let start = std::time::Instant::now();
        for _ in 0..iters {
            self.run_once()?;
        }
        let elapsed = start.elapsed();
        let total_flops = 2.0 * (self.n as f64).powi(3) * iters as f64;
        Ok((elapsed, total_flops / elapsed.as_secs_f64()))
    }

    /// Read back the C buffer (for a correctness check against CPU reference).
    pub fn read_result(&mut self) -> Result<Vec<f32>, String> {
        let mut out = vec![0f32; self.n * self.n];
        unsafe {
            self.queue
                .enqueue_read_buffer(&mut self.c, 1, 0, &mut out, &[])
                .map_err(|e| format!("read C: {e}"))?;
        }
        Ok(out)
    }

    /// Host-side reference GEMM for validating the GPU kernel.
    pub fn cpu_reference(a: &[f32], b: &[f32], n: usize) -> Vec<f32> {
        let mut c = vec![0f32; n * n];
        for i in 0..n {
            for k in 0..n {
                let aik = a[i * n + k];
                if aik == 0.0 {
                    continue;
                }
                for j in 0..n {
                    c[i * n + j] += aik * b[k * n + j];
                }
            }
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_reference_matches_expectation() {
        // A = [[1,2],[3,4]], B = [[5,6],[7,8]] -> C = [[19,22],[43,50]]
        let a = vec![1.0, 2.0, 3.0, 4.0];
        let b = vec![5.0, 6.0, 7.0, 8.0];
        let c = GpuMatmul::cpu_reference(&a, &b, 2);
        assert!((c[0] - 19.0).abs() < 1e-3, "c00={}", c[0]);
        assert!((c[1] - 22.0).abs() < 1e-3, "c01={}", c[1]);
        assert!((c[2] - 43.0).abs() < 1e-3, "c10={}", c[2]);
        assert!((c[3] - 50.0).abs() < 1e-3, "c11={}", c[3]);
    }

    /// GPU kernel must match the CPU reference on a small N (GPU-gated).
    #[test]
    #[ignore]
    fn gpu_kernel_matches_cpu_reference() {
        let n = 64;
        let a = GpuMatmul::fill(n, 1.0);
        let b = GpuMatmul::fill(n, 2.0);
        let expect = GpuMatmul::cpu_reference(&a, &b, n);

        let mut mm = GpuMatmul::new(0, n).expect("open GPU matmul");
        mm.run_once().expect("run kernel");
        let got = mm.read_result().expect("read back");

        for i in 0..n {
            for j in 0..n {
                let idx = i * n + j;
                let rel = (got[idx] - expect[idx]).abs() / expect[idx].max(1e-9);
                assert!(rel < 1e-2, "mismatch at ({i},{j}): got {} expect {}", got[idx], expect[idx]);
            }
        }
    }
}
