//! Elliptic-curve performance primitives for the Groth16 prover.
//!
//! Profiling the note-spend circuit (87k vars × 121k constraints) showed the
//! cost is dominated by two shapes of scalar multiplication:
//!
//! 1. **Multi-scalar multiplication (MSM)** — sums `Σ sᵢ·Pᵢ` over *varying*
//!    points (the A/B/C/b_g1/H terms in `prove`). The naive loop is O(n) point
//!    mults; Pippenger's bucket method is ~O(n / log n) group ops → ~10×.
//! 2. **Fixed-base multiplication** — `Σ`-free but many `g·sᵢ` with the **same**
//!    base `g` (building `a_g1/b_g1/b_g2/abc_delta/ht_delta` in `setup`). A
//!    windowed **shared precomputed table** turns each mult into ~32 point adds
//!    instead of ~hundreds of doublings+adds.
//!
//! Both are byte-for-byte equivalent to the naive computation (same group
//! element), only faster, so they never change proof bytes / keys.

use bls12_381::{G1Affine, G1Projective, G2Affine, G2Projective, Scalar};
use ff::Field;
use rayon::prelude::*;
use std::sync::OnceLock;

/// Little-endian bits of a scalar (canonical representation).
#[inline]
fn scalar_bytes(s: &Scalar) -> [u8; 32] {
    s.to_bytes()
}

/// Extract the `c`-bit window `w` (bits `[w·c, w·c + c)`) from a scalar's bytes.
#[inline]
fn window_digit(bytes: &[u8; 32], w: usize, c: usize) -> usize {
    let start = w * c;
    let mut v = 0usize;
    for b in 0..c {
        let bit = start + b;
        if bit < 256 {
            let set = (bytes[bit >> 3] >> (bit & 7)) & 1;
            v |= (set as usize) << b;
        }
    }
    v
}

/// Number of `c`-bit windows needed to cover a 255-bit scalar field element.
#[inline]
fn num_windows(c: usize) -> usize {
    (255 + c - 1) / c
}

// ===========================================================================
// Pippenger multi-scalar multiplication
// ===========================================================================

/// Pippenger `Σ scalars[i] · points[i]` over G1. Byte-identical to the naive sum.
///
/// Uses an adaptive window size (small for small inputs) and falls back to a
/// parallel naive sum for tiny inputs, so it is never *slower* than naive.
pub fn msm_g1(points: &[G1Affine], scalars: &[Scalar]) -> G1Projective {
    assert_eq!(points.len(), scalars.len());
    let n = points.len();
    if n == 0 {
        return G1Projective::identity();
    }
    // Small inputs: bucket machinery doesn't pay off — plain parallel sum.
    if n < 64 {
        return naive_g1_sum(points, scalars);
    }
    // Adaptive window size ~ log2(n), clamped to [4, 14].
    let lg = (usize::BITS - n.leading_zeros()) as usize; // ≈ floor(log2 n)+1
    let c = lg.saturating_sub(1).clamp(4, 14);
    pippenger_g1(points, scalars, c)
}

fn naive_g1_sum(points: &[G1Affine], scalars: &[Scalar]) -> G1Projective {
    points
        .par_iter()
        .zip(scalars.par_iter())
        .map(|(p, s)| G1Projective::from(*p) * s)
        .reduce(G1Projective::identity, |a, b| a + b)
}

fn pippenger_g1(points: &[G1Affine], scalars: &[Scalar], c: usize) -> G1Projective {
    let w_count = num_windows(c);
    let n_buckets = 1usize << c;
    let bytes: Vec<[u8; 32]> = scalars.iter().map(scalar_bytes).collect();

    let mut acc = G1Projective::identity();
    for w in (0..w_count).rev() {
        let mut buckets = vec![G1Projective::identity(); n_buckets];
        for (p, b) in points.iter().zip(bytes.iter()) {
            let d = window_digit(b, w, c);
            if d != 0 {
                buckets[d] = buckets[d] + G1Projective::from(*p);
            }
        }
        let mut partial = G1Projective::identity();
        let mut running = G1Projective::identity();
        for j in (1..n_buckets).rev() {
            running = running + buckets[j];
            partial = partial + running;
        }
        for _ in 0..c {
            acc = acc.double();
        }
        acc = acc + partial;
    }
    acc
}

/// Pippenger `Σ scalars[i] · points[i]` over G2. Byte-identical to the naive sum.
pub fn msm_g2(points: &[G2Affine], scalars: &[Scalar]) -> G2Projective {
    assert_eq!(points.len(), scalars.len());
    let n = points.len();
    if n == 0 {
        return G2Projective::identity();
    }
    if n < 64 {
        return naive_g2_sum(points, scalars);
    }
    let lg = (usize::BITS - n.leading_zeros()) as usize;
    let c = lg.saturating_sub(1).clamp(4, 14);
    pippenger_g2(points, scalars, c)
}

fn naive_g2_sum(points: &[G2Affine], scalars: &[Scalar]) -> G2Projective {
    points
        .par_iter()
        .zip(scalars.par_iter())
        .map(|(p, s)| G2Projective::from(*p) * s)
        .reduce(G2Projective::identity, |a, b| a + b)
}

fn pippenger_g2(points: &[G2Affine], scalars: &[Scalar], c: usize) -> G2Projective {
    let w_count = num_windows(c);
    let n_buckets = 1usize << c;
    let bytes: Vec<[u8; 32]> = scalars.iter().map(scalar_bytes).collect();

    let mut acc = G2Projective::identity();
    for w in (0..w_count).rev() {
        let mut buckets = vec![G2Projective::identity(); n_buckets];
        for (p, b) in points.iter().zip(bytes.iter()) {
            let d = window_digit(b, w, c);
            if d != 0 {
                buckets[d] = buckets[d] + G2Projective::from(*p);
            }
        }
        let mut partial = G2Projective::identity();
        let mut running = G2Projective::identity();
        for j in (1..n_buckets).rev() {
            running = running + buckets[j];
            partial = partial + running;
        }
        for _ in 0..c {
            acc = acc.double();
        }
        acc = acc + partial;
    }
    acc
}

// ===========================================================================
// Fixed-base windowed multiplication (shared table, many scalars)
// ===========================================================================

/// A precomputed table for fast repeated multiplication of a **fixed** G1 base.
struct FixedBaseG1 {
    /// `table[w][d] = d · 2^(C·w) · base`.
    table: Vec<Vec<G1Projective>>,
    c: usize,
    windows: usize,
}

impl FixedBaseG1 {
    fn new(base: G1Projective, c: usize) -> Self {
        let windows = num_windows(c);
        let size = 1usize << c;
        // Window 0: table[0][d] = d·base.
        let mut t0 = Vec::with_capacity(size);
        t0.push(G1Projective::identity());
        for d in 1..size {
            t0.push(t0[d - 1] + base);
        }
        let mut table = Vec::with_capacity(windows);
        table.push(t0);
        for w in 1..windows {
            // table[w][d] = 2^c · table[w-1][d].
            let prev = &table[w - 1];
            let mut cur = Vec::with_capacity(size);
            for p in prev {
                let mut q = *p;
                for _ in 0..c {
                    q = q.double();
                }
                cur.push(q);
            }
            table.push(cur);
        }
        FixedBaseG1 { table, c, windows }
    }

    #[inline]
    fn mul(&self, bytes: &[u8; 32]) -> G1Projective {
        let mut acc = G1Projective::identity();
        for w in 0..self.windows {
            let d = window_digit(bytes, w, self.c);
            if d != 0 {
                acc = acc + self.table[w][d];
            }
        }
        acc
    }
}

/// Multiply the fixed G1 generator by each scalar, in parallel. Returns affine
/// points byte-identical to `scalars.map(|s| (g1 * s).into())`.
///
/// The windowed table for the generator is built **once per process** and
/// cached, so repeated `setup` calls (across circuits/tests) pay no table cost.
pub fn fixed_base_g1_gen(scalars: &[Scalar]) -> Vec<G1Affine> {
    let table = g1_gen_table();
    scalars
        .par_iter()
        .map(|s| {
            let b = scalar_bytes(s);
            table.mul(&b).into()
        })
        .collect()
}

fn g1_gen_table() -> &'static FixedBaseG1 {
    static T: OnceLock<FixedBaseG1> = OnceLock::new();
    T.get_or_init(|| FixedBaseG1::new(G1Projective::generator(), 8))
}

/// Like [`fixed_base_g1_gen`] but with a caller-supplied per-scalar extra
/// factor `inv` folded in (computes `g · (sᵢ·inv)`), avoiding a second mult.
pub fn fixed_base_g1_gen_scaled(scalars: &[Scalar], inv: &Scalar) -> Vec<G1Affine> {
    let table = g1_gen_table();
    scalars
        .par_iter()
        .map(|s| {
            let b = scalar_bytes(&(*s * inv));
            table.mul(&b).into()
        })
        .collect()
}

/// A precomputed table for fast repeated multiplication of a **fixed** G2 base.
struct FixedBaseG2 {
    table: Vec<Vec<G2Projective>>,
    c: usize,
    windows: usize,
}

impl FixedBaseG2 {
    fn new(base: G2Projective, c: usize) -> Self {
        let windows = num_windows(c);
        let size = 1usize << c;
        let mut t0 = Vec::with_capacity(size);
        t0.push(G2Projective::identity());
        for d in 1..size {
            t0.push(t0[d - 1] + base);
        }
        let mut table = Vec::with_capacity(windows);
        table.push(t0);
        for w in 1..windows {
            let prev = &table[w - 1];
            let mut cur = Vec::with_capacity(size);
            for p in prev {
                let mut q = *p;
                for _ in 0..c {
                    q = q.double();
                }
                cur.push(q);
            }
            table.push(cur);
        }
        FixedBaseG2 { table, c, windows }
    }

    #[inline]
    fn mul(&self, bytes: &[u8; 32]) -> G2Projective {
        let mut acc = G2Projective::identity();
        for w in 0..self.windows {
            let d = window_digit(bytes, w, self.c);
            if d != 0 {
                acc = acc + self.table[w][d];
            }
        }
        acc
    }
}

/// Multiply the fixed G2 generator by each scalar, in parallel.
pub fn fixed_base_g2_gen(scalars: &[Scalar]) -> Vec<G2Affine> {
    let table = g2_gen_table();
    scalars
        .par_iter()
        .map(|s| {
            let b = scalar_bytes(s);
            table.mul(&b).into()
        })
        .collect()
}

fn g2_gen_table() -> &'static FixedBaseG2 {
    static T: OnceLock<FixedBaseG2> = OnceLock::new();
    T.get_or_init(|| FixedBaseG2::new(G2Projective::generator(), 8))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_g1(points: &[G1Affine], scalars: &[Scalar]) -> G1Projective {
        let mut acc = G1Projective::identity();
        for (p, s) in points.iter().zip(scalars.iter()) {
            acc += G1Projective::from(*p) * s;
        }
        acc
    }

    fn naive_g2(points: &[G2Affine], scalars: &[Scalar]) -> G2Projective {
        let mut acc = G2Projective::identity();
        for (p, s) in points.iter().zip(scalars.iter()) {
            acc += G2Projective::from(*p) * s;
        }
        acc
    }

    fn rng_scalars(n: usize, seed: u64) -> Vec<Scalar> {
        let mut x = seed | 1;
        let mut next = || {
            x ^= x << 13; x ^= x >> 7; x ^= x << 17; x
        };
        (0..n).map(|_| {
            let a = next() & 0xFFFF_FFFF;
            let b = next() & 0xFFFF_FFFF;
            Scalar::from(a) + Scalar::from(b) * Scalar::from(1u64 << 32)
        }).collect()
    }

    fn rng_g1_points(n: usize, seed: u64) -> Vec<G1Affine> {
        let mut x = seed | 1;
        let mut next = || { x ^= x << 13; x ^= x >> 7; x ^= x << 17; x };
        let g = G1Projective::generator();
        (0..n).map(|_| {
            let s = Scalar::from(next() & 0xFFFF_FFFF) + Scalar::ONE;
            (g * s).into()
        }).collect()
    }

    fn rng_g2_points(n: usize, seed: u64) -> Vec<G2Affine> {
        let mut x = seed | 1;
        let mut next = || { x ^= x << 13; x ^= x >> 7; x ^= x << 17; x };
        let g = G2Projective::generator();
        (0..n).map(|_| {
            let s = Scalar::from(next() & 0xFFFF_FFFF) + Scalar::ONE;
            (g * s).into()
        }).collect()
    }

    #[test]
    fn msm_g1_matches_naive() {
        for n in [0usize, 1, 5, 17, 100, 500] {
            let pts = rng_g1_points(n, 0xA1 ^ n as u64);
            let sc = rng_scalars(n, 0xB2 ^ n as u64);
            assert_eq!(msm_g1(&pts, &sc), naive_g1(&pts, &sc), "msm_g1 mismatch n={n}");
        }
    }

    #[test]
    fn msm_g2_matches_naive() {
        for n in [0usize, 1, 5, 17, 100, 300] {
            let pts = rng_g2_points(n, 0xC3 ^ n as u64);
            let sc = rng_scalars(n, 0xD4 ^ n as u64);
            assert_eq!(msm_g2(&pts, &sc), naive_g2(&pts, &sc), "msm_g2 mismatch n={n}");
        }
    }

    #[test]
    fn msm_handles_zero_scalars_and_large_scalars() {
        let pts = rng_g1_points(8, 7);
        let mut sc = rng_scalars(8, 9);
        sc[2] = Scalar::ZERO;
        sc[5] = -Scalar::ONE; // large (near modulus) scalar
        sc[6] = crate::ntt::scalar_pow(Scalar::from(2u64), 200); // high bit
        assert_eq!(msm_g1(&pts, &sc), naive_g1(&pts, &sc));
    }

    #[test]
    fn fixed_base_g1_matches_naive() {
        let sc = rng_scalars(50, 0xE5);
        let got = fixed_base_g1_gen(&sc);
        let g = G1Projective::generator();
        for (s, p) in sc.iter().zip(got.iter()) {
            let want: G1Affine = (g * s).into();
            assert_eq!(*p, want, "fixed_base_g1 mismatch");
        }
    }

    #[test]
    fn fixed_base_g2_matches_naive() {
        let sc = rng_scalars(30, 0xF6);
        let got = fixed_base_g2_gen(&sc);
        let g = G2Projective::generator();
        for (s, p) in sc.iter().zip(got.iter()) {
            let want: G2Affine = (g * s).into();
            assert_eq!(*p, want, "fixed_base_g2 mismatch");
        }
    }

    #[test]
    fn fixed_base_scaled_matches_naive() {
        let sc = rng_scalars(20, 0x11);
        let inv = Scalar::from(1234567u64);
        let got = fixed_base_g1_gen_scaled(&sc, &inv);
        let g = G1Projective::generator();
        for (s, p) in sc.iter().zip(got.iter()) {
            let want: G1Affine = (g * (*s * inv)).into();
            assert_eq!(*p, want, "fixed_base scaled mismatch");
        }
    }
}
