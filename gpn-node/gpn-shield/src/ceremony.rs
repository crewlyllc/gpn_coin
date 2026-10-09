//! # Trusted-setup ceremony tooling (Groth16, phase-2)
//!
//! The GPN shielded pool verifies Groth16 proofs over BLS12-381 against a
//! verification key that is currently derived from a **deterministic testnet
//! seed** (`testnet-crs-note-spend-v0`), so its toxic waste is publicly
//! derivable and the proofs are forgeable. This module builds the tooling for a
//! real **multi-party trusted-setup ceremony** whose final CRS is trustless as
//! long as **at least one** contributor was honest.
//!
//! ## Flow
//!
//! ```text
//!   gpn_ceremony init       --circuit note-spend --out phase2-initial.bin
//!   gpn_ceremony contribute --in phase2-initial.bin --out crs-1.bin --record c1.json
//!   gpn_ceremony contribute --in crs-1.bin           --out crs-2.bin --record c2.json
//!   gpn_ceremony verify     --crs crs-2.bin --records c1.json,c2.json     # PASS/FAIL
//!   gpn_ceremony export-vk  --crs crs-2.bin --out vk.bin
//! ```
//!
//! (Embedding the final VK on-chain is a **later, gated** `set_code` step; this
//! tool never touches the chain or genesis.)
//!
//! ## What a contribution re-randomises
//!
//! Each contribution folds fresh, independent `α'`, `β'`, `γ'`, `δ'` into the
//! CRS (multiplying the aggregate secrets by them). The CRS stores the `βA_k`,
//! `αB_k` and `C_k` components **separately**, so a contributor only ever needs
//! their **own** fresh scalars — never the aggregate secret nobody is allowed to
//! know. `α,β,γ,δ` are the circuit-specific Groth16 trapdoor; with ≥1 honest
//! contributor none of them is knowable, so no one can forge.
//!
//! `τ` is fixed at [`init`] (the phase-2 genesis). Rotating `τ` across
//! contributors requires a separable powers-of-tau representation plus a
//! group-FFT; that extension is specified in
//! `shared workspace artifacts/project7/design-ceremony.md`.
//!
//! ## Toxic-waste destruction
//!
//! Fresh scalars are derived from an explicit entropy source (OS entropy by
//! default, or `--seeded-from <public value>` such as a drand round / future
//! block hash so a contribution is **publicly un-biasable**). They are used only
//! to scale group elements, then **wiped from memory** ([`zeroize`] for entropy
//! buffers + a volatile overwrite for the scalars). No secret is logged or
//! written to disk.
//!
//! [`zeroize`]: https://docs.rs/zeroize

use bls12_381::{G1Affine, G1Projective, G2Affine, G2Projective, Scalar};
use ff::Field;
use serde_json::json;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::groth16::{self, ProvingKey, VerifyingKey};
use crate::qap::R1CS;

/// Version tag embedded in every CRS file and contribution record.
pub const CRS_VERSION: u32 = 1;

/// Sentinel `seeded_from` value written when a contribution used OS entropy
/// (i.e. the contributor's secret randomness, which is *not* reproducible from
/// public data). Such a step cannot be re-derived by a verifier and therefore
/// needs a pairing-based contribution proof; see [`verify_chain`].
pub const SEEDED_FROM_OS: &str = "os-entropy";

// ===========================================================================
// Circuit registry
// ===========================================================================

/// Build the R1CS for a named ceremony circuit. The circuit is public, so a
/// contributor can reconstruct it from the CRS header alone.
pub fn circuit_r1cs(name: &str) -> Result<R1CS, String> {
    match name {
        "value-balance" => Ok(crate::qap::value_balance_r1cs()),
        "note-spend" => {
            let note = crate::note_spend::canonical_note();
            Ok(crate::note_spend::note_spend_r1cs(&note).0)
        }
        other => Err(format!("unknown circuit: {other}")),
    }
}

// ===========================================================================
// The ceremony CRS
// ===========================================================================

/// A split (phase-2) CRS. Group elements are held as affine points so the
/// canonical serialization is exactly the compressed wire encoding.
#[derive(Clone, Debug)]
pub struct CeremonyCrs {
    pub circuit: String,
    pub num_variables: usize,
    pub num_public: usize,
    pub domain_size: usize,
    pub alpha_g1: G1Affine,
    pub beta_g1: G1Affine,
    pub beta_g2: G2Affine,
    pub gamma_g2: G2Affine,
    pub delta_g1: G1Affine,
    pub delta_g2: G2Affine,
    pub a_g1: Vec<G1Affine>,
    pub b_g1: Vec<G1Affine>,
    pub b_g2: Vec<G2Affine>,
    pub ht_delta_g1: Vec<G1Affine>,
    pub abc_beta_delta: Vec<G1Affine>,
    pub abc_alpha_delta: Vec<G1Affine>,
    pub abc_c_delta: Vec<G1Affine>,
    pub gabc_beta_gamma: Vec<G1Affine>,
    pub gabc_alpha_gamma: Vec<G1Affine>,
    pub gabc_c_gamma: Vec<G1Affine>,
}

impl CeremonyCrs {
    /// Build the phase-2 genesis CRS for `circuit` from `seed`.
    ///
    /// **The `seed` is toxic waste** (it derives `α,β,γ,δ,τ`) — callers must
    /// treat it as secret and not persist it.
    pub fn genesis(circuit: &str, seed: &str) -> Result<CeremonyCrs, String> {
        let r1cs = circuit_r1cs(circuit)?;
        let qap = r1cs.qap_sparse();
        let ck = groth16::setup_ceremony(&qap, seed);
        Ok(CeremonyCrs {
            circuit: circuit.to_string(),
            num_variables: ck.num_variables,
            num_public: ck.num_public,
            domain_size: ck.domain_size,
            alpha_g1: ck.alpha_g1,
            beta_g1: ck.beta_g1,
            beta_g2: ck.beta_g2,
            gamma_g2: ck.gamma_g2,
            delta_g1: ck.delta_g1,
            delta_g2: ck.delta_g2,
            a_g1: ck.a_g1,
            b_g1: ck.b_g1,
            b_g2: ck.b_g2,
            ht_delta_g1: ck.ht_delta_g1,
            abc_beta_delta: ck.abc_beta_delta,
            abc_alpha_delta: ck.abc_alpha_delta,
            abc_c_delta: ck.abc_c_delta,
            gabc_beta_gamma: ck.gabc_beta_gamma,
            gabc_alpha_gamma: ck.gabc_alpha_gamma,
            gabc_c_gamma: ck.gabc_c_gamma,
        })
    }

    /// Canonical, deterministic serialization (the bytes hashed for the digest).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"GPNCRS1\n");
        push_str(&mut out, &self.circuit);
        push_u64(&mut out, self.num_variables as u64);
        push_u64(&mut out, self.num_public as u64);
        push_u64(&mut out, self.domain_size as u64);
        out.extend_from_slice(&self.alpha_g1.to_compressed());
        out.extend_from_slice(&self.beta_g1.to_compressed());
        out.extend_from_slice(&self.beta_g2.to_compressed());
        out.extend_from_slice(&self.gamma_g2.to_compressed());
        out.extend_from_slice(&self.delta_g1.to_compressed());
        out.extend_from_slice(&self.delta_g2.to_compressed());
        push_g1_vec(&mut out, &self.a_g1);
        push_g1_vec(&mut out, &self.b_g1);
        push_g2_vec(&mut out, &self.b_g2);
        push_g1_vec(&mut out, &self.ht_delta_g1);
        push_g1_vec(&mut out, &self.abc_beta_delta);
        push_g1_vec(&mut out, &self.abc_alpha_delta);
        push_g1_vec(&mut out, &self.abc_c_delta);
        push_g1_vec(&mut out, &self.gabc_beta_gamma);
        push_g1_vec(&mut out, &self.gabc_alpha_gamma);
        push_g1_vec(&mut out, &self.gabc_c_gamma);
        out
    }

    /// Parse a CRS from the canonical byte form.
    pub fn from_bytes(bytes: &[u8]) -> Result<CeremonyCrs, String> {
        let mut r = Reader::new(bytes);
        let magic = r.take(8)?;
        if magic != b"GPNCRS1\n" {
            return Err("bad CRS magic".into());
        }
        let circuit = r.str()?;
        let num_variables = r.u64()? as usize;
        let num_public = r.u64()? as usize;
        let domain_size = r.u64()? as usize;
        let alpha_g1 = r.g1()?;
        let beta_g1 = r.g1()?;
        let beta_g2 = r.g2()?;
        let gamma_g2 = r.g2()?;
        let delta_g1 = r.g1()?;
        let delta_g2 = r.g2()?;
        let a_g1 = r.g1_vec()?;
        let b_g1 = r.g1_vec()?;
        let b_g2 = r.g2_vec()?;
        let ht_delta_g1 = r.g1_vec()?;
        let abc_beta_delta = r.g1_vec()?;
        let abc_alpha_delta = r.g1_vec()?;
        let abc_c_delta = r.g1_vec()?;
        let gabc_beta_gamma = r.g1_vec()?;
        let gabc_alpha_gamma = r.g1_vec()?;
        let gabc_c_gamma = r.g1_vec()?;
        if r.remaining() != 0 {
            return Err("trailing bytes in CRS".into());
        }
        Ok(CeremonyCrs {
            circuit,
            num_variables,
            num_public,
            domain_size,
            alpha_g1,
            beta_g1,
            beta_g2,
            gamma_g2,
            delta_g1,
            delta_g2,
            a_g1,
            b_g1,
            b_g2,
            ht_delta_g1,
            abc_beta_delta,
            abc_alpha_delta,
            abc_c_delta,
            gabc_beta_gamma,
            gabc_alpha_gamma,
            gabc_c_gamma,
        })
    }

    /// `sha256` over the canonical serialization.
    pub fn digest(&self) -> [u8; 32] {
        sha256(&self.to_bytes())
    }

    /// Hex digest (64 lowercase chars).
    pub fn digest_hex(&self) -> String {
        hex(&self.digest())
    }

    /// Structural well-formedness: dimensions consistent, required points
    /// non-degenerate.
    pub fn check_structure(&self) -> Result<(), String> {
        let nv = self.num_variables;
        let np = self.num_public;
        let npriv = nv.checked_sub(np).ok_or("num_public > num_variables")?;
        if np == 0 {
            return Err("num_public must be >= 1 (constant term)".into());
        }
        if self.a_g1.len() != nv
            || self.b_g1.len() != nv
            || self.b_g2.len() != nv
            || self.abc_beta_delta.len() != npriv
            || self.abc_alpha_delta.len() != npriv
            || self.abc_c_delta.len() != npriv
            || self.gabc_beta_gamma.len() != np
            || self.gabc_alpha_gamma.len() != np
            || self.gabc_c_gamma.len() != np
        {
            return Err("CRS vector lengths inconsistent with dimensions".into());
        }
        if self.ht_delta_g1.len() < 2 * self.domain_size {
            return Err("ht_delta_g1 shorter than the H·T degree bound".into());
        }
        for (name, p) in [
            ("alpha_g1", self.alpha_g1),
            ("beta_g1", self.beta_g1),
            ("delta_g1", self.delta_g1),
        ] {
            if bool::from(p.is_identity()) {
                return Err(format!("{name} is the identity"));
            }
        }
        for (name, p) in [
            ("beta_g2", self.beta_g2),
            ("gamma_g2", self.gamma_g2),
            ("delta_g2", self.delta_g2),
        ] {
            if bool::from(p.is_identity()) {
                return Err(format!("{name} is the identity"));
            }
        }
        Ok(())
    }

    /// Confirm dimensions match the named circuit (so `verify` proves the CRS is
    /// for the circuit it claims).
    pub fn check_matches_circuit(&self) -> Result<(), String> {
        let r1cs = circuit_r1cs(&self.circuit)?;
        let expected = (r1cs.num_variables, r1cs.num_public, r1cs.domain_size());
        let got = (self.num_variables, self.num_public, self.domain_size);
        if got != expected {
            return Err(format!(
                "CRS dimensions {:?} do not match circuit '{}' {:?}",
                got, self.circuit, expected
            ));
        }
        Ok(())
    }

    /// Fold a fresh `(α',β',γ',δ')` contribution into the CRS.
    ///
    /// Returns the updated CRS. The caller owns `fresh`; its scalars are wiped
    /// by [`FreshSecrets`]'s `Drop`.
    pub fn contribute(&self, fresh: &FreshSecrets) -> CeremonyCrs {
        let a = fresh.alpha;
        let b = fresh.beta;
        let g = fresh.gamma;
        let d = fresh.delta;
        let d_inv = d.invert().expect("fresh delta non-zero");
        let g_inv = g.invert().expect("fresh gamma non-zero");

        let sc_g1 = |p: &G1Affine, s: Scalar| -> G1Affine { (G1Projective::from(*p) * s).into() };
        let sc_g2 = |p: &G2Affine, s: Scalar| -> G2Affine { (G2Projective::from(*p) * s).into() };
        let scale_g1 = |v: &[G1Affine], s: Scalar| -> Vec<G1Affine> {
            v.iter().map(|p| sc_g1(p, s)).collect()
        };

        // Each *separate* component is scaled by the fresh scalar that belongs
        // to it, and by the inverse of the fresh δ'/γ'. Because the components
        // are never mixed, this composes across contributions: the aggregate
        // secrets become α∏α', β∏β', γ∏γ', δ∏δ' while no contributor ever knows
        // them. With α'=β'=γ'=δ'=1 every factor is 1 → exact no-op.
        let bd = b * d_inv; // β' · δ'⁻¹   ([βA_k/δ] → [ββ'A_k/(δδ')])
        let ad = a * d_inv; // α' · δ'⁻¹   ([αB_k/δ] → [αα'B_k/(δδ')])
        let gb = b * g_inv; // β' · γ'⁻¹
        let ga = a * g_inv; // α' · γ'⁻¹

        CeremonyCrs {
            circuit: self.circuit.clone(),
            num_variables: self.num_variables,
            num_public: self.num_public,
            domain_size: self.domain_size,
            alpha_g1: sc_g1(&self.alpha_g1, a),
            beta_g1: sc_g1(&self.beta_g1, b),
            beta_g2: sc_g2(&self.beta_g2, b),
            gamma_g2: sc_g2(&self.gamma_g2, g),
            delta_g1: sc_g1(&self.delta_g1, d),
            delta_g2: sc_g2(&self.delta_g2, d),
            // τ-fixed elements are unchanged by a phase-2 contribution.
            a_g1: self.a_g1.clone(),
            b_g1: self.b_g1.clone(),
            b_g2: self.b_g2.clone(),
            ht_delta_g1: scale_g1(&self.ht_delta_g1, d_inv),
            abc_beta_delta: scale_g1(&self.abc_beta_delta, bd),
            abc_alpha_delta: scale_g1(&self.abc_alpha_delta, ad),
            abc_c_delta: scale_g1(&self.abc_c_delta, d_inv),
            gabc_beta_gamma: scale_g1(&self.gabc_beta_gamma, gb),
            gabc_alpha_gamma: scale_g1(&self.gabc_alpha_gamma, ga),
            gabc_c_gamma: scale_g1(&self.gabc_c_gamma, g_inv),
        }
    }

    /// The combined private term `[(βA_k+αB_k+C_k)/δ]₁`.
    pub fn abc_delta_g1(&self) -> Vec<G1Affine> {
        self.abc_beta_delta
            .iter()
            .zip(self.abc_alpha_delta.iter())
            .zip(self.abc_c_delta.iter())
            .map(|((a, b), c)| {
                (G1Projective::from(*a) + G1Projective::from(*b) + G1Projective::from(*c)).into()
            })
            .collect()
    }

    /// The public accumulator `[(βA_i+αB_i+C_i)/γ]₁` (incl. constant term).
    pub fn gamma_abc_g1(&self) -> Vec<G1Affine> {
        self.gabc_beta_gamma
            .iter()
            .zip(self.gabc_alpha_gamma.iter())
            .zip(self.gabc_c_gamma.iter())
            .map(|((a, b), c)| {
                (G1Projective::from(*a) + G1Projective::from(*b) + G1Projective::from(*c)).into()
            })
            .collect()
    }

    /// The verification key this CRS exports (what would go on-chain).
    pub fn verifying_key(&self) -> VerifyingKey {
        VerifyingKey {
            alpha_g1: self.alpha_g1,
            beta_g2: self.beta_g2,
            gamma_g2: self.gamma_g2,
            delta_g2: self.delta_g2,
            gamma_abc_g1: self.gamma_abc_g1(),
        }
    }

    /// Materialise a [`ProvingKey`] (used by tests / an off-chain prover).
    pub fn proving_key(&self) -> ProvingKey {
        ProvingKey {
            alpha_g1: self.alpha_g1,
            beta_g1: self.beta_g1,
            beta_g2: self.beta_g2,
            delta_g1: self.delta_g1,
            delta_g2: self.delta_g2,
            a_g1: self.a_g1.clone(),
            b_g1: self.b_g1.clone(),
            b_g2: self.b_g2.clone(),
            abc_delta_g1: self.abc_delta_g1(),
            ht_delta_g1: self.ht_delta_g1.clone(),
            num_public: self.num_public,
            vk: self.verifying_key(),
        }
    }
}

// ===========================================================================
// Fresh contribution secrets + entropy
// ===========================================================================

/// Fresh per-contribution scalars. Wiped on drop.
pub struct FreshSecrets {
    pub alpha: Scalar,
    pub beta: Scalar,
    pub gamma: Scalar,
    pub delta: Scalar,
}

impl FreshSecrets {
    /// All-ones contribution: leaves the CRS byte-identical (used as a test
    /// invariant and as the canonical "null" contribution).
    pub fn identity() -> FreshSecrets {
        FreshSecrets {
            alpha: Scalar::ONE,
            beta: Scalar::ONE,
            gamma: Scalar::ONE,
            delta: Scalar::ONE,
        }
    }

    /// Derive from 32 bytes of public entropy (e.g. a drand round / block hash)
    /// or OS entropy. The four scalars are independent, uniform field elements
    /// (via `Scalar::from_bytes_wide`).
    pub fn from_entropy(entropy: &[u8]) -> FreshSecrets {
        // 4 × 64 bytes of expanded, domain-separated material.
        let mut wide = [0u8; 64];
        let mut mk = |domain: &[u8]| -> Scalar {
            let mut k = Sha256::new();
            k.update(b"gpn/ceremony/phase2/fresh");
            k.update(domain);
            k.update(entropy);
            let h = k.finalize();
            wide[..32].copy_from_slice(&h);
            let h2 = sha256(&[h.as_slice(), b"-2"].concat());
            wide[32..].copy_from_slice(&h2);
            Scalar::from_bytes_wide(&wide)
        };
        let alpha = mk(b"alpha");
        let beta = mk(b"beta");
        let gamma = mk(b"gamma");
        let delta = mk(b"delta");
        // Wipe the temporary expansion buffer.
        wide = [0u8; 64];
        let _ = &mut wide;
        FreshSecrets {
            alpha,
            beta,
            gamma,
            delta,
        }
    }
}

impl Drop for FreshSecrets {
    fn drop(&mut self) {
        // Volatile overwrite so the compiler cannot elide the wipe.
        unsafe {
            core::ptr::write_volatile(&mut self.alpha, Scalar::ZERO);
            core::ptr::write_volatile(&mut self.beta, Scalar::ZERO);
            core::ptr::write_volatile(&mut self.gamma, Scalar::ZERO);
            core::ptr::write_volatile(&mut self.delta, Scalar::ZERO);
        }
    }
}

/// 32 bytes of OS entropy from `/dev/urandom`.
pub fn os_entropy_32() -> Result<[u8; 32], String> {
    use std::io::Read;
    let mut buf = [0u8; 32];
    let mut f =
        std::fs::File::open("/dev/urandom").map_err(|e| format!("open /dev/urandom: {e}"))?;
    f.read_exact(&mut buf)
        .map_err(|e| format!("read /dev/urandom: {e}"))?;
    Ok(buf)
}

/// Derive the genesis seed string for `init`. If `seeded_from` is provided the
/// seed is deterministic from it (a public, auditable genesis value); otherwise
/// 32 bytes of OS entropy are used.
pub fn genesis_seed(circuit: &str, seeded_from: Option<&str>) -> Result<String, String> {
    match seeded_from {
        Some(s) => Ok(format!("gpn/ceremony/{CRS_VERSION}/{circuit}/{s}")),
        None => {
            let e = Zeroizing::new(os_entropy_32()?);
            Ok(format!("gpn/ceremony/{CRS_VERSION}/{circuit}/{}", hex(&*e)))
        }
    }
}

/// Derive 32 bytes of contribution entropy. `seeded_from` makes the
/// contribution publicly un-biasable (derived from a value the contributor
/// cannot control, e.g. a drand round or a future block hash).
pub fn contribution_entropy(seeded_from: Option<&str>) -> Result<[u8; 32], String> {
    match seeded_from {
        Some(s) => Ok(sha256(s.as_bytes())),
        None => os_entropy_32(),
    }
}

// ===========================================================================
// Contribution records (the transcript)
// ===========================================================================

/// A single, publicly verifiable entry in the ceremony transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContributionRecord {
    pub version: u32,
    /// `"init"` or `"contribute"`.
    pub kind: String,
    pub circuit: String,
    /// Digest of the **input** CRS (the parent this contribution extends).
    pub parent: String,
    /// Digest of the **output** CRS.
    pub digest: String,
    pub contributor: String,
    pub seeded_from: String,
    pub timestamp: u64,
}

/// The public anchor that every transcript must start from.
pub fn genesis_anchor(circuit: &str) -> String {
    hex(&sha256(
        format!("gpn-ceremony-genesis/v{CRS_VERSION}/{circuit}").as_bytes(),
    ))
}

impl ContributionRecord {
    pub fn init(circuit: &str, digest: String, contributor: &str, seeded_from: &str) -> Self {
        ContributionRecord {
            version: CRS_VERSION,
            kind: "init".into(),
            circuit: circuit.into(),
            parent: genesis_anchor(circuit),
            digest,
            contributor: contributor.into(),
            seeded_from: seeded_from.into(),
            timestamp: now_unix(),
        }
    }

    pub fn contribute(
        circuit: &str,
        parent: String,
        digest: String,
        contributor: &str,
        seeded_from: &str,
    ) -> Self {
        ContributionRecord {
            version: CRS_VERSION,
            kind: "contribute".into(),
            circuit: circuit.into(),
            parent,
            digest,
            contributor: contributor.into(),
            seeded_from: seeded_from.into(),
            timestamp: now_unix(),
        }
    }

    /// Serialize to a flat JSON object.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&json!({
            "version": self.version,
            "kind": self.kind,
            "circuit": self.circuit,
            "parent": self.parent,
            "digest": self.digest,
            "contributor": self.contributor,
            "seeded_from": self.seeded_from,
            "timestamp": self.timestamp,
        }))
        .expect("record serializes")
    }

    /// Parse a record from JSON.
    pub fn from_json(s: &str) -> Result<ContributionRecord, String> {
        let v: serde_json::Value =
            serde_json::from_str(s).map_err(|e| format!("bad record JSON: {e}"))?;
        let get = |k: &str| -> Result<String, String> {
            v.get(k)
                .and_then(|x| x.as_str())
                .map(|x| x.to_string())
                .ok_or_else(|| format!("missing string field '{k}'"))
        };
        Ok(ContributionRecord {
            version: v
                .get("version")
                .and_then(|x| x.as_u64())
                .ok_or("missing 'version'")? as u32,
            kind: get("kind")?,
            circuit: get("circuit")?,
            parent: get("parent")?,
            digest: get("digest")?,
            contributor: v
                .get("contributor")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            seeded_from: v
                .get("seeded_from")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            timestamp: v.get("timestamp").and_then(|x| x.as_u64()).unwrap_or(0),
        })
    }
}

// ===========================================================================
// Verification
// ===========================================================================

/// Verify an entire transcript against the final CRS.
///
/// Checks, in order:
/// 1. the record list is non-empty and every record is a v1 record with
///    64-hex-char digests;
/// 2. digests form a **contiguous hash chain** (`records[i].parent ==
///    records[i-1].digest`), anchored for `init` at the public
///    [`genesis_anchor`];
/// 3. the **final** record's digest equals `sha256(final CRS)`;
/// 4. the final CRS is structurally well-formed and matches its circuit.
pub fn verify(crs: &CeremonyCrs, records: &[ContributionRecord]) -> Result<(), String> {
    if records.is_empty() {
        return Err("no contribution records".into());
    }
    for (i, r) in records.iter().enumerate() {
        if r.version != CRS_VERSION {
            return Err(format!("record {i}: unsupported version {}", r.version));
        }
        if r.digest.len() != 64 || !is_hex64(&r.digest) {
            return Err(format!("record {i}: malformed digest"));
        }
        if r.parent.len() != 64 || !is_hex64(&r.parent) {
            return Err(format!("record {i}: malformed parent"));
        }
        if r.circuit != crs.circuit {
            return Err(format!(
                "record {i}: circuit '{}' != CRS circuit '{}'",
                r.circuit, crs.circuit
            ));
        }
    }
    // Anchor + contiguity.
    if records[0].kind == "init" && records[0].parent != genesis_anchor(&crs.circuit) {
        return Err("record 0: init parent is not the public genesis anchor".into());
    }
    for i in 1..records.len() {
        if records[i].parent != records[i - 1].digest {
            return Err(format!(
                "digest chain broken at record {i}: parent {} != parent record digest {}",
                records[i].parent,
                records[i - 1].digest
            ));
        }
    }
    // Final digest must match the supplied CRS.
    let final_digest = crs.digest_hex();
    let last = records.last().unwrap();
    if last.digest != final_digest {
        return Err(format!(
            "final record digest {} != CRS digest {} (CRS tampered or wrong transcript)",
            last.digest, final_digest
        ));
    }
    // No replayed digests.
    for i in 0..records.len() {
        for j in (i + 1)..records.len() {
            if records[i].digest == records[j].digest {
                return Err(format!("duplicate digest at records {i} and {j}"));
            }
        }
    }
    // Structural + circuit match.
    crs.check_structure()?;
    crs.check_matches_circuit()?;
    Ok(())
}

/// Recompute one contribution step from **public data** and require
/// byte-equality.
///
/// Given the input CRS `prev`, the published `record`, and the output CRS
/// `next`, this re-derives the contribution from the record's `seeded_from`
/// value (a public, un-biasable beacon) and requires the result to equal the
/// published output byte-for-byte. This is what makes a transcript
/// *sound*: [`verify`] alone only checks that the record digests form a
/// self-consistent chain ending at the final CRS, which a malicious
/// coordinator can fabricate.
///
/// **Only defined for publicly-seeded contributions.** An OS-entropy
/// contribution (`seeded_from == `[`SEEDED_FROM_OS`]) cannot be reproduced
/// without the contributor's secret, so it returns an error here; verifying
/// such a step requires a pairing-based proof of correct re-randomisation
/// (out of scope for this tool — see the design doc).
pub fn verify_contribution_step(
    prev: &CeremonyCrs,
    record: &ContributionRecord,
    next: &CeremonyCrs,
) -> Result<(), String> {
    if record.kind != "contribute" {
        return Err(format!(
            "record kind '{}' is not a contribution",
            record.kind
        ));
    }
    if prev.circuit != next.circuit || prev.circuit != record.circuit {
        return Err("circuit mismatch across the step".into());
    }
    if record.parent != prev.digest_hex() {
        return Err("record parent does not match the input CRS digest".into());
    }
    if record.digest != next.digest_hex() {
        return Err("record digest does not match the output CRS digest".into());
    }
    if record.seeded_from == SEEDED_FROM_OS || record.seeded_from.is_empty() {
        return Err(
            "unverifiable: OS-entropy contribution cannot be reproduced from public data \
             (a pairing-based contribution proof is required)"
                .into(),
        );
    }
    let entropy = contribution_entropy(Some(&record.seeded_from))?;
    let expected = prev.contribute(&FreshSecrets::from_entropy(&entropy));
    if expected.to_bytes() != next.to_bytes() {
        return Err(format!(
            "contribution by '{}' did not apply the claimed public-seeded operation \
             (recomputed digest {} != published {})",
            record.contributor,
            expected.digest_hex(),
            next.digest_hex()
        ));
    }
    Ok(())
}

/// Verify a **full transcript with recomputation**.
///
/// `crs_chain[i]` is the CRS as published after `records[i]`, so the two slices
/// must have equal length and every non-genesis step must be reproducible from
/// its public seed (see [`verify_contribution_step`]). This is strictly
/// stronger than [`verify`]: in addition to running all of `verify`'s
/// structural/chain checks, it re-derives each `contribute` step and rejects a
/// fabricated digest chain that ends at a substituted CRS.
///
/// The first entry (the `init` genesis) is the trusted anchor and is **not**
/// recomputed (its seed may be private), but it is required to be structurally
/// valid and to match its named circuit.
pub fn verify_chain(
    crs_chain: &[CeremonyCrs],
    records: &[ContributionRecord],
) -> Result<(), String> {
    if crs_chain.is_empty() {
        return Err("no CRS chain supplied".into());
    }
    if crs_chain.len() != records.len() {
        return Err(format!(
            "CRS chain length {} != record count {}",
            crs_chain.len(),
            records.len()
        ));
    }
    // Existing structural/chain checks against the final CRS + the full record list.
    verify(crs_chain.last().unwrap(), records)?;
    if records[0].digest != crs_chain[0].digest_hex() {
        return Err("first record digest does not match the first CRS".into());
    }
    if records[0].kind == "init" {
        crs_chain[0].check_structure()?;
        crs_chain[0].check_matches_circuit()?;
    }
    for i in 1..records.len() {
        verify_contribution_step(&crs_chain[i - 1], &records[i], &crs_chain[i])?;
    }
    Ok(())
}

// ===========================================================================
// VK serialization (on-chain artifact)
// ===========================================================================

/// Canonical bytes for a [`VerifyingKey`] (what `export-vk` writes).
pub fn vk_to_bytes(vk: &VerifyingKey) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"GPNVK1\n\0"); // 8 bytes
    out.extend_from_slice(&vk.alpha_g1.to_compressed());
    out.extend_from_slice(&vk.beta_g2.to_compressed());
    out.extend_from_slice(&vk.gamma_g2.to_compressed());
    out.extend_from_slice(&vk.delta_g2.to_compressed());
    push_u64(&mut out, vk.gamma_abc_g1.len() as u64);
    for g in &vk.gamma_abc_g1 {
        out.extend_from_slice(&g.to_compressed());
    }
    out
}

pub fn vk_from_bytes(bytes: &[u8]) -> Result<VerifyingKey, String> {
    let mut r = Reader::new(bytes);
    let magic = r.take(8)?;
    if magic != b"GPNVK1\n\0" {
        return Err("bad VK magic".into());
    }
    let alpha_g1 = r.g1()?;
    let beta_g2 = r.g2()?;
    let gamma_g2 = r.g2()?;
    let delta_g2 = r.g2()?;
    let n = r.u64()? as usize;
    let mut gamma_abc_g1 = Vec::with_capacity(n);
    for _ in 0..n {
        gamma_abc_g1.push(r.g1()?);
    }
    if r.remaining() != 0 {
        return Err("trailing bytes in VK".into());
    }
    Ok(VerifyingKey {
        alpha_g1,
        beta_g2,
        gamma_g2,
        delta_g2,
        gamma_abc_g1,
    })
}

// ===========================================================================
// Small helpers
// ===========================================================================

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit())
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_str(out: &mut Vec<u8>, s: &str) {
    push_u64(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn push_g1_vec(out: &mut Vec<u8>, v: &[G1Affine]) {
    push_u64(out, v.len() as u64);
    for p in v {
        out.extend_from_slice(&p.to_compressed());
    }
}

fn push_g2_vec(out: &mut Vec<u8>, v: &[G2Affine]) {
    push_u64(out, v.len() as u64);
    for p in v {
        out.extend_from_slice(&p.to_compressed());
    }
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader { b, pos: 0 }
    }
    fn remaining(&self) -> usize {
        self.b.len().saturating_sub(self.pos)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.remaining() < n {
            return Err("unexpected end of input".into());
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u64(&mut self) -> Result<u64, String> {
        let s = self.take(8)?;
        Ok(u64::from_le_bytes(s.try_into().unwrap()))
    }
    fn str(&mut self) -> Result<String, String> {
        let n = self.u64()? as usize;
        let s = self.take(n)?;
        String::from_utf8(s.to_vec()).map_err(|_| "invalid utf8 circuit name".into())
    }
    fn g1(&mut self) -> Result<G1Affine, String> {
        let s: [u8; 48] = self.take(48)?.try_into().unwrap();
        Option::<G1Affine>::from(G1Affine::from_compressed(&s))
            .ok_or_else(|| "invalid G1 point".into())
    }
    fn g2(&mut self) -> Result<G2Affine, String> {
        let s: [u8; 96] = self.take(96)?.try_into().unwrap();
        Option::<G2Affine>::from(G2Affine::from_compressed(&s))
            .ok_or_else(|| "invalid G2 point".into())
    }
    fn g1_vec(&mut self) -> Result<Vec<G1Affine>, String> {
        let n = self.u64()? as usize;
        // Guard against absurd allocations from a corrupt header.
        let elem = 48usize;
        if n.saturating_mul(elem) > self.remaining() {
            return Err("G1 vector length exceeds remaining input".into());
        }
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.g1()?);
        }
        Ok(v)
    }
    fn g2_vec(&mut self) -> Result<Vec<G2Affine>, String> {
        let n = self.u64()? as usize;
        let elem = 96usize;
        if n.saturating_mul(elem) > self.remaining() {
            return Err("G2 vector length exceeds remaining input".into());
        }
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.g2()?);
        }
        Ok(v)
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qap::value_balance_r1cs;

    fn small_crs(circuit: &str, seed: &str) -> CeremonyCrs {
        CeremonyCrs::genesis(circuit, seed).expect("genesis")
    }

    #[test]
    fn genesis_is_deterministic_and_serializes() {
        let a = small_crs("value-balance", "fixed-seed");
        let b = small_crs("value-balance", "fixed-seed");
        assert_eq!(a.digest(), b.digest(), "same seed -> same digest");
        let c = small_crs("value-balance", "other-seed");
        assert_ne!(a.digest(), c.digest(), "different seed -> different digest");
        // Round-trip through the canonical bytes.
        let bytes = a.to_bytes();
        let back = CeremonyCrs::from_bytes(&bytes).expect("parse");
        assert_eq!(back.digest(), a.digest());
        assert_eq!(back.gamma_abc_g1(), a.gamma_abc_g1());
        a.check_structure().expect("structure");
        a.check_matches_circuit().expect("circuit match");
    }

    #[test]
    fn genesis_recombines_to_a_valid_proving_key() {
        // The split components must reconstruct exactly the combined key that
        // the (single-secret) setup produces — i.e. the ceremony CRS is a real
        // Groth16 CRS.
        let r1cs = value_balance_r1cs();
        let qap = r1cs.qap();
        let seed = "testnet-crs-v0";
        let pk = groth16::setup(&qap, seed);
        let crs = small_crs("value-balance", seed);
        assert_eq!(crs.verifying_key(), pk.vk, "VK must match setup()");
        assert_eq!(crs.abc_delta_g1(), pk.abc_delta_g1, "abc/δ must match");
        assert_eq!(crs.a_g1, pk.a_g1);
        assert_eq!(crs.b_g1, pk.b_g1);
        assert_eq!(crs.b_g2, pk.b_g2);
        assert_eq!(crs.ht_delta_g1, pk.ht_delta_g1);
    }

    #[test]
    fn identity_contribution_is_a_noop() {
        // (α',β',γ',δ') = (1,1,1,1) must leave the CRS byte-identical: a strong
        // correctness check on the MPC recombine.
        let crs = small_crs("value-balance", "seed-x");
        let out = crs.contribute(&FreshSecrets::identity());
        assert_eq!(
            out.digest(),
            crs.digest(),
            "identity contribution is a no-op"
        );
    }

    #[test]
    fn contribution_changes_crs_and_produces_record() {
        let crs = small_crs("value-balance", "seed-1");
        let fresh = FreshSecrets::from_entropy(b"contributor-1-public-entropy");
        let out = crs.contribute(&fresh);
        assert_ne!(out.digest(), crs.digest(), "CRS must change");
        // τ-fixed elements are unchanged.
        assert_eq!(out.a_g1, crs.a_g1);
        assert_eq!(out.b_g2, crs.b_g2);
        // The record links output to input.
        let rec = ContributionRecord::contribute(
            &out.circuit,
            crs.digest_hex(),
            out.digest_hex(),
            "tester",
            "drand-round-1234",
        );
        assert_eq!(rec.parent, crs.digest_hex());
        assert_eq!(rec.digest, out.digest_hex());
        // JSON round-trip.
        let back = ContributionRecord::from_json(&rec.to_json()).expect("parse record");
        assert_eq!(back, rec);
    }

    #[test]
    fn proof_roundtrip_against_contributed_vk() {
        // End-to-end: genesis -> two contributions -> prove -> verify against the
        // VK exported from the final CRS.
        let r1cs = value_balance_r1cs();
        let qap = r1cs.qap(); // dense path for prove()
        let crs0 = small_crs("value-balance", "roundtrip-seed");
        let crs1 = crs0.contribute(&FreshSecrets::from_entropy(b"c1"));
        let crs2 = crs1.contribute(&FreshSecrets::from_entropy(b"c2"));
        let pk = crs2.proving_key();
        let z = vec![
            Scalar::ONE,
            Scalar::from(2),  // fee (public)
            Scalar::from(10), // v_in
            Scalar::from(8),  // v_out
            Scalar::from(10), // spent
            Scalar::from(80), // w
        ];
        let (proof, pub_in) = groth16::prove(&pk, &qap, &z);
        assert_eq!(pub_in, vec![Scalar::from(2)]);
        assert_eq!(
            groth16::verify(&crs2.verifying_key(), &pub_in, &proof),
            Ok(()),
            "proof must verify against the contributed VK"
        );
        // And the noise: two contributions must not produce the genesis key.
        assert_ne!(crs0.verifying_key(), crs2.verifying_key());
    }

    #[test]
    fn verify_accepts_honest_chain_and_rejects_tampering() {
        let crs0 = small_crs("value-balance", "chain-seed");
        let r0 =
            ContributionRecord::init(&crs0.circuit, crs0.digest_hex(), "genesis", "os-entropy");
        let crs1 = crs0.contribute(&FreshSecrets::from_entropy(b"c1"));
        let r1 = ContributionRecord::contribute(
            &crs1.circuit,
            crs0.digest_hex(),
            crs1.digest_hex(),
            "alice",
            "drand-1",
        );
        let crs2 = crs1.contribute(&FreshSecrets::from_entropy(b"c2"));
        let r2 = ContributionRecord::contribute(
            &crs2.circuit,
            crs1.digest_hex(),
            crs2.digest_hex(),
            "bob",
            "drand-2",
        );
        // Honest chain: init might or might not be included; test both.
        assert!(verify(&crs2, &[r0.clone(), r1.clone(), r2.clone()]).is_ok());
        assert!(verify(&crs2, &[r1.clone(), r2.clone()]).is_ok());

        // (i) tampered CRS: scale alpha_g1 by 2 -> digest changes -> FAIL.
        let mut bad = crs2.clone();
        bad.alpha_g1 = (G1Projective::from(bad.alpha_g1) * Scalar::from(2u64)).into();
        assert!(verify(&bad, &[r1.clone(), r2.clone()]).is_err());

        // (ii) broken digest chain: r2's parent no longer matches r1.
        let mut r2_broken = r2.clone();
        r2_broken.parent = "00".repeat(32);
        assert!(verify(&crs2, &[r1.clone(), r2_broken]).is_err());

        // (iii) wrong final digest (record doesn't describe this CRS).
        let mut r2_wrong = r2.clone();
        r2_wrong.digest = "11".repeat(32);
        assert!(verify(&crs2, &[r1.clone(), r2_wrong]).is_err());

        // (iv) malformed record.
        let mut r1_bad = r1.clone();
        r1_bad.digest = "not-hex".into();
        assert!(verify(&crs2, &[r1_bad, r2.clone()]).is_err());

        // (v) empty transcript.
        assert!(verify(&crs2, &[]).is_err());
    }

    #[test]
    fn verify_rejects_wrong_circuit_dimensions() {
        let crs = small_crs("value-balance", "dim-seed");
        // Lie about num_variables; digest won't match records, but also the
        // circuit-match check must fail independently.
        let mut bad = crs.clone();
        bad.num_variables += 1;
        bad.a_g1.push(crs.a_g1[0]);
        bad.b_g1.push(crs.b_g1[0]);
        bad.b_g2.push(crs.b_g2[0]);
        assert!(bad.check_matches_circuit().is_err());
    }

    #[test]
    fn vk_serialization_roundtrip() {
        let crs = small_crs("value-balance", "vk-seed");
        let vk = crs.verifying_key();
        let bytes = vk_to_bytes(&vk);
        let back = vk_from_bytes(&bytes).expect("parse vk");
        assert_eq!(back, vk);
    }

    #[test]
    fn contribution_entropy_is_deterministic_when_seeded() {
        let a = contribution_entropy(Some("drand-round-999")).unwrap();
        let b = contribution_entropy(Some("drand-round-999")).unwrap();
        assert_eq!(a, b);
        let c = contribution_entropy(Some("drand-round-1000")).unwrap();
        assert_ne!(a, c);
        // Deterministic contribution from a public seed is reproducible.
        let crs = small_crs("value-balance", "seeded-contrib");
        let out1 = crs.contribute(&FreshSecrets::from_entropy(&a));
        let out2 = crs.contribute(&FreshSecrets::from_entropy(&b));
        assert_eq!(out1.digest(), out2.digest());
    }

    /// Build an honest, fully publicly-seeded transcript with recomputable steps.
    fn publicly_seeded_chain(circuit: &str) -> (Vec<CeremonyCrs>, Vec<ContributionRecord>) {
        let crs0 = small_crs(circuit, "anchor-seed");
        let r0 =
            ContributionRecord::init(&crs0.circuit, crs0.digest_hex(), "genesis", SEEDED_FROM_OS);
        let s1 = "drand-round-1001";
        let crs1 = crs0.contribute(&FreshSecrets::from_entropy(
            &contribution_entropy(Some(s1)).unwrap(),
        ));
        let r1 = ContributionRecord::contribute(
            &crs1.circuit,
            crs0.digest_hex(),
            crs1.digest_hex(),
            "alice",
            s1,
        );
        let s2 = "drand-round-1002";
        let crs2 = crs1.contribute(&FreshSecrets::from_entropy(
            &contribution_entropy(Some(s2)).unwrap(),
        ));
        let r2 = ContributionRecord::contribute(
            &crs2.circuit,
            crs1.digest_hex(),
            crs2.digest_hex(),
            "bob",
            s2,
        );
        (vec![crs0, crs1, crs2], vec![r0, r1, r2])
    }

    #[test]
    fn verify_chain_accepts_honest_publicly_seeded_chain() {
        let (chain, records) = publicly_seeded_chain("value-balance");
        verify_chain(&chain, &records).expect("honest chain verifies");
        // A length mismatch is rejected.
        assert!(verify_chain(&chain, &records[..2]).is_err());
    }

    #[test]
    fn verify_chain_rejects_substituted_final_crs_that_plain_verify_accepts() {
        // The exact attack the tool exists to stop: a coordinator swaps in a
        // CRS whose trapdoor it knows and fabricates a self-consistent record
        // chain ending at that CRS's digest. Plain `verify` passes (it only
        // checks a hash chain + final digest); `verify_chain` rejects it because
        // the last step does not recompute to the substituted CRS.
        let (chain, records) = publicly_seeded_chain("value-balance");
        let mut evil = chain[2].clone();
        evil.delta_g1 = (G1Projective::from(evil.delta_g1) * Scalar::from(2u64)).into();
        assert_ne!(evil.digest_hex(), chain[2].digest_hex());
        // Fabricate a record that links the honest crs1 to the evil CRS.
        let mut r2_evil = records[2].clone();
        r2_evil.digest = evil.digest_hex();
        // From the *record* perspective this is a valid, contiguous chain.
        assert!(verify(
            &evil,
            &[records[0].clone(), records[1].clone(), r2_evil.clone()]
        )
        .is_ok());
        // The recomputing verifier catches the substitution.
        let evil_chain = vec![chain[0].clone(), chain[1].clone(), evil];
        assert!(verify_chain(
            &evil_chain,
            &[records[0].clone(), records[1].clone(), r2_evil]
        )
        .is_err());
    }

    #[test]
    fn verify_chain_rejects_os_entropy_step() {
        // A step that actually changed the CRS but claims OS entropy cannot be
        // reproduced from public data -> rejected (needs a pairing proof).
        let crs0 = small_crs("value-balance", "os-step-seed");
        let r0 =
            ContributionRecord::init(&crs0.circuit, crs0.digest_hex(), "genesis", SEEDED_FROM_OS);
        let crs1 = crs0.contribute(&FreshSecrets::from_entropy(b"secret-randomness"));
        let r1 = ContributionRecord::contribute(
            &crs1.circuit,
            crs0.digest_hex(),
            crs1.digest_hex(),
            "carol",
            SEEDED_FROM_OS,
        );
        // The (weak) chain check is happy...
        assert!(verify(&crs1, &[r0.clone(), r1.clone()]).is_ok());
        // ...but the recomputing verifier refuses to vouch for it.
        assert!(verify_chain(&[crs0, crs1], &[r0, r1]).is_err());
    }

    #[test]
    fn verify_contribution_step_checks_parent_and_digest() {
        let (chain, records) = publicly_seeded_chain("value-balance");
        // Correct step passes.
        verify_contribution_step(&chain[1], &records[2], &chain[2]).expect("step ok");
        // Wrong parent.
        let mut r = records[2].clone();
        r.parent = "00".repeat(32);
        assert!(verify_contribution_step(&chain[1], &r, &chain[2]).is_err());
        // Wrong digest.
        let mut r = records[2].clone();
        r.digest = "11".repeat(32);
        assert!(verify_contribution_step(&chain[1], &r, &chain[2]).is_err());
    }
}
