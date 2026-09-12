//! Benchmarking for `pallet-profitability-oracle` (workplan item 9.2).

#![cfg(feature = "runtime-benchmarks")]

use super::*;
use crate::pallet::Pallet as Oracle;
use frame_benchmarking::v2::*;
use frame_support::sp_runtime::traits::{Bounded, Zero};
use frame_support::traits::{Currency, Get};
use frame_support::BoundedVec;
use frame_system::pallet_prelude::BlockNumberFor;
use frame_system::RawOrigin;

const SEED: u32 = 0;

fn fund<T: Config>(who: &T::AccountId) {
    T::Currency::make_free_balance_be(who, BalanceOf::<T>::max_value() / 2u32.into());
}

/// Fund the pallet account (holds bonds) so outbound transfers/slashes succeed.
fn fund_pallet<T: Config>() {
    T::Currency::make_free_balance_be(
        &Oracle::<T>::account_id(),
        BalanceOf::<T>::max_value() / 2u32.into(),
    );
}

#[benchmarks]
mod benchmarks {
    use super::*;

    #[benchmark]
    fn update_feed() {
        let caller: T::AccountId = whitelisted_caller();
        OracleOperators::<T>::insert(&caller, ());

        #[extrinsic_call]
        update_feed(RawOrigin::Signed(caller.clone()), 1_000u64, 500u64, 8_000u32, 1_200u32);

        assert!(OperatorFeeds::<T>::contains_key(&caller));
    }

    #[benchmark]
    fn propose_operator() {
        let caller: T::AccountId = whitelisted_caller();
        let operator: T::AccountId = account("operator", 0, SEED);
        Committee::<T>::insert(&caller, ());

        #[extrinsic_call]
        propose_operator(RawOrigin::Signed(caller.clone()), operator.clone());

        // Either admitted immediately (threshold 1) or pending.
        assert!(
            OracleOperators::<T>::contains_key(&operator)
                || PendingAdmissions::<T>::contains_key(&operator)
        );
    }

    #[benchmark]
    fn approve_operator() {
        let proposer: T::AccountId = account("proposer", 0, SEED);
        let approver: T::AccountId = whitelisted_caller();
        let operator: T::AccountId = account("operator", 0, SEED);
        Committee::<T>::insert(&approver, ());
        let mut approvals: BoundedVec<T::AccountId, T::CommitteeSize> = BoundedVec::new();
        approvals.try_push(proposer.clone()).unwrap();
        PendingAdmissions::<T>::insert(
            &operator,
            PendingAdmission {
                proposer,
                approvals,
            },
        );

        #[extrinsic_call]
        approve_operator(RawOrigin::Signed(approver.clone()), operator.clone());
    }

    #[benchmark]
    fn remove_operator() {
        let caller: T::AccountId = whitelisted_caller();
        let operator: T::AccountId = account("operator", 0, SEED);
        Committee::<T>::insert(&caller, ());
        OracleOperators::<T>::insert(&operator, ());
        OperatorCount::<T>::put(1u32);

        #[extrinsic_call]
        remove_operator(RawOrigin::Signed(caller.clone()), operator.clone());

        assert!(!OracleOperators::<T>::contains_key(&operator));
    }

    #[benchmark]
    fn bond_operator() {
        let caller: T::AccountId = whitelisted_caller();
        fund::<T>(&caller);

        #[extrinsic_call]
        bond_operator(RawOrigin::Signed(caller.clone()));

        assert!(StakedOperators::<T>::contains_key(&caller));
    }

    #[benchmark]
    fn bond_more() {
        let caller: T::AccountId = whitelisted_caller();
        fund::<T>(&caller);
        Oracle::<T>::bond_operator(RawOrigin::Signed(caller.clone()).into()).unwrap();
        let additional: BalanceOf<T> = 10_000u32.into();

        #[extrinsic_call]
        bond_more(RawOrigin::Signed(caller.clone()), additional);

        assert!(StakedOperators::<T>::get(&caller) > T::OperatorBond::get());
    }

    #[benchmark]
    fn unbond_operator() {
        let caller: T::AccountId = whitelisted_caller();
        fund::<T>(&caller);
        Oracle::<T>::bond_operator(RawOrigin::Signed(caller.clone()).into()).unwrap();

        #[extrinsic_call]
        unbond_operator(RawOrigin::Signed(caller.clone()));

        assert!(!StakedOperators::<T>::contains_key(&caller));
    }

    #[benchmark]
    fn challenge_operator() {
        let challenger: T::AccountId = whitelisted_caller();
        let target: T::AccountId = account("target", 0, SEED);
        fund::<T>(&challenger);
        StakedOperators::<T>::insert(&target, T::OperatorBond::get());

        #[extrinsic_call]
        challenge_operator(RawOrigin::Signed(challenger.clone()), target.clone());

        assert!(OpenChallenges::<T>::contains_key(&target));
    }

    #[benchmark]
    fn resolve_challenge() {
        let challenger: T::AccountId = whitelisted_caller();
        let target: T::AccountId = account("target", 0, SEED);
        fund::<T>(&challenger);
        fund_pallet::<T>();
        StakedOperators::<T>::insert(&target, T::OperatorBond::get());
        // Record a feed, then advance past the staleness window.
        let last: BlockNumberFor<T> = Zero::zero();
        OperatorLastFeedBlock::<T>::insert(&target, last);
        let stale = T::StaleOperatorBlocks::get();
        frame_system::Pallet::<T>::set_block_number(stale + stale);
        // Post a challenge bond so a real challenge exists to resolve.
        Oracle::<T>::challenge_operator(RawOrigin::Signed(challenger.clone()).into(), target.clone())
            .unwrap();

        #[extrinsic_call]
        resolve_challenge(RawOrigin::Signed(challenger.clone()), target.clone(), 0u8);

        assert!(!OpenChallenges::<T>::contains_key(&target));
    }

    impl_benchmark_test_suite!(Oracle, crate::tests::new_test_ext(), crate::tests::Test);
}
