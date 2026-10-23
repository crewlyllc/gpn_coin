// GPN AI-compute kernel: dense float32 GEMM (C = A * B, all NxN).
//
// This is the core primitive of neural-network inference / MLP / attention
// linear layers. The compute executor runs this on the GPU as a *real* AI
// workload — the same GPU that mines Ethash — so that "Compute" mode
// genuinely occupies the card, exactly as mining does.
//
// It is ALSO the measurement kernel: `gpu::measure_gflops` runs it to attest a
// node's compute capability on-chain, and the marketplace gates work on that
// number (`accept_job: attested_gflops >= min_gflops`).
//
// Shape tuning was tried and REJECTED (queue item #87, measured on a 24-CU
// RTX 4060, best of 5, miner held off):
//   this 16x16 tile, 1 output/thread   1293 GFLOPS
//   32x32 block, 2x2 registers/thread   383 GFLOPS
//   64x64 block, 4x4 registers/thread   201 GFLOPS
// Register blocking is the textbook win on big GPUs, but here the larger
// shared-memory footprint cuts work-groups per SM and the smaller grid has too
// few warps to hide latency, so it costs 3-6x. Keep this shape unless a real
// benchmark on real hardware says otherwise — the numbers above are the
// baseline to beat, and `gpu::matmul::tests::gpu_benchmark_reports_peak_gflops`
// prints them.
//
// Tiled with 16x16 local-memory tiles for good occupancy on consumer GPUs.

__kernel void gpn_sgemm(
    const uint N,
    __global const float* restrict A,
    __global const float* restrict B,
    __global float* restrict C)
{
    const uint TILE = 16;
    const uint lx = get_local_id(0);
    const uint ly = get_local_id(1);
    const uint gx = get_global_id(0);
    const uint gy = get_global_id(1);

    __local float As[TILE][TILE];
    __local float Bs[TILE][TILE];

    float acc = 0.0f;
    const uint numTiles = (N + TILE - 1) / TILE;

    for (uint t = 0; t < numTiles; ++t) {
        const uint aCol = t * TILE + lx;
        const uint aRow = gy;
        const uint bCol = gx;
        const uint bRow = t * TILE + ly;
        As[ly][lx] = (aRow < N && aCol < N) ? A[aRow * N + aCol] : 0.0f;
        Bs[ly][lx] = (bRow < N && bCol < N) ? B[bRow * N + bCol] : 0.0f;
        barrier(CLK_LOCAL_MEM_FENCE);

        #pragma unroll 4
        for (uint k = 0; k < TILE; ++k) {
            acc += As[ly][k] * Bs[k][lx];
        }
        barrier(CLK_LOCAL_MEM_FENCE);
    }

    if (gx < N && gy < N) {
        C[gy * N + gx] = acc;
    }
}
