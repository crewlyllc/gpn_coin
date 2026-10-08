//! Benchmarking for `pallet-gpu-attestation` (workplan item 9.2).

#![cfg(feature = "runtime-benchmarks")]

use super::*;
use crate::pallet::Pallet as Attest;
use frame_benchmarking::v2::*;
use frame_support::sp_runtime::traits::Bounded;
use frame_support::traits::{Currency, Get};
use frame_system::RawOrigin;

const SEED: u32 = 0;

/// Fund `who` richly so stake transfers always succeed.
fn fund<T: Config>(who: &T::AccountId) {
    T::Currency::make_free_balance_be(who, BalanceOf::<T>::max_value() / 2u32.into());
}

#[benchmarks]
mod benchmarks {
    use super::*;

    #[benchmark]
    fn deposit_stake() {
        let caller: T::AccountId = whitelisted_caller();
        fund::<T>(&caller);
        let amount: BalanceOf<T> = T::MinAttestationStake::get();

        #[extrinsic_call]
        deposit_stake(RawOrigin::Signed(caller.clone()), amount);

        assert!(ProviderStake::<T>::contains_key(&caller));
    }

    #[benchmark]
    fn submit_attestation() {
        let caller: T::AccountId = whitelisted_caller();
        fund::<T>(&caller);
        Attest::<T>::deposit_stake(
            RawOrigin::Signed(caller.clone()).into(),
            T::MinAttestationStake::get(),
        )
        .unwrap();

        // Clear the per-tier (Bronze = 100) AND the eligibility floor (item 19)
        // so the benchmark exercises the success path under any runtime config.
        let gflops = T::MinEligibilityGflops::get().max(100u64);

        #[extrinsic_call]
        submit_attestation(
            RawOrigin::Signed(caller.clone()),
            [1u8; 32],
            [2u8; 32],
            1u8,
            gflops,
        );

        assert!(Attestations::<T>::contains_key(&caller));
    }

    #[benchmark]
    fn withdraw_stake() {
        let caller: T::AccountId = whitelisted_caller();
        fund::<T>(&caller);
        Attest::<T>::deposit_stake(
            RawOrigin::Signed(caller.clone()).into(),
            T::MinAttestationStake::get(),
        )
        .unwrap();

        #[extrinsic_call]
        withdraw_stake(RawOrigin::Signed(caller.clone()));

        assert!(!ProviderStake::<T>::contains_key(&caller));
    }

    #[benchmark]
    fn check_expired() {
        let caller: T::AccountId = whitelisted_caller();
        let target: T::AccountId = account("target", 0, SEED);
        fund::<T>(&target);
        Attest::<T>::deposit_stake(
            RawOrigin::Signed(target.clone()).into(),
            T::MinAttestationStake::get(),
        )
        .unwrap();
        Attest::<T>::submit_attestation(
            RawOrigin::Signed(target.clone()).into(),
            [1u8; 32],
            [2u8; 32],
            1u8,
            T::MinEligibilityGflops::get().max(100u64),
        )
        .unwrap();
        // Set the record one miss away from the slash limit so the (heavier)
        // slashing branch is exercised, and advance past the max age.
        if let Some(mut rec) = Attestations::<T>::get(&target) {
            rec.missed_count = T::MaxMissedAttestations::get().saturating_sub(1);
            Attestations::<T>::insert(&target, rec);
        }
        let age = T::MaxAttestationAge::get();
        let now = frame_system::Pallet::<T>::block_number();
        frame_system::Pallet::<T>::set_block_number(now + age + age);

        #[extrinsic_call]
        check_expired(RawOrigin::Signed(caller.clone()), target.clone());
    }

    impl_benchmark_test_suite!(Attest, crate::tests::new_test_ext(), crate::tests::Test);
}
