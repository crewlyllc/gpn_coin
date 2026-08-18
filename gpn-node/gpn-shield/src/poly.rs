//! Polynomial arithmetic over `bls12_381::Scalar` (Fr) for the QAP reduction.
//!
//! Coefficients are stored low-degree-first (`p.0[i]` = coefficient of x^i),
//! kept in canonical (trimmed) form: leading coefficient is non-zero, and the
//! zero polynomial is `[0]` (degree 0).

use bls12_381::Scalar;
use ff::Field;

/// A univariate polynomial `p(x) = Σ p.0[i]·x^i` over Fr.
#[derive(Clone, Debug, PartialEq)]
pub struct Poly(pub Vec<Scalar>);

impl Poly {
    /// The zero polynomial.
    pub fn zero() -> Self {
        Poly(vec![Scalar::ZERO])
    }

    /// The constant polynomial `c`.
    pub fn constant(c: Scalar) -> Self {
        Poly(vec![c])
    }

    /// The monomial `x^n`.
    pub fn monomial(n: usize) -> Self {
        let mut v = vec![Scalar::ZERO; n + 1];
        v[n] = Scalar::ONE;
        Poly(v)
    }

    /// Trim trailing zeros so the leading coefficient is non-zero (canonical).
    pub fn trim(&mut self) {
        while self.0.len() > 1 && self.0.last() == Some(&Scalar::ZERO) {
            self.0.pop();
        }
        if self.0.is_empty() {
            self.0.push(Scalar::ZERO);
        }
    }

    /// Degree (index of highest non-zero coefficient). Zero poly has degree 0.
    pub fn deg(&self) -> usize {
        self.0.len() - 1
    }

    /// Evaluate at `x` via Horner's method.
    pub fn eval(&self, x: &Scalar) -> Scalar {
        let mut acc = Scalar::ZERO;
        for c in self.0.iter().rev() {
            acc *= x;
            acc += c;
        }
        acc
    }

    /// Polynomial addition.
    pub fn add(&self, o: &Poly) -> Poly {
        let n = self.0.len().max(o.0.len());
        let mut out = vec![Scalar::ZERO; n];
        for (i, c) in self.0.iter().enumerate() {
            out[i] += c;
        }
        for (i, c) in o.0.iter().enumerate() {
            out[i] += c;
        }
        let mut p = Poly(out);
        p.trim();
        p
    }

    /// Polynomial negation.
    pub fn neg(&self) -> Poly {
        Poly(self.0.iter().map(|c| -c).collect())
    }

    /// Polynomial subtraction.
    pub fn sub(&self, o: &Poly) -> Poly {
        self.add(&o.neg())
    }

    /// Scalar multiplication.
    pub fn scalar_mul(&self, s: &Scalar) -> Poly {
        if *s == Scalar::ZERO {
            return Poly::zero();
        }
        Poly(self.0.iter().map(|c| c * s).collect())
    }

    /// Polynomial multiplication (schoolbook, O(n·m)).
    pub fn mul(&self, o: &Poly) -> Poly {
        let mut out = vec![Scalar::ZERO; self.deg() + o.deg() + 1];
        for (i, a) in self.0.iter().enumerate() {
            if *a == Scalar::ZERO {
                continue;
            }
            for (j, b) in o.0.iter().enumerate() {
                out[i + j] += a * b;
            }
        }
        let mut p = Poly(out);
        p.trim();
        p
    }

    /// Polynomial long division: returns `(quotient, remainder)` with
    /// `self = quotient·divisor + remainder`, `deg(remainder) < deg(divisor)`.
    pub fn div_rem(&self, divisor: &Poly) -> (Poly, Poly) {
        assert!(divisor.deg() > 0 || divisor.0[0] != Scalar::ZERO, "division by zero");
        let mut rem = self.0.clone();
        let d = divisor.deg();
        let lead = divisor.0[d];
        let lead_inv = lead.invert().expect("divisor lead non-zero");
        let mut q = vec![Scalar::ZERO; rem.len().saturating_sub(d)];
        // Work from highest degree down.
        let mut i = rem.len();
        while i > d {
            i -= 1;
            if rem[i] == Scalar::ZERO {
                continue;
            }
            let coef = rem[i] * lead_inv;
            q[i - d] = coef;
            // rem -= coef · x^(i-d) · divisor
            for (j, c) in divisor.0.iter().enumerate() {
                rem[i - d + j] -= coef * c;
            }
        }
        let mut rem_p = Poly(rem);
        rem_p.trim();
        let mut q_p = Poly(q);
        q_p.trim();
        (q_p, rem_p)
    }
}

/// Lagrange interpolation: the unique polynomial of degree < `xs.len()`
/// satisfying `p(xs[i]) = ys[i]` for all `i`. Naive O(n²) — fine for the
/// small (≤ a few constraints) QAP circuits we build.
pub fn interpolate(xs: &[Scalar], ys: &[Scalar]) -> Poly {
    assert_eq!(xs.len(), ys.len(), "interpolate: point/value count mismatch");
    assert!(!xs.is_empty(), "interpolate: empty domain");
    let n = xs.len();
    // Result = Σ_j ys[j] · ℓ_j(x),  ℓ_j(x) = ∏_{k≠j} (x - x_k)/(x_j - x_k)
    let mut out = Poly::zero();
    for j in 0..n {
        let mut num = Poly::constant(Scalar::ONE);
        let mut den = Scalar::ONE;
        for k in 0..n {
            if k == j {
                continue;
            }
            // num *= (x - x_k)
            let factor = Poly(vec![-(xs[k]), Scalar::ONE]);
            num = num.mul(&factor);
            den *= xs[j] - xs[k];
        }
        let lj = num.scalar_mul(&den.invert().expect("distinct domain points"));
        out = out.add(&lj.scalar_mul(&ys[j]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_horner_matches_naive() {
        // p(x) = 1 + 2x + 3x^2
        let p = Poly(vec![Scalar::from(1), Scalar::from(2), Scalar::from(3)]);
        let x = Scalar::from(5);
        assert_eq!(p.eval(&x), Scalar::from(1 + 2 * 5 + 3 * 25));
    }

    #[test]
    fn mul_and_deg() {
        let a = Poly(vec![Scalar::from(1), Scalar::from(1)]); // 1 + x
        let b = Poly(vec![Scalar::from(1), Scalar::from(1)]); // 1 + x
        let c = a.mul(&b); // 1 + 2x + x^2
        assert_eq!(c, Poly(vec![Scalar::from(1), Scalar::from(2), Scalar::from(1)]));
        assert_eq!(c.deg(), 2);
    }

    #[test]
    fn div_rem_exact() {
        // (1 + 2x + x^2) / (1 + x) = 1 + x, rem 0
        let num = Poly(vec![Scalar::from(1), Scalar::from(2), Scalar::from(1)]);
        let den = Poly(vec![Scalar::from(1), Scalar::from(1)]);
        let (q, r) = num.div_rem(&den);
        assert_eq!(q, Poly(vec![Scalar::from(1), Scalar::from(1)]));
        assert_eq!(r, Poly::zero());
        // check num = q*den + r
        assert_eq!(q.mul(&den).add(&r), num);
    }

    #[test]
    fn div_rem_with_remainder() {
        // (x^2 + 1) / x = x, rem 1
        let num = Poly(vec![Scalar::from(1), Scalar::ZERO, Scalar::from(1)]);
        let den = Poly(vec![Scalar::ZERO, Scalar::from(1)]); // x
        let (q, r) = num.div_rem(&den);
        assert_eq!(q, Poly(vec![Scalar::ZERO, Scalar::from(1)]));
        assert_eq!(r, Poly::constant(Scalar::from(1)));
        assert_eq!(q.mul(&den).add(&r), num);
    }

    #[test]
    fn interpolate_recovers_poly() {
        // Points (0,1),(1,3),(2,7) → p(x)=1 + x + x^2
        let xs = [Scalar::ZERO, Scalar::ONE, Scalar::from(2)];
        let ys = [Scalar::from(1), Scalar::from(3), Scalar::from(7)];
        let p = interpolate(&xs, &ys);
        for (x, y) in xs.iter().zip(ys.iter()) {
            assert_eq!(p.eval(x), *y, "interpolant must hit all points");
        }
        assert_eq!(p, Poly(vec![Scalar::from(1), Scalar::from(1), Scalar::from(1)]));
    }
}
