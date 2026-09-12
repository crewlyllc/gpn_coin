//! Benchmarking for `pallet-gpn-token` (workplan item 9.2).

#![cfg(feature = "runtime-benchmarks")]

use super::*;
use crate::pallet::Pallet as Token;
use frame_benchmarking::v2::*;
use frame_support::sp_runtime::traits::Bounded;
use frame_support::traits::Currency;
use frame_system::RawOrigin;
use sp_std::vec::Vec;

const SEED: u32 = 0;

/// Fund the token pallet account so outbound transfers / burns succeed.
fn fund_pallet<T: Config>() {
    T::Currency::make_free_balance_be(
        &Token::<T>::account_id(),
        BalanceOf::<T>::max_value() / 2u32.into(),
    );
}

#[benchmarks]
mod benchmarks {
    use super::*;

    #[benchmark]
    fn mint_block_reward() {
        let beneficiary: T::AccountId = account("beneficiary", 0, SEED);
        let amount: BalanceOf<T> = 1_000u32.into();

        #[extrinsic_call]
        mint_block_reward(RawOrigin::Root, amount, beneficiary);
    }

    #[benchmark]
    fn distribute_fees() {
        fund_pallet::<T>();
        let total_fee: BalanceOf<T> = 10_000u32.into();

        #[extrinsic_call]
        distribute_fees(RawOrigin::Root, total_fee);
    }

    #[benchmark]
    fn set_fee_distribution() {
        let cfg = FeeDistribution {
            staking_pool_bps: 250,
            treasury_bps: 100,
            burn_bps: 100,
            oracle_bps: 50,
        };

        #[extrinsic_call]
        set_fee_distribution(RawOrigin::Root, cfg);
    }

    #[benchmark]
    fn distribute_staking_rewards(
        s: Linear<1, 100>,
    ) {
        let caller: T::AccountId = whitelisted_caller();
        fund_pallet::<T>();
        let pool: BalanceOf<T> = 1_000_000u32.into();
        StakingRewardPool::<T>::put(pool);
        let stakers: Vec<T::AccountId> =
            (0..s).map(|i| account("staker", i, SEED)).collect();

        #[extrinsic_call]
        distribute_staking_rewards(RawOrigin::Signed(caller.clone()), stakers);
    }

    #[benchmark]
    fn distribute_oracle_rewards(
        o: Linear<1, 100>,
    ) {
        let caller: T::AccountId = whitelisted_caller();
        fund_pallet::<T>();
        let pool: BalanceOf<T> = 1_000_000u32.into();
        OracleRewardPool::<T>::put(pool);
        let operators: Vec<T::AccountId> =
            (0..o).map(|i| account("operator", i, SEED)).collect();

        #[extrinsic_call]
        distribute_oracle_rewards(RawOrigin::Signed(caller.clone()), operators);
    }

    impl_benchmark_test_suite!(Token, crate::tests::new_test_ext(), crate::tests::Test);
}
