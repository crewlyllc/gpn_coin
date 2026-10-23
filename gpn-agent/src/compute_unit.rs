//! **The one canonical unit for `compute_gflops`** (queue item #97).
//!
//! ## What the number is
//!
//! `gpu::measure_gflops` runs GPN's own dense float32 matrix multiply
//! (`gpu::matmul`, OpenCL kernel `gpn_sgemm`, 16x16 shared-memory tile,
//! `-cl-mad-enable -cl-fast-relaxed-math`) on a square **N = 1024** matrix and
//! divides `2·N³` FLOPs by the wall time of the single fastest run out of N
//! (after one warm-up). That number — real FP32 FLOPs per second — is what
//! `GPUAttestation::compute_gflops` stores and what `min_gflops` compares
//! against.
//!
//! It is therefore *literally* FP32 GFLOP/s. It is **not** the card's spec-sheet
//! peak: the shipped tile is shared-memory bound and reaches only ~5–7 % of the
//! FP32 peak on a discrete card (an RTX 4060 at 15.1 TFLOPS peak measures
//! 823–912). That is fine for its job — separating device classes reproducibly —
//! but it means a floor written on the *peak* scale (3000 "GFLOPS" = 3 TFLOPS of
//! spec sheet) rejects every consumer card while admitting none of them.
//!
//! ## Reproducing a measurement
//!
//! ```text
//! unit       = GPN-GEMM-F32-1024          (this module's UNIT_NAME)
//! dtype      = f32 (IEEE binary32, OpenCL `float`)
//! matrix     = N x N, N = 1024, C = A*B
//! flops/run  = 2 * N^3 = 2 147 483 648
//! warm-up    = 1 run (JIT + clock ramp)
//! timed runs = 3 (attestation; `gpn_gpu_compute` uses 8 and reports the mean)
//! value      = max(timed runs), rounded to whole GFLOP/s
//! conditions = GPU idle: cross-process GPU lock held, miner yielded, util <=
//!              10 %, VRAM < 50 % (see `attest_idle`)
//! ```
//!
//! Expected repeatability is ±12 % **between clock states** on the same card
//! (observed 823 … 912 on one RTX 4060, i.e. low/high power state), and within
//! ~1 % across repeats in the same state. Treat a value as a capability band,
//! not a certificate.
//!
//! ## Why the old values were wrong
//!
//! Three separate defects, all fixed before this module existed:
//! * pre-2026-10 the on-chain field was written from raw FLOP/s (or from a
//!   spec-sheet table), leaving `30000` on a 15.1 TFLOPS card — 2 085 % above
//!   what the card can actually measure and above its hardware peak;
//! * box1 attested `188` from a *contended* sample (the operator's own AI
//!   workload held the card), i.e. iGPU-class for a 44 TFLOPS card;
//! * `attest_measurement_useful` only ever accepted an upward move, so neither
//!   number could be replaced.
//!
//! ## The floor
//!
//! [`MIN_ELIGIBLE_GFLOPS`] is expressed in **this** unit, not in spec-sheet
//! TFLOPS. It is 30 % of the lowest idle measurement of the reference card
//! (RTX 4060 8 GB, [`REFERENCE_MEASURED_GFLOPS`]), which is also the weakest
//! card GPN currently supports for a 7B-class job (8 GB VRAM). Every discrete
//! 8 GB card measured or estimated clears it; every integrated GPU stays well
//! under it (~50–110 at this kernel size).
//!
//! The **runtime** constant `MinEligibilityGflops` must equal this value for a
//! fresh attestation to be accepted (see
//! `shared/artifacts/project7/staged/gflops-floor-v1.md`: changing it is a
//! governed runtime change, not a config knob).

/// Name of the canonical unit. Carried in logs and `/status` so a number can
/// always be traced to the method that produced it.
pub const UNIT_NAME: &str = "GPN-GEMM-F32-1024";

/// Matrix edge length used by `gpu::measure_gflops` / `measure_gflops_samples_locked`.
pub const BENCH_N: usize = 1024;
/// FLOPs in one benchmark run: `2 * N^3`.
pub const BENCH_FLOPS_PER_RUN: u64 = 2 * (BENCH_N as u64) * (BENCH_N as u64) * (BENCH_N as u64);
/// Timed runs per attestation measurement (`GPN_ATTEST_SAMPLES` default).
pub const BENCH_TIMED_RUNS: usize = 3;

/// The card the floor is derived from — the weakest card GPN supports today.
pub const REFERENCE_CARD: &str = "NVIDIA GeForce RTX 4060 8 GB";
/// Published FP32 peak of that card (TFLOPS x 1000), for the plausibility check.
pub const REFERENCE_PEAK_GFLOPS: u64 = 15_100;
/// **Lowest** idle measurement observed on the reference card, whole GFLOP/s.
/// Observed range 2026-10-23 on an idle card: 822, 825, 825 (attestation,
/// best-of-3) and 827, 911, 911, 912 (benchmark, best-of-5) — the spread is the
/// power-state band. The low end is deliberately used for the floor so a cold
/// card is not excluded.
pub const REFERENCE_MEASURED_GFLOPS: u64 = 823;
/// How much of the reference card's measurement a card must reach to be
/// eligible: ~30 % (250 of 823). High enough to exclude every integrated GPU,
/// low enough that any discrete 8 GB card clears it.
pub const MIN_ELIGIBLE_PCT_OF_REFERENCE: u64 = 30;

/// **The eligibility floor in the canonical unit** — 250 GFLOPS.
///
/// Keep this in lock-step with the runtime constant
/// `MinEligibilityGflops` (`gpn-node/runtime/src/lib.rs`). While they differ,
/// the provider refuses to submit a measurement the chain would reject.
pub const MIN_ELIGIBLE_GFLOPS: u64 = 250;

/// A record more than 1.5x the card's estimated FP32 peak cannot be a
/// measurement of that card: it is a legacy (pre-units-fix) or fabricated
/// value, and must be correctable downward.
pub const IMPLAUSIBLE_RECORD_FACTOR_NUM: u64 = 3;
pub const IMPLAUSIBLE_RECORD_FACTOR_DEN: u64 = 2;

/// Measured GFLOP/s as a fraction of the card's spec-sheet FP32 peak. Used only
/// to translate between the two scales in docs and pricing; never to invent a
/// measurement.
pub const MEASURED_FRACTION_OF_PEAK: f64 = 0.055;

/// Human-readable one-liner, used in startup logs (and as the deploy marker).
pub fn describe() -> String {
    format!(
        "compute unit {UNIT_NAME}: dense FP32 GEMM, N={BENCH_N}, {} GFLOP/run, best of {} idle runs",
        BENCH_FLOPS_PER_RUN as f64 / 1e9,
        BENCH_TIMED_RUNS
    )
}

/// The derivation of the floor, in one line, for logs and docs.
pub fn floor_derivation() -> String {
    format!(
        "{MIN_ELIGIBLE_GFLOPS} GFLOPS ≈ {MIN_ELIGIBLE_PCT_OF_REFERENCE}% of {REFERENCE_MEASURED_GFLOPS} \
         GFLOPS (lowest idle {UNIT_NAME} measurement on the {REFERENCE_CARD}, the weakest supported card)"
    )
}

/// Is `record` physically possible for a card whose estimated FP32 peak is
/// `peak_gflops`? `peak_gflops == 0` means "unknown device": nothing can be
/// called implausible without evidence, so the answer is `true`.
///
/// The peak estimate is only trusted when it is at least
/// [`MIN_TRUSTWORTHY_PEAK_GFLOPS`]; a smaller one means the model-name table did
/// not recognise the card (or recognised an integrated GPU), and a wrong table
/// entry must not be allowed to downgrade a valid record.
pub fn record_plausible(record: u64, peak_gflops: u64) -> bool {
    if peak_gflops < MIN_TRUSTWORTHY_PEAK_GFLOPS || record == 0 {
        return true;
    }
    record
        <= peak_gflops.saturating_mul(IMPLAUSIBLE_RECORD_FACTOR_NUM) / IMPLAUSIBLE_RECORD_FACTOR_DEN
}

/// Below this estimated FP32 peak (2 TFLOPS) the model-name table is not
/// trustworthy enough to declare a record impossible.
pub const MIN_TRUSTWORTHY_PEAK_GFLOPS: u64 = 2_000;

/// A record that is implausible for the card: a legacy/unit-bug artefact.
pub fn record_implausible(record: u64, peak_gflops: u64) -> bool {
    !record_plausible(record, peak_gflops)
}

/// Convert a measured value to the spec-sheet TFLOPS scale it corresponds to
/// (~1/0.055). **Estimate only** — for documentation and for deciding whether a
/// hand-typed `--gflops` bound is in the right ballpark, never for attestation.
pub fn peak_equivalent_gflops(measured: u64) -> f64 {
    measured as f64 / MEASURED_FRACTION_OF_PEAK
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The unit is pinned: if someone changes the benchmark geometry, this
    /// test must change with it and every stored value becomes a new unit.
    #[test]
    fn unit_is_pinned_to_the_benchmark_geometry() {
        assert_eq!(UNIT_NAME, "GPN-GEMM-F32-1024");
        assert_eq!(BENCH_N, 1024);
        assert_eq!(BENCH_FLOPS_PER_RUN, 2_147_483_648);
        assert_eq!(BENCH_FLOPS_PER_RUN, 2 * 1024u64.pow(3));
        assert!(describe().contains(UNIT_NAME));
    }

    /// The floor is derived from the reference measurement, not invented.
    #[test]
    fn floor_is_thirty_percent_of_the_reference_measurement() {
        assert_eq!(MIN_ELIGIBLE_GFLOPS, 250);
        let pct = MIN_ELIGIBLE_GFLOPS as f64 / REFERENCE_MEASURED_GFLOPS as f64;
        assert!(
            (0.25..=0.35).contains(&pct),
            "floor must stay 25-35% of the reference card, got {pct}"
        );
        // The reference card itself always clears its own derived floor.
        assert!(REFERENCE_MEASURED_GFLOPS > MIN_ELIGIBLE_GFLOPS);
        assert!(floor_derivation().contains("250 GFLOPS"));
    }

    /// A floor of 250 sits ~3x above the strongest integrated GPU at this
    /// kernel size (~80-110) and ~3x below a discrete 8 GB card (823+).
    #[test]
    fn floor_separates_integrated_from_discrete() {
        let igpu_upper = 110;
        assert!(MIN_ELIGIBLE_GFLOPS > igpu_upper);
        assert!(REFERENCE_MEASURED_GFLOPS >= 3 * MIN_ELIGIBLE_GFLOPS);
    }

    /// A legacy 30000 record on a 15.1 TFLOPS card is not a measurement of it.
    #[test]
    fn a_record_above_peak_is_implausible() {
        assert!(!record_plausible(30_000, REFERENCE_PEAK_GFLOPS));
        assert!(record_implausible(30_000, REFERENCE_PEAK_GFLOPS));
        assert!(record_plausible(
            REFERENCE_MEASURED_GFLOPS,
            REFERENCE_PEAK_GFLOPS
        ));
        // Unknown device: no evidence, so never "implausible".
        assert!(record_plausible(30_000, 0));
        assert!(!record_implausible(30_000, 0));
        // An untrustworthy (too small / mis-detected) peak cannot downgrade.
        assert!(record_plausible(30_000, 1_500));
    }

    #[test]
    fn peak_equivalent_is_an_estimate_with_the_documented_band() {
        // 823 measured ~ 15 TFLOPS peak card (5.5% efficiency).
        let p = peak_equivalent_gflops(REFERENCE_MEASURED_GFLOPS);
        assert!((12_000.0..=18_000.0).contains(&p), "{p}");
    }
}
