//! Benchmarking for `pallet-gpn-treasury` (workplan item 9.2).

#![cfg(feature = "runtime-benchmarks")]

use super::*;
use crate::pallet::Pallet as Treasury;
use frame_benchmarking::v2::*;
use frame_support::sp_runtime::traits::{Bounded, Zero};
use frame_support::traits::Currency;
use frame_support::BoundedVec;
use frame_system::RawOrigin;
use sp_std::vec::Vec;

const SEED: u32 = 0;

fn member<T: Config>(i: u32) -> T::AccountId {
    account("member", i, SEED)
}

/// Install a roster + threshold directly into storage (the benchmark runtime's
/// genesis roster is not assumed).
fn install_roster<T: Config>(members: &[T::AccountId], threshold: u32) {
    let mut roster: BoundedVec<T::AccountId, T::MaxRoster> = BoundedVec::default();
    for m in members {
        roster.try_push(m.clone()).unwrap();
    }
    Roster::<T>::put(roster);
    Threshold::<T>::put(threshold);
}

#[benchmarks]
mod benchmarks {
    use super::*;

    #[benchmark]
    fn propose_spend() {
        let who = member::<T>(0);
        let dest = member::<T>(1);
        install_roster::<T>(&[who.clone()], 1);
        let amount: BalanceOf<T> = 100u32.into();
        let id = NextProposalId::<T>::get();

        #[extrinsic_call]
        propose_spend(RawOrigin::Signed(who.clone()), dest, amount);

        assert!(ProposalDest::<T>::contains_key(id));
    }

    #[benchmark]
    fn approve_spend() {
        let proposer = member::<T>(0);
        let approver = member::<T>(1);
        let dest = member::<T>(2);
        install_roster::<T>(&[proposer.clone(), approver.clone()], 1);
        Treasury::<T>::propose_spend(
            RawOrigin::Signed(proposer.clone()).into(),
            dest,
            100u32.into(),
        )
        .unwrap();
        let id = NextProposalId::<T>::get().saturating_sub(1);

        #[extrinsic_call]
        approve_spend(RawOrigin::Signed(approver.clone()), id);

        assert_eq!(ProposalApprovals::<T>::get(id).len(), 2);
    }

    #[benchmark]
    fn execute_spend() {
        let member_acct = member::<T>(0);
        install_roster::<T>(&[member_acct.clone()], 1);
        // Fund the treasury and give it an unlocked tranche.
        T::Currency::make_free_balance_be(
            &Treasury::<T>::account_id(),
            BalanceOf::<T>::max_value() / 2u32.into(),
        );
        let mut schedule: BoundedVec<TrancheOf<T>, T::MaxTranches> = BoundedVec::default();
        schedule
            .try_push(Tranche {
                unlock_block: Zero::zero(),
                amount: 1_000_000u32.into(),
            })
            .unwrap();
        Schedule::<T>::put(schedule);
        let dest = member::<T>(1);
        Treasury::<T>::propose_spend(
            RawOrigin::Signed(member_acct.clone()).into(),
            dest,
            100u32.into(),
        )
        .unwrap();
        let id = NextProposalId::<T>::get().saturating_sub(1);

        #[extrinsic_call]
        execute_spend(RawOrigin::Signed(member_acct.clone()), id);

        assert!(ProposalExecuted::<T>::get(id));
    }

    #[benchmark]
    fn set_roster() {
        let new_roster: Vec<T::AccountId> = Vec::from([member::<T>(0), member::<T>(1)]);

        #[extrinsic_call]
        set_roster(RawOrigin::Root, new_roster);

        assert_eq!(Roster::<T>::get().len(), 2);
    }

    #[benchmark]
    fn set_threshold() {
        #[extrinsic_call]
        set_threshold(RawOrigin::Root, 5u32);

        assert_eq!(Threshold::<T>::get(), 5);
    }

    impl_benchmark_test_suite!(Treasury, crate::tests::new_test_ext(), crate::tests::Test);
}
