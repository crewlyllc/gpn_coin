// GPN AI-compute kernel: dense float32 GEMM (C = A * B, all NxN).
//
// This is the core primitive of neural-network inference / MLP / attention
// linear layers. The compute executor runs this on the GPU as a *real* AI
// workload — the same GPU that mines Ethash — so that "Compute" mode
// genuinely occupies the card, exactly as mining does. Tiled with 16x16
// local-memory tiles for good occupancy on consumer GPUs.

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
