//! # Compute Marketplace Pallet
//!
//! Decentralized GPU compute marketplace where providers offer GPU time
//! and requestors submit AI inference/training jobs.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "runtime-benchmarks")]
pub mod benchmarking;
pub mod weights;

use codec::{Decode, DecodeWithMemTracking, Encode, MaxEncodedLen};
use core::fmt::Debug;
use pallet_gpu_attestation::GPUAttestationProvider;
use scale_info::TypeInfo;

pub use pallet::*;
pub use weights::WeightInfo;

/// GPU capability requirements
#[derive(
    Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo,
)]
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
        Self {
            min_vram_mb: 8_000,
            min_compute_capability: 75,
            min_gpu_count: 1,
            min_cuda_cores: 0,
            min_gflops: 0,
        }
    }
}

#[derive(
    Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo,
)]
pub enum JobType {
    LLMInference,
    FineTuning,
    ImageGeneration,
    MLInference,
    Custom,
    VideoGeneration,
}
impl Default for JobType {
    fn default() -> Self {
        Self::LLMInference
    }
}

#[derive(
    Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo,
)]
pub enum JobTier {
    Premium,
    Spot,
    Reserved,
}
impl Default for JobTier {
    fn default() -> Self {
        Self::Spot
    }
}

#[derive(
    Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo,
)]
pub enum JobState {
    Pending,
    Accepted,
    Running,
    AwaitingVerification,
    Completed,
    Failed,
    Cancelled,
}
impl Default for JobState {
    fn default() -> Self {
        Self::Pending
    }
}

#[derive(
    Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo,
)]
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

/// Lifecycle of an off-chain escrow channel (`design-offchain-escrow-channels.md`).
#[derive(
    Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo,
)]
pub enum ChannelState {
    /// Funds locked on-chain; per-job payments happen off-chain via signed
    /// balance updates. No per-job tx.
    Open,
    /// Settled on-chain; escrow released to the two parties.
    Closed,
}
impl Default for ChannelState {
    fn default() -> Self {
        Self::Open
    }
}

/// A payment channel between a requestor and a provider. The on-chain record
/// only tracks the **latest settled** balances + nonce; the per-job money
/// movement stays off-chain as two signatures. Open/close are the only on-chain
/// txs, so an AI job no longer waits a block for escrow/settlement.
#[derive(
    Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo,
)]
pub struct Channel<AccountId> {
    pub id: u64,
    pub requestor: AccountId,
    pub provider: AccountId,
    /// Latest settled balances (must sum to the channel deposit).
    pub balance_requestor: u128,
    pub balance_provider: u128,
    /// Monotonic settlement counter — a later state MUST carry a strictly
    /// greater `nonce`, so an old (stale) state can never be replayed.
    pub nonce: u64,
    pub opened_at: u32,
    /// Block after which either party may force-close using the counterparty's
    /// signature — the liveness escape hatch when the peer goes dark.
    pub deadline: u32,
    pub state: ChannelState,
}

/// Domain tag for the payload a channel party signs to authorize a settlement.
pub const CHANNEL_SETTLE_DOMAIN: &[u8] = b"gpn/channel-settle-v1";

/// Verifies that `signature` is a valid authorization by `who` over a channel
/// settlement payload. Injected (like `GPUAttestation`/`FeeSink`) so the pallet
/// stays account-type agnostic and unit-testable.
pub trait ChannelSignatureVerifier<AccountId> {
    fn verify(who: &AccountId, payload: &[u8; 32], signature: &[u8; 64]) -> bool;
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
        #[pallet::constant]
        type MarketplaceFeeBps: Get<u32>;
        #[pallet::constant]
        type MaxJobsPerProvider: Get<u32>;
        #[pallet::constant]
        type EscrowLockDuration: Get<BlockNumberFor<Self>>;
        #[pallet::constant]
        type MaxActiveJobs: Get<u32>;
        /// Per-requestor cap on concurrent non-terminal jobs. Bounds a single
        /// account from draining the global `MaxActiveJobs` pool (self-inflicted
        /// DoS, incident 2026-10-05) — one requestor can no longer monopolize
        /// every slot. `MaxActiveJobs` stays the network-wide ceiling.
        #[pallet::constant]
        type MaxActiveJobsPerRequestor: Get<u32>;
        #[pallet::constant]
        type MaxJobDuration: Get<BlockNumberFor<Self>>;
        /// Upper bound on how many job ids the `on_initialize` expiry sweep may
        /// examine per block. Keeps the hook weight bounded (O(1) per block)
        /// while still guaranteeing every job id is visited over time (the
        /// cursor wraps around `NextJobId`). 0 disables the sweep.
        #[pallet::constant]
        type MaxExpirySweepPerBlock: Get<u32>;
        /// Read-only access to providers' attested GPU compute credentials.
        /// Used to enforce that a provider accepting a job actually meets the
        /// job's `min_gflops` requirement (proof-of-compute gate at match time).
        type GPUAttestation: GPUAttestationProvider<Self::AccountId>;
        /// Where marketplace fees are routed on completion. Wired to the GPN
        /// token pallet (`gpn/tokn`) so collected fees feed `distribute_fees`
        /// (staking/oracle pools + treasury + burn) — self-funding fee economy.
        type FeeSink: pallet_gpn_token::FeeSinkProvider<Self::AccountId>;
        /// Weight information for extrinsics in this pallet (workplan item 9).
        type WeightInfo: WeightInfo;
        /// Off-chain escrow channels (`design-offchain-escrow-channels.md`):
        /// verifies a party's authorization signature over a channel-settlement
        /// state so the chain stays the root of trust for open/close/settle.
        type ChannelAuth: ChannelSignatureVerifier<Self::AccountId>;
    }

    #[pallet::pallet]
    #[pallet::without_storage_info]
    pub struct Pallet<T>(_);

    // Storage
    #[pallet::storage]
    pub type NextJobId<T: Config> = StorageValue<_, u64, ValueQuery>;
    #[pallet::storage]
    pub type Jobs<T: Config> = StorageMap<_, Blake2_128Concat, u64, ComputeJob, OptionQuery>;
    #[pallet::storage]
    pub type JobRequestor<T: Config> =
        StorageMap<_, Blake2_128Concat, u64, T::AccountId, OptionQuery>;
    #[pallet::storage]
    pub type JobProvider<T: Config> =
        StorageMap<_, Blake2_128Concat, u64, T::AccountId, OptionQuery>;
    #[pallet::storage]
    pub type ProviderJobs<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, Vec<u64>, ValueQuery>;
    #[pallet::storage]
    pub type RequestorJobs<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, Vec<u64>, ValueQuery>;
    #[pallet::storage]
    pub type JobEscrow<T: Config> = StorageMap<_, Blake2_128Concat, u64, BalanceOf<T>, OptionQuery>;

    /// Per-token billing for a job: `(price_per_token, max_tokens, actual_tokens)`.
    /// Kept in its own map (added post-genesis) so `ComputeJob`'s storage layout is
    /// unchanged and no migration is required.
    #[pallet::storage]
    pub type JobBilling<T: Config> =
        StorageMap<_, Blake2_128Concat, u64, (u128, u32, u32), ValueQuery>;
    #[pallet::storage]
    pub type RegisteredProviders<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, GPURequirements, OptionQuery>;
    /// Provider-advertised reachable endpoint (e.g. its `/status` heartbeat URL), set on-chain so
    /// access points / the scheduler can DISCOVER providers from the chain instead of a static list.
    /// Capped at 256 bytes by `set_provider_endpoint`.
    #[pallet::storage]
    pub type ProviderEndpoints<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, Vec<u8>, OptionQuery>;
    #[pallet::storage]
    pub type ActiveJobCount<T: Config> = StorageValue<_, u32, ValueQuery>;

    /// Cursor for the on-chain expiry sweep: the next job id `on_initialize`
    /// should examine. Advances by `MaxExpirySweepPerBlock` each block and wraps
    /// at `NextJobId`, so the sweep is bounded per block yet eventually visits
    /// every job id (removing reliance on someone calling `expire_job`).
    #[pallet::storage]
    pub type JobSweepCursor<T: Config> = StorageValue<_, u64, ValueQuery>;

    // ── Off-chain escrow channels (design-offchain-escrow-channels.md) ────────
    #[pallet::storage]
    pub type NextChannelId<T: Config> = StorageValue<_, u64, ValueQuery>;
    #[pallet::storage]
    pub type Channels<T: Config> =
        StorageMap<_, Blake2_128Concat, u64, Channel<T::AccountId>, OptionQuery>;
    #[pallet::storage]
    pub type ChannelEscrow<T: Config> =
        StorageMap<_, Blake2_128Concat, u64, BalanceOf<T>, OptionQuery>;
    /// (requestor, provider) → channel id, to enforce one live channel per pair.
    #[pallet::storage]
    pub type ChannelIndex<T: Config> =
        StorageMap<_, Blake2_128Concat, (T::AccountId, T::AccountId), u64, OptionQuery>;

    // Genesis
    #[pallet::genesis_config]
    #[derive(frame_support::DefaultNoBound)]
    pub struct GenesisConfig<T: Config> {
        #[serde(skip)]
        pub _config: sp_std::marker::PhantomData<T>,
    }
    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            <NextJobId<T>>::put(1u64);
            <NextChannelId<T>>::put(1u64);
            <ActiveJobCount<T>>::put(0u32);
        }
    }

    // Events
    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        JobCreated {
            job_id: u64,
            requestor: T::AccountId,
            tier: JobTier,
        },
        JobAccepted {
            job_id: u64,
            provider: T::AccountId,
        },
        JobStarted {
            job_id: u64,
        },
        JobCompleted {
            job_id: u64,
            provider: T::AccountId,
        },
        JobFailed {
            job_id: u64,
            reason: u8,
        },
        JobCancelled {
            job_id: u64,
        },
        /// A non-terminal job passed its `created_at + max_duration` deadline and
        /// was reclaimed permissionlessly; escrow refunded to the requestor.
        JobExpired {
            job_id: u64,
        },
        ProviderRegistered {
            who: T::AccountId,
        },
        ProviderUnregistered {
            who: T::AccountId,
        },
        ProviderEndpointSet {
            who: T::AccountId,
            endpoint: Vec<u8>,
        },
        /// An off-chain escrow channel was opened; `amount` locked on-chain.
        ChannelOpened {
            channel_id: u64,
            requestor: T::AccountId,
            provider: T::AccountId,
            amount: u128,
        },
        /// A channel was settled on-chain (cooperative or forced); `nonce` is the
        /// settlement counter of the state that was accepted.
        ChannelSettled {
            channel_id: u64,
            balance_requestor: u128,
            balance_provider: u128,
            nonce: u64,
        },
    }

    // Errors
    #[pallet::error]
    pub enum Error<T> {
        TooManyActiveJobs,
        /// The requestor already holds `MaxActiveJobsPerRequestor` non-terminal
        /// jobs. Cancelling/completing/failing/expiring one frees a slot.
        TooManyRequestorJobs,
        MaxProviderCapacity,
        JobNotFound,
        JobNotPending,
        JobNotAccepted,
        JobNotRunning,
        AlreadyAccepted,
        NotJobProvider,
        NotJobRequestor,
        InsufficientPayment,
        InvalidWorkload,
        JobExpired,
        EscrowMismatch,
        ProviderNotRegistered,
        ProviderAlreadyRegistered,
        /// Provider-declared endpoint exceeds the 256-byte cap.
        EndpointTooLong,
        /// The provider's attested compute throughput (GFLOPS) is below the
        /// job's `min_gflops` requirement — it cannot honestly serve this job.
        ProviderComputeInsufficient,
        /// `expire_job` was called before `created_at + max_duration` elapsed.
        JobNotExpired,
        /// A channel already exists (open) for this (requestor, provider) pair.
        ChannelAlreadyOpen,
        ChannelNotFound,
        /// The channel is not in the `Open` state.
        ChannelNotOpen,
        /// `balance_requestor + balance_provider` must equal the channel deposit.
        ChannelBalanceMismatch,
        /// A settlement must carry a strictly greater nonce than the last one.
        ChannelNonceTooLow,
        /// The counterparty's authorization signature did not verify.
        InvalidChannelSignature,
        /// `force_close` was attempted before the channel `deadline`.
        ChannelNotExpired,
    }

    // Hooks
    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
        /// On-chain expiry sweep. Every block, examine up to
        /// `MaxExpirySweepPerBlock` job ids from a wrapping cursor and
        /// auto-expire any non-terminal job past its `created_at + max_duration`
        /// deadline (same state transition as the permissionless `expire_job`:
        /// mark `Failed`, refund escrow, free slots, emit `JobExpired`).
        ///
        /// This removes the marketplace's reliance on an external caller (the
        /// AP janitor) to reclaim orphaned jobs: a requestor that goes away or a
        /// provider that dies can no longer hold a global `ActiveJobCount` slot
        /// (or a per-requestor slot) forever. The work is bounded O(1) per block.
        fn on_initialize(now: BlockNumberFor<T>) -> Weight {
            Self::expiry_sweep(now)
        }

        /// One-time capacity cleanup. Before the `complete_job` fix, finished jobs
        /// were never removed from `ProviderJobs`/`RequestorJobs`, so every
        /// completed job permanently consumed a `MaxJobsPerProvider` slot and
        /// eventually wedged busy providers. Prune any terminal
        /// (Completed/Failed/Cancelled) job still sitting in a capacity list.
        /// Idempotent — a re-run finds nothing to prune.
        fn on_runtime_upgrade() -> Weight {
            let mut reads: u64 = 0;
            let mut writes: u64 = 0;

            let provider_rows: Vec<(T::AccountId, Vec<u64>)> = <ProviderJobs<T>>::iter().collect();
            for (who, jobs) in provider_rows {
                reads += 1;
                let mut kept = Vec::with_capacity(jobs.len());
                for id in jobs {
                    reads += 1;
                    let terminal = matches!(
                        <Jobs<T>>::get(id).map(|j| j.state),
                        Some(JobState::Completed)
                            | Some(JobState::Failed)
                            | Some(JobState::Cancelled)
                    );
                    if !terminal {
                        kept.push(id);
                    }
                }
                if kept.len() != <ProviderJobs<T>>::get(&who).len() {
                    writes += 1;
                    <ProviderJobs<T>>::insert(&who, kept);
                }
            }

            let requestor_rows: Vec<(T::AccountId, Vec<u64>)> =
                <RequestorJobs<T>>::iter().collect();
            for (who, jobs) in requestor_rows {
                reads += 1;
                let mut kept = Vec::with_capacity(jobs.len());
                for id in jobs {
                    reads += 1;
                    let terminal = matches!(
                        <Jobs<T>>::get(id).map(|j| j.state),
                        Some(JobState::Completed)
                            | Some(JobState::Failed)
                            | Some(JobState::Cancelled)
                    );
                    if !terminal {
                        kept.push(id);
                    }
                }
                if kept.len() != <RequestorJobs<T>>::get(&who).len() {
                    writes += 1;
                    <RequestorJobs<T>>::insert(&who, kept);
                }
            }

            log::info!("gpn-marketplace: capacity migration pruned terminal jobs from provider/requestor lists");
            T::DbWeight::get().reads_writes(reads, writes)
        }
    }

    // Dispatchable calls
    #[pallet::call]
    impl<T: Config> Pallet<T> {
        #[pallet::call_index(0)]
        #[pallet::weight(<T as Config>::WeightInfo::register_provider())]
        pub fn register_provider(
            origin: OriginFor<T>,
            requirements: GPURequirements,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(
                !<RegisteredProviders<T>>::contains_key(&who),
                Error::<T>::ProviderAlreadyRegistered
            );
            <RegisteredProviders<T>>::insert(&who, requirements);
            Self::deposit_event(Event::<T>::ProviderRegistered { who });
            Ok(())
        }

        #[pallet::call_index(1)]
        #[pallet::weight(<T as Config>::WeightInfo::unregister_provider())]
        pub fn unregister_provider(origin: OriginFor<T>) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(
                <RegisteredProviders<T>>::contains_key(&who),
                Error::<T>::ProviderNotRegistered
            );
            <RegisteredProviders<T>>::remove(&who);
            Self::deposit_event(Event::<T>::ProviderUnregistered { who });
            Ok(())
        }

        /// Advertise (or update) the provider's reachable endpoint on-chain.
        ///
        /// Caller must be a registered provider. The endpoint (e.g. the provider's
        /// `/status` heartbeat URL) is read by access points / the scheduler so they can
        /// DISCOVER providers from the chain rather than a hand-configured list. Capped at
        /// 256 bytes.
        #[pallet::call_index(10)]
        #[pallet::weight(<T as Config>::WeightInfo::register_provider())]
        pub fn set_provider_endpoint(origin: OriginFor<T>, endpoint: Vec<u8>) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(
                <RegisteredProviders<T>>::contains_key(&who),
                Error::<T>::ProviderNotRegistered
            );
            ensure!(endpoint.len() <= 256, Error::<T>::EndpointTooLong);
            <ProviderEndpoints<T>>::insert(&who, endpoint.clone());
            Self::deposit_event(Event::<T>::ProviderEndpointSet { who, endpoint });
            Ok(())
        }

        /// Open a `(requestor, provider)` payment channel, locking `amount` in the
        /// marketplace escrow account. The chain only sees this open + the later
        /// close — per-job money movement stays off-chain as signed balance
        /// updates, so an AI job no longer waits a block for escrow/settlement
        /// (`design-offchain-escrow-channels.md`).
        #[pallet::call_index(11)]
        #[pallet::weight(<T as Config>::WeightInfo::create_job())]
        pub fn open_channel(
            origin: OriginFor<T>,
            provider: T::AccountId,
            amount: BalanceOf<T>,
            ttl: BlockNumberFor<T>,
        ) -> DispatchResult {
            let requestor = ensure_signed(origin)?;
            ensure!(
                <RegisteredProviders<T>>::contains_key(&provider),
                Error::<T>::ProviderNotRegistered
            );
            ensure!(requestor != provider, Error::<T>::InvalidWorkload);
            ensure!(amount > Zero::zero(), Error::<T>::InsufficientPayment);
            ensure!(ttl > Zero::zero(), Error::<T>::ChannelNotExpired);
            ensure!(
                !<ChannelIndex<T>>::contains_key((requestor.clone(), provider.clone())),
                Error::<T>::ChannelAlreadyOpen
            );

            let channel_id = <NextChannelId<T>>::get();
            <NextChannelId<T>>::mutate(|id| *id += 1);

            let now_raw = frame_system::Pallet::<T>::block_number();
            let now: u32 = now_raw.try_into().unwrap_or(0);
            let deadline: u32 = now_raw
                .saturating_add(ttl)
                .try_into()
                .unwrap_or(u32::MAX);
            let deposit: u128 = amount.try_into().unwrap_or(u128::MAX);

            T::Currency::transfer(
                &requestor,
                &Self::account_id(),
                amount,
                ExistenceRequirement::KeepAlive,
            )?;

            <Channels<T>>::insert(
                channel_id,
                Channel {
                    id: channel_id,
                    requestor: requestor.clone(),
                    provider: provider.clone(),
                    balance_requestor: deposit,
                    balance_provider: 0,
                    nonce: 0,
                    opened_at: now,
                    deadline,
                    state: ChannelState::Open,
                },
            );
            <ChannelEscrow<T>>::insert(channel_id, amount);
            <ChannelIndex<T>>::insert((requestor.clone(), provider.clone()), channel_id);

            Self::deposit_event(Event::<T>::ChannelOpened {
                channel_id,
                requestor,
                provider,
                amount: deposit,
            });
            Ok(())
        }

        /// Cooperative close: settle the channel to the latest off-chain state,
        /// authorized by BOTH parties' signatures. Releases escrow instantly.
        #[pallet::call_index(12)]
        #[pallet::weight(<T as Config>::WeightInfo::complete_job())]
        pub fn close_channel(
            origin: OriginFor<T>,
            channel_id: u64,
            balance_requestor: u128,
            balance_provider: u128,
            nonce: u64,
            sig_requestor: [u8; 64],
            sig_provider: [u8; 64],
        ) -> DispatchResult {
            let _who = ensure_signed(origin)?;
            let channel = <Channels<T>>::get(channel_id).ok_or(Error::<T>::ChannelNotFound)?;
            ensure!(
                channel.state == ChannelState::Open,
                Error::<T>::ChannelNotOpen
            );

            let payload = Self::channel_settle_payload(
                channel_id,
                balance_requestor,
                balance_provider,
                nonce,
            );
            ensure!(
                T::ChannelAuth::verify(&channel.requestor, &payload, &sig_requestor)
                    && T::ChannelAuth::verify(&channel.provider, &payload, &sig_provider),
                Error::<T>::InvalidChannelSignature
            );

            Self::settle_channel(
                channel_id,
                channel,
                balance_requestor,
                balance_provider,
                nonce,
            )
        }

        /// Force-close after `deadline` when the peer is unresponsive: the caller
        /// supplies the **counterparty's** signature over the final state (the
        /// caller's own authorization is implicit). Nonce monotonicity prevents
        /// replaying a stale (self-favoring) state.
        #[pallet::call_index(13)]
        #[pallet::weight(<T as Config>::WeightInfo::complete_job())]
        pub fn force_close_channel(
            origin: OriginFor<T>,
            channel_id: u64,
            balance_requestor: u128,
            balance_provider: u128,
            nonce: u64,
            sig_counterparty: [u8; 64],
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let channel = <Channels<T>>::get(channel_id).ok_or(Error::<T>::ChannelNotFound)?;
            ensure!(
                channel.state == ChannelState::Open,
                Error::<T>::ChannelNotOpen
            );
            ensure!(
                who == channel.requestor || who == channel.provider,
                Error::<T>::NotJobProvider
            );

            let now: u32 = frame_system::Pallet::<T>::block_number()
                .try_into()
                .unwrap_or(0);
            ensure!(now >= channel.deadline, Error::<T>::ChannelNotExpired);

            let counterparty = if who == channel.requestor {
                channel.provider.clone()
            } else {
                channel.requestor.clone()
            };
            let payload = Self::channel_settle_payload(
                channel_id,
                balance_requestor,
                balance_provider,
                nonce,
            );
            ensure!(
                T::ChannelAuth::verify(&counterparty, &payload, &sig_counterparty),
                Error::<T>::InvalidChannelSignature
            );

            Self::settle_channel(
                channel_id,
                channel,
                balance_requestor,
                balance_provider,
                nonce,
            )
        }

        #[pallet::call_index(2)]
        #[pallet::weight(<T as Config>::WeightInfo::create_job())]
        pub fn create_job(
            origin: OriginFor<T>,
            gpu_requirements: GPURequirements,
            job_type: JobType,
            tier: JobTier,
            price: BalanceOf<T>,
            price_per_token: u128,
            max_tokens: u32,
            max_duration: BlockNumberFor<T>,
            workload_hash: [u8; 32],
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let requestor = who.clone();

            ensure!(
                <ActiveJobCount<T>>::get() < T::MaxActiveJobs::get(),
                Error::<T>::TooManyActiveJobs
            );
            // Per-requestor cap: `RequestorJobs` holds exactly this account's
            // non-terminal jobs (freed on complete/fail/expire/cancel), so its
            // length is the faithful per-requestor active count.
            ensure!(
                (<RequestorJobs<T>>::get(&requestor).len() as u32)
                    < T::MaxActiveJobsPerRequestor::get(),
                Error::<T>::TooManyRequestorJobs
            );
            ensure!(
                !workload_hash.iter().all(|&b| b == 0),
                Error::<T>::InvalidWorkload
            );
            ensure!(
                max_duration <= T::MaxJobDuration::get(),
                Error::<T>::JobExpired
            );
            ensure!(price > Zero::zero(), Error::<T>::InsufficientPayment);

            let job_id = <NextJobId<T>>::get();
            <NextJobId<T>>::mutate(|id| *id += 1);

            let now: u32 = frame_system::Pallet::<T>::block_number()
                .try_into()
                .unwrap_or(0);
            let max_dur: u32 = max_duration.try_into().unwrap_or(100_000);

            let job_tier = tier.clone();
            let job = ComputeJob {
                id: job_id,
                gpu_requirements,
                job_type,
                tier: job_tier,
                state: JobState::Pending,
                price: price.try_into().unwrap_or(0),
                max_duration: max_dur,
                created_at: now,
                started_at: None,
                workload_hash,
                result_hash: None,
            };

            T::Currency::transfer(
                &requestor,
                &Self::account_id(),
                price,
                ExistenceRequirement::KeepAlive,
            )?;

            <Jobs<T>>::insert(job_id, job);
            // Per-token billing in a SEPARATE map so `ComputeJob`'s on-chain layout
            // is unchanged (no storage migration needed). (rate, cap, used)
            <JobBilling<T>>::insert(job_id, (price_per_token, max_tokens, 0u32));
            <JobRequestor<T>>::insert(job_id, who.clone());
            <JobEscrow<T>>::insert(job_id, price);

            let mut jobs = <RequestorJobs<T>>::get(&requestor);
            jobs.push(job_id);
            <RequestorJobs<T>>::insert(&requestor, jobs);
            <ActiveJobCount<T>>::mutate(|c| *c += 1);

            Self::deposit_event(Event::<T>::JobCreated {
                job_id,
                requestor: who,
                tier,
            });
            Ok(())
        }

        #[pallet::call_index(3)]
        #[pallet::weight(<T as Config>::WeightInfo::accept_job())]
        pub fn accept_job(origin: OriginFor<T>, job_id: u64) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let provider = who.clone();
            ensure!(
                <RegisteredProviders<T>>::contains_key(&provider),
                Error::<T>::ProviderNotRegistered
            );
            let mut job = <Jobs<T>>::get(job_id).ok_or(Error::<T>::JobNotFound)?;
            ensure!(job.state == JobState::Pending, Error::<T>::JobNotPending);
            ensure!(
                !<JobProvider<T>>::contains_key(job_id),
                Error::<T>::AlreadyAccepted
            );
            ensure!(
                (<ProviderJobs<T>>::get(&provider).len() as u32) < T::MaxJobsPerProvider::get(),
                Error::<T>::MaxProviderCapacity
            );

            // Proof-of-compute gate: if the job demands a minimum GFLOPS rate,
            // the provider must have *attested* to at least that throughput
            // (cross-pallet read of `gpu-attestation::AttestationRecord`). A
            // provider that hasn't attested (0 GFLOPS) can never meet a nonzero
            // requirement, so un-attested providers are blocked from paid jobs.
            let required_gflops = job.gpu_requirements.min_gflops;
            if required_gflops > 0 {
                let attested = T::GPUAttestation::attested_gflops(&provider);
                ensure!(
                    attested >= required_gflops,
                    Error::<T>::ProviderComputeInsufficient
                );
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
        #[pallet::weight(<T as Config>::WeightInfo::start_job())]
        pub fn start_job(origin: OriginFor<T>, job_id: u64) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let mut job = <Jobs<T>>::get(job_id).ok_or(Error::<T>::JobNotFound)?;
            ensure!(job.state == JobState::Accepted, Error::<T>::JobNotAccepted);
            let assigned_provider =
                <JobProvider<T>>::get(job_id).ok_or(Error::<T>::NotJobProvider)?;
            ensure!(assigned_provider == who, Error::<T>::NotJobProvider);

            job.state = JobState::Running;
            job.started_at = Some(
                frame_system::Pallet::<T>::block_number()
                    .try_into()
                    .unwrap_or(0),
            );
            <Jobs<T>>::insert(job_id, job);
            Self::deposit_event(Event::<T>::JobStarted { job_id });
            Ok(())
        }

        #[pallet::call_index(5)]
        #[pallet::weight(<T as Config>::WeightInfo::submit_result())]
        pub fn submit_result(
            origin: OriginFor<T>,
            job_id: u64,
            result_hash: [u8; 32],
            actual_tokens: u32,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let mut job = <Jobs<T>>::get(job_id).ok_or(Error::<T>::JobNotFound)?;
            ensure!(job.state == JobState::Running, Error::<T>::JobNotRunning);
            let assigned_provider =
                <JobProvider<T>>::get(job_id).ok_or(Error::<T>::NotJobProvider)?;
            ensure!(assigned_provider == who, Error::<T>::NotJobProvider);

            job.state = JobState::AwaitingVerification;
            job.result_hash = Some(result_hash);
            <Jobs<T>>::insert(job_id, job);
            let mut billing = <JobBilling<T>>::get(job_id);
            billing.2 = actual_tokens;
            <JobBilling<T>>::insert(job_id, billing);
            Self::deposit_event(Event::<T>::JobCompleted {
                job_id,
                provider: who,
            });
            Ok(())
        }

        #[pallet::call_index(6)]
        #[pallet::weight(<T as Config>::WeightInfo::complete_job())]
        pub fn complete_job(origin: OriginFor<T>, job_id: u64) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let mut job = <Jobs<T>>::get(job_id).ok_or(Error::<T>::JobNotFound)?;
            ensure!(
                job.state == JobState::AwaitingVerification || job.state == JobState::Running,
                Error::<T>::JobNotRunning
            );

            let provider = <JobProvider<T>>::get(job_id).ok_or(Error::<T>::NotJobProvider)?;
            let requestor = <JobRequestor<T>>::get(job_id).ok_or(Error::<T>::NotJobProvider)?;
            // Access control (red-team finding 2026-10-08): `complete_job` used to be
            // callable by *any* signed account. Because it is allowed from the
            // `Running` state (before a result is submitted), a third party — or the
            // requestor itself — could finalize an in-flight job early: with per-token
            // billing `actual_tokens` is still 0, so `charge == 0`, the whole escrow is
            // refunded to the requestor and the provider is paid nothing for work it is
            // still doing (theft-of-service / griefing). Only the job's own requestor or
            // assigned provider may complete it (mirrors `fail_job`).
            ensure!(
                who == requestor || who == provider,
                Error::<T>::NotJobProvider
            );
            let escrow = <JobEscrow<T>>::take(job_id).ok_or(Error::<T>::EscrowMismatch)?;
            // ── Per-token billing ──────────────────────────────────────────
            // When the job carries a rate + cap, the real charge is
            // `price_per_token × actual_tokens` (bounded by the escrow); the unused
            // remainder is refunded to the requestor. No rate => legacy flat price.
            let charge: BalanceOf<T> = {
                let (price_per_token, max_tokens, actual_tokens) = <JobBilling<T>>::get(job_id);
                if price_per_token > 0 && max_tokens > 0 {
                    let toks: u128 = (actual_tokens.min(max_tokens)) as u128;
                    let cap: u128 = escrow.try_into().unwrap_or(u128::MAX);
                    price_per_token
                        .saturating_mul(toks)
                        .min(cap)
                        .try_into()
                        .unwrap_or(escrow)
                } else {
                    escrow
                }
            };
            let ed = T::Currency::minimum_balance();
            let mut refund: BalanceOf<T> = escrow.saturating_sub(charge);
            let fee_bps = T::MarketplaceFeeBps::get();
            let mut fee: BalanceOf<T> = charge * fee_bps.into() / 10_000u32.into();
            let mut provider_payment = charge.saturating_sub(fee);

            // The marketplace pallet account is a pass-through escrow holder and
            // must never be left with a *dangling* balance below ED. If the payout
            // or fee is dust, fold it into the payout (no fee on dust jobs).
            if provider_payment < ed || fee < ed {
                provider_payment = charge;
                fee = Zero::zero();
            }
            // Fold a sub-ED refund into the payout too, so the pallet ends at 0.
            if refund > Zero::zero() && refund < ed {
                provider_payment = provider_payment.saturating_add(refund);
                refund = Zero::zero();
            }

            job.state = JobState::Completed;
            <Jobs<T>>::insert(job_id, job);
            <ActiveJobCount<T>>::mutate(|c| *c = c.saturating_sub(1));

            // Free the provider's capacity slot. Previously `complete_job` only set
            // the state, so every finished job kept consuming one of the provider's
            // `MaxJobsPerProvider` slots forever — a slow capacity leak that wedged
            // busy providers. `cancel_job` already did this; mirror it here.
            if let Some(provider) = <JobProvider<T>>::take(job_id) {
                let mut pj = <ProviderJobs<T>>::get(&provider);
                pj.retain(|&id| id != job_id);
                <ProviderJobs<T>>::insert(&provider, pj);
            }
            // Likewise free the requestor's slot (same leak on the requester side).
            if let Some(requestor) = <JobRequestor<T>>::get(job_id) {
                let mut rj = <RequestorJobs<T>>::get(&requestor);
                rj.retain(|&id| id != job_id);
                <RequestorJobs<T>>::insert(&requestor, rj);
            }

            // Route the marketplace fee into the token pallet account first so the
            // fee economy is self-funding: `distribute_fees` draws from the `gpn/tokn`
            // pallet account. AllowDeath: sweeping the fee may take the marketplace
            // account below ED (it holds only per-job escrow and is recreated on the
            // next deposit), but only down to the provider payout, which is >= ED.
            if !fee.is_zero() {
                T::Currency::transfer(
                    &Self::account_id(),
                    &T::FeeSink::fee_sink(),
                    fee,
                    ExistenceRequirement::AllowDeath,
                )?;
            }
            // Refund any unused escrow to the requestor (fee already out, so the
            // pallet keeps exactly `provider_payment`).
            if !refund.is_zero() {
                T::Currency::transfer(
                    &Self::account_id(),
                    &requestor,
                    refund,
                    ExistenceRequirement::AllowDeath,
                )?;
            }
            // Pay the provider last, sweeping the marketplace account to exactly 0.
            T::Currency::transfer(
                &Self::account_id(),
                &provider,
                provider_payment,
                ExistenceRequirement::AllowDeath,
            )?;
            Self::deposit_event(Event::<T>::JobCompleted { job_id, provider });

            Ok(())
        }

        #[pallet::call_index(7)]
        #[pallet::weight(<T as Config>::WeightInfo::cancel_job())]
        pub fn cancel_job(origin: OriginFor<T>, job_id: u64) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let job = <Jobs<T>>::get(job_id).ok_or(Error::<T>::JobNotFound)?;
            let requestor = <JobRequestor<T>>::get(job_id).ok_or(Error::<T>::JobNotFound)?;
            ensure!(who == requestor, Error::<T>::NotJobRequestor);
            ensure!(job.state == JobState::Pending, Error::<T>::JobNotPending);

            <Jobs<T>>::mutate(job_id, |j| {
                if let Some(ref mut j) = j {
                    j.state = JobState::Cancelled;
                }
            });
            <ActiveJobCount<T>>::mutate(|c| *c = c.saturating_sub(1));

            if let Some(escrow) = <JobEscrow<T>>::take(job_id) {
                // Use AllowDeath: the escrow account may be fully emptied by a refund.
                // KeepAlive would block the transfer if the escrow drops below ED.
                T::Currency::transfer(
                    &Self::account_id(),
                    &who,
                    escrow,
                    ExistenceRequirement::AllowDeath,
                )?;
            }

            // Remove from provider job list if accepted
            if let Some(provider) = <JobProvider<T>>::take(job_id) {
                let mut pj = <ProviderJobs<T>>::get(&provider);
                pj.retain(|&id| id != job_id);
                <ProviderJobs<T>>::insert(&provider, pj);
            }

            // Free the requestor's capacity slot too — previously `cancel_job`
            // decremented only `ActiveJobCount`, leaving the cancelled id in
            // `RequestorJobs` forever (a leak that also inflates the
            // per-requestor cap count). Mirror complete/fail/expire.
            let mut rj = <RequestorJobs<T>>::get(&requestor);
            rj.retain(|&id| id != job_id);
            <RequestorJobs<T>>::insert(&requestor, rj);

            Self::deposit_event(Event::<T>::JobCancelled { job_id });
            Ok(())
        }

        /// `fail_job(job_id, reason)` — mark a job **Failed** before completion and
        /// **refund the escrow immediately**. Callable by the assigned provider (to
        /// report an execution failure) or the requestor (to fail a stuck job).
        /// Unlike `cancel_job` this is allowed from any non-terminal state
        /// (Pending/Accepted/Running/AwaitingVerification) and frees the global
        /// `ActiveJobCount` slot plus the provider/requestor capacity slots. Emits
        /// `JobFailed { job_id, reason }` so off-chain callers can surface a
        /// "job failed" message to the client.
        #[pallet::call_index(8)]
        #[pallet::weight(<T as Config>::WeightInfo::fail_job())]
        pub fn fail_job(origin: OriginFor<T>, job_id: u64, reason: u8) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let mut job = <Jobs<T>>::get(job_id).ok_or(Error::<T>::JobNotFound)?;
            ensure!(
                !matches!(
                    job.state,
                    JobState::Completed | JobState::Cancelled | JobState::Failed
                ),
                Error::<T>::JobNotRunning
            );
            let requestor = <JobRequestor<T>>::get(job_id).ok_or(Error::<T>::NotJobProvider)?;
            let provider = <JobProvider<T>>::get(job_id);
            // Only the owning provider (if any) or the requestor may fail the job.
            ensure!(
                who == requestor || provider.as_ref() == Some(&who),
                Error::<T>::NotJobProvider
            );

            job.state = JobState::Failed;
            <Jobs<T>>::insert(job_id, job);
            <ActiveJobCount<T>>::mutate(|c| *c = c.saturating_sub(1));

            // Refund the full escrow to the requestor (no work delivered).
            if let Some(escrow) = <JobEscrow<T>>::take(job_id) {
                T::Currency::transfer(
                    &Self::account_id(),
                    &requestor,
                    escrow,
                    ExistenceRequirement::AllowDeath,
                )?;
            }
            // Free capacity slots (mirror cancel_job).
            if let Some(provider) = <JobProvider<T>>::take(job_id) {
                let mut pj = <ProviderJobs<T>>::get(&provider);
                pj.retain(|&id| id != job_id);
                <ProviderJobs<T>>::insert(&provider, pj);
            }
            if let Some(requestor) = <JobRequestor<T>>::get(job_id) {
                let mut rj = <RequestorJobs<T>>::get(&requestor);
                rj.retain(|&id| id != job_id);
                <RequestorJobs<T>>::insert(&requestor, rj);
            }

            Self::deposit_event(Event::<T>::JobFailed { job_id, reason });
            Ok(())
        }

        /// `expire_job(job_id)` — **permissionless** reclaim of a job that has
        /// outlived its deadline. Unlike `cancel_job` (requestor-only) and
        /// `fail_job` (provider-or-requestor), **any signed account** may call
        /// this. It wires up the previously-dead `created_at` + `max_duration`
        /// fields: allowed only once `now > created_at + max_duration`, and for
        /// any non-terminal state (Pending/Accepted/Running/AwaitingVerification),
        /// so a requestor that goes away or a provider that dies can no longer
        /// hold a global `ActiveJobCount` slot forever. Escrow is refunded to the
        /// requestor and `ActiveJobCount` is decremented; emits `JobExpired`.
        #[pallet::call_index(9)]
        #[pallet::weight(<T as Config>::WeightInfo::expire_job())]
        pub fn expire_job(origin: OriginFor<T>, job_id: u64) -> DispatchResult {
            let _who = ensure_signed(origin)?;
            let job = <Jobs<T>>::get(job_id).ok_or(Error::<T>::JobNotFound)?;
            ensure!(
                !matches!(
                    job.state,
                    JobState::Completed | JobState::Cancelled | JobState::Failed
                ),
                Error::<T>::JobNotRunning
            );
            let requestor = <JobRequestor<T>>::get(job_id).ok_or(Error::<T>::JobNotFound)?;

            let now: u32 = frame_system::Pallet::<T>::block_number()
                .try_into()
                .unwrap_or(0);
            let deadline = job.created_at.saturating_add(job.max_duration);
            ensure!(now > deadline, Error::<T>::JobNotExpired);

            Self::apply_expiry(job_id, &requestor);
            Ok(())
        }
    }

    // Public helpers
    impl<T: Config> Pallet<T> {
        pub fn account_id() -> T::AccountId {
            frame_support::PalletId(*b"gpn/cmkt").into_account_truncating()
        }

        /// The one shared expiry state transition, used by both the permissionless
        /// `expire_job` extrinsic and the `on_initialize` sweep: mark the job
        /// `Failed`, decrement the global count, refund the full escrow to the
        /// requestor, free the provider/requestor capacity slots, emit
        /// `JobExpired`. The caller must have already verified the job exists,
        /// is non-terminal, and is past its deadline.
        pub(crate) fn apply_expiry(job_id: u64, requestor: &T::AccountId) {
            let Some(mut job) = <Jobs<T>>::get(job_id) else {
                return;
            };
            job.state = JobState::Failed;
            <Jobs<T>>::insert(job_id, job);
            <ActiveJobCount<T>>::mutate(|c| *c = c.saturating_sub(1));

            // Refund the full escrow to the requestor (no work delivered).
            if let Some(escrow) = <JobEscrow<T>>::take(job_id) {
                // Best-effort: a pathological transfer failure (e.g. the
                // marketplace account under its ED) must not abort the sweep.
                let _ = T::Currency::transfer(
                    &Self::account_id(),
                    requestor,
                    escrow,
                    ExistenceRequirement::AllowDeath,
                );
            }
            // Free capacity slots (mirror fail_job/cancel_job).
            if let Some(provider) = <JobProvider<T>>::take(job_id) {
                let mut pj = <ProviderJobs<T>>::get(&provider);
                pj.retain(|&id| id != job_id);
                <ProviderJobs<T>>::insert(&provider, pj);
            }
            let mut rj = <RequestorJobs<T>>::get(requestor);
            rj.retain(|&id| id != job_id);
            <RequestorJobs<T>>::insert(requestor, rj);

            Self::deposit_event(Event::<T>::JobExpired { job_id });
        }

        /// Examine a single job id and expire it if it is non-terminal and past
        /// its deadline. Returns the weight consumed (bounded).
        fn sweep_one(job_id: u64, now: u32) -> Weight {
            let base = T::DbWeight::get().reads(1);
            let Some(job) = <Jobs<T>>::get(job_id) else {
                return base;
            };
            if matches!(
                job.state,
                JobState::Completed | JobState::Cancelled | JobState::Failed
            ) {
                return base;
            }
            let deadline = job.created_at.saturating_add(job.max_duration);
            if now <= deadline {
                return base;
            }
            let Some(requestor) = <JobRequestor<T>>::get(job_id) else {
                return base.saturating_add(T::DbWeight::get().reads(1));
            };
            Self::apply_expiry(job_id, &requestor);
            // Mirror `WeightInfo::expire_job` (marked-full cost; a hand estimate
            // since a hook cannot be benchmarked by the extrinsic harness).
            base.saturating_add(T::DbWeight::get().reads_writes(5, 4))
        }

        /// Bounded, wrapping expiry sweep run from `on_initialize`. Visits up to
        /// `MaxExpirySweepPerBlock` job ids starting at `JobSweepCursor` and
        /// expires any that are past their deadline. The cursor advances each
        /// block and wraps at `NextJobId`, so every id is eventually examined
        /// regardless of when jobs were created or cancelled. `MaxExpirySweepPerBlock
        /// = 0` disables the sweep entirely.
        fn expiry_sweep(now: BlockNumberFor<T>) -> Weight {
            let limit = T::MaxExpirySweepPerBlock::get();
            let next_id = <NextJobId<T>>::get();
            let mut weight = T::DbWeight::get().reads(2);
            if limit == 0 || next_id == 0 {
                return weight;
            }

            let now: u32 = now.try_into().unwrap_or(0);
            let mut id = <JobSweepCursor<T>>::get();
            if id >= next_id {
                id = 0;
            }

            let mut checked: u32 = 0;
            while checked < limit && id < next_id {
                weight = weight.saturating_add(Self::sweep_one(id, now));
                id = id.saturating_add(1);
                checked = checked.saturating_add(1);
            }
            // Wrap back to the start once we've walked past the newest job id.
            if id >= next_id {
                id = 0;
            }
            <JobSweepCursor<T>>::put(id);
            weight.saturating_add(T::DbWeight::get().writes(1))
        }
        pub fn provider_active_jobs(who: &T::AccountId) -> Vec<u64> {
            <ProviderJobs<T>>::get(who)
        }
        pub fn requestor_active_jobs(who: &T::AccountId) -> Vec<u64> {
            <RequestorJobs<T>>::get(who)
        }

        /// Borrow-free payload hashing shared by the open/close paths and (off-chain)
        /// the agent's channel manager. The domain tag keeps these signatures from
        /// being replayable in any other signing context.
        pub fn channel_settle_payload(
            channel_id: u64,
            balance_requestor: u128,
            balance_provider: u128,
            nonce: u64,
        ) -> [u8; 32] {
            let mut buf = Vec::new();
            buf.extend_from_slice(CHANNEL_SETTLE_DOMAIN);
            channel_id.encode_to(&mut buf);
            balance_requestor.encode_to(&mut buf);
            balance_provider.encode_to(&mut buf);
            nonce.encode_to(&mut buf);
            sp_io::hashing::blake2_256(&buf)
        }

        /// Shared settlement path for cooperative + forced close. Validates the
        /// nonce floor and balance conservation, releases the escrow, and clears
        /// the live-channel index.
        fn settle_channel(
            channel_id: u64,
            mut channel: Channel<T::AccountId>,
            balance_requestor: u128,
            balance_provider: u128,
            nonce: u64,
        ) -> DispatchResult {
            ensure!(
                nonce > channel.nonce,
                Error::<T>::ChannelNonceTooLow
            );
            let escrow =
                <ChannelEscrow<T>>::get(channel_id).ok_or(Error::<T>::ChannelNotFound)?;
            let deposit: u128 = escrow.try_into().unwrap_or(u128::MAX);
            ensure!(
                balance_requestor.saturating_add(balance_provider) == deposit,
                Error::<T>::ChannelBalanceMismatch
            );

            channel.balance_requestor = balance_requestor;
            channel.balance_provider = balance_provider;
            channel.nonce = nonce;
            channel.state = ChannelState::Closed;
            let requestor = channel.requestor.clone();
            let provider = channel.provider.clone();
            <Channels<T>>::insert(channel_id, channel);
            <ChannelEscrow<T>>::remove(channel_id);
            <ChannelIndex<T>>::remove((requestor.clone(), provider.clone()));

            if balance_requestor > 0 {
                let amt: BalanceOf<T> = balance_requestor.try_into().unwrap_or(escrow);
                T::Currency::transfer(
                    &Self::account_id(),
                    &requestor,
                    amt,
                    ExistenceRequirement::AllowDeath,
                )?;
            }
            if balance_provider > 0 {
                let amt: BalanceOf<T> = balance_provider.try_into().unwrap_or(escrow);
                T::Currency::transfer(
                    &Self::account_id(),
                    &provider,
                    amt,
                    ExistenceRequirement::AllowDeath,
                )?;
            }

            Self::deposit_event(Event::<T>::ChannelSettled {
                channel_id,
                balance_requestor,
                balance_provider,
                nonce,
            });
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frame_support::{
        assert_noop, construct_runtime, derive_impl, parameter_types,
        traits::{BuildGenesisConfig, ConstU128, ConstU16, ConstU32, ConstU64, Everything, Hooks},
    };
    use sp_core::H256;
    use sp_core::Pair;
    use sp_runtime::{
        traits::{BlakeTwo256, IdentityLookup},
        BuildStorage,
    };

    use crate::pallet as local_market;
    use crate::pallet::{
        ActiveJobCount, ChannelEscrow, ChannelIndex, Channels, JobEscrow, JobProvider, JobSweepCursor,
        Jobs, NextChannelId, NextJobId, RegisteredProviders,
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
        pub const TestMaxActiveJobsPerRequestor: u32 = 10;
        pub const TestMaxJobDuration: u64 = 10_000;
        pub const TestMaxExpirySweepPerBlock: u32 = 25;
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
        fn attested_gflops(_: &u64) -> u64 {
            MOCK_GFLOPS.with(|c| c.get())
        }
        fn attested_tier(_: &u64) -> pallet_gpu_attestation::AttestationTier {
            pallet_gpu_attestation::AttestationTier::Silver
        }
    }

    impl crate::pallet::Config for Test {
        type WeightInfo = ();
        type RuntimeEvent = RuntimeEvent;
        type Currency = Balances;
        type MarketplaceFeeBps = TestFeeBps;
        type MaxJobsPerProvider = TestMaxJobsPerProvider;
        type EscrowLockDuration = TestEscrowLock;
        type MaxActiveJobs = TestMaxActiveJobs;
        type MaxActiveJobsPerRequestor = TestMaxActiveJobsPerRequestor;
        type MaxJobDuration = TestMaxJobDuration;
        type MaxExpirySweepPerBlock = TestMaxExpirySweepPerBlock;
        type GPUAttestation = MockAttestation;
        // Test fee sink: a dedicated account (99) that collects marketplace fees.
        type FeeSink = MockFeeSink;
        type ChannelAuth = MockChannelAuth;
    }

    // Test-only channel signature verifier backed by REAL sr25519 keys. Each
    // account id gets a deterministic key via `register_channel_key`, so tests
    // exercise the actual signature path (not a rubber-stamp mock).
    thread_local! {
        static CHANNEL_KEYS: std::cell::RefCell<
            std::collections::HashMap<u64, sp_core::sr25519::Pair>,
        > = std::cell::RefCell::new(std::collections::HashMap::new());
    }
    pub struct MockChannelAuth;
    impl crate::ChannelSignatureVerifier<u64> for MockChannelAuth {
        fn verify(who: &u64, payload: &[u8; 32], signature: &[u8; 64]) -> bool {
            CHANNEL_KEYS.with(|m| match m.borrow().get(who) {
                Some(pair) => {
                    let sig = sp_core::sr25519::Signature::from_raw(*signature);
                    <sp_core::sr25519::Pair as sp_core::Pair>::verify(
                        &sig,
                        payload,
                        &pair.public(),
                    )
                }
                None => false,
            })
        }
    }
    /// Register (and return) the deterministic sr25519 key for `id`.
    fn register_channel_key(id: u64) -> sp_core::sr25519::Pair {
        let pair = sp_core::sr25519::Pair::from_seed_slice(&[id as u8; 32]).unwrap();
        CHANNEL_KEYS.with(|m| m.borrow_mut().insert(id, pair.clone()));
        pair
    }
    /// Sign a channel settlement as `id`.
    fn sign_settle(
        id: u64,
        channel_id: u64,
        br: u128,
        bp: u128,
        nonce: u64,
    ) -> [u8; 64] {
        let pair = register_channel_key(id);
        let payload = MarketPallet::channel_settle_payload(channel_id, br, bp, nonce);
        pair.sign(&payload).to_raw()
    }

    /// Test-only fee sink: routes marketplace fees to account 99 so tests can
    /// assert the fee is forwarded off the marketplace pallet account.
    pub struct MockFeeSink;
    impl pallet_gpn_token::FeeSinkProvider<u64> for MockFeeSink {
        fn fee_sink() -> u64 {
            99
        }
    }

    const ALICE: u64 = 1; // requestor
    const BOB: u64 = 2; // provider
    const CAROL: u64 = 3; // third party

    pub fn new_test_ext() -> sp_io::TestExternalities {
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

    type MarketPallet = crate::pallet::Pallet<Test>;

    fn default_reqs() -> GPURequirements {
        GPURequirements {
            min_vram_mb: 16_000,
            min_compute_capability: 80,
            min_gpu_count: 1,
            min_cuda_cores: 6_000,
            min_gflops: 1_000,
        }
    }

    fn workload() -> [u8; 32] {
        [9u8; 32]
    }

    fn register_provider(who: u64) {
        MarketPallet::register_provider(RuntimeOrigin::signed(who), default_reqs()).unwrap();
    }

    fn create_job(who: u64) -> u64 {
        MarketPallet::create_job(
            RuntimeOrigin::signed(who),
            default_reqs(),
            JobType::LLMInference,
            JobTier::Spot,
            1_000,
            0,
            0,
            100,
            workload(),
        )
        .unwrap();
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
            assert_eq!(
                RegisteredProviders::<Test>::get(BOB).unwrap().min_vram_mb,
                16_000
            );
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
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(ALICE),
                before - 1_000
            );
            assert_eq!(JobEscrow::<Test>::get(job_id), Some(1_000));
            assert_eq!(Jobs::<Test>::get(job_id).unwrap().state, JobState::Pending);
            assert_eq!(ActiveJobCount::<Test>::get(), 1);
        });
    }

    #[test]
    fn test_create_job_rejects_zero_price() {
        new_test_ext().execute_with(|| {
            let result = MarketPallet::create_job(
                RuntimeOrigin::signed(ALICE),
                default_reqs(),
                JobType::LLMInference,
                JobTier::Spot,
                0,
                0,
                0,
                100,
                workload(),
            );
            assert_eq!(result, Err(Error::<Test>::InsufficientPayment.into()));
        });
    }

    #[test]
    fn test_create_job_rejects_zero_workload() {
        new_test_ext().execute_with(|| {
            let result = MarketPallet::create_job(
                RuntimeOrigin::signed(ALICE),
                default_reqs(),
                JobType::LLMInference,
                JobTier::Spot,
                1_000,
                0,
                0,
                100,
                [0u8; 32],
            );
            assert_eq!(result, Err(Error::<Test>::InvalidWorkload.into()));
        });
    }

    #[test]
    fn test_create_job_rejects_excessive_duration() {
        new_test_ext().execute_with(|| {
            let result = MarketPallet::create_job(
                RuntimeOrigin::signed(ALICE),
                default_reqs(),
                JobType::LLMInference,
                JobTier::Spot,
                1_000,
                0,
                0,
                100_001,
                workload(),
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
            MarketPallet::submit_result(RuntimeOrigin::signed(BOB), job_id, [5u8; 32], 0).unwrap();
            assert_eq!(
                Jobs::<Test>::get(job_id).unwrap().state,
                JobState::AwaitingVerification
            );
            assert_eq!(
                Jobs::<Test>::get(job_id).unwrap().result_hash,
                Some([5u8; 32])
            );

            // Complete & pay (the assigned provider finalizes its own job).
            let provider_before = pallet_balances::Pallet::<Test>::free_balance(BOB);
            MarketPallet::complete_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            assert_eq!(
                Jobs::<Test>::get(job_id).unwrap().state,
                JobState::Completed
            );
            assert_eq!(ActiveJobCount::<Test>::get(), 0);
            // 1% fee taken: 1000 - 10 = 990 to provider
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(BOB),
                provider_before + 990
            );
        });
    }

    #[test]
    fn test_complete_job_rejects_third_party() {
        // Red-team finding 2026-10-08: `complete_job` was callable by ANY signed
        // account. From the `Running` state (no result yet) that let a third party
        // — or the requestor — finalize an in-flight job early: per-token billing
        // `actual_tokens` is still 0, so the whole escrow is refunded to the
        // requestor and the provider is paid nothing. Only the requestor or the
        // assigned provider may complete.
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            let job_id = create_job(ALICE);
            MarketPallet::accept_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            MarketPallet::start_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            assert_eq!(Jobs::<Test>::get(job_id).unwrap().state, JobState::Running);

            // A third party cannot complete the still-running job…
            let escrow_before =
                pallet_balances::Pallet::<Test>::free_balance(MarketPallet::account_id());
            assert_noop!(
                MarketPallet::complete_job(RuntimeOrigin::signed(CAROL), job_id),
                Error::<Test>::NotJobProvider
            );
            // …and the job stays Running with its escrow intact.
            assert_eq!(Jobs::<Test>::get(job_id).unwrap().state, JobState::Running);
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(MarketPallet::account_id()),
                escrow_before
            );

            // The assigned provider still completes normally.
            MarketPallet::submit_result(RuntimeOrigin::signed(BOB), job_id, [5u8; 32], 1).unwrap();
            MarketPallet::complete_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            assert_eq!(Jobs::<Test>::get(job_id).unwrap().state, JobState::Completed);
        });
    }

    #[test]
    fn test_complete_job_routes_fee_to_fee_sink() {
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            let job_id = create_job(ALICE);
            MarketPallet::accept_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            MarketPallet::start_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            MarketPallet::submit_result(RuntimeOrigin::signed(BOB), job_id, [7u8; 32], 0).unwrap();

            // Fee sink (MockFeeSink -> 99) starts at 0; marketplace pallet
            // account holds the 1000 escrow.
            let sink_before = pallet_balances::Pallet::<Test>::free_balance(99);
            assert_eq!(sink_before, 0);

            MarketPallet::complete_job(RuntimeOrigin::signed(BOB), job_id).unwrap();

            // 1% fee (10) must be forwarded to the fee sink, NOT left in gpn/cmkt.
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(99),
                sink_before + 10
            );
            // The marketplace pallet account keeps nothing after escrow + fee.
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(MarketPallet::account_id()),
                0
            );
        });
    }

    #[test]
    fn test_complete_job_small_escrow_below_ed() {
        // Regression: a job whose fee residue is below the existential deposit
        // (escrow * fee% < ED) used to fail `complete_job` with an opaque Token
        // error because the provider payout used KeepAlive on the marketplace
        // pallet account, which must stay >= ED after the transfer. Here escrow
        // = 50, fee = 1% = 0 (integer), so the residue (0) < ED (1): the payout
        // must use AllowDeath and still succeed, sweeping the account to 0.
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            MarketPallet::create_job(
                RuntimeOrigin::signed(ALICE),
                default_reqs(),
                JobType::LLMInference,
                JobTier::Spot,
                50,
                0,
                0,
                100,
                workload(),
            )
            .unwrap();
            let job_id = NextJobId::<Test>::get() - 1;
            MarketPallet::accept_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            MarketPallet::start_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            MarketPallet::submit_result(RuntimeOrigin::signed(BOB), job_id, [7u8; 32], 0).unwrap();

            let provider_before = pallet_balances::Pallet::<Test>::free_balance(BOB);
            MarketPallet::complete_job(RuntimeOrigin::signed(BOB), job_id).unwrap();

            // Provider paid the full escrow (fee rounds to 0); marketplace
            // account fully drained (AllowDeath), not blocked by ED.
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(BOB),
                provider_before + 50
            );
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(MarketPallet::account_id()),
                0
            );
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
            let result = MarketPallet::submit_result(RuntimeOrigin::signed(BOB), job_id, [5u8; 32], 0);
            assert_eq!(result, Err(Error::<Test>::JobNotRunning.into()));
        });
    }

    #[test]
    fn test_cancel_job_refunds_escrow() {
        new_test_ext().execute_with(|| {
            let job_id = create_job(ALICE);
            let before = pallet_balances::Pallet::<Test>::free_balance(ALICE);
            MarketPallet::cancel_job(RuntimeOrigin::signed(ALICE), job_id).unwrap();
            assert_eq!(
                Jobs::<Test>::get(job_id).unwrap().state,
                JobState::Cancelled
            );
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(ALICE),
                before + 1_000
            );
            assert!(JobEscrow::<Test>::get(job_id).is_none());
            assert_eq!(ActiveJobCount::<Test>::get(), 0);
        });
    }

    #[test]
    fn test_fail_job_refunds_escrow_and_frees_slot() {
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            let job_id = create_job(ALICE);
            MarketPallet::accept_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            MarketPallet::start_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            assert_eq!(ActiveJobCount::<Test>::get(), 1);
            let before = pallet_balances::Pallet::<Test>::free_balance(ALICE);

            // The assigned provider reports failure → state Failed, full escrow
            // refunded to the requestor, ActiveJobCount freed.
            MarketPallet::fail_job(RuntimeOrigin::signed(BOB), job_id, 7).unwrap();
            assert_eq!(Jobs::<Test>::get(job_id).unwrap().state, JobState::Failed);
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(ALICE),
                before + 1_000
            );
            assert!(JobEscrow::<Test>::get(job_id).is_none());
            assert_eq!(ActiveJobCount::<Test>::get(), 0);

            // A stranger cannot fail a job they don't own/request.
            let j2 = create_job(ALICE);
            assert!(MarketPallet::fail_job(RuntimeOrigin::signed(CAROL), j2, 1).is_err());
            // The requestor can (fail a stuck job).
            MarketPallet::fail_job(RuntimeOrigin::signed(ALICE), j2, 2).unwrap();
            assert_eq!(Jobs::<Test>::get(j2).unwrap().state, JobState::Failed);
            assert_eq!(ActiveJobCount::<Test>::get(), 0);
        });
    }

    #[test]
    fn test_expire_job_permissionless_refunds_and_frees_slot() {
        new_test_ext().execute_with(|| {
            // Helper create_job uses max_duration = 100, created at block 1,
            // so the deadline is block 101.
            let job_id = create_job(ALICE);
            assert_eq!(ActiveJobCount::<Test>::get(), 1);
            let before = pallet_balances::Pallet::<Test>::free_balance(ALICE);

            // Before the deadline it is NOT expirable, even by a stranger.
            System::set_block_number(50);
            assert_eq!(
                MarketPallet::expire_job(RuntimeOrigin::signed(CAROL), job_id),
                Err(Error::<Test>::JobNotExpired.into())
            );

            // Past the deadline ANY signed account (a third party) may reclaim it,
            // the escrow is refunded and the global slot is freed.
            System::set_block_number(200);
            MarketPallet::expire_job(RuntimeOrigin::signed(CAROL), job_id).unwrap();
            assert_eq!(Jobs::<Test>::get(job_id).unwrap().state, JobState::Failed);
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(ALICE),
                before + 1_000
            );
            assert!(JobEscrow::<Test>::get(job_id).is_none());
            assert_eq!(ActiveJobCount::<Test>::get(), 0);

            // A terminal job cannot be expired again.
            assert!(MarketPallet::expire_job(RuntimeOrigin::signed(CAROL), job_id).is_err());
        });
    }

    #[test]
    fn test_expiry_sweep_expires_past_deadline() {
        new_test_ext().execute_with(|| {
            // Helper create_job uses max_duration = 100, created at block 1 →
            // deadline block 101.
            let job_id = create_job(ALICE);
            assert_eq!(ActiveJobCount::<Test>::get(), 1);
            let before = pallet_balances::Pallet::<Test>::free_balance(ALICE);

            // A block before the deadline leaves the job untouched.
            System::set_block_number(50);
            MarketPallet::on_initialize(50);
            assert_eq!(Jobs::<Test>::get(job_id).unwrap().state, JobState::Pending);
            assert_eq!(ActiveJobCount::<Test>::get(), 1);

            // Past the deadline the hook auto-expires it — no external caller
            // needed: state Failed, escrow refunded, slot freed.
            System::set_block_number(200);
            MarketPallet::on_initialize(200);
            assert_eq!(Jobs::<Test>::get(job_id).unwrap().state, JobState::Failed);
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(ALICE),
                before + 1_000
            );
            assert!(JobEscrow::<Test>::get(job_id).is_none());
            assert_eq!(ActiveJobCount::<Test>::get(), 0);
            // Terminal → a second sweep is a no-op (idempotent).
            MarketPallet::on_initialize(201);
            assert_eq!(ActiveJobCount::<Test>::get(), 0);
        });
    }

    #[test]
    fn test_expiry_sweep_is_bounded_and_covers_all_ids() {
        new_test_ext().execute_with(|| {
            // 30 jobs across 3 requestors (each capped at 10), ids 1..=30, all
            // with deadline block 101. Sweep limit is 25 per block.
            for who in [ALICE, BOB, CAROL] {
                for _ in 0..10 {
                    create_job(who);
                }
            }
            assert_eq!(ActiveJobCount::<Test>::get(), 30);
            assert_eq!(NextJobId::<Test>::get(), 31);

            System::set_block_number(200);

            // First sweep visits at most `limit` ids → does not expire all 30.
            MarketPallet::on_initialize(200);
            let after_first = ActiveJobCount::<Test>::get();
            assert!(after_first > 0, "a single block must not sweep everything");
            assert!(
                after_first >= 30 - 25,
                "sweep examined at most the per-block limit"
            );

            // Subsequent sweeps (cursor wrapped) finish the job set.
            MarketPallet::on_initialize(201);
            assert_eq!(ActiveJobCount::<Test>::get(), 0);
            for id in 1..=30u64 {
                assert_eq!(Jobs::<Test>::get(id).unwrap().state, JobState::Failed);
            }
        });
    }

    #[test]
    fn test_expiry_sweep_skips_terminal_jobs() {
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            // Complete one job (terminal) and leave one Pending past deadline.
            let done = create_job(ALICE);
            MarketPallet::accept_job(RuntimeOrigin::signed(BOB), done).unwrap();
            MarketPallet::start_job(RuntimeOrigin::signed(BOB), done).unwrap();
            MarketPallet::submit_result(RuntimeOrigin::signed(BOB), done, [7u8; 32], 0).unwrap();
            MarketPallet::complete_job(RuntimeOrigin::signed(BOB), done).unwrap();
            let stuck = create_job(ALICE);

            System::set_block_number(500);
            MarketPallet::on_initialize(500);
            // The completed job is untouched; the stuck one expires.
            assert_eq!(Jobs::<Test>::get(done).unwrap().state, JobState::Completed);
            assert_eq!(Jobs::<Test>::get(stuck).unwrap().state, JobState::Failed);
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
    fn test_max_active_jobs_per_requestor() {
        new_test_ext().execute_with(|| {
            // Test cap = 10 concurrent non-terminal jobs per requestor.
            for _ in 0..10 {
                create_job(ALICE);
            }
            let over = MarketPallet::create_job(
                RuntimeOrigin::signed(ALICE),
                default_reqs(),
                JobType::LLMInference,
                JobTier::Spot,
                1_000,
                0,
                0,
                100,
                workload(),
            );
            assert_eq!(over, Err(Error::<Test>::TooManyRequestorJobs.into()));
            // A DIFFERENT requestor is unaffected — the cap is per-account, not global.
            create_job(CAROL);
            assert_eq!(ActiveJobCount::<Test>::get(), 11);
        });
    }

    #[test]
    fn test_cancel_frees_requestor_slot() {
        new_test_ext().execute_with(|| {
            // Fill ALICE's cap (10), cancel one, and confirm a create is allowed again.
            let j1 = create_job(ALICE);
            for _ in 0..9 {
                create_job(ALICE);
            }
            assert_eq!(RequestorJobs::<Test>::get(ALICE).len(), 10);
            MarketPallet::cancel_job(RuntimeOrigin::signed(ALICE), j1).unwrap();
            // Cancelled id must leave `RequestorJobs` (leak fix), not linger.
            assert_eq!(RequestorJobs::<Test>::get(ALICE).len(), 9);
            create_job(ALICE);
            assert_eq!(RequestorJobs::<Test>::get(ALICE).len(), 10);
        });
    }

    #[test]
    fn test_complete_frees_requestor_slot() {
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            let j1 = create_job(ALICE);
            for _ in 0..9 {
                create_job(ALICE);
            }
            // Complete one job end-to-end, which must free ALICE's slot.
            MarketPallet::accept_job(RuntimeOrigin::signed(BOB), j1).unwrap();
            MarketPallet::start_job(RuntimeOrigin::signed(BOB), j1).unwrap();
            MarketPallet::submit_result(RuntimeOrigin::signed(BOB), j1, [7u8; 32], 0).unwrap();
            MarketPallet::complete_job(RuntimeOrigin::signed(BOB), j1).unwrap();
            assert_eq!(RequestorJobs::<Test>::get(ALICE).len(), 9);
            create_job(ALICE);
            assert_eq!(RequestorJobs::<Test>::get(ALICE).len(), 10);
        });
    }

    #[test]
    fn test_max_jobs_per_provider() {
        new_test_ext().execute_with(|| {
            register_provider(BOB);
            // Create 5 jobs (max = 5)
            for _ in 0..5 {
                create_job(ALICE);
            }
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
            assert_eq!(
                result,
                Err(Error::<Test>::ProviderComputeInsufficient.into())
            );
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
            assert_eq!(
                result,
                Err(Error::<Test>::ProviderComputeInsufficient.into())
            );
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
                RuntimeOrigin::signed(ALICE),
                reqs,
                JobType::LLMInference,
                JobTier::Spot,
                1_000,
                0,
                0,
                100,
                workload(),
            )
            .unwrap();
            let job_id = NextJobId::<Test>::get() - 1;
            MOCK_GFLOPS.with(|c| c.set(0));
            MarketPallet::accept_job(RuntimeOrigin::signed(BOB), job_id).unwrap();
            assert_eq!(JobProvider::<Test>::get(job_id), Some(BOB));
        });
    }

    // ── Off-chain escrow channel tests (item #2, build-order step 1) ────────

    fn open_default_channel() -> u64 {
        register_provider(BOB);
        MarketPallet::open_channel(RuntimeOrigin::signed(ALICE), BOB, 5_000, 100).unwrap();
        NextChannelId::<Test>::get() - 1
    }

    #[test]
    fn test_open_channel_locks_funds_and_indexes_pair() {
        new_test_ext().execute_with(|| {
            let ch = open_default_channel();
            assert_eq!(ChannelEscrow::<Test>::get(ch), Some(5_000));
            assert_eq!(ChannelIndex::<Test>::get((ALICE, BOB)), Some(ch));
            let channel = Channels::<Test>::get(ch).unwrap();
            assert_eq!(channel.balance_requestor, 5_000);
            assert_eq!(channel.balance_provider, 0);
            assert_eq!(channel.state, ChannelState::Open);
            // Funds actually moved into the marketplace escrow account.
            assert_eq!(Balances::free_balance(MarketPallet::account_id()), 5_000);
        });
    }

    #[test]
    fn test_open_channel_rejects_unregistered_provider() {
        new_test_ext().execute_with(|| {
            let res = MarketPallet::open_channel(RuntimeOrigin::signed(ALICE), CAROL, 5_000, 100);
            assert_eq!(res, Err(Error::<Test>::ProviderNotRegistered.into()));
        });
    }

    #[test]
    fn test_cooperative_close_pays_both_parties() {
        new_test_ext().execute_with(|| {
            let ch = open_default_channel();
            // Off-chain agreed final state: provider earned 1_200 out of 5_000.
            let (br, bp, n) = (3_800u128, 1_200u128, 1u64);
            let sr = sign_settle(ALICE, ch, br, bp, n);
            let sp = sign_settle(BOB, ch, br, bp, n);
            let alice_before = Balances::free_balance(ALICE);
            let bob_before = Balances::free_balance(BOB);
            // Anyone can relay a fully-signed cooperative close.
            MarketPallet::close_channel(RuntimeOrigin::signed(CAROL), ch, br, bp, n, sr, sp)
                .unwrap();
            assert_eq!(Balances::free_balance(ALICE), alice_before + br);
            assert_eq!(Balances::free_balance(BOB), bob_before + bp);
            assert_eq!(Channels::<Test>::get(ch).unwrap().state, ChannelState::Closed);
            assert_eq!(ChannelEscrow::<Test>::get(ch), None);
            assert_eq!(ChannelIndex::<Test>::get((ALICE, BOB)), None);
            assert_eq!(Balances::free_balance(MarketPallet::account_id()), 0);
        });
    }

    #[test]
    fn test_close_rejects_forged_signature() {
        new_test_ext().execute_with(|| {
            let ch = open_default_channel();
            // Requestor tries to settle ALL funds back to itself; the "provider"
            // signature is forged (signed by CAROL, not BOB).
            let (br, bp, n) = (5_000u128, 0u128, 1u64);
            let sr = sign_settle(ALICE, ch, br, bp, n);
            let forged = sign_settle(CAROL, ch, br, bp, n);
            let res = MarketPallet::close_channel(
                RuntimeOrigin::signed(ALICE),
                ch,
                br,
                bp,
                n,
                sr,
                forged,
            );
            assert_eq!(res, Err(Error::<Test>::InvalidChannelSignature.into()));
            assert_eq!(Channels::<Test>::get(ch).unwrap().state, ChannelState::Open);
        });
    }

    #[test]
    fn test_close_rejects_balance_mismatch() {
        new_test_ext().execute_with(|| {
            let ch = open_default_channel();
            // Sum (4_800) != deposit (5_000): escrow couldn't be fully drained.
            let (br, bp, n) = (3_800u128, 1_000u128, 1u64);
            let sr = sign_settle(ALICE, ch, br, bp, n);
            let sp = sign_settle(BOB, ch, br, bp, n);
            let res = MarketPallet::close_channel(
                RuntimeOrigin::signed(ALICE),
                ch,
                br,
                bp,
                n,
                sr,
                sp,
            );
            assert_eq!(res, Err(Error::<Test>::ChannelBalanceMismatch.into()));
        });
    }

    #[test]
    fn test_force_close_blocked_before_deadline() {
        new_test_ext().execute_with(|| {
            let ch = open_default_channel();
            let (br, bp, n) = (3_800u128, 1_200u128, 1u64);
            let sp = sign_settle(BOB, ch, br, bp, n);
            let res = MarketPallet::force_close_channel(
                RuntimeOrigin::signed(ALICE),
                ch,
                br,
                bp,
                n,
                sp,
            );
            assert_eq!(res, Err(Error::<Test>::ChannelNotExpired.into()));
        });
    }

    #[test]
    fn test_force_close_after_deadline_settles_with_counterparty_sig() {
        new_test_ext().execute_with(|| {
            let ch = open_default_channel();
            let (br, bp, n) = (3_800u128, 1_200u128, 1u64);
            let sp = sign_settle(BOB, ch, br, bp, n);
            System::set_block_number(101); // opened_at(1) + ttl(100)
            let bob_before = Balances::free_balance(BOB);
            MarketPallet::force_close_channel(
                RuntimeOrigin::signed(ALICE),
                ch,
                br,
                bp,
                n,
                sp,
            )
            .unwrap();
            assert_eq!(Balances::free_balance(BOB), bob_before + bp);
            assert_eq!(Channels::<Test>::get(ch).unwrap().state, ChannelState::Closed);
        });
    }
}
