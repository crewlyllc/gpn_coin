//! Benchmarking for `pallet-gpn-token` (workplan item 9.2).

#![cfg(feature = "runtime-benchmarks")]

use super::*;
use crate::pallet::Pallet as Token;
use frame_benchmarking::v2::*;
use frame_support::sp_runtime::traits::Bounded;
use frame_support::traits::Currency;
use frame_system::RawOrigin;
use pallet_gpu_attestation::GPUAttestationProvider;
use sp_std::vec::Vec;

const SEED: u32 = 0;

/// Fund the token pallet account so outbound transfers / burns succeed.
fn fund_pallet<T: Config>() {
    T::Currency::make_free_balance_be(
        &Token::<T>::account_id(),
        BalanceOf::<T>::max_value() / 2u32.into(),
    );
}

/// `n` times the runtime existential deposit. Reward payouts are real balance
/// transfers, so amounts must keep fresh recipient accounts alive.
fn ed_n<T: Config>(n: u32) -> BalanceOf<T> {
    T::Currency::minimum_balance() * n.into()
}

#[benchmarks]
mod benchmarks {
    use super::*;

    #[benchmark]
    fn mint_block_reward() {
        let beneficiary: T::AccountId = account("beneficiary", 0, SEED);
        let amount: BalanceOf<T> = ed_n::<T>(2);

        #[extrinsic_call]
        mint_block_reward(RawOrigin::Root, amount, beneficiary);
    }

    #[benchmark]
    fn distribute_fees() {
        fund_pallet::<T>();
        // Large enough that every share clears the existential deposit so the
        // treasury transfer creates the (possibly fresh) treasury account.
        let total_fee: BalanceOf<T> = ed_n::<T>(100);

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
    fn distribute_staking_rewards(s: Linear<1, 100>) {
        fund_pallet::<T>();
        let pool: BalanceOf<T> = ed_n::<T>(200);
        StakingRewardPool::<T>::put(pool);
        let stakers: Vec<T::AccountId> = (0..s).map(|i| account("staker", i, SEED)).collect();
        // Attest every staker so the payout loop runs the attested (transfer)
        // path and the measured weight reflects the real cost per staker.
        for staker in &stakers {
            T::GPUAttestation::benchmark_set_attestation(
                staker,
                pallet_gpu_attestation::AttestationTier::Bronze,
            );
        }

        #[extrinsic_call]
        distribute_staking_rewards(RawOrigin::Root, stakers);
    }

    #[benchmark]
    fn distribute_oracle_rewards(o: Linear<1, 100>) {
        fund_pallet::<T>();
        let pool: BalanceOf<T> = ed_n::<T>(200);
        OracleRewardPool::<T>::put(pool);
        let operators: Vec<T::AccountId> = (0..o).map(|i| account("operator", i, SEED)).collect();

        #[extrinsic_call]
        distribute_oracle_rewards(RawOrigin::Root, operators);
    }

    impl_benchmark_test_suite!(Token, crate::tests::new_test_ext(), crate::tests::Test);
}
