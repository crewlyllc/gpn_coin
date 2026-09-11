//! Real GPU Ethash miner via OpenCL.
//!
//! Produces the exact same 40-byte seal (`nonce(8 LE) ++ mix_hash(32)`) that
//! gpn-node verifies with `ethash::hashimoto_light`. The DAG is generated on
//! the GPU from the light cache; `gpn_hashimoto` searches nonces in parallel.
//!
//! OpenCL kernels are compiled at runtime from `ethash_kernel.cl` via
//! `clBuildProgram`, so no CUDA toolchain or C headers are required — only the
//! OpenCL runtime + vendor ICD (present on this box: NVIDIA CUDA platform).

use opencl3::command_queue::CommandQueue;
use opencl3::context::Context;
use opencl3::device::{get_device_ids, Device, CL_DEVICE_TYPE_ALL, CL_DEVICE_TYPE_GPU};
use opencl3::kernel::Kernel;
use opencl3::memory::{Buffer, CL_MEM_COPY_HOST_PTR, CL_MEM_READ_WRITE};
use opencl3::platform::get_platforms;
use opencl3::program::Program;
use std::os::raw::c_void;

const KERNEL_SRC: &str = include_str!("ethash_kernel.cl");

/// Result of a successful GPU hashimoto search.
#[derive(Clone, Copy, Debug)]
pub struct MinedSeal {
    pub nonce: u64,
    pub mix: [u8; 32],
    pub result: [u8; 32],
}

impl MinedSeal {
    /// The 40-byte seal the node expects: `nonce(8 LE) ++ mix_hash(32)`.
    pub fn to_seal_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(40);
        v.extend_from_slice(&self.nonce.to_le_bytes());
        v.extend_from_slice(&self.mix);
        v
    }
}

/// A full Ethash dataset resident in GPU memory (one 64-byte item per entry).
pub struct DagBuffer {
    buffer: Buffer<u8>,
    full_size: usize,
}

/// A GPU Ethash miner bound to a single OpenCL device.
pub struct EthashGpuMiner {
    device: Device,
    context: Context,
    queue: CommandQueue,
    dag_kernel: Kernel,
    hashimoto_kernel: Kernel,
    debug_seed_kernel: Kernel,
    debug_trace_kernel: Kernel,
}

impl EthashGpuMiner {
    /// Enumerate OpenCL GPUs and print a summary (used for diagnostics).
    pub fn device_summary() -> Vec<String> {
        let mut out = Vec::new();
        let Ok(platforms) = get_platforms() else {
            return out;
        };
        for p in &platforms {
            let pname = p.name().unwrap_or_default();
            let devices = get_device_ids(p.id(), CL_DEVICE_TYPE_ALL).unwrap_or_default();
            for (i, did) in devices.iter().enumerate() {
                let d = Device::new(*did);
                out.push(format!(
                    "[{}] {} :: {} (GPU={})",
                    i,
                    pname,
                    d.name().unwrap_or_default(),
                    (d.dev_type().unwrap_or(0) & CL_DEVICE_TYPE_GPU) != 0
                ));
            }
        }
        out
    }

    /// Create a miner on the GPU at `device_index` (0 = first GPU).
    pub fn new(device_index: usize) -> Result<Self, String> {
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
        let device = gpus.get(device_index).cloned().ok_or_else(|| {
            format!(
                "GPU index {device_index} out of range ({} GPU(s))",
                gpus.len()
            )
        })?;
        let gpu_name = device.name().unwrap_or_default();

        let context = Context::from_device(&device).map_err(|e| format!("context: {e}"))?;
        let queue = CommandQueue::create_default(&context, 0).map_err(|e| format!("queue: {e}"))?;

        let mut program = Program::create_from_source(&context, KERNEL_SRC)
            .map_err(|e| format!("create program: {e}"))?;
        program.build(&[device.id()], "").map_err(|e| {
            let log = program.get_build_log(device.id()).unwrap_or_default();
            format!("kernel build failed: {e}\n--- build log ---\n{log}")
        })?;

        let dag_kernel =
            Kernel::create(&program, "gpn_dag_item").map_err(|e| format!("dag kernel: {e}"))?;
        let hashimoto_kernel = Kernel::create(&program, "gpn_hashimoto")
            .map_err(|e| format!("hashimoto kernel: {e}"))?;
        let debug_seed_kernel = Kernel::create(&program, "gpn_debug_seed")
            .map_err(|e| format!("debug seed kernel: {e}"))?;
        let debug_trace_kernel = Kernel::create(&program, "gpn_debug_trace")
            .map_err(|e| format!("debug trace kernel: {e}"))?;

        log::info!("GPU Ethash miner ready on {gpu_name}");
        Ok(Self {
            device,
            context,
            queue,
            dag_kernel,
            hashimoto_kernel,
            debug_seed_kernel,
            debug_trace_kernel,
        })
    }

    pub fn device_name(&self) -> String {
        self.device.name().unwrap_or_default()
    }

    /// Debug: GPU keccak512(header || nonce_le) — the hashimoto seed.
    pub fn debug_seed(&self, header: &[u8; 32], nonce: u64) -> Result<[u8; 64], String> {
        let header_buf = unsafe {
            Buffer::<u8>::create(
                &self.context,
                CL_MEM_READ_WRITE | CL_MEM_COPY_HOST_PTR,
                32,
                header.as_ptr() as *mut c_void,
            )
        }
        .map_err(|e| format!("header buf: {e}"))?;
        let mut out = unsafe {
            Buffer::<u8>::create(&self.context, CL_MEM_READ_WRITE, 64, std::ptr::null_mut())
        }
        .map_err(|e| format!("out buf: {e}"))?;
        unsafe {
            self.debug_seed_kernel
                .set_arg(0, &header_buf)
                .map_err(|e| format!("a0: {e}"))?;
            self.debug_seed_kernel
                .set_arg(1, &nonce)
                .map_err(|e| format!("a1: {e}"))?;
            self.debug_seed_kernel
                .set_arg(2, &out)
                .map_err(|e| format!("a2: {e}"))?;
        }
        let gws: [usize; 1] = [1];
        unsafe {
            self.queue
                .enqueue_nd_range_kernel(
                    self.debug_seed_kernel.get(),
                    1,
                    std::ptr::null(),
                    gws.as_ptr(),
                    std::ptr::null(),
                    &[],
                )
                .map_err(|e| format!("seed ndrange: {e}"))?;
        }
        self.queue
            .finish()
            .map_err(|e| format!("seed finish: {e}"))?;
        let mut res = [0u8; 64];
        unsafe {
            self.queue
                .enqueue_read_buffer(&mut out, 1, 0, &mut res, &[])
                .map_err(|e| format!("read seed: {e}"))?;
        }
        Ok(res)
    }

    /// Debug: full GPU hashimoto trace (seed[64], p[64] as u32, mix[128], cmix[32], result[32]).
    pub fn debug_trace(
        &self,
        header: &[u8; 32],
        nonce: u64,
        dag: &DagBuffer,
    ) -> Result<[u8; 512], String> {
        let header_buf = unsafe {
            Buffer::<u8>::create(
                &self.context,
                CL_MEM_READ_WRITE | CL_MEM_COPY_HOST_PTR,
                32,
                header.as_ptr() as *mut c_void,
            )
        }
        .map_err(|e| format!("header buf: {e}"))?;
        let mut out = unsafe {
            Buffer::<u8>::create(&self.context, CL_MEM_READ_WRITE, 512, std::ptr::null_mut())
        }
        .map_err(|e| format!("out buf: {e}"))?;
        let trace_kernel = self.debug_trace_kernel.clone();
        unsafe {
            trace_kernel
                .set_arg(0, &header_buf)
                .map_err(|e| format!("a0: {e}"))?;
            trace_kernel
                .set_arg(1, &nonce)
                .map_err(|e| format!("a1: {e}"))?;
            trace_kernel
                .set_arg(2, &dag.buffer)
                .map_err(|e| format!("a2: {e}"))?;
            trace_kernel
                .set_arg(3, &(dag.full_size as u64))
                .map_err(|e| format!("a3: {e}"))?;
            trace_kernel
                .set_arg(4, &out)
                .map_err(|e| format!("a4: {e}"))?;
        }
        let gws: [usize; 1] = [1];
        unsafe {
            self.queue
                .enqueue_nd_range_kernel(
                    trace_kernel.get(),
                    1,
                    std::ptr::null(),
                    gws.as_ptr(),
                    std::ptr::null(),
                    &[],
                )
                .map_err(|e| format!("trace ndrange: {e}"))?;
        }
        self.queue
            .finish()
            .map_err(|e| format!("trace finish: {e}"))?;
        let mut res = [0u8; 512];
        unsafe {
            self.queue
                .enqueue_read_buffer(&mut out, 1, 0, &mut res, &[])
                .map_err(|e| format!("read trace: {e}"))?;
        }
        Ok(res)
    }

    /// Debug: run gpn_hashimoto for exactly ONE work item (nonce = `nonce`).
    /// Launches with local size 1 so the batch is not rounded up to 256.
    pub fn mine_single_nonce(
        &self,
        dag: &DagBuffer,
        header: &[u8; 32],
        target: &[u8; 32],
        nonce: u64,
    ) -> Result<Option<MinedSeal>, String> {
        let results_cap = 64usize;
        let header_buf = unsafe {
            Buffer::<u8>::create(
                &self.context,
                CL_MEM_READ_WRITE | CL_MEM_COPY_HOST_PTR,
                32,
                header.as_ptr() as *mut c_void,
            )
        }
        .map_err(|e| format!("header buf: {e}"))?;
        let target_buf = unsafe {
            Buffer::<u8>::create(
                &self.context,
                CL_MEM_READ_WRITE | CL_MEM_COPY_HOST_PTR,
                32,
                target.as_ptr() as *mut c_void,
            )
        }
        .map_err(|e| format!("target buf: {e}"))?;
        let mut counter = unsafe {
            Buffer::<u64>::create(&self.context, CL_MEM_READ_WRITE, 1, std::ptr::null_mut())
        }
        .map_err(|e| format!("counter: {e}"))?;
        let zero = 0u64;
        unsafe {
            self.queue
                .enqueue_write_buffer(&mut counter, 1, 0, &[zero], &[])
                .map_err(|e| format!("zero counter: {e}"))?;
        }
        let mut results = unsafe {
            Buffer::<u8>::create(
                &self.context,
                CL_MEM_READ_WRITE,
                results_cap * 72,
                std::ptr::null_mut(),
            )
        }
        .map_err(|e| format!("results buf: {e}"))?;
        unsafe {
            self.hashimoto_kernel
                .set_arg(0, &header_buf)
                .map_err(|e| format!("a0: {e}"))?;
            self.hashimoto_kernel
                .set_arg(1, &nonce)
                .map_err(|e| format!("a1: {e}"))?;
            self.hashimoto_kernel
                .set_arg(2, &dag.buffer)
                .map_err(|e| format!("a2: {e}"))?;
            self.hashimoto_kernel
                .set_arg(3, &(dag.full_size as u64))
                .map_err(|e| format!("a3: {e}"))?;
            self.hashimoto_kernel
                .set_arg(4, &target_buf)
                .map_err(|e| format!("a4: {e}"))?;
            self.hashimoto_kernel
                .set_arg(5, &counter)
                .map_err(|e| format!("a5: {e}"))?;
            self.hashimoto_kernel
                .set_arg(6, &results)
                .map_err(|e| format!("a6: {e}"))?;
            self.hashimoto_kernel
                .set_arg(7, &(results_cap as u32))
                .map_err(|e| format!("a7: {e}"))?;
        }
        let gws: [usize; 1] = [1];
        let lws: [usize; 1] = [1];
        unsafe {
            self.queue
                .enqueue_nd_range_kernel(
                    self.hashimoto_kernel.get(),
                    1,
                    std::ptr::null(),
                    gws.as_ptr(),
                    lws.as_ptr(),
                    &[],
                )
                .map_err(|e| format!("ndrange: {e}"))?;
        }
        self.queue.finish().map_err(|e| format!("finish: {e}"))?;
        let mut cnt = [0u64];
        unsafe {
            self.queue
                .enqueue_read_buffer(&mut counter, 1, 0, &mut cnt, &[])
                .map_err(|e| format!("read counter: {e}"))?;
        }
        if cnt[0] == 0 {
            return Ok(None);
        }
        let mut data = vec![0u8; 72];
        unsafe {
            self.queue
                .enqueue_read_buffer(&mut results, 1, 0, &mut data, &[])
                .map_err(|e| format!("read results: {e}"))?;
        }
        let mut mix = [0u8; 32];
        let mut result = [0u8; 32];
        let mut nb = [0u8; 8];
        nb.copy_from_slice(&data[0..8]);
        mix.copy_from_slice(&data[8..40]);
        result.copy_from_slice(&data[40..72]);
        Ok(Some(MinedSeal {
            nonce: u64::from_le_bytes(nb),
            mix,
            result,
        }))
    }

    /// Read back one 64-byte DAG item from GPU memory (debug/validation).
    pub fn read_dag_item(&self, dag: &DagBuffer, item: usize) -> Result<[u8; 64], String> {
        let mut out = [0u8; 64];
        unsafe {
            self.queue
                .enqueue_read_buffer(&dag.buffer, 1, item * 64, &mut out, &[])
                .map_err(|e| format!("read dag item: {e}"))?;
        }
        Ok(out)
    }

    /// Generate the full Ethash dataset for `full_size` bytes on the GPU from
    /// the light `cache` (produced by `ethash::make_cache`).
    pub fn build_dag(&self, cache: &[u8], full_size: usize) -> Result<DagBuffer, String> {
        let n_items = full_size / 64;
        let dag = unsafe {
            Buffer::<u8>::create(
                &self.context,
                CL_MEM_READ_WRITE,
                full_size,
                std::ptr::null_mut(),
            )
        }
        .map_err(|e| format!("alloc dag: {e}"))?;

        let cache_buf = unsafe {
            Buffer::<u8>::create(
                &self.context,
                CL_MEM_READ_WRITE | CL_MEM_COPY_HOST_PTR,
                cache.len(),
                cache.as_ptr() as *mut c_void,
            )
        }
        .map_err(|e| format!("cache buffer: {e}"))?;

        // gpn_dag_item(cache, cache_len, dag, item_index)
        unsafe {
            self.dag_kernel
                .set_arg(0, &cache_buf)
                .map_err(|e| format!("arg0: {e}"))?;
            self.dag_kernel
                .set_arg(1, &(cache.len() as u32))
                .map_err(|e| format!("arg1: {e}"))?;
            self.dag_kernel
                .set_arg(2, &dag)
                .map_err(|e| format!("arg2: {e}"))?;
            self.dag_kernel
                .set_arg(3, &0u64)
                .map_err(|e| format!("arg3: {e}"))?;
        }

        let local: [usize; 1] = [256];
        let global: [usize; 1] = [round_up(n_items, local[0])];
        unsafe {
            self.queue
                .enqueue_nd_range_kernel(
                    self.dag_kernel.get(),
                    1,
                    std::ptr::null(),
                    global.as_ptr(),
                    local.as_ptr(),
                    &[],
                )
                .map_err(|e| format!("dag ndrange: {e}"))?;
        }
        self.queue
            .finish()
            .map_err(|e| format!("dag finish: {e}"))?;
        log::info!(
            "GPU DAG built: {} MiB ({} items)",
            full_size / (1024 * 1024),
            n_items
        );
        Ok(DagBuffer {
            buffer: dag,
            full_size,
        })
    }

    /// Search a batch of `batch` nonces starting at `nonce_start`. Returns the
    /// first solution found in this batch, or `None`.
    pub fn mine_batch(
        &self,
        dag: &DagBuffer,
        header: &[u8; 32],
        target: &[u8; 32],
        nonce_start: u64,
        batch: usize,
    ) -> Result<Option<MinedSeal>, String> {
        let results_cap = 4096usize;
        let header_buf = unsafe {
            Buffer::<u8>::create(
                &self.context,
                CL_MEM_READ_WRITE | CL_MEM_COPY_HOST_PTR,
                32,
                header.as_ptr() as *mut c_void,
            )
        }
        .map_err(|e| format!("header buf: {e}"))?;
        let target_buf = unsafe {
            Buffer::<u8>::create(
                &self.context,
                CL_MEM_READ_WRITE | CL_MEM_COPY_HOST_PTR,
                32,
                target.as_ptr() as *mut c_void,
            )
        }
        .map_err(|e| format!("target buf: {e}"))?;
        let mut counter = unsafe {
            Buffer::<u64>::create(&self.context, CL_MEM_READ_WRITE, 1, std::ptr::null_mut())
        }
        .map_err(|e| format!("counter: {e}"))?;
        let zero = 0u64;
        unsafe {
            self.queue
                .enqueue_write_buffer(&mut counter, 1, 0, &[zero], &[])
                .map_err(|e| format!("zero counter: {e}"))?;
        }
        let mut results = unsafe {
            Buffer::<u8>::create(
                &self.context,
                CL_MEM_READ_WRITE,
                results_cap * 72,
                std::ptr::null_mut(),
            )
        }
        .map_err(|e| format!("results buf: {e}"))?;

        // gpn_hashimoto(header, nonce_start, dag, full_size, target, counter, results, cap)
        unsafe {
            self.hashimoto_kernel
                .set_arg(0, &header_buf)
                .map_err(|e| format!("a0: {e}"))?;
            self.hashimoto_kernel
                .set_arg(1, &nonce_start)
                .map_err(|e| format!("a1: {e}"))?;
            self.hashimoto_kernel
                .set_arg(2, &dag.buffer)
                .map_err(|e| format!("a2: {e}"))?;
            self.hashimoto_kernel
                .set_arg(3, &(dag.full_size as u64))
                .map_err(|e| format!("a3: {e}"))?;
            self.hashimoto_kernel
                .set_arg(4, &target_buf)
                .map_err(|e| format!("a4: {e}"))?;
            self.hashimoto_kernel
                .set_arg(5, &counter)
                .map_err(|e| format!("a5: {e}"))?;
            self.hashimoto_kernel
                .set_arg(6, &results)
                .map_err(|e| format!("a6: {e}"))?;
            self.hashimoto_kernel
                .set_arg(7, &(results_cap as u32))
                .map_err(|e| format!("a7: {e}"))?;
        }

        let local: [usize; 1] = [256];
        let global: [usize; 1] = [round_up(batch, local[0])];
        unsafe {
            self.queue
                .enqueue_nd_range_kernel(
                    self.hashimoto_kernel.get(),
                    1,
                    std::ptr::null(),
                    global.as_ptr(),
                    local.as_ptr(),
                    &[],
                )
                .map_err(|e| format!("hashimoto ndrange: {e}"))?;
        }
        self.queue
            .finish()
            .map_err(|e| format!("hashimoto finish: {e}"))?;

        let mut cnt = [0u64];
        unsafe {
            self.queue
                .enqueue_read_buffer(&mut counter, 1, 0, &mut cnt, &[])
                .map_err(|e| format!("read counter: {e}"))?;
        }
        if cnt[0] == 0 {
            return Ok(None);
        }
        let read = (cnt[0] as usize).min(results_cap);
        let mut data = vec![0u8; read * 72];
        unsafe {
            self.queue
                .enqueue_read_buffer(&mut results, 1, 0, &mut data, &[])
                .map_err(|e| format!("read results: {e}"))?;
        }
        let mut mix = [0u8; 32];
        let mut result = [0u8; 32];
        let mut nonce_bytes = [0u8; 8];
        nonce_bytes.copy_from_slice(&data[0..8]);
        mix.copy_from_slice(&data[8..40]);
        result.copy_from_slice(&data[40..72]);
        Ok(Some(MinedSeal {
            nonce: u64::from_le_bytes(nonce_bytes),
            mix,
            result,
        }))
    }

    /// Mine until a solution below `target` is found, searching batches of
    /// `batch` nonces. Returns the first valid seal.
    pub fn mine(
        &self,
        dag: &DagBuffer,
        header: &[u8; 32],
        target: &[u8; 32],
        mut nonce_start: u64,
        batch: usize,
    ) -> Result<MinedSeal, String> {
        loop {
            if let Some(seal) = self.mine_batch(dag, header, target, nonce_start, batch)? {
                return Ok(seal);
            }
            nonce_start = nonce_start.wrapping_add(batch as u64);
        }
    }
}

fn round_up(n: usize, mult: usize) -> usize {
    ((n + mult - 1) / mult) * mult
}

// GPU-miner correctness is validated by the authoritative (heavier) test suite
// in gpn-agent/src/gpu/ethash_miner.rs (gpu_hashimoto_matches_reference,
// gpu_dag_items_match_reference, gpu_hashimoto_trace_diff).
