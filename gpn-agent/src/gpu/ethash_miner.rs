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
        let mut gpus: Vec<(String, Device)> = Vec::new();
        for p in &platforms {
            let pname = p.name().unwrap_or_default();
            let devices = get_device_ids(p.id(), CL_DEVICE_TYPE_ALL).unwrap_or_default();
            for did in devices {
                let d = Device::new(did);
                if (d.dev_type().unwrap_or(0) & CL_DEVICE_TYPE_GPU) != 0 {
                    gpus.push((pname.clone(), d));
                }
            }
        }
        // Prefer discrete (NVIDIA/AMD) over integrated (Intel) so index 0 is the
        // most powerful card.
        gpus.sort_by_key(|(_, d)| {
            let name = d.name().unwrap_or_default().to_lowercase();
            if name.contains("intel") { 1 } else { 0 }
        });
        for (i, (pname, d)) in gpus.iter().enumerate() {
            out.push(format!("[{}] {} :: {}", i, pname, d.name().unwrap_or_default()));
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
        // Prefer discrete GPUs (NVIDIA/AMD) over integrated (Intel) so device 0 is
        // the most powerful card, not a CPU-integrated iGPU.
        gpus.sort_by_key(|d| {
            let name = d.name().unwrap_or_default().to_lowercase();
            if name.contains("intel") { 1 } else { 0 }
        });
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
        let out = unsafe {
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
                .enqueue_read_buffer(&out, 1, 0, &mut res, &[])
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
        let out = unsafe {
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
                .enqueue_read_buffer(&out, 1, 0, &mut res, &[])
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
            Buffer::<u32>::create(&self.context, CL_MEM_READ_WRITE, 1, std::ptr::null_mut())
        }
        .map_err(|e| format!("counter: {e}"))?;
        let zero = 0u32;
        unsafe {
            self.queue
                .enqueue_write_buffer(&mut counter, 1, 0, &[zero], &[])
                .map_err(|e| format!("zero counter: {e}"))?;
        }
        let results = unsafe {
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
        let mut cnt = [0u32];
        unsafe {
            self.queue
                .enqueue_read_buffer(&counter, 1, 0, &mut cnt, &[])
                .map_err(|e| format!("read counter: {e}"))?;
        }
        if cnt[0] == 0 {
            return Ok(None);
        }
        let mut data = vec![0u8; 72];
        unsafe {
            self.queue
                .enqueue_read_buffer(&results, 1, 0, &mut data, &[])
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
            Buffer::<u32>::create(&self.context, CL_MEM_READ_WRITE, 1, std::ptr::null_mut())
        }
        .map_err(|e| format!("counter: {e}"))?;
        let zero = 0u32;
        unsafe {
            self.queue
                .enqueue_write_buffer(&mut counter, 1, 0, &[zero], &[])
                .map_err(|e| format!("zero counter: {e}"))?;
        }
        let results = unsafe {
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

        let mut cnt = [0u32];
        unsafe {
            self.queue
                .enqueue_read_buffer(&counter, 1, 0, &mut cnt, &[])
                .map_err(|e| format!("read counter: {e}"))?;
        }
        if cnt[0] == 0 {
            return Ok(None);
        }
        let read = (cnt[0] as usize).min(results_cap);
        let mut data = vec![0u8; read * 72];
        unsafe {
            self.queue
                .enqueue_read_buffer(&results, 1, 0, &mut data, &[])
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
    n.div_ceil(mult) * mult
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the epoch-0 cache + DAG on the GPU and verify the GPU's
    /// hashimoto output matches the `ethash` crate reference bit-for-bit for
    /// several nonces. Heavy (builds the ~1 GiB DAG once) — run with
    /// `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn gpu_hashimoto_matches_reference() {
        let miner = match EthashGpuMiner::new(0) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("SKIP (no GPU): {e}");
                return;
            }
        };

        let epoch = 0usize;
        let cache_size = ethash::get_cache_size(epoch);
        let full_size = ethash::get_full_size(epoch);
        let seed = ethash::get_seedhash(epoch);
        let mut cache = vec![0u8; cache_size];
        ethash::make_cache(&mut cache, seed);
        eprintln!(
            "cache={} bytes, full_size={} MiB",
            cache_size,
            full_size / (1024 * 1024)
        );

        let dag = miner.build_dag(&cache, full_size).expect("dag");

        let target_all = [0xffu8; 32]; // every nonce qualifies
                                       // `batch=1` still launches `round_up(1,256)=256` work items (nonces
                                       // start..start+255); the kernel returns whichever nonce wins the atomic
                                       // append. So we validate the RETURNED nonce's mix+result against the
                                       // ethash reference, which is the correctness claim that matters.
        let start_nonces: [u64; 5] = [0, 1, 42, 0xdead_beef, 0x0123_4567_89ab_cdef];
        let mut checked = 0u32;
        for start in start_nonces {
            let header = [0x11u8; 32];
            let seal = miner
                .mine_batch(&dag, &header, &target_all, start, 1)
                .expect("mine_batch")
                .expect("should find nonce");

            let ref_h = ethereum_types::H256::from_slice(&header);
            let ref_n = ethereum_types::H64::from_low_u64_be(seal.nonce);
            let (ref_mix, ref_result) = ethash::hashimoto_light(ref_h, ref_n, full_size, &cache);
            assert_eq!(
                seal.mix.as_slice(),
                ref_mix.as_bytes(),
                "mix mismatch nonce={:#x}",
                seal.nonce
            );
            assert_eq!(
                seal.result.as_slice(),
                ref_result.as_bytes(),
                "result mismatch nonce={:#x}",
                seal.nonce
            );
            checked += 1;
            eprintln!("nonce {:#018x}: mix+result match reference ✓", seal.nonce);
        }
        assert!(checked > 0, "no nonces validated");
        eprintln!("GPU Ethash miner validated against ethash crate reference ({checked} nonces) ✓");
    }

    /// Isolate the failing kernel: compare GPU DAG items to the reference.
    #[test]
    #[ignore]
    fn gpu_dag_items_match_reference() {
        let miner = match EthashGpuMiner::new(0) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("SKIP (no GPU): {e}");
                return;
            }
        };
        let epoch = 0usize;
        let cache_size = ethash::get_cache_size(epoch);
        let full_size = ethash::get_full_size(epoch);
        let seed = ethash::get_seedhash(epoch);
        let mut cache = vec![0u8; cache_size];
        ethash::make_cache(&mut cache, seed);
        let dag = miner.build_dag(&cache, full_size).expect("dag");

        for item in [0usize, 1, 2, 3, 100, 1000, 100000] {
            let gpu = miner.read_dag_item(&dag, item).expect("read");
            let ref_item: [u8; 64] = ethash::calc_dataset_item(&cache, item)
                .as_bytes()
                .try_into()
                .unwrap();
            assert_eq!(
                gpu.as_slice(),
                ref_item.as_slice(),
                "DAG item {item} mismatch"
            );
            eprintln!("dag item {item}: match ✓");
        }
        eprintln!("GPU DAG generation matches reference ✓");
    }

    /// Rust reference hashimoto: (seed, p[64], mix[128], cmix[32], result[32]).
    fn ref_hashimoto(
        header: &[u8; 32],
        nonce: u64,
        full_size: usize,
        cache: &[u8],
    ) -> (Vec<u8>, Vec<u32>, Vec<u8>, Vec<u8>, Vec<u8>) {
        use sha3::{Digest, Keccak256, Keccak512};
        let fnv = |a: u32, b: u32| ((a as u64 * 0x0100_0193) ^ b as u64) as u32;

        let mut h = Keccak512::new();
        h.update(header);
        h.update(nonce.to_le_bytes());
        let seed: Vec<u8> = h.finalize().to_vec();

        let mut mix = vec![0u8; 128];
        for j in 0..128 {
            mix[j] = seed[j % 64];
        }
        let n = full_size / 64;
        let w = 32usize;
        let mixhashes = 2usize;
        let mut ps = Vec::with_capacity(64);
        for i in 0..64 {
            let p = (fnv(
                (i as u32) ^ u32::from_le_bytes(seed[0..4].try_into().unwrap()),
                u32::from_le_bytes(mix[(i % w) * 4..(i % w) * 4 + 4].try_into().unwrap()),
            ) as usize)
                % (n / mixhashes)
                * mixhashes;
            ps.push(p as u32);
            let mut newdata = [0u8; 128];
            for j in 0..2 {
                let item = ethash::calc_dataset_item(cache, p + j);
                newdata[j * 64..j * 64 + 64].copy_from_slice(item.as_bytes());
            }
            for j in (0..128).step_by(4) {
                let a = u32::from_le_bytes(mix[j..j + 4].try_into().unwrap());
                let b = u32::from_le_bytes(newdata[j..j + 4].try_into().unwrap());
                mix[j..j + 4].copy_from_slice(&fnv(a, b).to_le_bytes());
            }
        }
        let mut cmix = vec![0u8; 32];
        for g in 0..8 {
            let j = g * 4;
            let a = fnv(
                u32::from_le_bytes(mix[j * 4..j * 4 + 4].try_into().unwrap()),
                u32::from_le_bytes(mix[(j + 1) * 4..(j + 1) * 4 + 4].try_into().unwrap()),
            );
            let b = fnv(
                a,
                u32::from_le_bytes(mix[(j + 2) * 4..(j + 2) * 4 + 4].try_into().unwrap()),
            );
            let c = fnv(
                b,
                u32::from_le_bytes(mix[(j + 3) * 4..(j + 3) * 4 + 4].try_into().unwrap()),
            );
            cmix[g * 4..g * 4 + 4].copy_from_slice(&c.to_le_bytes());
        }
        let mut h2 = Keccak256::new();
        h2.update(&seed);
        h2.update(&cmix);
        let result = h2.finalize().to_vec();
        (seed, ps, mix, cmix, result)
    }

    /// Diff the GPU hashimoto trace against the Rust reference for one nonce.
    #[test]
    #[ignore]
    fn gpu_hashimoto_trace_diff() {
        let miner = match EthashGpuMiner::new(0) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("SKIP (no GPU): {e}");
                return;
            }
        };
        let epoch = 0usize;
        let cache_size = ethash::get_cache_size(epoch);
        let full_size = ethash::get_full_size(epoch);
        let seed = ethash::get_seedhash(epoch);
        let mut cache = vec![0u8; cache_size];
        ethash::make_cache(&mut cache, seed);
        let dag = miner.build_dag(&cache, full_size).expect("dag");

        let header = [0x11u8; 32];
        let nonce: u64 = 0x80;
        let trace = miner.debug_trace(&header, nonce, &dag).expect("trace");
        let (r_seed, r_p, r_mix, r_cmix, r_result) =
            ref_hashimoto(&header, nonce, full_size, &cache);

        // seed [0..64]
        eprintln!("seed gpu={:02x?}", &trace[0..64]);
        eprintln!("seed ref={:02x?}", &r_seed[..]);
        eprintln!("seed match: {}", trace[0..64] == r_seed[..]);

        // p [64..320]
        let mut gpu_p = Vec::new();
        for i in 0..64 {
            gpu_p.push(u32::from_le_bytes(
                trace[64 + i * 4..64 + i * 4 + 4].try_into().unwrap(),
            ));
        }
        let p_diff: Vec<usize> = (0..64).filter(|&i| gpu_p[i] != r_p[i]).collect();
        eprintln!(
            "p match count: {}/64, first diffs: {:?}",
            64 - p_diff.len(),
            &p_diff[..p_diff.len().min(8)]
        );
        for &i in p_diff.iter().take(4) {
            eprintln!("  p[{i}] gpu={} ref={}", gpu_p[i], r_p[i]);
        }

        eprintln!("mix match: {}", trace[320..448] == r_mix[..]);
        eprintln!("cmix match: {}", trace[448..480] == r_cmix[..]);
        eprintln!("result match: {}", trace[480..512] == r_result[..]);

        assert!(trace[0..64] == r_seed[..], "seed mismatch");
        assert!(gpu_p == r_p, "p mismatch");
        assert!(trace[320..448] == r_mix[..], "mix mismatch");
        assert!(trace[448..480] == r_cmix[..], "cmix mismatch");
        assert!(trace[480..512] == r_result[..], "result mismatch");

        // Same nonce via the real mining kernel, single work item:
        let target_all = [0xffu8; 32];
        let seal = miner
            .mine_single_nonce(&dag, &header, &target_all, nonce)
            .expect("single")
            .expect("should find");
        assert_eq!(seal.nonce, nonce, "single-nonce nonce mismatch");
        assert_eq!(
            seal.mix.as_slice(),
            r_cmix.as_slice(),
            "single-nonce mix mismatch"
        );
        assert_eq!(
            seal.result.as_slice(),
            r_result.as_slice(),
            "single-nonce result mismatch"
        );
        eprintln!("single work-item mine matches reference ✓");
    }
}
