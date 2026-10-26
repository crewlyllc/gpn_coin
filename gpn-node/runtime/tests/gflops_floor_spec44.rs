//! Spec 44 (queue #97) — the eligibility floor is pinned in the **measured** unit.
//!
//! `GPUAttestation::compute_gflops` stores the measured `GPN-GEMM-F32-1024`
//! throughput (dense FP32 GEMM, N = 1024, best of 3 idle runs), which is ~5.5%
//! of a card's spec-sheet peak. The old floor of 3 000 was written on the peak
//! scale, so every honest attestation was rejected (`BelowEligibilityFloor`) and
//! the chain could only ever carry fiction.
//!
//! Derivation and migration: `shared/artifacts/project7/staged/gflops-floor-v1.md`.

/// 250 = 30% of the lowest idle measurement of the weakest supported card
/// (RTX 4060 8 GB: 823 measured). Any discrete 8 GB card clears it; no iGPU does.
#[test]
fn min_eligibility_gflops_is_250_measured() {
    assert_eq!(gpn_runtime::MinEligibilityGflops::get(), 250);
}

/// The floor must sit below the reference card's measurement and above the
/// strongest integrated GPU (≈50–110 at this kernel size), or it either blocks
/// honest hardware or lets an iGPU in.
#[test]
fn floor_sits_between_igpu_and_discrete_card() {
    let floor = gpn_runtime::MinEligibilityGflops::get();
    assert!(floor > 110, "floor {floor} would admit an integrated GPU");
    assert!(
        floor < 823,
        "floor {floor} exceeds the reference 4060 measurement"
    );
}
