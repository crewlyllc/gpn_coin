//! Benchmarking for `pallet-compute-marketplace` (workplan item 9.2).

#![cfg(feature = "runtime-benchmarks")]

use super::*;
use crate::pallet::Pallet as Market;
use frame_benchmarking::v2::*;
use frame_support::sp_runtime::traits::Bounded;
use frame_support::traits::{Currency, Get};
use frame_system::RawOrigin;

const SEED: u32 = 0;

fn fund<T: Config>(who: &T::AccountId) {
    T::Currency::make_free_balance_be(who, BalanceOf::<T>::max_value() / 2u32.into());
}

/// Requirements with no compute gate, so a provider does not need a matching
/// attestation to accept (keeps the benchmark self-contained).
fn reqs() -> GPURequirements {
    GPURequirements {
        min_vram_mb: 0,
        min_compute_capability: 0,
        min_gpu_count: 0,
        min_cuda_cores: 0,
        min_gflops: 0,
    }
}

/// Escrow price comfortably above the runtime existential deposit.
fn price<T: Config>() -> BalanceOf<T> {
    let extra: BalanceOf<T> = 10_000u32.into();
    T::Currency::minimum_balance() + extra
}

fn register_provider_for<T: Config>(who: &T::AccountId) {
    Market::<T>::register_provider(RawOrigin::Signed(who.clone()).into(), reqs()).unwrap();
}

fn make_job<T: Config>(requestor: &T::AccountId) -> u64 {
    Market::<T>::create_job(
        RawOrigin::Signed(requestor.clone()).into(),
        reqs(),
        JobType::LLMInference,
        JobTier::Spot,
        price::<T>(),
        T::MaxJobDuration::get(),
        [9u8; 32],
    )
    .unwrap();
    NextJobId::<T>::get().saturating_sub(1)
}

/// Register a provider, create a job, and have the provider accept it.
fn setup_accepted_job<T: Config>() -> (T::AccountId, T::AccountId, u64) {
    let provider: T::AccountId = whitelisted_caller();
    let requestor: T::AccountId = account("requestor", 0, SEED);
    fund::<T>(&provider);
    fund::<T>(&requestor);
    register_provider_for::<T>(&provider);
    let job_id = make_job::<T>(&requestor);
    Market::<T>::accept_job(RawOrigin::Signed(provider.clone()).into(), job_id).unwrap();
    (provider, requestor, job_id)
}

#[benchmarks]
mod benchmarks {
    use super::*;

    #[benchmark]
    fn register_provider() {
        let caller: T::AccountId = whitelisted_caller();

        #[extrinsic_call]
        register_provider(RawOrigin::Signed(caller.clone()), reqs());

        assert!(RegisteredProviders::<T>::contains_key(&caller));
    }

    #[benchmark]
    fn unregister_provider() {
        let caller: T::AccountId = whitelisted_caller();
        register_provider_for::<T>(&caller);

        #[extrinsic_call]
        unregister_provider(RawOrigin::Signed(caller.clone()));

        assert!(!RegisteredProviders::<T>::contains_key(&caller));
    }

    #[benchmark]
    fn create_job() {
        let caller: T::AccountId = whitelisted_caller();
        fund::<T>(&caller);

        #[extrinsic_call]
        create_job(
            RawOrigin::Signed(caller.clone()),
            reqs(),
            JobType::LLMInference,
            JobTier::Spot,
            price::<T>(),
            T::MaxJobDuration::get(),
            [9u8; 32],
        );

        assert_eq!(ActiveJobCount::<T>::get(), 1);
    }

    #[benchmark]
    fn accept_job() {
        let provider: T::AccountId = whitelisted_caller();
        let requestor: T::AccountId = account("requestor", 0, SEED);
        fund::<T>(&provider);
        fund::<T>(&requestor);
        register_provider_for::<T>(&provider);
        let job_id = make_job::<T>(&requestor);

        #[extrinsic_call]
        accept_job(RawOrigin::Signed(provider.clone()), job_id);

        assert_eq!(Jobs::<T>::get(job_id).unwrap().state, JobState::Accepted);
    }

    #[benchmark]
    fn start_job() {
        let (provider, _requestor, job_id) = setup_accepted_job::<T>();

        #[extrinsic_call]
        start_job(RawOrigin::Signed(provider.clone()), job_id);

        assert_eq!(Jobs::<T>::get(job_id).unwrap().state, JobState::Running);
    }

    #[benchmark]
    fn submit_result() {
        let (provider, _requestor, job_id) = setup_accepted_job::<T>();
        Market::<T>::start_job(RawOrigin::Signed(provider.clone()).into(), job_id).unwrap();

        #[extrinsic_call]
        submit_result(RawOrigin::Signed(provider.clone()), job_id, [5u8; 32]);

        assert_eq!(
            Jobs::<T>::get(job_id).unwrap().state,
            JobState::AwaitingVerification
        );
    }

    #[benchmark]
    fn complete_job() {
        let (provider, _requestor, job_id) = setup_accepted_job::<T>();
        Market::<T>::start_job(RawOrigin::Signed(provider.clone()).into(), job_id).unwrap();
        Market::<T>::submit_result(RawOrigin::Signed(provider.clone()).into(), job_id, [5u8; 32])
            .unwrap();
        let completer: T::AccountId = account("completer", 0, SEED);

        #[extrinsic_call]
        complete_job(RawOrigin::Signed(completer.clone()), job_id);

        assert_eq!(Jobs::<T>::get(job_id).unwrap().state, JobState::Completed);
    }

    #[benchmark]
    fn cancel_job() {
        let requestor: T::AccountId = whitelisted_caller();
        fund::<T>(&requestor);
        let job_id = make_job::<T>(&requestor);

        #[extrinsic_call]
        cancel_job(RawOrigin::Signed(requestor.clone()), job_id);

        assert_eq!(Jobs::<T>::get(job_id).unwrap().state, JobState::Cancelled);
    }

    impl_benchmark_test_suite!(Market, crate::tests::new_test_ext(), crate::tests::Test);
}
