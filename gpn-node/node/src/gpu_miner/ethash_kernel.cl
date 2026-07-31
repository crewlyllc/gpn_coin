// Ethash OpenCL kernels for GPN. Translated from the `ethash` crate (0.4.0)
// that gpn-node uses, so GPU output matches the node's `hashimoto_light`
// verification bit-for-bit.
//
// Two kernels:
//   1. `gpn_dag_item`   - generate one 64-byte DAG item per work-item from the cache
//   2. `gpn_hashimoto`  - compute Ethash hashimoto for one nonce per work-item
//
// Uses ORIGINAL Keccak (domain pad 0x01), NOT NIST SHA3 (0x06), to match the
// `sha3::Keccak256/Keccak512` used by the ethash crate.

// ---------- Keccak-f[1600] ----------
typedef unsigned long ulong;

#define ROTL64(x, n) rotate((x), (ulong)(n))

__constant ulong keccak_rndc[24] = {
    0x0000000000000001UL, 0x0000000000008082UL, 0x800000000000808aUL,
    0x8000000080008000UL, 0x000000000000808bUL, 0x0000000080000001UL,
    0x8000000080008081UL, 0x8000000000008009UL, 0x000000000000008aUL,
    0x0000000000000088UL, 0x0000000080008009UL, 0x000000008000000aUL,
    0x000000008000808bUL, 0x800000000000008bUL, 0x8000000000008089UL,
    0x8000000000008003UL, 0x8000000000008002UL, 0x8000000000000080UL,
    0x000000000000800aUL, 0x800000008000000aUL, 0x8000000080008081UL,
    0x8000000000008080UL, 0x0000000080000001UL, 0x8000000080008008UL
};

__constant uint keccak_rotc[24] = {
    1, 3, 6, 10, 15, 21, 28, 36, 45, 55, 2, 14,
    27, 41, 56, 8, 25, 43, 62, 18, 39, 61, 20, 44
};

__constant uint keccak_piln[24] = {
    10, 7, 11, 17, 18, 3, 5, 16, 8, 21, 24, 4,
    15, 23, 19, 13, 12, 2, 20, 14, 22, 9, 6, 1
};

static ulong load64_le(const uchar *p) {
    ulong v = 0;
    for (int i = 7; i >= 0; i--) v = (v << 8) | (ulong)p[i];
    return v;
}

static void store64_le(uchar *p, ulong v) {
    for (int i = 0; i < 8; i++) { p[i] = (uchar)(v & 0xff); v >>= 8; }
}

static uint load32_le(const uchar *p) {
    return (uint)p[0] | ((uint)p[1] << 8) | ((uint)p[2] << 16) | ((uint)p[3] << 24);
}

static void store32_le(uchar *p, uint v) {
    p[0] = (uchar)(v & 0xff); p[1] = (uchar)((v >> 8) & 0xff);
    p[2] = (uchar)((v >> 16) & 0xff); p[3] = (uchar)((v >> 24) & 0xff);
}

static void keccak_f1600(ulong *s) {
    ulong bc[5];
    for (int round = 0; round < 24; round++) {
        // theta
        for (int i = 0; i < 5; i++)
            bc[i] = s[i] ^ s[i + 5] ^ s[i + 10] ^ s[i + 15] ^ s[i + 20];
        for (int i = 0; i < 5; i++) {
            ulong t = bc[(i + 4) % 5] ^ ROTL64(bc[(i + 1) % 5], 1);
            for (int j = 0; j < 25; j += 5) s[j + i] ^= t;
        }
        // rho + pi
        ulong t = s[1];
        for (int i = 0; i < 24; i++) {
            int j = keccak_piln[i];
            ulong tmp = s[j];
            s[j] = ROTL64(t, keccak_rotc[i]);
            t = tmp;
        }
        // chi
        for (int j = 0; j < 25; j += 5) {
            bc[0] = s[j]; bc[1] = s[j + 1]; bc[2] = s[j + 2];
            bc[3] = s[j + 3]; bc[4] = s[j + 4];
            for (int i = 0; i < 5; i++)
                s[j + i] = bc[i] ^ ((~bc[(i + 1) % 5]) & bc[(i + 2) % 5]);
        }
        // iota
        s[0] ^= keccak_rndc[round];
    }
}

// Sponge with ORIGINAL Keccak padding (0x01), rate `rsiz` bytes.
static void keccak(const uchar *in, uint inlen, uint rsiz, uint outlen, uchar *out) {
    ulong s[25];
    for (int i = 0; i < 25; i++) s[i] = 0;

    // absorb full blocks
    while (inlen >= rsiz) {
        for (uint i = 0; i < rsiz / 8; i++) s[i] ^= load64_le(in + i * 8);
        keccak_f1600(s);
        in += rsiz; inlen -= rsiz;
    }
    // final padded block
    uchar block[200];
    for (uint i = 0; i < rsiz; i++) block[i] = 0;
    for (uint i = 0; i < inlen; i++) block[i] = in[i];
    block[inlen] ^= 0x01;      // Keccak domain pad (matches sha3::Keccak, NOT NIST 0x06)
    block[rsiz - 1] ^= 0x80;
    for (uint i = 0; i < rsiz / 8; i++) s[i] ^= load64_le(block + i * 8);
    keccak_f1600(s);

    // squeeze
    uint oi = 0;
    while (oi < outlen) {
        for (uint i = 0; i < rsiz / 8 && oi < outlen; i++, oi += 8)
            store64_le(out + oi, s[i]);
        if (oi < outlen) keccak_f1600(s);
    }
}

static void keccak512(const uchar *in, uint inlen, uchar *out) { keccak(in, inlen, 72, 64, out); }
static void keccak256(const uchar *in, uint inlen, uchar *out) { keccak(in, inlen, 136, 32, out); }

// ---------- Ethash primitives ----------
#define FNV_PRIME 0x01000193u
static uint fnv(uint v1, uint v2) {
    return (uint)(((ulong)v1 * (ulong)FNV_PRIME) ^ (ulong)v2);
}

static void fnv64(uchar a[64], const uchar b[64]) {
    for (uint j = 0; j < 64; j += 4)
        store32_le(a + j, fnv(load32_le(a + j), load32_le(b + j)));
}

static void fnv128(uchar a[128], const uchar b[128]) {
    for (uint j = 0; j < 128; j += 4)
        store32_le(a + j, fnv(load32_le(a + j), load32_le(b + j)));
}

// ---------- Debug: expose the hashimoto seed (keccak512(header || nonce_le)) ----------
__kernel void gpn_debug_seed(__global const uchar *header, ulong nonce, __global uchar *out) {
    uchar s[72];
    for (uint j = 0; j < 32; j++) s[j] = header[j];
    store64_le(s + 32, nonce);
    uchar seed[64];
    keccak512(s, 40, seed);
    for (uint j = 0; j < 64; j++) out[j] = seed[j];
}

// ---------- Debug: full hashimoto trace (seed, p[64], mix[128], cmix[32], result[32]) ----------
// Layout: [0..64]=seed, [64..320]=p (4B each), [320..448]=mix, [448..480]=cmix, [480..512]=result
__kernel void gpn_debug_trace(__global const uchar *header, ulong nonce,
                              __global const uchar *dag, ulong full_size,
                              __global uchar *out) {
    uchar s[72];
    for (uint j = 0; j < 32; j++) s[j] = header[j];
    store64_le(s + 32, nonce);
    uchar seed[64];
    keccak512(s, 40, seed);

    uchar mix[128];
    for (uint j = 0; j < 128; j++) mix[j] = seed[j % 64];

    ulong n = full_size / 64;
    uint w = 32;
    uint mixhashes = 2;
    uchar newdata[128];
    for (uint i = 0; i < 64; i++) {
        uint p = (uint)(fnv((uint)i ^ load32_le(seed), load32_le(mix + (i % w) * 4))
                        % (n / mixhashes)) * mixhashes;
        store32_le(out + 64 + i * 4, p);
        ulong off = (ulong)p * 64;
        for (uint j = 0; j < 128; j++) newdata[j] = dag[off + j];
        fnv128(mix, newdata);
    }
    uchar cmix[32];
    for (uint g = 0; g < 8; g++) {
        uint j = g * 4;
        uint a = fnv(load32_le(mix + j * 4), load32_le(mix + (j + 1) * 4));
        uint b = fnv(a, load32_le(mix + (j + 2) * 4));
        uint c = fnv(b, load32_le(mix + (j + 3) * 4));
        store32_le(cmix + g * 4, c);
    }
    uchar seedc[96];
    for (uint j = 0; j < 64; j++) seedc[j] = seed[j];
    for (uint j = 0; j < 32; j++) seedc[64 + j] = cmix[j];
    uchar result[32];
    keccak256(seedc, 96, result);

    for (uint j = 0; j < 64; j++) out[j] = seed[j];
    for (uint j = 0; j < 128; j++) out[320 + j] = mix[j];
    for (uint j = 0; j < 32; j++) out[448 + j] = cmix[j];
    for (uint j = 0; j < 32; j++) out[480 + j] = result[j];
}

// ---------- Kernel 1: DAG item generation ----------
// cache: __global cache bytes; item i written to dag[i*64 .. +64]
__kernel void gpn_dag_item(__global const uchar *cache, uint cache_len,
                           __global uchar *dag, ulong item_index) {
    ulong i = item_index + get_global_id(0);
    uint n = cache_len / 64;
    uint r = 64 / 4; // 16 words

    uchar mix[64];
    uint base = (uint)(i % n) * 64;
    for (uint j = 0; j < 64; j++) mix[j] = cache[base + j];
    store32_le(mix, load32_le(mix) ^ (uint)i);

    // mix = keccak512(mix)
    uchar tmp[64];
    for (uint j = 0; j < 64; j++) tmp[j] = mix[j];
    keccak512(tmp, 64, mix);

    for (uint j = 0; j < 256; j++) {
        uint idx = fnv((uint)i ^ j, load32_le(mix + (j % r) * 4)) % n;
        uchar item[64];
        uint ib = idx * 64;
        for (uint k = 0; k < 64; k++) item[k] = cache[ib + k];
        fnv64(mix, item);
    }

    uchar out[64];
    keccak512(mix, 64, out);
    __global uchar *dst = dag + i * 64;
    for (uint j = 0; j < 64; j++) dst[j] = out[j];
}

// ---------- Kernel 2: hashimoto ----------
// header: 32 bytes. target: 32 bytes little-endian. nonce_start: each work item
// handles nonce_start + get_global_id(0). dag: full dataset. On a solution,
// atomically append {nonce, mix[32], result[32]} to results and bump counter.
__kernel void gpn_hashimoto(__global const uchar *header, ulong nonce_start,
                            __global const uchar *dag, ulong full_size,
                            __global const uchar *target,
                            __global ulong *counter,
                            __global uchar *results, uint results_cap) {
    ulong nonce = nonce_start + get_global_id(0);

    // seed = keccak512(header || nonce_le)
    uchar s[72];
    for (uint j = 0; j < 32; j++) s[j] = header[j];
    store64_le(s + 32, nonce);
    uchar seed[64];
    keccak512(s, 40, seed);

    // mix = seed repeated twice (128 bytes)
    uchar mix[128];
    for (uint j = 0; j < 128; j++) mix[j] = seed[j % 64];

    ulong n = full_size / 64;
    uint w = 128 / 4;          // 32
    uint mixhashes = 128 / 64; // 2

    uchar newdata[128];
    for (uint i = 0; i < 64; i++) {
        uint p = (uint)(fnv((uint)i ^ load32_le(seed), load32_le(mix + (i % w) * 4))
                        % (n / mixhashes)) * mixhashes;
        ulong off = (ulong)p * 64;
        for (uint j = 0; j < 128; j++) newdata[j] = dag[off + j];
        fnv128(mix, newdata);
    }

    // compress mix -> cmix[32]
    uchar cmix[32];
    for (uint g = 0; g < 8; g++) {
        uint j = g * 4; // word index into mix
        uint a = fnv(load32_le(mix + j * 4), load32_le(mix + (j + 1) * 4));
        uint b = fnv(a, load32_le(mix + (j + 2) * 4));
        uint c = fnv(b, load32_le(mix + (j + 3) * 4));
        store32_le(cmix + g * 4, c);
    }

    // result = keccak256(seed || cmix)
    uchar seedc[96];
    for (uint j = 0; j < 64; j++) seedc[j] = seed[j];
    for (uint j = 0; j < 32; j++) seedc[64 + j] = cmix[j];
    uchar result[32];
    keccak256(seedc, 96, result);

    // compare result (LE) < target (LE)
    int lt = 1;
    for (int b = 31; b >= 0; b--) {
        if (result[b] != target[b]) { lt = (result[b] < target[b]) ? 1 : 0; break; }
    }
    if (lt) {
        uint idx = (uint)atomic_add(counter, 1);
        if (idx < results_cap) {
            __global uchar *r = results + (ulong)idx * 72;
            store64_le(r, nonce);
            for (uint j = 0; j < 32; j++) r[8 + j] = cmix[j];
            for (uint j = 0; j < 32; j++) r[40 + j] = result[j];
        }
    }
}
