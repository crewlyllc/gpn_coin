//! Standalone GPU AI-compute benchmark for GPN.
//!
//! Proves the "GPU serves AI compute" leg of the auto-switch without a live
//! node: opens the GPU and runs a real dense float32 GEMM (the core primitive
//! of neural inference), reporting measured throughput in GFLOPS. This is the
//! exact workload the `ComputeExecutor` engine runs when the agent is in
//! `Compute` mode.
//!
//! Usage:
//!   gpn_gpu_compute [matrix_size] [iterations] [device_index]
//!
//! Defaults: matrix_size=2048, iterations=8, device_index=0

use gpn_agent::gpu::matmul::GpuMatmul;

fn main() {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(2048);
    let iters: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);
    let device: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);

    println!("== GPN GPU AI-compute benchmark (dense GEMM) ==");
    for line in gpn_agent::gpu::ethash_miner::EthashGpuMiner::device_summary() {
        println!("  device: {line}");
    }

    let mut mm = GpuMatmul::new(device, n).expect("open GPU matmul");
    println!("using GPU: {}  N={n}  iterations={iters}", mm.device_name());

    // Warm-up so the first timed iteration reflects steady-state scheduling.
    mm.run_once().expect("warm-up");
    println!("GPU matmul ready — running {iters} iterations...");

    let (elapsed, flops) = mm.run_many(iters).expect("run matmuls");
    let gflops = flops / 1e9;
    let per_iter_ms = elapsed.as_secs_f64() * 1000.0 / iters as f64;
    println!();
    println!("  iterations : {iters}");
    println!("  total time : {:.3}s", elapsed.as_secs_f64());
    println!("  per iter   : {:.2} ms", per_iter_ms);
    println!("  throughput : {:.2} GFLOPS", gflops);
    println!(
        "  (matmul FLOPs = 2*N^3 = {:.2} GFLOP/iter)",
        2.0 * (n as f64).powi(3) / 1e9
    );
    println!();
    println!("✅ Real GPU AI compute verified — this workload is what Compute mode serves.");
}
