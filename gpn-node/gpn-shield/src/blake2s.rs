//! Full blake2s **compression function** as R1CS gadgets (Feature 2, step 5c).
//!
//! Gate #1 (Merkle membership) is done; the word-layer primitives (`WordCircuit`,
//! `word_add`, `word_xor`, `word_rotr`, `g_round`) are done (step 5c, C306).
//! This module assembles those primitives into the complete blake2s
//! **compression core** — 10 rounds × 8 G-calls with the RFC 7693 message
//! schedule, counter fold (`v[12] ^= t0`, `v[13] ^= t1`), and final
//! `h'[i] = h[i] ^ v[i] ^ v[i+8]` — as R1CS gates. This is the shared engine
//! for both remaining note-spend gates:
//!   - gate #2  `cm = NoteCommitment(v, rcm, addr)`  (a blake2s hash of the
//!     domain-tagged preimage), and
//!   - gate #3  `nf = Nullifier(rcm, position)`      (blake2s of `gpn/nf00 || rcm || pos`).
//!
//! The full 88-byte commitment preimage spans two 64-byte blocks (with the
//! parameter block + counter folding), which a later cycle wires together with
//! byte-to-word marshalling. Here we prove the compression itself is correct
//! in-field against a plain-Rust reference, and that the reference reproduces
//! the canonical RFC 7693 `blake2s-256` test vector end-to-end.

use crate::gadget::{g_round_ref, word_to_u32, Word, WordCircuit};

/// blake2s initial state (RFC 7693 §2.6).
pub const IV: [u32; 8] = [
    0x6A09_E667,
    0xBB67_AE85,
    0x3C6E_F372,
    0xA54F_F53A,
    0x510E_527F,
    0x9B05_688C,
    0x1F83_D9AB,
    0x5BE0_CD19,
];

/// blake2s message schedule — `sigma[r]` picks which message words feed each
/// of the 16 G-call slots in round `r` (RFC 7693 §2.7).
pub const SIGMA: [[usize; 16]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
];

/// A single G call on four named positions of the 16-word working state,
/// consuming message words `x`, `y`. Wraps `WordCircuit::g_round`.
pub fn g_round_at(
    c: &mut WordCircuit,
    v: &mut [Word; 16],
    a: usize,
    b: usize,
    d: usize,
    e: usize,
    x: &Word,
    y: &Word,
) {
    let mut quad = [v[a].clone(), v[b].clone(), v[d].clone(), v[e].clone()];
    c.g_round(&mut quad, x, y);
    v[a] = quad[0].clone();
    v[b] = quad[1].clone();
    v[d] = quad[2].clone();
    v[e] = quad[3].clone();
}

/// Full blake2s compression as R1CS gates:
/// `h' = compress(h, m, t0, t1, last)` over a 16-word message block and an
/// 8-word chaining state, with the counter words `t0`/`t1` folded into
/// `v[12]`/`v[13]` and — for the final block — the last-block flag folded into
/// `v[14] = ~v[14]`, exactly as RFC 7693 does. Returns the 8-word new state.
pub fn compress_gadget(
    c: &mut WordCircuit,
    h: &[Word; 8],
    m: &[Word; 16],
    t0: u32,
    t1: u32,
    last: bool,
) -> [Word; 8] {
    // Working state: v[0..8] = h, v[8..16] = IV.
    let mut v: [Word; 16] = std::array::from_fn(|i| {
        if i < 8 {
            h[i].clone()
        } else {
            c.alloc_word(IV[i - 8])
        }
    });
    // Counter fold: v[12] ^= t0, v[13] ^= t1.
    let tw0 = c.alloc_word(t0);
    let tw1 = c.alloc_word(t1);
    v[12] = c.word_xor(&v[12], &tw0);
    v[13] = c.word_xor(&v[13], &tw1);
    // Last-block flag: v[14] = ~v[14].
    if last {
        let ones = c.alloc_word(0xFFFF_FFFF);
        v[14] = c.word_xor(&v[14], &ones);
    }

    // 10 rounds × 8 G-calls with the sigma schedule.
    for r in 0..10 {
        let s = &SIGMA[r];
        g_round_at(c, &mut v, 0, 4, 8, 12, &m[s[0]], &m[s[1]]);
        g_round_at(c, &mut v, 1, 5, 9, 13, &m[s[2]], &m[s[3]]);
        g_round_at(c, &mut v, 2, 6, 10, 14, &m[s[4]], &m[s[5]]);
        g_round_at(c, &mut v, 3, 7, 11, 15, &m[s[6]], &m[s[7]]);
        g_round_at(c, &mut v, 0, 5, 10, 15, &m[s[8]], &m[s[9]]);
        g_round_at(c, &mut v, 1, 6, 11, 12, &m[s[10]], &m[s[11]]);
        g_round_at(c, &mut v, 2, 7, 8, 13, &m[s[12]], &m[s[13]]);
        g_round_at(c, &mut v, 3, 4, 9, 14, &m[s[14]], &m[s[15]]);
    }

    // Finalize: out[i] = h[i] ^ v[i] ^ v[i+8].
    let mut out: [Word; 8] = std::array::from_fn(|_| Vec::new());
    for i in 0..8 {
        let t = c.word_xor(&h[i], &v[i]);
        out[i] = c.word_xor(&t, &v[i + 8]);
    }
    out
}

/// Plain-Rust G call on four named positions of a `[u32; 16]` state.
pub fn g_round_at_ref(v: &mut [u32; 16], a: usize, b: usize, d: usize, e: usize, x: u32, y: u32) {
    let mut quad = [v[a], v[b], v[d], v[e]];
    g_round_ref(&mut quad, x, y);
    v[a] = quad[0];
    v[b] = quad[1];
    v[d] = quad[2];
    v[e] = quad[3];
}

/// Plain-Rust blake2s compression reference (mod-2^32 arithmetic).
pub fn compress_ref(h: &[u32; 8], m: &[u32; 16], t0: u32, t1: u32, last: bool) -> [u32; 8] {
    let mut v = [0u32; 16];
    v[..8].copy_from_slice(h);
    v[8..].copy_from_slice(&IV);
    v[12] ^= t0;
    v[13] ^= t1;
    if last {
        v[14] ^= 0xFFFF_FFFF;
    }
    for r in 0..10 {
        let s = &SIGMA[r];
        g_round_at_ref(&mut v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
        g_round_at_ref(&mut v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
        g_round_at_ref(&mut v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
        g_round_at_ref(&mut v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
        g_round_at_ref(&mut v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
        g_round_at_ref(&mut v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
        g_round_at_ref(&mut v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
        g_round_at_ref(&mut v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
    }
    let mut out = [0u32; 8];
    for i in 0..8 {
        out[i] = h[i] ^ v[i] ^ v[i + 8];
    }
    out
}

/// Convert a 64-byte block (little-endian words) into 16 u32 words.
fn block_to_words(buf: &[u8; 64]) -> [u32; 16] {
    let mut m = [0u32; 16];
    for i in 0..16 {
        let mut w = 0u32;
        for j in 0..4 {
            w |= (buf[i * 4 + j] as u32) << (8 * j);
        }
        m[i] = w;
    }
    m
}

/// Full self-contained `blake2s-256` (nn=32, kk=0) reference, used only to
/// validate `compress_ref` against the RFC 7693 test vector.
pub fn blake2s_256_ref(data: &[u8]) -> [u8; 32] {
    let nn = 32u32;
    let mut h = IV;
    // Parameter block: kk=0, nn=32 → h[0] ^= 0x01010000 ^ nn.
    h[0] ^= 0x0101_0000u32 ^ nn;
    let n = data.len();
    let blocks = if n == 0 { 1 } else { n.div_ceil(64) };
    let mut t: u64 = 0;
    for blk in 0..blocks {
        let start = blk * 64;
        let is_last = blk == blocks - 1;
        let mut buf = [0u8; 64];
        let len = if start < n { (n - start).min(64) } else { 0 };
        if start < n {
            buf[..len].copy_from_slice(&data[start..start + len]);
        }
        let m = block_to_words(&buf);
        // Counter increments by the ACTUAL bytes in this block; the final
        // (possibly padded) block counts only its real bytes, per the blake2s
        // reference implementation (blake2s_final uses buflen, not BLOCKBYTES).
        t += if is_last { len as u64 } else { 64 };
        let t0 = (t & 0xFFFF_FFFF) as u32;
        let t1 = (t >> 32) as u32;
        h = compress_ref(&h, &m, t0, t1, is_last);
    }
    // h as little-endian bytes (nn=32 → all 8 words).
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_le_bytes());
    }
    out
}

/// Helper: read an 8-word chaining state out of a witness assignment.
pub fn state_to_u32s(z: &[bls12_381::Scalar], h: &[Word; 8]) -> [u32; 8] {
    std::array::from_fn(|i| word_to_u32(z, &h[i]))
}

// ---------------------------------------------------------------------------
// Byte→word marshalling + full blake2s-256 hash gadget (gate #2 / gate #3)
// ---------------------------------------------------------------------------
//
// The on-chain note scheme (`pallet-shielded-pool::note`) hashes a domain-
// separated preimage with `blake2_256` (i.e. blake2s-256):
//   - commitment: `cm = H("gpn/cm00" || v.to_be_bytes() || rcm || addr)`  (88 bytes)
//   - nullifier:  `nf = H("gpn/nf00" || rcm || position.to_be_bytes())`  (48 bytes)
//
// This section turns the single-block `compress_gadget` into a *full* multi-
// block blake2s-256 hash gadget (byte→word marshalling + chaining across
// 64-byte blocks), then builds the two note-spend gates on top of it:
//   - gate #2 `note_commitment_gate(v, rcm, addr) -> cm` (88-byte preimage, 2 blocks)
//   - gate #3 `nullifier_gate(rcm, position) -> nf`     (48-byte preimage, 1 block)

/// A single byte as 8 boolean bit-vars (bit 0 = LSB), each boolean-constrained.
pub type Byte = [usize; 8];

/// Allocate a byte's 8 boolean bit-vars from a u8 value.
pub fn alloc_byte(c: &mut WordCircuit, b: u8) -> Byte {
    let mut bits = [0usize; 8];
    for i in 0..8 {
        let bit = ((b >> i) & 1) as u64;
        let bv = c.alloc(bls12_381::Scalar::from(bit));
        c.builder.bool_gate(bv);
        bits[i] = bv;
    }
    bits
}

/// Marshal 4 little-endian bytes into a 32-bit `Word` (bit `i` of the word is
/// bit `i % 8` of byte `i / 8`). Pure reindexing — no new constraints, since
/// the byte bits are already boolean-constrained.
pub fn word_from_4bytes(c: &mut WordCircuit, b: &[Byte; 4]) -> Word {
    let _ = c; // no allocation; bits are reused from the caller's bytes
    (0..32).map(|i| b[i / 8][i % 8]).collect()
}

/// Full blake2s-256 hash gadget over an arbitrary byte witness: applies the
/// parameter block (`h[0] ^= 0x01010020` for kk=0, nn=32), marshals each
/// 64-byte block into 16 words, and chains `compress_gadget` across blocks
/// with the byte-accurate counter (the final padded block counts only its real
/// bytes). Returns the 8-word digest state. `data` must be the caller's byte
/// bit-vars (each a `Byte`), so the preimage stays private.
pub fn blake2s_256_gadget(c: &mut WordCircuit, data: &[Byte]) -> [Word; 8] {
    // Parameter block: kk=0, nn=32 → h[0] ^= 0x01010000 ^ 32.
    let mut h: [Word; 8] = std::array::from_fn(|i| c.alloc_word(IV[i]));
    let param = c.alloc_word(0x0101_0000u32 ^ 32);
    h[0] = c.word_xor(&h[0], &param);

    // A shared all-zero byte for padding the final block.
    let zero_byte = alloc_byte(c, 0);

    let n = data.len();
    let blocks = if n == 0 { 1 } else { n.div_ceil(64) };
    let mut t: u64 = 0;
    for blk in 0..blocks {
        let start = blk * 64;
        let is_last = blk == blocks - 1;
        let len = if start < n { (n - start).min(64) } else { 0 };

        // Marshal bytes [start..start+64) into 16 words, zero-padded past len.
        let mut m: [Word; 16] = std::array::from_fn(|_| Vec::new());
        for w in 0..16 {
            let mut quad = [
                zero_byte.clone(),
                zero_byte.clone(),
                zero_byte.clone(),
                zero_byte.clone(),
            ];
            for j in 0..4 {
                let idx = start + w * 4 + j;
                if idx < n {
                    quad[j] = data[idx].clone();
                }
            }
            m[w] = word_from_4bytes(c, &quad);
        }

        // Counter increments by the ACTUAL bytes in this block (blake2s uses
        // buflen, not BLOCKBYTES, for the final padded block).
        t += if is_last { len as u64 } else { 64 };
        let t0 = (t & 0xFFFF_FFFF) as u32;
        let t1 = (t >> 32) as u32;
        h = compress_gadget(c, &h, &m, t0, t1, is_last);
    }
    h
}

/// Commitment preimage domain tag (must match `pallet-shielded-pool::note::CM_DOMAIN`).
pub const CM_DOMAIN: &[u8; 8] = b"gpn/cm00";
/// Nullifier preimage domain tag (must match `pallet-shielded-pool::note::NF_DOMAIN`).
pub const NF_DOMAIN: &[u8; 8] = b"gpn/nf00";

/// Convert the 8-word gadget state to the 32-byte digest (little-endian words).
pub fn digest_to_bytes(z: &[bls12_381::Scalar], state: &[Word; 8]) -> [u8; 32] {
    let words = state_to_u32s(z, state);
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[i * 4..i * 4 + 4].copy_from_slice(&words[i].to_le_bytes());
    }
    out
}

/// **Gate #2** — note commitment `cm = H("gpn/cm00" || v.to_be_bytes() || rcm || addr)`.
///
/// `v` is a u128 value, `rcm`/`addr` are 32-byte witness bit-vars. Returns the
/// 8-word digest state (commitment). Preimage = 8 + 16 + 32 + 32 = 88 bytes,
/// spanning 2 compression blocks.
pub fn note_commitment_gate(
    c: &mut WordCircuit,
    v: u128,
    rcm: &[Byte; 32],
    addr: &[Byte; 32],
) -> [Word; 8] {
    let mut pre: Vec<Byte> = Vec::with_capacity(8 + 16 + 32 + 32);
    for &b in CM_DOMAIN {
        pre.push(alloc_byte(c, b));
    }
    for b in v.to_be_bytes() {
        pre.push(alloc_byte(c, b));
    }
    pre.extend_from_slice(rcm);
    pre.extend_from_slice(addr);
    blake2s_256_gadget(c, &pre)
}

/// **Gate #3** — nullifier `nf = H("gpn/nf00" || rcm || position.to_be_bytes())`.
///
/// `rcm` is the 32-byte trapdoor witness, `position` the pool index (u64).
/// Preimage = 8 + 32 + 8 = 48 bytes, spanning 1 block.
pub fn nullifier_gate(c: &mut WordCircuit, rcm: &[Byte; 32], position: u64) -> [Word; 8] {
    let mut pre: Vec<Byte> = Vec::with_capacity(8 + 32 + 8);
    for &b in NF_DOMAIN {
        pre.push(alloc_byte(c, b));
    }
    pre.extend_from_slice(rcm);
    for b in position.to_be_bytes() {
        pre.push(alloc_byte(c, b));
    }
    blake2s_256_gadget(c, &pre)
}

/// Allocate a 32-byte witness array from raw bytes (for building gates in tests
/// and the spend circuit).
pub fn alloc_bytes32(c: &mut WordCircuit, bytes: &[u8; 32]) -> [Byte; 32] {
    std::array::from_fn(|i| alloc_byte(c, bytes[i]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gadget::constraints_hold;
    use ff::Field;

    #[test]
    fn compress_ref_matches_rfc7693_empty_vector() {
        // RFC 7693 §Appendix A: blake2s-256 of the empty string.
        let digest = blake2s_256_ref(b"");
        let expected: [u8; 32] = [
            0x69, 0x21, 0x7a, 0x30, 0x79, 0x90, 0x80, 0x94, 0xe1, 0x11, 0x21, 0xd0, 0x42, 0x35,
            0x4a, 0x7c, 0x1f, 0x55, 0xb6, 0x48, 0x2c, 0xa1, 0xa5, 0x1e, 0x1b, 0x25, 0x0d, 0xfd,
            0x1e, 0xd0, 0xee, 0xf9,
        ];
        assert_eq!(digest, expected, "blake2s-256('') must match RFC 7693");
    }

    #[test]
    fn compress_ref_matches_rfc7693_abc_vector() {
        // blake2s-256 of "abc" (RFC 7693 Appendix A).
        let digest = blake2s_256_ref(b"abc");
        let expected: [u8; 32] = [
            0x50, 0x8c, 0x5e, 0x8c, 0x32, 0x7c, 0x14, 0xe2, 0xe1, 0xa7, 0x2b, 0xa3, 0x4e, 0xeb,
            0x45, 0x2f, 0x37, 0x45, 0x8b, 0x20, 0x9e, 0xd6, 0x3a, 0x29, 0x4d, 0x99, 0x9b, 0x4c,
            0x86, 0x67, 0x59, 0x82,
        ];
        assert_eq!(digest, expected, "blake2s-256('abc') must match RFC 7693");
    }

    #[test]
    fn compress_gadget_matches_reference() {
        // Drive the R1CS gadget with a deterministic h/m/t and check the witness
        // satisfies every constraint AND equals compress_ref's output.
        let h = [
            0x6A09_E667u32,
            0xBB67_AE85,
            0x3C6E_F372,
            0xA54F_F53A,
            0x510E_527F,
            0x9B05_688C,
            0x1F83_D9AB,
            0x5BE0_CD19,
        ];
        let m = [
            0x0000_0001,
            0x0000_0002,
            0x0000_0003,
            0x0000_0004,
            0x0000_0005,
            0x0000_0006,
            0x0000_0007,
            0x0000_0008,
            0x0000_0009,
            0x0000_000A,
            0x0000_000B,
            0x0000_000C,
            0x0000_000D,
            0x0000_000E,
            0x0000_000F,
            0x0000_0010,
        ];
        let t0 = 64u32;
        let t1 = 0u32;
        let expected = compress_ref(&h, &m, t0, t1, true);

        let mut c = WordCircuit::new();
        let hw: [Word; 8] = std::array::from_fn(|i| c.alloc_word(h[i]));
        let mw: [Word; 16] = std::array::from_fn(|i| c.alloc_word(m[i]));
        let out = compress_gadget(&mut c, &hw, &mw, t0, t1, true);
        let (r1cs, z) = c.into_parts(1);
        assert!(
            constraints_hold(&r1cs, &z),
            "all R1CS constraints must hold"
        );
        let got = state_to_u32s(&z, &out);
        assert_eq!(got, expected, "gadget compression must equal reference");
    }

    #[test]
    fn compress_gadget_produces_real_blake2s_empty() {
        // The R1CS gadget's compression of the parameter-block state over an
        // all-zero 64-byte block with the last-block flag must reproduce the
        // real blake2s-256('') digest — a full gadget↔real-hash end-to-end check.
        let expected = blake2s_256_ref(b"");
        let mut h = IV;
        h[0] ^= 0x0101_0000u32 ^ 32; // blake2s-256 parameter block (kk=0)
        let m = [0u32; 16];
        let t0 = 0u32;
        let t1 = 0u32;

        let mut c = WordCircuit::new();
        let hw: [Word; 8] = std::array::from_fn(|i| c.alloc_word(h[i]));
        let mw: [Word; 16] = std::array::from_fn(|i| c.alloc_word(m[i]));
        let out = compress_gadget(&mut c, &hw, &mw, t0, t1, true);
        let (r1cs, z) = c.into_parts(1);
        assert!(
            constraints_hold(&r1cs, &z),
            "all R1CS constraints must hold"
        );
        let got = state_to_u32s(&z, &out);
        let mut bytes = [0u8; 32];
        for i in 0..8 {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&got[i].to_le_bytes());
        }
        assert_eq!(
            bytes, expected,
            "gadget must reproduce real blake2s-256('')"
        );
    }

    #[test]
    fn compress_gadget_tamper_breaks_satisfiability() {
        // Confirm the circuit actually enforces the compression: mutate an input
        // word's LSB after the fact and the witness must stop satisfying.
        let h = [1, 2, 3, 4, 5, 6, 7, 8];
        let m = [
            9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        ];
        let t0 = 128;
        let t1 = 0;
        let mut c = WordCircuit::new();
        let hw: [Word; 8] = std::array::from_fn(|i| c.alloc_word(h[i]));
        let mw: [Word; 16] = std::array::from_fn(|i| c.alloc_word(m[i]));
        let out = compress_gadget(&mut c, &hw, &mw, t0, t1, false);
        let (r1cs, z) = c.into_parts(1);
        assert!(constraints_hold(&r1cs, &z));
        // Flip the LSB of message word 0 → contradicts its bit decomposition.
        let mut zbad = z.clone();
        zbad[mw[0][0]] = bls12_381::Scalar::ONE - zbad[mw[0][0]];
        assert!(!constraints_hold(&r1cs, &zbad));
    }

    #[test]
    fn full_hash_gadget_matches_ref_across_blocks() {
        // 88-byte preimage (spans 2 blocks: 64 + 24) — the exact shape of the
        // commitment preimage. The full multi-block hash gadget must reproduce
        // blake2s_256_ref byte-for-byte, and the witness must satisfy every
        // constraint.
        let data: Vec<u8> = (0u8..88).collect();
        let mut c = WordCircuit::new();
        let bytes: Vec<Byte> = data.iter().map(|&b| alloc_byte(&mut c, b)).collect();
        let state = blake2s_256_gadget(&mut c, &bytes);
        let (r1cs, z) = c.into_parts(1);
        assert!(
            constraints_hold(&r1cs, &z),
            "all R1CS constraints must hold"
        );
        let got = digest_to_bytes(&z, &state);
        assert_eq!(
            got,
            blake2s_256_ref(&data),
            "multi-block hash must match ref"
        );
    }

    #[test]
    fn note_commitment_gate_matches_ref() {
        // Gate #2 must reproduce blake2s-256("gpn/cm00"||v_be||rcm||addr).
        let v: u128 = 123456789012345;
        let rcm_raw: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(3).wrapping_add(1));
        let addr_raw: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(5).wrapping_add(2));
        let mut c = WordCircuit::new();
        let rcm = alloc_bytes32(&mut c, &rcm_raw);
        let addr = alloc_bytes32(&mut c, &addr_raw);
        let cm = note_commitment_gate(&mut c, v, &rcm, &addr);
        let (r1cs, z) = c.into_parts(1);
        assert!(
            constraints_hold(&r1cs, &z),
            "commitment gate constraints must hold"
        );
        let got = digest_to_bytes(&z, &cm);
        // Reference: same preimage bytes as the on-chain note::note_commitment.
        let mut pre = Vec::with_capacity(8 + 16 + 32 + 32);
        pre.extend_from_slice(CM_DOMAIN);
        pre.extend_from_slice(&v.to_be_bytes());
        pre.extend_from_slice(&rcm_raw);
        pre.extend_from_slice(&addr_raw);
        assert_eq!(
            got,
            blake2s_256_ref(&pre),
            "cm must match on-chain construction"
        );
    }

    #[test]
    fn nullifier_gate_matches_ref() {
        // Gate #3 must reproduce blake2s-256("gpn/nf00"||rcm||position_be).
        let rcm_raw: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(7).wrapping_add(4));
        let position: u64 = 424242;
        let mut c = WordCircuit::new();
        let rcm = alloc_bytes32(&mut c, &rcm_raw);
        let nf = nullifier_gate(&mut c, &rcm, position);
        let (r1cs, z) = c.into_parts(1);
        assert!(
            constraints_hold(&r1cs, &z),
            "nullifier gate constraints must hold"
        );
        let got = digest_to_bytes(&z, &nf);
        let mut pre = Vec::with_capacity(8 + 32 + 8);
        pre.extend_from_slice(NF_DOMAIN);
        pre.extend_from_slice(&rcm_raw);
        pre.extend_from_slice(&position.to_be_bytes());
        assert_eq!(
            got,
            blake2s_256_ref(&pre),
            "nf must match on-chain construction"
        );
    }

    #[test]
    fn commitment_gate_tamper_breaks_satisfiability() {
        // Flip one bit of rcm in the witness → the commitment gates must break.
        let v: u128 = 999;
        let rcm_raw: [u8; 32] = [7u8; 32];
        let addr_raw: [u8; 32] = [9u8; 32];
        let mut c = WordCircuit::new();
        let rcm = alloc_bytes32(&mut c, &rcm_raw);
        let addr = alloc_bytes32(&mut c, &addr_raw);
        let _cm = note_commitment_gate(&mut c, v, &rcm, &addr);
        let (r1cs, z) = c.into_parts(1);
        assert!(constraints_hold(&r1cs, &z));
        // Tamper: flip the LSB of rcm byte 0 (bit var rcm[0][0]).
        let mut zbad = z.clone();
        zbad[rcm[0][0]] = bls12_381::Scalar::ONE - zbad[rcm[0][0]];
        assert!(
            !constraints_hold(&r1cs, &zbad),
            "flipping rcm bit must break cm"
        );
    }
}
