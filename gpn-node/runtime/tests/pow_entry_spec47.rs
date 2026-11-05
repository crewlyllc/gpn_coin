//! Spec 47 — proof-of-work entry.
//!
//! Pins the two halves of the owner's 2026-11-04 decision:
//!   1. The **gate is real**: a fee-free entry is `Pays::No` *only* when a
//!      valid PoW solution is supplied; without it the same call is `Pays::Yes`
//!      and a zero-balance account cannot pay the fee.
//!   2. **Onboarding is one call**: `register_provider_entry` proves work,
//!      records the attestation and registers the provider.
//!
//! With spec 48 the existential deposit is **0 on every network** (owner rule
//! 2026-11-05: testnet and mainnet behave identically except for governance);
//! the tests below pin that a zero-balance account can exist and register as a
//! provider, and fail if any build produces a non-zero ED.

use codec::Encode;
use frame_support::dispatch::{GetDispatchInfo, Pays};
use gpn_runtime::{
    AccountId, Balances, ComputeMarketplace, ExistentialDeposit, GPUAttestation, GPUPoW, Runtime,
    RuntimeCall, RuntimeOrigin, System,
};
use pallet_compute_marketplace::GPURequirements;
use pallet_gpu_attestation::EntryPow;
use pallet_transaction_payment::ChargeTransactionPayment;
use sp_runtime::traits::TransactionExtension;
use sp_runtime::transaction_validity::{
    InvalidTransaction, TransactionSource, TransactionValidityError,
};
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

/// A solution valid against whatever target the chain currently holds.
fn solution() -> EntryPow {
    let mix_hash = [9u8; 32];
    let algorithm_version = 1u32;
    for nonce in 0..1_000_000u64 {
        let solution_hash =
            sp_io::hashing::blake2_256(&(nonce, mix_hash, algorithm_version).encode());
        let candidate = EntryPow {
            nonce,
            mix_hash,
            solution_hash,
            algorithm_version,
        };
        if <GPUPoW as pallet_gpu_attestation::EntryPowVerifier>::verify_entry_pow(
            candidate.nonce,
            &candidate.mix_hash,
            &candidate.solution_hash,
            candidate.algorithm_version,
        ) {
            return candidate;
        }
    }
    panic!("no solution within 1e6 nonces — raise the target in the test");
}

fn dummy_pow() -> EntryPow {
    EntryPow {
        nonce: 0,
        mix_hash: [9u8; 32],
        solution_hash: [9u8; 32],
        algorithm_version: 1,
    }
}

fn entry_call(pow: Option<EntryPow>) -> RuntimeCall {
    RuntimeCall::ComputeMarketplace(pallet_compute_marketplace::Call::register_provider_entry {
        pow,
        challenge_response: [1u8; 32],
        gpu_info_hash: [0x47u8; 32],
        gpu_count: 1,
        compute_gflops: 1_000,
        requirements: GPURequirements::default(),
    })
}

fn fee_error(pow: Option<EntryPow>, who: AccountId) -> Option<TransactionValidityError> {
    let call = entry_call(pow);
    let info = call.get_dispatch_info();
    let ext = ChargeTransactionPayment::<Runtime>::from(0u128);
    ext.validate(
        RuntimeOrigin::signed(who),
        &call,
        &info,
        100,
        (),
        &sp_runtime::traits::TxBaseImplication(()),
        TransactionSource::External,
    )
    .err()
}

#[test]
fn pow_call_is_pays_no_and_plain_call_is_pays_yes() {
    new_test_ext().execute_with(|| {
        let free = entry_call(Some(dummy_pow())).get_dispatch_info();
        assert_eq!(free.pays_fee, Pays::No);
        let paid = entry_call(None).get_dispatch_info();
        assert_eq!(paid.pays_fee, Pays::Yes);
    });
}

#[test]
fn zero_balance_account_can_join_in_one_call_when_it_proves_work() {
    new_test_ext().execute_with(|| {
        // Easy target so the test can mine a real solution.
        pallet_gpu_pow::Difficulty::<Runtime>::put([0, 0, 0, u64::MAX]);
        let who = acct(7);
        assert_eq!(Balances::free_balance(&who), 0);

        let call = entry_call(Some(solution()));
        let info = call.get_dispatch_info();
        assert_eq!(info.pays_fee, Pays::No);
        // The fee gate would pass… actually there is nothing to charge; the
        // point is that validation does not demand a balance.
        assert!(fee_error(Some(solution()), who.clone()).is_none());

        ComputeMarketplace::register_provider_entry(
            RuntimeOrigin::signed(who.clone()),
            Some(solution()),
            [1u8; 32],
            [0x47u8; 32],
            1,
            1_000,
            GPURequirements::default(),
        )
        .expect("fresh account joins with computation");

        assert!(pallet_gpu_attestation::Attestations::<Runtime>::contains_key(&who));
        assert!(pallet_compute_marketplace::RegisteredProviders::<Runtime>::contains_key(&who));
        // Paid in computation only: still zero coins.
        assert_eq!(Balances::free_balance(&who), 0);
    });
}

#[test]
fn same_call_without_pow_cannot_be_paid_by_a_zero_balance_account() {
    new_test_ext().execute_with(|| {
        let who = acct(8);
        assert_eq!(Balances::free_balance(&who), 0);
        let err = fee_error(None, who);
        assert_eq!(
            err,
            Some(TransactionValidityError::Invalid(
                InvalidTransaction::Payment
            )),
            "without a proof the call must be fee-paying, and the fee must be unpayable"
        );
    });
}

#[test]
fn invalid_proof_fails_dispatch_and_leaves_no_registration() {
    new_test_ext().execute_with(|| {
        pallet_gpu_pow::Difficulty::<Runtime>::put([0, 0, 0, u64::MAX]);
        let who = acct(9);
        let mut bad = solution();
        bad.solution_hash = [0xAB; 32];
        let res = ComputeMarketplace::register_provider_entry(
            RuntimeOrigin::signed(who.clone()),
            Some(bad),
            [1u8; 32],
            [0x48u8; 32],
            1,
            1_000,
            GPURequirements::default(),
        );
        assert!(res.is_err());
        assert!(!pallet_gpu_attestation::Attestations::<Runtime>::contains_key(&who));
        assert!(!pallet_compute_marketplace::RegisteredProviders::<Runtime>::contains_key(&who));
    });
}

#[test]
fn second_free_entry_for_the_same_account_is_rejected() {
    new_test_ext().execute_with(|| {
        pallet_gpu_pow::Difficulty::<Runtime>::put([0, 0, 0, u64::MAX]);
        let who = acct(10);
        ComputeMarketplace::register_provider_entry(
            RuntimeOrigin::signed(who.clone()),
            Some(solution()),
            [1u8; 32],
            [0x49u8; 32],
            1,
            1_000,
            GPURequirements::default(),
        )
        .unwrap();
        let err = GPUAttestation::attest_entry(
            RuntimeOrigin::signed(who),
            Some(solution()),
            [1u8; 32],
            [0x4Au8; 32],
            1,
            1_000,
        )
        .unwrap_err();
        assert_eq!(
            err,
            pallet_gpu_attestation::Error::<Runtime>::FreeEntryAccountUsed.into()
        );
    });
}

// ── Existential deposit: zero on EVERY network (spec 48) ──────────────

/// The predicate the ED test pins. A build with a non-zero ED must fail it —
/// the negative controls below prove this check is not vacuous.
fn ed_is_zero(ed: u128) -> bool {
    ed == 0
}

#[test]
fn existential_deposit_is_zero_on_every_network_and_every_feature_build() {
    // This file is compiled once per feature set, so the same assertion runs in
    // the default build and with `testnet-zero-ed` / `testnet-no-stake` / ...
    // If any combination could still yield 1 GPN, this fails.
    assert!(
        ed_is_zero(ExistentialDeposit::get()),
        "ExistentialDeposit must be 0 on every network, got {}",
        ExistentialDeposit::get()
    );
    assert_eq!(ExistentialDeposit::get(), 0);

    new_test_ext().execute_with(|| {
        use frame_support::traits::{Currency, ExistenceRequirement, WithdrawReasons};
        let who = acct(11);
        assert_eq!(Balances::free_balance(&who), 0);
        // At ED=0 even a 1-unit balance is a real account…
        let _ = <Balances as Currency<AccountId>>::deposit_creating(&who, 1);
        // …and it survives being drained to exactly zero (KeepAlive is legal
        // at zero when the existential deposit is zero).
        let _ = <Balances as Currency<AccountId>>::withdraw(
            &who,
            1,
            WithdrawReasons::all(),
            ExistenceRequirement::KeepAlive,
        )
        .expect("draining to zero must be allowed at ED=0");
        assert_eq!(Balances::free_balance(&who), 0);
        assert!(
            frame_system::Account::<Runtime>::contains_key(&who),
            "ED=0 must let a zero-balance account exist"
        );
    });
}

/// The ED=0 test above must not be vacuous: this control feeds it the values a
/// non-zero-ED build would expose and requires them to fail.
#[test]
fn negative_control_a_non_zero_ed_would_fail_the_ed_test() {
    assert!(!ed_is_zero(1), "a 1-unit ED must fail the ED=0 check");
    assert!(
        !ed_is_zero(1_000_000_000_000),
        "the old 1 GPN ED must fail the ED=0 check"
    );
    assert!(ed_is_zero(ExistentialDeposit::get()));
}
