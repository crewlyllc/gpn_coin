//! Spec 43 — GPU identity + one-GPU-one-provider enforcement.
//!
//! **No economics change.** `MinAttestationStake` and `deposit_stake` are
//! untouched (identity + reputation, not capital, are the gate); staking
//! remains an optional reward booster exactly as before. This suite pins that.
//!
//! It also exercises the storage migration: existing attestations keep their
//! (legacy, model+VRAM) fingerprint and are *grandfathered* into `GpuOwner`
//! — never retro-rejected — while the second run is a no-op.

use frame_support::traits::{OnRuntimeUpgrade, StorageVersion};
use gpn_runtime::{
    migrations::GrandfatherGpuOwner, AccountId, GPUAttestation, RequireGpuIdentity, Runtime,
    System, VERSION,
};
use pallet_gpu_attestation::{AttestationRecord, AttestationTier};
use sp_runtime::BuildStorage;

fn acct(raw: u8) -> AccountId {
    AccountId::new([raw; 32])
}

fn new_test_ext() -> sp_io::TestExternalities {
    let storage = frame_system::GenesisConfig::<Runtime>::default()
        .build_storage()
        .expect("system genesis");
    let mut ext: sp_io::TestExternalities = storage.into();
    ext.execute_with(|| System::set_block_number(1));
    ext
}

fn record(hash: [u8; 32]) -> AttestationRecord {
    AttestationRecord {
        submitted_at: 1,
        gpu_info_hash: hash,
        challenge_response: [1u8; 32],
        gpu_count: 1,
        compute_gflops: 1_000,
        tier: AttestationTier::Bronze,
        missed_count: 0,
    }
}

/// A plain +1 for the new storage item.
#[test]
fn spec_version_matches_live_chain() {
    assert_eq!(VERSION.spec_version, 46);
}

/// `testnet-no-stake` actually flips the runtime constant to zero; the default
/// build keeps the owner-decided placeholder (never silently zero).
#[cfg(feature = "testnet-no-stake")]
#[test]
fn testnet_build_has_zero_minimum_stake() {
    assert_eq!(
        <Runtime as pallet_gpu_attestation::Config>::MinAttestationStake::get(),
        0,
        "testnet-no-stake must pin the minimum to 0"
    );
}

#[cfg(not(feature = "testnet-no-stake"))]
#[test]
fn default_build_keeps_a_non_zero_placeholder() {
    assert_eq!(
        <Runtime as pallet_gpu_attestation::Config>::MinAttestationStake::get(),
        1_000_000_000_000_000,
        "the mainnet value is owner-decided; the default build keeps the placeholder"
    );
}

/// Mainnet requires hardware identity; the lenient testnet feature does not.
#[cfg(not(feature = "testnet-lenient-identity"))]
#[test]
fn mainnet_requires_gpu_identity() {
    assert!(RequireGpuIdentity::get());
}

#[cfg(feature = "testnet-lenient-identity")]
#[test]
fn testnet_build_may_relax_gpu_identity() {
    assert!(!RequireGpuIdentity::get());
}

#[test]
fn migration_grandfathers_existing_attestations() {
    new_test_ext().execute_with(|| {
        let alice = acct(1);
        let bob = acct(2);
        let zero = acct(3);
        // Legacy records: model+VRAM digests, one of them a shared collision.
        pallet_gpu_attestation::Attestations::<Runtime>::insert(alice.clone(), record([0xAA; 32]));
        pallet_gpu_attestation::Attestations::<Runtime>::insert(bob.clone(), record([0xAA; 32]));
        pallet_gpu_attestation::Attestations::<Runtime>::insert(zero.clone(), record([0u8; 32]));
        assert_eq!(
            StorageVersion::get::<GPUAttestation>(),
            StorageVersion::new(0)
        );

        GrandfatherGpuOwner::<Runtime>::on_runtime_upgrade();

        // Records are untouched (grandfathered, not rewritten).
        assert_eq!(
            pallet_gpu_attestation::Attestations::<Runtime>::get(&alice)
                .unwrap()
                .gpu_info_hash,
            [0xAA; 32]
        );
        assert!(pallet_gpu_attestation::Attestations::<Runtime>::get(&zero).is_some());

        // First writer wins the shared legacy digest; the zero key is never owned.
        let owner = pallet_gpu_attestation::GpuOwner::<Runtime>::get([0xAA; 32]);
        assert!(owner == Some(alice) || owner == Some(bob));
        assert_eq!(
            pallet_gpu_attestation::GpuOwner::<Runtime>::get([0u8; 32]),
            None
        );

        // Stamped, so a re-run cannot churn storage.
        assert_eq!(
            StorageVersion::get::<GPUAttestation>(),
            StorageVersion::new(1)
        );
        GrandfatherGpuOwner::<Runtime>::on_runtime_upgrade();
        assert_eq!(
            pallet_gpu_attestation::GpuOwner::<Runtime>::get([0xAA; 32]),
            owner
        );
    });
}
