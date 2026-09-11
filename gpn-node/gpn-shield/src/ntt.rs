//! Radix-2 NTT over the BLS12-381 scalar field (`bls12_381::Scalar`, Fr).
//!
//! Required to make the Groth16 prover scale to large circuits (e.g. the
//! note-spend circuit, ~87k vars × ~121k constraints): polynomial
//! multiplication and interpolation must run in O(N log N) rather than O(N²).
//!
//! The field has **2-adicity 32** (`ff::PrimeField::S = 32`), so NTTs of any
//! power-of-two length up to 2³² are supported — far more than we need.
//!
//! `Scalar::ROOT_OF_UNITY` is a primitive 2^S-th root of unity; the n-th root
//! for a transform of length n = 2^k is `ROOT_OF_UNITY^(2^(S-k))`.
//!
//! This module is **internal to the prover** — it never touches the on-chain
//! verifier or the proof byte format (G1/G2 compressed + Fr big-endian).

use bls12_381::Scalar;
use ff::Field;

/// `S` — the 2-adicity of the scalar field (2^S divides p−1, and 2^(S+1) does
/// not). Matches `ff::PrimeField::S` for `bls12_381::Scalar`.
const S: u32 = <Scalar as ff::PrimeField>::S;

/// Return a primitive `n`-th root of unity (n must be a power of two, n ≤ 2^S).
pub fn root_of_unity(n: usize) -> Scalar {
    assert!(n.is_power_of_two(), "NTT size must be a power of two");
    let k = n.trailing_zeros();
    assert!(k <= S, "NTT size exceeds the field's 2-adicity");
    // ROOT_OF_UNITY has order 2^S. Raise to 2^(S-k) to get order 2^k = n.
    let mut r = <Scalar as ff::PrimeField>::ROOT_OF_UNITY;
    for _ in 0..(S - k) {
        r = r * r;
    }
    r
}

/// `base^exp` via square-and-multiply (exp is a small u64).
fn pow_u64(base: Scalar, mut exp: u64) -> Scalar {
    let mut result = Scalar::ONE;
    let mut b = base;
    while exp > 0 {
        if exp & 1 == 1 {
            result *= b;
        }
        b *= b;
        exp >>= 1;
    }
    result
}

/// `base^exp` via square-and-multiply (small u64 exponent). Public so tests and
/// the QAP evaluator can compute `ω^j` domain nodes.
pub fn scalar_pow(base: Scalar, mut exp: u64) -> Scalar {
    let mut result = Scalar::ONE;
    let mut b = base;
    while exp > 0 {
        if exp & 1 == 1 {
            result *= b;
        }
        b *= b;
        exp >>= 1;
    }
    result
}

/// In-place iterative radix-2 Cooley–Tukey NTT. `a.len()` must be a power of
/// two. If `invert` is true, computes the inverse transform (including the
/// 1/n scaling), so `ntt(ntt(a,false),true) == a`.
pub fn ntt(a: &mut [Scalar], invert: bool) {
    let n = a.len();
    if n <= 1 {
        return;
    }
    assert!(n.is_power_of_two(), "NTT size must be a power of two");
    let log_n = n.trailing_zeros();

    // Bit-reversal permutation.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            a.swap(i, j);
        }
    }

    // Butterfly stages.
    let mut root = root_of_unity(n);
    if invert {
        root = root.invert().expect("root of unity is invertible");
    }
    // len = current transform block size (2, 4, 8, ... n)
    let mut len = 2usize;
    while len <= n {
        // w_len = root^(n/len) is a primitive len-th root of unity.
        let w_len = pow_u64(root, (n / len) as u64);
        let half = len / 2;
        let mut i = 0usize;
        while i < n {
            let mut w = Scalar::ONE;
            for k in 0..half {
                let u = a[i + k];
                let v = a[i + k + half] * w;
                a[i + k] = u + v;
                a[i + k + half] = u - v;
                w *= w_len;
            }
            i += len;
        }
        len <<= 1;
    }

    if invert {
        let n_inv = Scalar::from(n as u64).invert().expect("n invertible");
        for x in a.iter_mut() {
            *x *= n_inv;
        }
    }
    let _ = log_n;
}

/// Next power of two ≥ `n` (returns 1 for n ≤ 1).
pub fn next_pow2(n: usize) -> usize {
    let mut p = 1usize;
    while p < n {
        p <<= 1;
    }
    p
}

/// Polynomial multiplication via NTT. Inputs/outputs are coefficient vectors
/// (low-degree first). Returns `a * b`, trimmed to the exact product length.
pub fn mul_ntt(a: &[Scalar], b: &[Scalar]) -> Vec<Scalar> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let out_len = a.len() + b.len() - 1;
    let n = next_pow2(out_len);
    let mut fa = vec![Scalar::ZERO; n];
    let mut fb = vec![Scalar::ZERO; n];
    fa[..a.len()].copy_from_slice(a);
    fb[..b.len()].copy_from_slice(b);
    ntt(&mut fa, false);
    ntt(&mut fb, false);
    for i in 0..n {
        fa[i] *= fb[i];
    }
    ntt(&mut fa, true);
    fa.truncate(out_len);
    fa
}

/// Evaluate a coefficient vector at **all** `n`-th roots of unity
/// `ω^0, ω^1, …, ω^(n-1)` (n a power of two ≥ coeffs.len()). This is a forward
/// NTT of the (zero-padded) coefficient vector.
pub fn eval_on_roots(coeffs: &[Scalar], n: usize) -> Vec<Scalar> {
    assert!(n.is_power_of_two() && n >= coeffs.len());
    let mut a = vec![Scalar::ZERO; n];
    a[..coeffs.len()].copy_from_slice(coeffs);
    ntt(&mut a, false);
    a
}

/// Inverse of [`eval_on_roots`]: given values at `ω^0..ω^(n-1)`, recover the
/// unique polynomial of degree < n through them (coefficient vector).
pub fn interpolate_on_roots(vals: &[Scalar]) -> Vec<Scalar> {
    let n = vals.len();
    assert!(n.is_power_of_two(), "root-count must be a power of two");
    let mut a = vals.to_vec();
    ntt(&mut a, true);
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poly_eval(c: &[Scalar], x: Scalar) -> Scalar {
        let mut acc = Scalar::ZERO;
        for coef in c.iter().rev() {
            acc = acc * x + coef;
        }
        acc
    }

    #[test]
    fn ntt_roundtrip() {
        for n in [1usize, 2, 4, 8, 16, 1024] {
            let a: Vec<Scalar> = (0..n).map(|i| Scalar::from(i as u64 + 1)).collect();
            let mut b = a.clone();
            ntt(&mut b, false);
            ntt(&mut b, true);
            assert_eq!(a, b, "roundtrip failed for n={n}");
        }
    }

    #[test]
    fn mul_ntt_matches_schoolbook() {
        // Random-ish small polynomials; compare against schoolbook.
        let a: Vec<Scalar> = [3u64, 0, 5, 7, 9].iter().map(|&x| Scalar::from(x)).collect();
        let b: Vec<Scalar> = [1u64, 2, 4].iter().map(|&x| Scalar::from(x)).collect();
        let expected: Vec<Scalar> = {
            let mut out = vec![Scalar::ZERO; a.len() + b.len() - 1];
            for (i, &x) in a.iter().enumerate() {
                for (j, &y) in b.iter().enumerate() {
                    out[i + j] += x * y;
                }
            }
            out
        };
        assert_eq!(mul_ntt(&a, &b), expected);
    }

    #[test]
    fn eval_and_interpolate_on_roots() {
        // p(x) = 1 + 2x + 3x^2 + 4x^3 over a size-8 root domain.
        let coeffs: Vec<Scalar> = (1..=4u64).map(Scalar::from).collect();
        let n = 8;
        let evals = eval_on_roots(&coeffs, n);
        // Each eval at ω^i must equal p(ω^i).
        let w = root_of_unity(n);
        let mut wi = Scalar::ONE;
        for e in &evals {
            assert_eq!(*e, poly_eval(&coeffs, wi), "eval mismatch");
            wi *= w;
        }
        // Interpolation recovers the coefficients (padded to n).
        let back = interpolate_on_roots(&evals);
        for (i, c) in coeffs.iter().enumerate() {
            assert_eq!(back[i], *c, "coeff {i} mismatch");
        }
        for i in coeffs.len()..n {
            assert_eq!(back[i], Scalar::ZERO, "padding coeff {i} must be 0");
        }
    }

    #[test]
    fn large_mul_is_consistent_at_root_points() {
        // A moderately large product: verify mul_ntt at several points against
        // direct evaluation of the factors.
        let a: Vec<Scalar> = (0..1000u64).map(|i| Scalar::from(i * 7 + 1)).collect();
        let b: Vec<Scalar> = (0..700u64).map(|i| Scalar::from(i * 3 + 2)).collect();
        let c = mul_ntt(&a, &b);
        assert_eq!(c.len(), a.len() + b.len() - 1);
        for &x in &[Scalar::from(2u64), Scalar::from(123456u64)] {
            assert_eq!(poly_eval(&c, x), poly_eval(&a, x) * poly_eval(&b, x));
        }
    }
}
