//! Benchmarking for `pallet-gpn-liveness` (workplan item 9.2).

#![cfg(feature = "runtime-benchmarks")]

use super::*;
use crate::pallet::Pallet as Liveness;
use frame_benchmarking::v2::*;
use frame_support::sp_runtime::traits::Bounded;
use frame_support::sp_runtime::SaturatedConversion;
use frame_support::traits::{Currency, Get};
use frame_system::pallet_prelude::BlockNumberFor;
use frame_system::RawOrigin;
use sp_std::vec::Vec;

const SEED: u32 = 0;

/// Fund `who` richly so bond transfers always succeed.
fn fund<T: Config>(who: &T::AccountId) {
    T::Currency::make_free_balance_be(who, BalanceOf::<T>::max_value() / 2u32.into());
}

fn endpoint() -> Vec<u8> {
    b"/ip4/1.2.3.4/tcp/30333".to_vec()
}

#[benchmarks]
mod benchmarks {
    use super::*;

    #[benchmark]
    fn register_fullnode() {
        let caller: T::AccountId = whitelisted_caller();
        fund::<T>(&caller);
        let endpoint: Vec<u8> = endpoint();

        #[extrinsic_call]
        register_fullnode(RawOrigin::Signed(caller.clone()), endpoint);

        assert!(Endpoints::<T>::contains_key(&caller));
    }

    #[benchmark]
    fn unregister_fullnode() {
        let caller: T::AccountId = whitelisted_caller();
        fund::<T>(&caller);
        Liveness::<T>::register_fullnode(RawOrigin::Signed(caller.clone()).into(), endpoint())
            .unwrap();

        #[extrinsic_call]
        unregister_fullnode(RawOrigin::Signed(caller.clone()));

        assert!(!Endpoints::<T>::contains_key(&caller));
    }

    #[benchmark]
    fn witness_reachable() {
        let witness: T::AccountId = whitelisted_caller();
        let target: T::AccountId = account("target", 0, SEED);
        fund::<T>(&witness);
        fund::<T>(&target);
        Liveness::<T>::register_fullnode(RawOrigin::Signed(witness.clone()).into(), endpoint())
            .unwrap();
        Liveness::<T>::register_fullnode(RawOrigin::Signed(target.clone()).into(), endpoint())
            .unwrap();

        #[extrinsic_call]
        witness_reachable(RawOrigin::Signed(witness.clone()), target.clone());

        assert!(Endpoints::<T>::contains_key(&target));
    }

    #[benchmark]
    fn submit_liveness() {
        let caller: T::AccountId = whitelisted_caller();
        let target: T::AccountId = account("target", 0, SEED);
        fund::<T>(&target);
        Liveness::<T>::register_fullnode(RawOrigin::Signed(target.clone()).into(), endpoint())
            .unwrap();
        // Advance two windows so the previous window is finalizable.
        let window: u64 = T::LivenessWindow::get();
        let target_block: BlockNumberFor<T> = window.saturating_mul(2).saturated_into();
        frame_system::Pallet::<T>::set_block_number(target_block);

        #[extrinsic_call]
        submit_liveness(RawOrigin::Signed(caller.clone()), target.clone());
    }

    impl_benchmark_test_suite!(Liveness, crate::tests::new_test_ext(), crate::tests::Test);
}
