//! # Compute Marketplace Pallet
//!
//! Decentralized GPU compute marketplace where providers offer GPU time
//! and requestors submit AI inference/training jobs.

#![cfg_attr(not(feature = "std"), no_std)]

use codec::{Decode, DecodeWithMemTracking, Encode, MaxEncodedLen};
use core::fmt::Debug;
use pallet_gpu_attestation::GPUAttestationProvider;
use scale_info::TypeInfo;

pub use pallet::*;

/// GPU capability requirements
#[derive(Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo)]
pub struct GPURequirements {
    pub min_vram_mb: u32,
    pub min_compute_capability: u16,
    pub min_gpu_count: u8,
    pub min_cuda_cores: u32,
    /// Minimum measured compute throughput (FP32 GFLOPS) the provider must
    /// attest to (see `gpu-attestation::AttestationRecord::compute_gflops`).
    /// 0 = no throughput requirement.
    pub min_gflops: u64,
}

impl Default for GPURequirements {
    fn default() -> Self {
        Self { min_vram_mb: 8_000, min_compute_capability: 75, min_gpu_count: 1, min_cuda_cores: 0, min_gflops: 0 }
    }
}

#[derive(Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo)]
pub enum JobType { LLMInference, FineTuning, ImageGeneration, MLInference, Custom }
impl Default for JobType { fn default() -> Self { Self::LLMInference } }

#[derive(Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo)]
pub enum JobTier { Premium, Spot, Reserved }
impl Default for JobTier { fn default() -> Self { Self::Spot } }

#[derive(Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo)]
pub enum JobState { Pending, Accepted, Running, AwaitingVerification, Completed, Failed, Cancelled }
impl Default for JobState { fn default() -> Self { Self::Pending } }

#[derive(Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo)]
pub struct ComputeJob {
    pub id: u64,
    pub gpu_requirements: GPURequirements,
    pub job_type: JobType,
    pub tier: JobTier,
    pub state: JobState,
    pub price: u128,
    pub max_duration: u32,
    pub created_at: u32,
    pub started_at: Option<u32>,
    pub workload_hash: [u8; 32],
    pub result_hash: Option<[u8; 32]>,
}

#[frame_support::pallet]
pub mod pallet {
    use super::*;
    use frame_support::pallet_prelude::*;
    use frame_support::sp_runtime::traits::{AccountIdConversion, Saturating, Zero};
    use frame_support::traits::{Currency, ExistenceRequirement};
    use frame_system::pallet_prelude::*;
    use pallet_gpn_token::FeeSinkProvider;
    use sp_std::vec::Vec;

    pub type BalanceOf<T> =
        <<T as Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::Balance;

    #[pallet::config]
    pub trait Config: frame_system::Config {
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;
        type Currency: Currency<Self::AccountId>;
        #[pallet::constant] type MarketplaceFeeBps: Get<u32>;
        #[pallet::constant] type MaxJobsPerProvider: Get<u32>;
        #[pallet::constant] type EscrowLockDuration: Get<BlockNumberFor<Self>>;
        #[pallet::constant] type MaxActiveJobs: Get<u32>;
        #[pallet::constant] type MaxJobDuration: Get<BlockNumberFor<Self>>;
        /// Read-only access to providers' attested GPU compute credentials.
        /// Used to enforce that a provider accepting a job actually meets the
        /// job's `min_gflops` requirement (proof-of-compute gate at match time).
        type GPUAttestation: GPUAttestationProvider<Self::AccountId>;
        /// Where marketplace fees are routed on completion. Wired to the GPN
        /// token pallet (`gpn/tokn`) so collected fees feed `distribute_fees`
        /// (staking/oracle pools + treasury + burn) — self-funding fee economy.
        type FeeSink: pallet_gpn_token::FeeSinkProvider<Self::AccountId>;
    }

    #[pallet::pallet]
    #[pallet::without_storage_info]
    pub struct Pallet<T>(_);

    // Storage
    #[pallet::storage] pub type NextJobId<T: Config> = StorageValue<_, u64, ValueQuery>;
    #[pallet::storage] pub type Jobs<T: Config> = StorageMap<_, Blake2_128Concat, u64, ComputeJob, OptionQuery>;
    #[pallet::storage] pub type JobRequestor<T: Config> = StorageMap<_, Blake2_128Concat, u64, T::AccountId, OptionQuery>;
    #[pallet::storage] pub type JobProvider<T: Config> = StorageMap<_, Blake2_128Concat, u64, T::AccountId, OptionQuery>;
    #[pallet::storage] pub type ProviderJobs<T: Config> = StorageMap<_, Blake2_128Concat, T::AccountId, Vec<u64>, ValueQuery>;
    #[pallet::storage] pub type RequestorJobs<T: Config> = StorageMap<_, Blake2_128Concat, T::AccountId, Vec<u64>, ValueQuery>;
    #[pallet::storage] pub type JobEscrow<T: Config> = StorageMap<_, Blake2_128Concat, u64, BalanceOf<T>, OptionQuery>;
    #[pallet::storage] pub type RegisteredProviders<T: Config> = StorageMap<_, Blake2_128Concat, T::AccountId, GPURequirements, OptionQuery>;
    #[pallet::storage] pub type ActiveJobCount<T: Config> = StorageValue<_, u32, ValueQuery>;

    // Genesis
    #[pallet::genesis_config]
    #[derive(frame_support::DefaultNoBound)]
    pub struct GenesisConfig<T: Config> { #[serde(skip)] pub _config: sp_std::marker::PhantomData<T>, }
    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            <NextJobId<T>>::put(1u64);
            <ActiveJobCount<T>>::put(0u32);
        }
    }

    // Events
    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        JobCreated { job_id: u64, requestor: T::AccountId, tier: JobTier },
        JobAccepted { job_id: u64, provider: T::AccountId },
        JobStarted { job_id: u64 },
        JobCompleted { job_id: u64, provider: T::AccountId },
        JobFailed { job_id: u64, reason: u8 },
        JobCancelled { job_id: u64 },
        ProviderRegistered { who: T::AccountId },
        ProviderUnregistered { who: T::AccountId },
    }

    // Errors
    #[pallet::error]
    pub enum Error<T> {
        TooManyActiveJobs, MaxProviderCapacity, JobNotFound, JobNotPending,
        JobNotAccepted, JobNotRunning, AlreadyAccepted, NotJobProvider,
        NotJobRequestor, InsufficientPayment, InvalidWorkload, JobExpired,
        EscrowMismatch, ProviderNotRegistered, ProviderAlreadyRegistered,
        /// The provider's attested compute throughput (GFLOPS) is below the
        /// job's `min_gflops` requirement — it cannot honestly serve this job.
        ProviderComputeInsufficient,
    }

    // Hooks
    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
        fn on_initialize(_now: BlockNumberFor<T>) -> Weight { Weight::zero() }
    }

    // Dispatchable calls
    #[pallet::call]
    impl<T: Config> Pallet<T> {
        #[pallet::call_index(0)]
        #[pallet::weight(10_000)]
        pub fn register_provider(origin: OriginFor<T>, requirements: GPURequirements) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(!<RegisteredProviders<T>>::contains_key(&who), Error::<T>::ProviderAlreadyRegistered);
            <RegisteredProviders<T>>::insert(&who, requirements);
            Self::deposit_event(Event::<T>::ProviderRegistered { who });
            Ok(())
        }

        #[pallet::call_index(1)]
        #[pallet::weight(10_000)]
        pub fn unregister_provider(origin: OriginFor<T>) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(<RegisteredProviders<T>>::contains_key(&who), Error::<T>::ProviderNotRegistered);
            <RegisteredProviders<T>>::remove(&who);
            Self::deposit_event(Event::<T>::ProviderUnregistered { who });
            Ok(())
        }

        #[pallet::call_index(2)]
        #[pallet::weight(50_000)]
        pub fn create_job(
            origin: OriginFor<T>,
            gpu_requirements: GPURequirements,
            job_type: JobType,
            tier: JobTier,
            price: BalanceOf<T>,
            max_duration: BlockNumberFor<T>,
            workload_hash: [u8; 32],
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let requestor = who.clone();

            ensure!(<ActiveJobCount<T>>::get() < T::MaxActiveJobs::get(), Error::<T>::TooManyActiveJobs);
            ensure!(!workload_hash.iter().all(|&b| b == 0), Error::<T>::InvalidWorkload);
            ensure!(max_duration <= T::MaxJobDuration::get(), Error::<T>::JobExpired);
            ensure!(price > Zero::zero(), Error::<T>::InsufficientPayment);

            let job_id = <NextJobId<T>>::get();
            <NextJobId<T>>::mutate(|id| *id += 1);

            let now: u32 = frame_system::Pallet::<T>::block_number().try_into().unwrap_or(0);
            let max_dur: u32 = max_duration.try_into().unwrap_or(100_000);

            let job_tier = tier.clone();
            let job = ComputeJob {
                id: job_id,
                gpu_requirements, job_type, tier: job_tier,
                state: JobState::Pending,
                price: price.try_into().unwrap_or(0),
                max_duration: max_dur,
                created_at: now,
                started_at: None,
                workload_hash,
                result_hash: None,
            };

            T::Currency::transfer(&requestor, &Self::account_id(), price, ExistenceRequirement::KeepAlive)?;

            <Jobs<T>>::insert(job_id, job);
            <JobRequestor<T>>::insert(job_id, who.clone());
            <JobEscrow<T>>::insert(job_id, price);

            let mut jobs = <RequestorJobs<T>>::get(&requestor);
            jobs.push(job_id);
            <RequestorJobs<T>>::insert(&requestor, jobs);
            <ActiveJobCount<T>>::mutate(|c| *c += 1);

            Self::deposit_event(Event::<T>::JobCreated { job_id, requestor: who, tier });
            Ok(())
        }

        #[pallet::call_index(3)]
        #[pallet::weight(30_000)]
        pub fn accept_job(origin: OriginFor<T>, job_id: u64) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let provider = who.clone();
            ensure!(<RegisteredProviders<T>>::contains_key(&provider), Error::<T>::ProviderNotRegistered);
            let mut job = <Jobs<T>>::get(job_id).ok_or(Error::<T>::JobNotFound)?;
            ensure!(job.state == JobState::Pending, Error::<T>::JobNotPending);
            ensure!(!<JobProvider<T>>::contains_key(job_id), Error::<T>::AlreadyAccepted);
            ensure!((<ProviderJobs<T>>::get(&provider).len() as u32) < T::MaxJobsPerProvider::get(), Error::<T>::MaxProviderCapacity);

            // Proof-of-compute gate: if the job demands a minimum GFLOPS rate,
            // the provider must have *attested* to at least that throughput
            // (cross-pallet read of `gpu-attestation::AttestationRecord`). A
            // provider that hasn't attested (0 GFLOPS) can never meet a nonzero
            // requirement, so un-attested providers are blocked from paid jobs.
            let required_gflops = job.gpu_requirements.min_gflops;
            if required_gflops > 0 {
                let attested = T::GPUAttestation::attested_gflops(&provider);
                ensure!(attested >= required_gflops, Error::<T>::ProviderComputeInsufficient);
            }

            job.state = JobState::Accepted;
            <Jobs<T>>::insert(job_id, job);
            <JobProvider<T>>::insert(job_id, provider.clone());

            let mut jobs = <ProviderJobs<T>>::get(&provider);
            jobs.push(job_id);
            <ProviderJobs<T>>::insert(&provider, jobs);
            Self::deposit_event(Event::<T>::JobAccepted { job_id, provider });
            Ok(())
        }

        #[pallet::call_index(4)]
        #[pallet::weight(10_000)]
        pub fn start_job(origin: OriginFor<T>, job_id: u64) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let mut job = <Jobs<T>>::get(job_id).ok_or(Error::<T>::JobNotFound)?;
            ensure!(job.state == JobState::Accepted, Error::<T>::JobNotAccepted);
            let assigned_provider = <JobProvider<T>>::get(job_id).ok_or(Error::<T>::NotJobProvider)?;
            ensure!(assigned_provider == who, Error::<T>::NotJobProvider);

            job.state = JobState::Running;
            job.started_at = Some(frame_system::Pallet::<T>::block_number().try_into().unwrap_or(0));
            <Jobs<T>>::insert(job_id, job);
            Self::deposit_event(Event::<T>::JobStarted { job_id });
            Ok(())
        }

        #[pallet::call_index(5)]
        #[pallet::weight(20_000)]
        pub fn submit_result(origin: OriginFor<T>, job_id: u64, result_hash: [u8; 32]) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let mut job = <Jobs<T>>::get(job_id).ok_or(Error::<T>::JobNotFound)?;
            ensure!(job.state == JobState::Running, Error::<T>::JobNotRunning);
            let assigned_provider = <JobProvider<T>>::get(job_id).ok_or(Error::<T>::NotJobProvider)?;
            ensure!(assigned_provider == who, Error::<T>::NotJobProvider);

            job.state = JobState::AwaitingVerification;
            job.result_hash = Some(result_hash);
            <Jobs<T>>::insert(job_id, job);
            Self::deposit_event(Event::<T>::JobCompleted { job_id, provider: who });
            Ok(())
        }

        #[pallet::call_index(6)]
        #[pallet::weight(20_000)]
        pub fn complete_job(origin: OriginFor<T>, job_id: u64) -> DispatchResult {
            let _who = ensure_signed(origin)?;
            let mut job = <Jobs<T>>::get(job_id).ok_or(Error::<T>::JobNotFound)?;
            ensure!(job.state == JobState::AwaitingVerification || job.state == JobState::Running, Error::<T>::JobNotRunning);

            let provider = <JobProvider<T>>::get(job_id).ok_or(Error::<T>::NotJobProvider)?;
            let escrow = <JobEscrow<T>>::take(job_id).ok_or(Error::<T>::EscrowMismatch)?;
            let fee_bps = T::MarketplaceFeeBps::get();
            let fee: BalanceOf<T> = escrow * fee_bps.into() / 10_000u32.into();
            let provider_payment = escrow.saturating_sub(fee);

            job.state = JobState::Completed;
            <Jobs<T>>::insert(job_id, job);
            <ActiveJobCount<T>>::mutate(|c| *c = c.saturating_sub(1));

            // Pay the provider directly (no iteration needed)
            T::Currency::transfer(&Self::account_id(), &provider, provider_payment, ExistenceRequirement::KeepAlive)?;
            // Route the marketplace fee into the token pallet account so the
            // fee economy is self-funding: `distribute_fees` draws from the
            // `gpn/tokn` pallet account, so fees collected here flow into the
            // staking/oracle pools + treasury + burn instead of accumulating
            // in `gpn/cmkt` forever.
            if !fee.is_zero() {
                // AllowDeath: sweeping the fee may fully empty the marketplace
                // pallet account (it holds only per-job escrow and is recreated
                // on the next deposit). KeepAlive would block the sweep when
                // the remaining balance drops below the existential deposit.
                T::Currency::transfer(
                    &Self::account_id(),
                    &T::FeeSink::fee_sink(),
                    fee,
                    ExistenceRequirement::AllowDeath,
                )?;
            }
            Self::deposit_event(Event::<T>::JobCompleted { job_id, provider });

            Ok(())
        }

        #[pallet::call_index(7)]
        #[pallet::weight(10_000)]
        pub fn cancel_job(origin: OriginFor<T>, job_id: u64) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let job = <Jobs<T>>::get(job_id).ok_or(Error::<T>::JobNotFound)?;
            let requestor = <JobRequestor<T>>::get(job_id).ok_or(Error::<T>::JobNotFound)?;
            ensure!(who == requestor, Error::<T>::NotJobRequestor);
            ensure!(job.state == JobState::Pending, Error::<T>::JobNotPending);

            <Jobs<T>>::mutate(job_id, |j| { if let Some(ref mut j) = j { j.state = JobState::Cancelled; } });
            <ActiveJobCount<T>>::mutate(|c| *c = c.saturating_sub(1));

            if let Some(escrow) = <JobEscrow<T>>::take(job_id) {
                // Use AllowDeath: the escrow account may be fully emptied by a refund.
                // KeepAlive would block the transfer if the escrow drops below ED.
                T::Currency::transfer(&Self::account_id(), &who, escrow, ExistenceRequirement::AllowDeath)?;
            }

            // Remove from provider job list if accepted
            if let Some(provider) = <JobProvider<T>>::take(job_id) {
                let mut pj = <ProviderJobs<T>>::get(&provider);
                pj.retain(|&id| id != job_id);
                <ProviderJobs<T>>::insert(&provider, pj);
            }

            Self::deposit_event(Event::<T>::JobCancelled { job_id });
            Ok(())
        }
    }

    // Public helpers
    impl<T: Config> Pallet<T> {
        pub fn account_id() -> T::AccountId {
            frame_support::PalletId(*b"gpn/cmkt").into_account_truncating()
        }
        pub fn provider_active_jobs(who: &T::AccountId) -> Vec<u64> { <ProviderJobs<T>>::get(who) }
        pub fn requestor_active_jobs(who: &T::AccountId) -> Vec<u64> { <RequestorJobs<T>>::get(who) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frame_support::{
        construct_runtime, derive_impl, parameter_types,
        traits::{BuildGenesisConfig, ConstU16, ConstU32, ConstU64, ConstU128, Everything},
    };
    use sp_core::H256;
    use sp_runtime::{
        traits::{BlakeTwo256, IdentityLookup},
        BuildStorage,
    };

    use crate::pallet as local_market;
    use crate::pallet::{
        ActiveJobCount, JobEscrow, JobProvider, Jobs, NextJobId, RegisteredProviders,
    };

    type Block = frame_system::mocking::MockBlock<Test>;
    type Balance = u128;

    construct_runtime!(
        pub struct Test {
            System: frame_system,
            Balances: pallet_balances,
            Market: local_market,
        }
    );

    #[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
    impl frame_system::Config for Test {
        type BaseCallFilter = Everything;
        type Block = Block;
        type RuntimeEvent = RuntimeEvent;
        type RuntimeCall = RuntimeCall;
        type RuntimeOrigin = RuntimeOrigin;
        type RuntimeTask = RuntimeTask;
        type AccountId = u64;
        type Lookup = IdentityLookup<Self::AccountId>;
        type Nonce = u32;
        type Hash = H256;
        type Hashing = BlakeTwo256;
        type BlockHashCount = ConstU64<250>;
        type DbWeight = ();
        type Version = ();
        type PalletInfo = PalletInfo;
        type AccountData = pallet_balances::AccountData<Balance>;
        type OnNewAccount = ();
        type OnKilledAccount = ();
        type SystemWeightInfo = ();
        type SS58Prefix = ConstU16<42>;
        type OnSetCode = ();
        type MaxConsumers = ConstU32<16>;
    }

    impl pallet_balances::Config for Test {
        type Balance = Balance;
        type RuntimeEvent = RuntimeEvent;
        type DustRemoval = ();
        type ExistentialDeposit = ConstU128<1>;
        type AccountStore = System;
        type WeightInfo = ();
        type MaxLocks = ConstU32<50>;
        type MaxReserves = ConstU32<50>;
        type ReserveIdentifier = [u8; 8];
        type RuntimeHoldReason = RuntimeHoldReason;
        type RuntimeFreezeReason = RuntimeFreezeReason;
        type FreezeIdentifier = [u8; 8];
        type MaxFreezes = ConstU32<10>;
        type DoneSlashHandler = ();
    }

    parameter_types! {
        pub const TestFeeBps: u32 = 100;          // 1% marketplace fee
        pub const TestMaxJobsPerProvider: u32 = 5;
        pub const TestEscrowLock: u64 = 100;
        pub const TestMaxActiveJobs: u32 = 100;
        pub const TestMaxJobDuration: u64 = 10_000;
    }

    // Test-only mock of the gpu-attestation provider: returns a configurable
    // attested GFLOPS value (mutated per-test via the atomic) so we can exercise
    // the proof-of-compute gate without pulling in the real attestation pallet.
    // Per-thread mock value: cargo's test harness runs each test on its own
    // thread, so a `thread_local` gives deterministic per-test isolation (a
    // global atomic would race across parallel tests).
    thread_local! {
        static MOCK_GFLOPS: std::cell::Cell<u64> = const { std::cell::Cell::new(1_000) };
    }
    pub struct MockAttestation;
    impl GPUAttestationProvider<u64> for MockAttestation {
        fn attested_gflops(_: &u64) -> u64 { MOCK_GFLOPS.with(|c| c.get()) }
        fn attested_tier(_: &u64) -> pallet_gpu_attestation::AttestationTier {
            pallet_gpu_attestation::AttestationTier::Silver
        }
    }

    impl crate::pallet::Config for Test {
        type RuntimeEvent = RuntimeEvent;
        type Currency = Balances;
        type MarketplaceFeeBps = TestFeeBps;
        type MaxJobsPerProvider = TestMaxJobsPerProvider;
        type EscrowLockDuration = TestEscrowLock;
        type MaxActiveJobs = TestMaxActiveJobs;
        type MaxJobDuration = TestMaxJobDuration;
        type GPUAttestation = MockAttestation;
        // Test fee sink: a dedicated account (99) that collects marketplace fees.
        type FeeSink = MockFeeSink;
    }

    /// Test-only fee sink: routes marketplace fees to account 99 so tests can
    /// assert the fee is forwarded off the marketplace pallet account.
    pub struct MockFeeSink;
    impl pallet_gpn_token::FeeSinkProvider<u64> for MockFeeSink {
        fn fee_sink() -> u64 { 99 }
    }

    const ALICE: u64 = 1; // requestor
    const BOB: u64 = 2;   // provider
    const CAROL: u64 = 3; // third party

    fn new_test_ext() -> sp_io::TestExternalities {
        let mut t = frame_system::GenesisConfig::<Test>::default()
            .build_storage()
            .unwrap();
        pallet_balances::GenesisConfig::<Test> {
            balances: vec![(ALICE, 100_000), (BOB, 100_000), (CAROL, 100_000)],
            dev_accounts: None,
        }
        .assimilate_storage(&mut t)
        .unwrap();
        let mut ext = sp_io::TestExternalities::new(t);
        ext.execute_with(|| {
            let cfg = crate::pallet::GenesisConfig::<Test> {
                _config: Default::default(),
            };
            BuildGenesisConfig::build(&cfg);
            System::set_block_number(1);
        });
        ext
    }

    type MarketPallet = crate::pallet::Pallet::<Test>;

    fn default_reqs() -> GPURequirements {
        GPURequirements { min_vram_mb: 16_000, min_compute_capability: 80, min_gpu_count: 1, min_cuda_cores: 6_000, min_gflops: 1_000 }
    }

    fn workload() -> [u8; 32] { [9u8; 32] }

    fn register_provider(who: u64) {
        MarketPallet::register_provider(RuntimeOrigin::signed(who), default_reqs()).unwrap();
    }

    fn create_job(who: u64) -> u64 {
        MarketPallet::create_job(
            RuntimeOrigin::signed(who), default_reqs(), JobType::LLMInference,
            JobTier::Spot, 1_000, 100, workload(),
        ).unwrap();
        NextJobId::<Test>::get() - 1
    }

    #[test]
    fn test_initial_state() {
        new_test_ext().execute_with(|| {
            assert_eq!(NextJobId::<Test>::get(), 1);
            assert_eq!(ActiveJobCount::<Test>::get(), 0);
        });
    }

    #[test]
    fn test_register_provider() {
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            assert!(RegisteredProviders::<Test>::contains_key(BOB));
            assert_eq!(RegisteredProviders::<Test>::get(BOB).unwrap().min_vram_mb, 16_000);
            // Duplicate registration fails
            let dup = MarketPallet::register_provider(RuntimeOrigin::signed(BOB), default_reqs());
            assert_eq!(dup, Err(Error::<Test>::ProviderAlreadyRegistered.into()));
        });
    }

    #[test]
    fn test_unregister_provider() {
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            MarketPallet::unregister_provider(RuntimeOrigin::signed(BOB)).unwrap();
            assert!(!RegisteredProviders::<Test>::contains_key(BOB));
            // Unregistering non-provider fails
            let miss = MarketPallet::unregister_provider(RuntimeOrigin::signed(ALICE));
            assert_eq!(miss, Err(Error::<Test>::ProviderNotRegistered.into()));
        });
    }

    #[test]
    fn test_create_job_escrows_funds() {
        new_test_ext().execute_with(|| {
            let before = pallet_balances::Pallet::<Test>::free_balance(ALICE);
            let job_id = create_job(ALICE);
            assert_eq!(job_id, 1);
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(ALICE), before - 1_000);
            assert_eq!(JobEscrow::<Test>::get(job_id), Some(1_000));
            assert_eq!(Jobs::<Test>::get(job_id).unwrap().state, JobState::Pending);
            assert_eq!(ActiveJobCount::<Test>::get(), 1);
        });
    }

    #[test]
    fn test_create_job_rejects_zero_price() {
        new_test_ext().execute_with(|| {
            let result = MarketPallet::create_job(
                RuntimeOrigin::signed(ALICE), default_reqs(), JobType::LLMInference,
                JobTier::Spot, 0, 100, workload(),
            );
            assert_eq!(result, Err(Error::<Test>::InsufficientPayment.into()));
        });
    }

    #[test]
    fn test_create_job_rejects_zero_workload() {
        new_test_ext().execute_with(|| {
            let result = MarketPallet::create_job(
                RuntimeOrigin::signed(ALICE), default_reqs(), JobType::LLMInference,
                JobTier::Spot, 1_000, 100, [0u8; 32],
            );
            assert_eq!(result, Err(Error::<Test>::InvalidWorkload.into()));
        });
    }

    #[test]
    fn test_create_job_rejects_excessive_duration() {
        new_test_ext().execute_with(|| {
            let result = MarketPallet::create_job(
                RuntimeOrigin::signed(ALICE), default_reqs(), JobType::LLMInference,
                JobTier::Spot, 1_000, 100_001, workload(),
            );
            assert_eq!(result, Err(Error::<Test>::JobExpired.into()));
        });
    }

    #[test]
    fn test_accept_job_full_lifecycle() {
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            let job_id = create_job(ALICE);

            // Accept
            MarketPallet::accept_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            assert_eq!(Jobs::<Test>::get(job_id).unwrap().state, JobState::Accepted);
            assert_eq!(JobProvider::<Test>::get(job_id), Some(BOB));
            assert_eq!(MarketPallet::provider_active_jobs(&BOB), vec![job_id]);

            // Start
            MarketPallet::start_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            assert_eq!(Jobs::<Test>::get(job_id).unwrap().state, JobState::Running);

            // Submit result
            MarketPallet::submit_result(RuntimeOrigin::signed(BOB), job_id, [5u8; 32]).unwrap();
            assert_eq!(Jobs::<Test>::get(job_id).unwrap().state, JobState::AwaitingVerification);
            assert_eq!(Jobs::<Test>::get(job_id).unwrap().result_hash, Some([5u8; 32]));

            // Complete & pay
            let provider_before = pallet_balances::Pallet::<Test>::free_balance(BOB);
            MarketPallet::complete_job(RuntimeOrigin::signed(CAROL), job_id).unwrap();
            assert_eq!(Jobs::<Test>::get(job_id).unwrap().state, JobState::Completed);
            assert_eq!(ActiveJobCount::<Test>::get(), 0);
            // 1% fee taken: 1000 - 10 = 990 to provider
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(BOB), provider_before + 990);
        });
    }

    #[test]
    fn test_complete_job_routes_fee_to_fee_sink() {
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            let job_id = create_job(ALICE);
            MarketPallet::accept_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            MarketPallet::start_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            MarketPallet::submit_result(RuntimeOrigin::signed(BOB), job_id, [7u8; 32]).unwrap();

            // Fee sink (MockFeeSink -> 99) starts at 0; marketplace pallet
            // account holds the 1000 escrow.
            let sink_before = pallet_balances::Pallet::<Test>::free_balance(99);
            assert_eq!(sink_before, 0);

            MarketPallet::complete_job(RuntimeOrigin::signed(CAROL), job_id).unwrap();

            // 1% fee (10) must be forwarded to the fee sink, NOT left in gpn/cmkt.
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(99), sink_before + 10);
            // The marketplace pallet account keeps nothing after escrow + fee.
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(MarketPallet::account_id()), 0);
        });
    }

    #[test]
    fn test_accept_job_requires_registered_provider() {
        new_test_ext().execute_with(|| {
            let job_id = create_job(ALICE);
            // Carol is not a registered provider
            let result = MarketPallet::accept_job(RuntimeOrigin::signed(CAROL), job_id);
            assert_eq!(result, Err(Error::<Test>::ProviderNotRegistered.into()));
        });
    }

    #[test]
    fn test_accept_job_not_pending() {
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            let job_id = create_job(ALICE);
            MarketPallet::accept_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            // Double accept fails
            let dup = MarketPallet::accept_job(RuntimeOrigin::signed(BOB), job_id);
            assert_eq!(dup, Err(Error::<Test>::JobNotPending.into()));
        });
    }

    #[test]
    fn test_start_job_only_by_assigned_provider() {
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            register_provider(CAROL);
            let job_id = create_job(ALICE);
            MarketPallet::accept_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            // Carol (not assigned) cannot start
            let result = MarketPallet::start_job(RuntimeOrigin::signed(CAROL), job_id);
            assert_eq!(result, Err(Error::<Test>::NotJobProvider.into()));
        });
    }

    #[test]
    fn test_submit_result_requires_running() {
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            let job_id = create_job(ALICE);
            // Not running yet — submit fails
            let result = MarketPallet::submit_result(RuntimeOrigin::signed(BOB), job_id, [5u8; 32]);
            assert_eq!(result, Err(Error::<Test>::JobNotRunning.into()));
        });
    }

    #[test]
    fn test_cancel_job_refunds_escrow() {
        new_test_ext().execute_with(|| {
            let job_id = create_job(ALICE);
            let before = pallet_balances::Pallet::<Test>::free_balance(ALICE);
            MarketPallet::cancel_job(RuntimeOrigin::signed(ALICE), job_id).unwrap();
            assert_eq!(Jobs::<Test>::get(job_id).unwrap().state, JobState::Cancelled);
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(ALICE), before + 1_000);
            assert!(JobEscrow::<Test>::get(job_id).is_none());
            assert_eq!(ActiveJobCount::<Test>::get(), 0);
        });
    }

    #[test]
    fn test_cancel_job_only_by_requestor() {
        new_test_ext().execute_with(|| {
            let job_id = create_job(ALICE);
            let result = MarketPallet::cancel_job(RuntimeOrigin::signed(CAROL), job_id);
            assert_eq!(result, Err(Error::<Test>::NotJobRequestor.into()));
        });
    }

    #[test]
    fn test_max_jobs_per_provider() {
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            // Create 5 jobs (max = 5)
            for _ in 0..5 { create_job(ALICE); }
            let job6 = create_job(ALICE);
            for i in 1..6u64 {
                MarketPallet::accept_job(RuntimeOrigin::signed(BOB), i).unwrap();
            }
            let result = MarketPallet::accept_job(RuntimeOrigin::signed(BOB), job6);
            assert_eq!(result, Err(Error::<Test>::MaxProviderCapacity.into()));
        });
    }

    #[test]
    fn test_accept_job_rejects_provider_below_min_gflops() {
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            let job_id = create_job(ALICE); // requires min_gflops = 1000
            // Attested throughput only 500 GFLOPS < 1000 required.
            MOCK_GFLOPS.with(|c| c.set(500));
            let result = MarketPallet::accept_job(RuntimeOrigin::signed(BOB), job_id);
            assert_eq!(result, Err(Error::<Test>::ProviderComputeInsufficient.into()));
            assert!(!JobProvider::<Test>::contains_key(job_id));
            assert_eq!(Jobs::<Test>::get(job_id).unwrap().state, JobState::Pending);
        });
    }

    #[test]
    fn test_accept_job_rejects_unattested_provider() {
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            let job_id = create_job(ALICE); // requires min_gflops = 1000
            // 0 GFLOPS = never attested; cannot serve a paid job with a compute gate.
            MOCK_GFLOPS.with(|c| c.set(0));
            let result = MarketPallet::accept_job(RuntimeOrigin::signed(BOB), job_id);
            assert_eq!(result, Err(Error::<Test>::ProviderComputeInsufficient.into()));
        });
    }

    #[test]
    fn test_accept_job_passes_when_attested_gflops_meets_requirement() {
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            let job_id = create_job(ALICE); // requires min_gflops = 1000
            MOCK_GFLOPS.with(|c| c.set(1_000)); // exactly meets it
            MarketPallet::accept_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            assert_eq!(JobProvider::<Test>::get(job_id), Some(BOB));
            assert_eq!(Jobs::<Test>::get(job_id).unwrap().state, JobState::Accepted);
        });
    }

    #[test]
    fn test_accept_job_ignores_gate_when_min_gflops_is_zero() {
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            // Job with no compute requirement (min_gflops = 0) is accepted even
            // by an un-attested provider (0 GFLOPS).
            let mut reqs = default_reqs();
            reqs.min_gflops = 0;
            MarketPallet::create_job(
                RuntimeOrigin::signed(ALICE), reqs, JobType::LLMInference,
                JobTier::Spot, 1_000, 100, workload(),
            ).unwrap();
            let job_id = NextJobId::<Test>::get() - 1;
            MOCK_GFLOPS.with(|c| c.set(0));
            MarketPallet::accept_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            assert_eq!(JobProvider::<Test>::get(job_id), Some(BOB));
        });
    }
}
