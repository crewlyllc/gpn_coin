//! Benchmarking for `pallet-gpu-pow` (workplan item 9.2).
//!
//! Measured with `frame-omni-bencher`; the generated values replace the
//! pre-benchmark baselines in [`crate::weights`]. Only public dispatchables are
//! benchmarked.

#![cfg(feature = "runtime-benchmarks")]

use super::*;
use crate::pallet::Pallet as GpuPow;
use frame_benchmarking::v2::*;
use frame_support::traits::Get;
use frame_system::RawOrigin;

/// Build an [`EthashSolution`] whose `solution_hash` matches the pallet's
/// recomputation, so `submit_solution` accepts it (same construction as the
/// unit tests).
fn valid_solution<T: Config>(nonce: u64) -> EthashSolution {
    let sol = EthashSolution {
        nonce,
        mix_hash: [0u8; 32],
        solution_hash: [0u8; 32],
        algorithm_version: T::AlgorithmVersion::get(),
    };
    let work_data = (sol.nonce, sol.mix_hash, sol.algorithm_version);
    let correct_hash = sp_io::hashing::blake2_256(&work_data.encode());
    EthashSolution {
        solution_hash: correct_hash,
        ..sol
    }
}

#[benchmarks]
mod benchmarks {
    use super::*;

    #[benchmark]
    fn submit_solution() {
        let caller: T::AccountId = whitelisted_caller();
        let solution = valid_solution::<T>(0);
        // `MaxDifficulty` is the easiest target the runtime ever allows, so any
        // recomputed hash satisfies it (mirrors the pallet's own unit test).
        let difficulty = T::MaxDifficulty::get();

        #[extrinsic_call]
        submit_solution(RawOrigin::Signed(caller.clone()), solution, difficulty);

        assert!(PendingRewardBeneficiary::<T>::get().is_some());
    }

    #[benchmark]
    fn set_block_beneficiary() {
        let beneficiary: T::AccountId = whitelisted_caller();

        #[extrinsic_call]
        set_block_beneficiary(RawOrigin::Root, beneficiary.clone());

        assert_eq!(BlockRewardBeneficiary::<T>::get(), Some(beneficiary));
    }

    impl_benchmark_test_suite!(GpuPow, crate::tests::new_test_ext(), crate::tests::Test);
}
