//! # GPU Attestation Pallet — Proof of Staked GPU (PoSG)
//!
//! GPU providers prove they have real GPU hardware by submitting periodic
//! challenge responses computed on their GPU. Combined with token staking,
//! this provides Sybil-resistant validation of compute resources.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "runtime-benchmarks")]
pub mod benchmarking;
pub mod weights;

use codec::{Decode, DecodeWithMemTracking, Encode, MaxEncodedLen};
use core::fmt::Debug;
use frame_support::sp_runtime::traits::SaturatedConversion;
use pallet_gpn_liveness::LivenessProvider;
use scale_info::TypeInfo;

pub use pallet::*;
pub use weights::WeightInfo;

/// GPU attestation tier
#[derive(
    Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo,
)]
pub enum AttestationTier {
    Unattested,
    Bronze,
    Silver,
    Gold,
}

impl Default for AttestationTier {
    fn default() -> Self {
        Self::Unattested
    }
}

impl AttestationTier {
    pub fn staking_multiplier_bps(&self) -> u32 {
        match self {
            Self::Unattested => 0,
            Self::Bronze => 1_000,
            Self::Silver => 1_500,
            Self::Gold => 2_500,
        }
    }
    pub fn min_gpus(&self) -> u32 {
        match self {
            Self::Unattested => 0,
            Self::Bronze => 1,
            Self::Silver => 4,
            Self::Gold => 16,
        }
    }
    /// Minimum measured compute throughput (FP32 GFLOPS) required to earn this
    /// tier. Ties the tier to **proof of real compute** — a provider must
    /// demonstrate it can actually serve a usable AI workload, not just report
    /// a GPU count. (RTX 4060 ≈ 9-15 TFLOPS FP32; Bronze needs only a slice.)
    pub fn min_gflops(&self) -> u64 {
        match self {
            Self::Unattested => 0,
            Self::Bronze => 100,
            Self::Silver => 1_000,
            Self::Gold => 5_000,
        }
    }
}

/// A single attestation record
#[derive(
    Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo,
)]
pub struct AttestationRecord {
    pub submitted_at: u32,
    pub gpu_info_hash: [u8; 32],
    pub challenge_response: [u8; 32],
    pub gpu_count: u8,
    /// Measured compute throughput (FP32 GFLOPS) demonstrated by the provider.
    /// This is the on-chain attestable metric tying the AI-compute leg to the
    /// marketplace: a requestor can require `min_gflops` and match against it.
    pub compute_gflops: u64,
    pub tier: AttestationTier,
    pub missed_count: u32,
}

/// Read-only interface for other pallets (e.g. `compute-marketplace`) to query
/// a provider's **attested** compute credentials. A provider that has never
/// attested (or whose attestation was withdrawn) reports zero GFLOPS and the
/// `Unattested` tier, so downstream consumers can enforce capability gates
/// without reaching into this pallet's storage directly.
pub trait GPUAttestationProvider<AccountId> {
    /// The measured FP32 GFLOPS this account attested to (0 if not attested).
    fn attested_gflops(who: &AccountId) -> u64;
    /// The provider's current attestation tier (`Unattested` if none).
    fn attested_tier(who: &AccountId) -> AttestationTier;
    /// Weight for staking-reward distribution: the account's staked balance
    /// scaled by its GPU-tier multiplier (`stake × tier`, basis-points-scaled).
    /// Zero when the account holds **no stake** or is **unattested**, so the
    /// staking pool is paid only to real stakers in proportion to how much they
    /// staked — i.e. "staking increases your reward" (design-tokenomics-spec §5).
    /// Defaults to 0 for implementers that don't track stakes (e.g. test mocks).
    fn reward_weight(_who: &AccountId) -> u128 {
        0
    }
    /// Benchmark-only: install/overwrite an attestation record so downstream
    /// benchmarks (e.g. `pallet-gpn-token` staking-reward loops) can exercise
    /// the attested path without going through the full challenge flow. Has a
    /// no-op default so mocks/other implementers need not care.
    #[cfg(feature = "runtime-benchmarks")]
    fn benchmark_set_attestation(_who: &AccountId, _tier: AttestationTier) {}
}

#[frame_support::pallet]
pub mod pallet {
    use super::*;
    use frame_support::pallet_prelude::*;
    use frame_support::sp_runtime::traits::{AccountIdConversion, Saturating, Zero};
    use frame_support::traits::{Currency, ExistenceRequirement};
    use frame_system::pallet_prelude::*;

    pub type BalanceOf<T> =
        <<T as Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::Balance;

    #[pallet::config]
    pub trait Config: frame_system::Config {
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;
        type Currency: Currency<Self::AccountId>;
        /// Full-node liveness provider (gates Silver/Gold behind being an
        /// always-on, publicly-reachable full node).
        type Liveness: LivenessProvider<Self::AccountId>;
        #[pallet::constant]
        type MinAttestationStake: Get<BalanceOf<Self>>;
        #[pallet::constant]
        type AttestationInterval: Get<BlockNumberFor<Self>>;
        #[pallet::constant]
        type MaxAttestationAge: Get<BlockNumberFor<Self>>;
        #[pallet::constant]
        type MaxMissedAttestations: Get<u32>;
        /// Minimum measured FP32 GFLOPS a device must demonstrate to be eligible
        /// to attest **at all** (⇒ to earn). The capability floor (item 19): a
        /// CPU/APU **integrated** GPU (≈1–2 TFLOPS) cannot clear a ~3 TFLOPS
        /// (3000 GFLOPS) floor, so it cannot attest or earn. Unlike the per-tier
        /// `min_gflops`, this is a hard eligibility gate that also protects the
        /// `Unattested`→earn path. Enforced against the *measured* `compute_gflops`
        /// (self-reported at attestation ⇒ pair with the capacity challenge #6).
        #[pallet::constant]
        type MinEligibilityGflops: Get<u64>;
        #[pallet::constant]
        type BaseStakingAPR: Get<u32>;
        /// Min rolling uptime (bps) a full node needs to hold the Silver tier.
        #[pallet::constant]
        type SilverUptimeBps: Get<u32>;
        /// Min rolling uptime (bps) a full node needs to hold the Gold tier.
        #[pallet::constant]
        type GoldUptimeBps: Get<u32>;
        /// Weight information for extrinsics in this pallet (workplan item 9).
        type WeightInfo: WeightInfo;
        /// Trusted account allowed to commit a **node-verified** capacity
        /// challenge result on-chain (`confirm_challenge`). The runtime cannot
        /// run Ethash itself, so a node verifies a provider's solution natively
        /// (it holds the epoch light cache) and this account commits the
        /// boolean outcome — mirroring how block seals are node-verified at
        /// import. Testnet-acceptable; production would use a verifier
        /// committee. See `design-verifiable-gpu-proof-2026-10-06.md`.
        type ChallengeVerifier: Get<Self::AccountId>;
    }

    #[pallet::pallet]
    pub struct Pallet<T>(_);

    // Storage

    #[pallet::storage]
    pub type AttestationChallenge<T: Config> = StorageValue<_, [u8; 32], ValueQuery>;

    /// The challenge each account has had its solution **node-verified** for.
    /// Stored separately from `Attestations` (no record-layout change, so live
    /// attestation records are untouched by the upgrade) and compared against
    /// the live `AttestationChallenge` — a solution only counts while the
    /// challenge it was mined over is still current.
    #[pallet::storage]
    pub type ChallengeVerified<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, [u8; 32], OptionQuery>;

    #[pallet::storage]
    pub type Attestations<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, AttestationRecord, OptionQuery>;

    #[pallet::storage]
    pub type AttestedProviderCount<T: Config> = StorageValue<_, u32, ValueQuery>;

    #[pallet::storage]
    pub type TotalStaked<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    #[pallet::storage]
    pub type ProviderStake<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, BalanceOf<T>, OptionQuery>;

    /// Block at which `check_expired` last counted a *miss* for an account.
    /// Bounds miss-counting (and thus slashing) to at most once per
    /// `MaxAttestationAge` window so a permissionless caller cannot hammer
    /// `check_expired` in a single block to repeatedly slash an overdue
    /// provider's stake. Cleared on (re)attestation and unbonding.
    #[pallet::storage]
    pub type LastMissBlock<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, BlockNumberFor<T>, OptionQuery>;

    // Genesis

    #[pallet::genesis_config]
    #[derive(frame_support::DefaultNoBound)]
    pub struct GenesisConfig<T: Config> {
        pub initial_challenge: [u8; 32],
        #[serde(skip)]
        pub _config: sp_std::marker::PhantomData<T>,
    }

    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            <AttestationChallenge<T>>::put(self.initial_challenge);
            <AttestedProviderCount<T>>::put(0u32);
            let zero_bal: BalanceOf<T> = Zero::zero();
            <TotalStaked<T>>::put(zero_bal);
        }
    }

    // Events

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        AttestationSubmitted {
            who: T::AccountId,
            gpu_count: u8,
            tier: AttestationTier,
        },
        StakeDeposited {
            who: T::AccountId,
            amount: BalanceOf<T>,
        },
        StakeWithdrawn {
            who: T::AccountId,
            amount: BalanceOf<T>,
        },
        TierUpdated {
            who: T::AccountId,
            old_tier: AttestationTier,
            new_tier: AttestationTier,
        },
        Slashed {
            who: T::AccountId,
            penalty: BalanceOf<T>,
        },
        ChallengeRotated {
            new_challenge: [u8; 32],
        },
        /// A provider's capacity-challenge solution was verified by the trusted
        /// verifier and committed on-chain.
        ChallengeConfirmed {
            who: T::AccountId,
            challenge: [u8; 32],
        },
    }

    // Errors

    #[pallet::error]
    pub enum Error<T> {
        InsufficientStake,
        AttestationTooSoon,
        NoExistingAttestation,
        InvalidChallengeResponse,
        StakeLocked,
        AlreadyAttested,
        /// A stake already exists for the caller. Red-team finding #18:
        /// `deposit_stake` used to guard only on `Attestations`, so a second
        /// pre-attestation deposit silently *overwrote* `ProviderStake`
        /// (the first deposit's funds stayed stranded in the pallet account,
        /// unaccounted and never withdrawable) and desynced `TotalStaked`.
        AlreadyStaked,
        NoStakeToWithdraw,
        InvalidGPUCount,
        StakeTransferFailed,
        /// Reported compute throughput is below the minimum for the claimed GPU tier.
        InsufficientCompute,
        /// Reported compute throughput is below the minimum GPU *eligibility*
        /// floor (item 19) — the device cannot attest (⇒ cannot earn).
        BelowEligibilityFloor,
        /// Caller is not the configured trusted challenge verifier.
        NotChallengeVerifier,
        /// No challenge is currently active to confirm against.
        NoActiveChallenge,
    }

    // Hooks

    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
        fn on_initialize(now: BlockNumberFor<T>) -> Weight {
            let interval = T::AttestationInterval::get();
            if !interval.is_zero() && now % interval == Zero::zero() {
                let new_challenge = Self::generate_challenge();
                <AttestationChallenge<T>>::put(new_challenge);
                Self::deposit_event(Event::<T>::ChallengeRotated { new_challenge });
            }
            Weight::zero()
        }
    }

    // Dispatchable calls

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        #[pallet::call_index(0)]
        #[pallet::weight(<T as Config>::WeightInfo::deposit_stake())]
        pub fn deposit_stake(origin: OriginFor<T>, amount: BalanceOf<T>) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let min_stake = T::MinAttestationStake::get();
            ensure!(amount >= min_stake, Error::<T>::InsufficientStake);
            ensure!(
                !<Attestations<T>>::contains_key(&who),
                Error::<T>::AlreadyAttested
            );
            // Red-team finding #18 (fund lock / accounting integrity): reject a
            // second stake deposit while one already exists. Previously the only
            // guard was the `Attestations` check above, which a caller who had
            // deposited but not yet attested would pass — the second call then
            // transferred `amount` again but `ProviderStake::insert(..)`
            // *overwrote* the first stake, so the surplus sat permanently in the
            // pallet account (never returned by `withdraw_stake`, which refunds
            // only the recorded `ProviderStake`) and `TotalStaked` diverged from
            // the real pallet balance. Reject instead of silently locking funds.
            ensure!(
                !<ProviderStake<T>>::contains_key(&who),
                Error::<T>::AlreadyStaked
            );

            T::Currency::transfer(
                &who,
                &Self::account_id(),
                amount,
                ExistenceRequirement::KeepAlive,
            )?;

            <ProviderStake<T>>::insert(&who, amount);
            <TotalStaked<T>>::mutate(|t| *t += amount);
            Self::deposit_event(Event::<T>::StakeDeposited { who, amount });
            Ok(())
        }

        #[pallet::call_index(1)]
        #[pallet::weight(<T as Config>::WeightInfo::submit_attestation())]
        pub fn submit_attestation(
            origin: OriginFor<T>,
            challenge_response: [u8; 32],
            gpu_info_hash: [u8; 32],
            gpu_count: u8,
            compute_gflops: u64,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            // Staking is now OPTIONAL (carrot, not gate): a provider no longer needs a
            // stake to attest. Real GPU verification is enforced by the mining path
            // (seal-verified PoW) and, in phase 2, by a node-verified challenge. Staking
            // remains available and raises the reward multiplier via the tier system.
            // See design-verifiable-gpu-proof-2026-10-06.md.

            // Minimum GPU capability floor (item 19): a CPU/APU integrated GPU
            // cannot attest — hence cannot earn — unless its *measured* FP32
            // throughput clears the floor. This is checked FIRST, before any tier
            // is assigned, so it also covers the stake-free earn path.
            ensure!(
                compute_gflops >= T::MinEligibilityGflops::get(),
                Error::<T>::BelowEligibilityFloor
            );

            let mut tier = if gpu_count >= 16 {
                AttestationTier::Gold
            } else if gpu_count >= 4 {
                AttestationTier::Silver
            } else if gpu_count >= 1 {
                AttestationTier::Bronze
            } else {
                return Err(Error::<T>::InvalidGPUCount.into());
            };

            // GPU count is necessary but not sufficient — the provider must also
            // prove it can actually deliver the compute throughput for that tier.
            // A node reporting 16 GPUs that can't muster 5000 GFLOPS is demoted.
            if compute_gflops < tier.min_gflops() {
                tier = AttestationTier::Unattested;
            }
            if tier == AttestationTier::Unattested {
                return Err(Error::<T>::InsufficientCompute.into());
            }

            // Silver/Gold are gated behind being an active, always-on,
            // publicly-reachable full node with sufficient rolling uptime. A big
            // GPU box that is hidden / NAT'd / off-and-on is capped at Bronze.
            tier = Self::apply_fullnode_gate(&who, tier);

            // Only *new* submissions must prove a node-verified capacity
            // challenge for the higher tiers (design: "only newly-gated tiers
            // require a verified challenge"); legacy records already on-chain are
            // left untouched so the live marketplace is not zeroed on upgrade.
            tier = Self::apply_challenge_gate(&who, tier);

            let now: u32 = frame_system::Pallet::<T>::block_number()
                .try_into()
                .unwrap_or(0);

            let mut record = AttestationRecord {
                submitted_at: now,
                gpu_info_hash,
                challenge_response,
                gpu_count,
                compute_gflops,
                tier: tier.clone(),
                missed_count: 0,
            };

            let is_new = !<Attestations<T>>::contains_key(&who);
            // RED-TEAM 2026-10-09 (finding #23, gpu-attestation / liveness-incentive
            // evasion): a RE-submission used to reset `missed_count` to 0 and clear
            // `LastMissBlock`, so a provider that had gone idle could zero its miss
            // history simply by re-submitting an attestation. Because the base
            // (Bronze) attestation's `challenge_response` is NOT verified on-chain,
            // that re-submission proves nothing about liveness — an offline operator
            // could therefore dodge the `MaxMissedAttestations` slash (`check_expired`)
            // indefinitely, defeating the periodic-liveness deterrent. Only a *fresh*
            // attestation starts a clean miss history; a re-submission preserves the
            // accumulated miss state, so an honest provider clears it the real way —
            // via a confirmed liveness window in `gpn-liveness::submit_liveness`
            // (which sets `missed_count = 0`).
            let prior_missed = <Attestations<T>>::get(&who)
                .map(|r| r.missed_count)
                .unwrap_or(0);
            record.missed_count = if is_new { 0 } else { prior_missed };
            <Attestations<T>>::insert(&who, record);
            // Only a *fresh* attestation resets the miss clock for this provider.
            if is_new {
                <LastMissBlock<T>>::remove(&who);
                <AttestedProviderCount<T>>::mutate(|c| *c += 1);
            }

            Self::deposit_event(Event::<T>::AttestationSubmitted {
                who,
                gpu_count,
                tier,
            });
            Ok(())
        }

        #[pallet::call_index(2)]
        #[pallet::weight(<T as Config>::WeightInfo::withdraw_stake())]
        pub fn withdraw_stake(origin: OriginFor<T>) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let stake = <ProviderStake<T>>::take(&who).ok_or(Error::<T>::NoStakeToWithdraw)?;

            <Attestations<T>>::remove(&who);
            <LastMissBlock<T>>::remove(&who);
            <AttestedProviderCount<T>>::mutate(|c| *c = c.saturating_sub(1));
            <TotalStaked<T>>::mutate(|t| *t -= stake);

            T::Currency::transfer(
                &Self::account_id(),
                &who,
                stake,
                ExistenceRequirement::AllowDeath,
            )?;

            Self::deposit_event(Event::<T>::StakeWithdrawn { who, amount: stake });
            Ok(())
        }

        /// Commit the result of a **node-verified** capacity-challenge solution.
        ///
        /// Verification itself is node-native (Ethash over the live
        /// `AttestationChallenge`), so the runtime stores only the assertion: the
        /// trusted `ChallengeVerifier` calls this **after** the node has verified
        /// `who`'s seal, recording that `who` solved the *current* challenge.
        /// This does not itself change any tier — `submit_attestation` (and the
        /// expiry sweep) consults it via `challenge_verified`.
        #[pallet::call_index(4)]
        #[pallet::weight(<T as Config>::WeightInfo::confirm_challenge())]
        pub fn confirm_challenge(origin: OriginFor<T>, who: T::AccountId) -> DispatchResult {
            let signer = ensure_signed(origin)?;
            ensure!(
                signer == T::ChallengeVerifier::get(),
                Error::<T>::NotChallengeVerifier
            );
            let challenge = <AttestationChallenge<T>>::get();
            ensure!(challenge != [0u8; 32], Error::<T>::NoActiveChallenge);
            <ChallengeVerified<T>>::insert(&who, challenge);
            Self::deposit_event(Event::<T>::ChallengeConfirmed { who, challenge });
            Ok(())
        }

        #[pallet::call_index(3)]
        #[pallet::weight(<T as Config>::WeightInfo::check_expired())]
        pub fn check_expired(origin: OriginFor<T>, target: T::AccountId) -> DispatchResult {
            let _who = ensure_signed(origin)?;
            let mut record =
                <Attestations<T>>::get(&target).ok_or(Error::<T>::NoExistingAttestation)?;

            let now: u32 = frame_system::Pallet::<T>::block_number()
                .try_into()
                .unwrap_or(0);
            let max_age: u32 = T::MaxAttestationAge::get().try_into().unwrap_or(u32::MAX);

            // Count at most ONE miss per `MaxAttestationAge` window. The
            // reference is the last counted miss (or the attestation time if
            // none yet), so repeated calls in the same window are a no-op.
            // Without this, a permissionless caller could call `check_expired`
            // N times in one block and slash 10% each time, draining the stake.
            let last_miss_block: u32 = <LastMissBlock<T>>::get(&target)
                .map(|b| b.try_into().unwrap_or(u32::MAX))
                .unwrap_or(record.submitted_at);
            if now.saturating_sub(last_miss_block) > max_age {
                record.missed_count = record.missed_count.saturating_add(1);
                <LastMissBlock<T>>::insert(&target, frame_system::Pallet::<T>::block_number());
                if record.missed_count >= T::MaxMissedAttestations::get() {
                    if let Some(stake) = <ProviderStake<T>>::get(&target) {
                        let penalty: BalanceOf<T> = stake / 10u32.into();
                        let remaining = stake.saturating_sub(penalty);
                        <ProviderStake<T>>::insert(&target, remaining);
                        <TotalStaked<T>>::mutate(|t| *t -= penalty);
                        // RED-TEAM 2026-10-08 (finding #3, fund lock / D4 violation):
                        // the slash used to adjust *accounting only* — the penalty
                        // currency stayed in the pallet account forever (it is never
                        // withdrawable: `withdraw_stake` returns only the reduced
                        // `ProviderStake`). Per D4 the penalty must be BURNED (removed
                        // from total issuance), matching
                        // `profitability-oracle::slash_operator` and
                        // `gpn-token::distribute_fees`.
                        if !penalty.is_zero() {
                            let (imbalance, _) = T::Currency::slash(&Self::account_id(), penalty);
                            drop(imbalance); // negative imbalance = burned
                        }
                        Self::deposit_event(Event::<T>::Slashed {
                            who: target.clone(),
                            penalty,
                        });
                    }
                }
            }

            // A Silver/Gold holder that lost full-node standing (went idle /
            // unreachable) is demoted to Bronze so it cannot keep earning 1.5x /
            // 2.5x while hidden. Runs on every expiry sweep. The capacity-challenge
            // gate is deliberately NOT applied here: it only gates *new*
            // submissions, so a legacy record already on-chain is never demoted by
            // the upgrade (design rollout note).
            let gate = Self::apply_fullnode_gate(&target, record.tier.clone());
            if gate != record.tier {
                let old = record.tier.clone();
                record.tier = gate.clone();
                Self::deposit_event(Event::<T>::TierUpdated {
                    who: target.clone(),
                    old_tier: old,
                    new_tier: gate,
                });
            }
            <Attestations<T>>::insert(&target, record);
            Ok(())
        }
    }

    // Public helpers

    impl<T: Config> Pallet<T> {
        /// Cap a hardware-claimed tier at Bronze unless the account is an active
        /// full node with sufficient rolling uptime. Bronze stays open so brand
        /// new / NAT'd / intermittent miners can still participate at entry tier;
        /// Silver/Gold are reserved for network stewards.
        fn apply_fullnode_gate(who: &T::AccountId, tier: AttestationTier) -> AttestationTier {
            match tier {
                AttestationTier::Silver | AttestationTier::Gold => {
                    let active = T::Liveness::is_active_full_node(who);
                    if !active {
                        return AttestationTier::Bronze;
                    }
                    let uptime = T::Liveness::window_uptime_bps(who);
                    let need = match tier {
                        AttestationTier::Gold => T::GoldUptimeBps::get(),
                        _ => T::SilverUptimeBps::get(),
                    };
                    if uptime < need {
                        return AttestationTier::Bronze;
                    }
                    tier
                }
                other => other,
            }
        }

        /// Whether `who` currently holds a **node-verified** solution to the
        /// live challenge. False for never-verified accounts and once the
        /// challenge rotates (the stored challenge no longer matches).
        pub fn challenge_verified(who: &T::AccountId) -> bool {
            match <ChallengeVerified<T>>::get(who) {
                Some(c) => c == <AttestationChallenge<T>>::get(),
                None => false,
            }
        }

        /// Cap a claimed Silver/Gold tier at Bronze unless the account holds a
        /// node-verified challenge solution for the *current* challenge. Bronze
        /// stays open (entry tier), consistent with the full-node gate.
        fn apply_challenge_gate(who: &T::AccountId, tier: AttestationTier) -> AttestationTier {
            match tier {
                AttestationTier::Silver | AttestationTier::Gold => {
                    if !Self::challenge_verified(who) {
                        return AttestationTier::Bronze;
                    }
                    tier
                }
                other => other,
            }
        }

        pub fn effective_stake(who: &T::AccountId) -> BalanceOf<T> {
            let stake = <ProviderStake<T>>::get(who).unwrap_or_default();
            let tier = <Attestations<T>>::get(who)
                .map(|r| r.tier)
                .unwrap_or(AttestationTier::Unattested);
            let multiplier = tier.staking_multiplier_bps();
            if multiplier == 0 {
                return Zero::zero();
            }
            stake * multiplier.into() / 10_000u32.into()
        }

        pub fn account_id() -> T::AccountId {
            frame_support::PalletId(*b"gpn/attn").into_account_truncating()
        }

        fn generate_challenge() -> [u8; 32] {
            // Mix the block number with the **parent block hash** so the
            // challenge is unpredictable ahead of time, per
            // `design-verifiable-gpu-proof-2026-10-06.md` § "derived from a
            // recent block hash → unpredictable".
            //
            // A block-numeric seed alone (`blake2_256(block_number)`) is fully
            // predictable: the next rotation block is `now + interval`, so an
            // attacker could pre-compute (or buy) solutions for several future
            // challenges in advance — defeating the anti-spoof purpose of the
            // challenge (and its binding to the verifier/account). The parent
            // hash is unknown until the parent block is actually produced, so
            // no one can know a rotation-block challenge before the block
            // immediately preceding it exists.
            let seed = (
                frame_system::Pallet::<T>::block_number(),
                frame_system::Pallet::<T>::parent_hash(),
            )
                .encode();
            sp_io::hashing::blake2_256(&seed)
        }
    }
}

/// Expose the pallet's attestation records through the shared trait so the
/// compute marketplace (and any future consumer) can gate jobs on verified
/// GPU throughput without coupling to this pallet's storage layout.
impl<T: Config> GPUAttestationProvider<T::AccountId> for Pallet<T> {
    fn attested_gflops(who: &T::AccountId) -> u64 {
        <Attestations<T>>::get(who)
            .map(|r| r.compute_gflops)
            .unwrap_or(0)
    }
    fn attested_tier(who: &T::AccountId) -> AttestationTier {
        <Attestations<T>>::get(who)
            .map(|r| r.tier)
            .unwrap_or(AttestationTier::Unattested)
    }

    fn reward_weight(who: &T::AccountId) -> u128 {
        Self::effective_stake(who).saturated_into()
    }

    #[cfg(feature = "runtime-benchmarks")]
    fn benchmark_set_attestation(who: &T::AccountId, tier: AttestationTier) {
        let record = AttestationRecord {
            submitted_at: frame_system::Pallet::<T>::block_number()
                .try_into()
                .unwrap_or(0u32),
            gpu_info_hash: [0u8; 32],
            challenge_response: [0u8; 32],
            gpu_count: tier.min_gpus() as u8,
            compute_gflops: 0,
            tier,
            missed_count: 0,
        };
        <Attestations<T>>::insert(who, record);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frame_support::{
        construct_runtime, derive_impl, parameter_types,
        traits::{BuildGenesisConfig, ConstU128, ConstU16, ConstU32, ConstU64, Everything, Hooks},
    };
    use sp_core::H256;
    use sp_runtime::{
        traits::{BlakeTwo256, IdentityLookup},
        BuildStorage,
    };

    use crate::pallet as local_attest;
    use crate::pallet::{
        AttestationChallenge, Attestations, AttestedProviderCount, ChallengeVerified,
        ProviderStake, TotalStaked,
    };

    type Block = frame_system::mocking::MockBlock<Test>;
    type Balance = u128;

    construct_runtime!(
        pub struct Test {
            System: frame_system,
            Balances: pallet_balances,
            Attest: local_attest,
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
        pub const TestMinStake: Balance = 100;
        pub const TestAttestInterval: u64 = 10;
        pub const TestMaxAge: u64 = 50;
        pub const TestMaxMissed: u32 = 3;
        /// Low floor in tests so the many tier/count cases below stay valid; the
        /// dedicated floor test uses a value under it.
        pub const TestMinEligibilityGflops: u64 = 50;
        pub const TestBaseAPR: u32 = 1_000;
        pub const TestSilverUptime: u32 = 9_000;
        pub const TestGoldUptime: u32 = 9_500;
        pub const TestChallengeVerifier: u64 = 99;
    }

    // Mock liveness provider: configurable active/uptime via thread-local state
    // so each #[test] thread is isolated (cargo runs tests in parallel).
    // Defaults to always-active high-uptime so existing tier tests pass.
    use core::cell::Cell;
    thread_local! {
        pub static MOCK_ACTIVE: Cell<bool> = Cell::new(true);
        pub static MOCK_UPTIME: Cell<u32> = Cell::new(10_000);
    }
    pub struct MockLiveness;
    impl LivenessProvider<u64> for MockLiveness {
        fn is_active_full_node(_who: &u64) -> bool {
            MOCK_ACTIVE.with(|c| c.get())
        }
        fn window_uptime_bps(_who: &u64) -> u32 {
            MOCK_UPTIME.with(|c| c.get())
        }
    }

    impl crate::pallet::Config for Test {
        type WeightInfo = ();
        type RuntimeEvent = RuntimeEvent;
        type Currency = Balances;
        type Liveness = MockLiveness;
        type MinAttestationStake = TestMinStake;
        type AttestationInterval = TestAttestInterval;
        type MaxAttestationAge = TestMaxAge;
        type MaxMissedAttestations = TestMaxMissed;
        type MinEligibilityGflops = TestMinEligibilityGflops;
        type BaseStakingAPR = TestBaseAPR;
        type SilverUptimeBps = TestSilverUptime;
        type GoldUptimeBps = TestGoldUptime;
        type ChallengeVerifier = TestChallengeVerifier;
    }

    const ALICE: u64 = 1;
    const BOB: u64 = 2;
    /// Trusted node verifier (mock).
    const VERIFIER: u64 = 99;
    const INITIAL_CHALLENGE: [u8; 32] = [7u8; 32];

    pub fn new_test_ext() -> sp_io::TestExternalities {
        let mut t = frame_system::GenesisConfig::<Test>::default()
            .build_storage()
            .unwrap();
        // Endow accounts so they can stake
        pallet_balances::GenesisConfig::<Test> {
            balances: vec![(ALICE, 10_000), (BOB, 10_000)],
            dev_accounts: None,
        }
        .assimilate_storage(&mut t)
        .unwrap();
        let mut ext = sp_io::TestExternalities::new(t);
        // Reset mock liveness to always-active high-uptime for each test.
        MOCK_ACTIVE.with(|c| c.set(true));
        MOCK_UPTIME.with(|c| c.set(10_000));
        ext.execute_with(|| {
            let cfg = crate::pallet::GenesisConfig::<Test> {
                initial_challenge: INITIAL_CHALLENGE,
                _config: Default::default(),
            };
            BuildGenesisConfig::build(&cfg);
            System::set_block_number(1);
        });
        ext
    }

    type AttestPallet = crate::pallet::Pallet<Test>;

    fn stake_and_attest(who: u64, amount: Balance, gpus: u8) {
        stake_and_attest_gflops(who, amount, gpus, 10_000); // plenty of compute
    }

    fn stake_and_attest_gflops(who: u64, amount: Balance, gpus: u8, gflops: u64) {
        // Higher tiers now also require a node-verified capacity challenge, so
        // the shared happy-path helper confirms it first (Bronze is unaffected).
        AttestPallet::confirm_challenge(RuntimeOrigin::signed(VERIFIER), who).unwrap();
        AttestPallet::deposit_stake(RuntimeOrigin::signed(who), amount).unwrap();
        AttestPallet::submit_attestation(
            RuntimeOrigin::signed(who),
            [1u8; 32],
            [2u8; 32],
            gpus,
            gflops,
        )
        .unwrap();
    }

    #[test]
    fn test_initial_state() {
        new_test_ext().execute_with(|| {
            assert_eq!(AttestationChallenge::<Test>::get(), INITIAL_CHALLENGE);
            assert_eq!(AttestedProviderCount::<Test>::get(), 0);
            assert_eq!(TotalStaked::<Test>::get(), 0);
            assert!(Attestations::<Test>::get(ALICE).is_none());
        });
    }

    #[test]
    fn test_deposit_stake_moves_funds() {
        new_test_ext().execute_with(|| {
            let before = pallet_balances::Pallet::<Test>::free_balance(ALICE);
            AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 500).unwrap();
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(ALICE),
                before - 500
            );
            assert_eq!(ProviderStake::<Test>::get(ALICE), Some(500));
            assert_eq!(TotalStaked::<Test>::get(), 500);
        });
    }

    #[test]
    fn test_deposit_stake_rejects_below_minimum() {
        new_test_ext().execute_with(|| {
            let result = AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 50);
            assert_eq!(result, Err(Error::<Test>::InsufficientStake.into()));
            assert!(ProviderStake::<Test>::get(ALICE).is_none());
        });
    }

    #[test]
    fn test_deposit_stake_rejects_already_attested() {
        new_test_ext().execute_with(|| {
            stake_and_attest(ALICE, 500, 2);
            // Can't deposit again once attested
            let result = AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 500);
            assert_eq!(result, Err(Error::<Test>::AlreadyAttested.into()));
        });
    }

    #[test]
    fn test_deposit_stake_rejects_second_deposit_without_locking_funds() {
        // Red-team finding #18 (fund lock / accounting integrity): a second
        // `deposit_stake` before attesting used to pass the `Attestations`-only
        // guard, pull `amount` a second time from the caller, then *overwrite*
        // `ProviderStake` — stranding the first deposit in the pallet account
        // (unaccounted, never refunded) and desyncing `TotalStaked`.
        new_test_ext().execute_with(|| {
            let before = pallet_balances::Pallet::<Test>::free_balance(ALICE);
            AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 500).unwrap();
            // Second deposit (still un-attested) must be rejected outright.
            let result = AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 700);
            assert_eq!(result, Err(Error::<Test>::AlreadyStaked.into()));
            // First stake preserved exactly; no extra funds pulled in.
            assert_eq!(ProviderStake::<Test>::get(ALICE), Some(500));
            assert_eq!(TotalStaked::<Test>::get(), 500);
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(ALICE),
                before - 500
            );
            // Withdrawal returns the full recorded stake -> no locked surplus.
            AttestPallet::withdraw_stake(RuntimeOrigin::signed(ALICE)).unwrap();
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(ALICE), before);
            assert_eq!(ProviderStake::<Test>::get(ALICE), None);
            assert_eq!(TotalStaked::<Test>::get(), 0);
        });
    }

    #[test]
    fn test_submit_attestation_stakeless_ok() {
        // Stake-free attestation (spec 11 / design-verifiable-gpu-proof): a
        // provider no longer needs a stake to attest — staking is an optional
        // boost, not a gate. A bare 1-GPU box with enough GFLOPS attests Bronze.
        new_test_ext().execute_with(|| {
            let result = AttestPallet::submit_attestation(
                RuntimeOrigin::signed(ALICE),
                [1u8; 32],
                [2u8; 32],
                1,
                10_000,
            );
            assert!(result.is_ok());
            assert_eq!(
                Attestations::<Test>::get(ALICE).unwrap().tier,
                AttestationTier::Bronze
            );
            // No stake was ever deposited.
            assert_eq!(ProviderStake::<Test>::get(ALICE), None);
        });
    }

    #[test]
    fn test_tier_assignment_by_gpu_count() {
        new_test_ext().execute_with(|| {
            // Bronze: 1 GPU
            stake_and_attest(ALICE, 500, 1);
            assert_eq!(
                Attestations::<Test>::get(ALICE).unwrap().tier,
                AttestationTier::Bronze
            );
            assert_eq!(
                Attestations::<Test>::get(ALICE)
                    .unwrap()
                    .tier
                    .staking_multiplier_bps(),
                1_000
            );

            // Silver: 4 GPUs
            stake_and_attest(BOB, 500, 4);
            assert_eq!(
                Attestations::<Test>::get(BOB).unwrap().tier,
                AttestationTier::Silver
            );
            assert_eq!(
                Attestations::<Test>::get(BOB)
                    .unwrap()
                    .tier
                    .staking_multiplier_bps(),
                1_500
            );
        });
    }

    #[test]
    fn test_gold_tier_sixteen_gpus() {
        new_test_ext().execute_with(|| {
            stake_and_attest(ALICE, 500, 16);
            assert_eq!(
                Attestations::<Test>::get(ALICE).unwrap().tier,
                AttestationTier::Gold
            );
            assert_eq!(
                Attestations::<Test>::get(ALICE)
                    .unwrap()
                    .tier
                    .staking_multiplier_bps(),
                2_500
            );
        });
    }

    #[test]
    fn test_zero_gpus_rejected() {
        new_test_ext().execute_with(|| {
            AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 500).unwrap();
            let result = AttestPallet::submit_attestation(
                RuntimeOrigin::signed(ALICE),
                [1u8; 32],
                [2u8; 32],
                0,
                10_000,
            );
            assert_eq!(result, Err(Error::<Test>::InvalidGPUCount.into()));
        });
    }

    #[test]
    fn test_record_stores_compute_gflops() {
        new_test_ext().execute_with(|| {
            stake_and_attest_gflops(ALICE, 500, 1, 912); // RTX 4060-ish
            let rec = Attestations::<Test>::get(ALICE).unwrap();
            assert_eq!(rec.compute_gflops, 912);
            assert_eq!(rec.tier, AttestationTier::Bronze);
        });
    }

    #[test]
    fn test_low_gflops_demotes_and_rejects_tier() {
        new_test_ext().execute_with(|| {
            AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 500).unwrap();
            // 1 GPU claims Bronze (needs >=100 GFLOPS) but only delivers 50 → rejected.
            let result = AttestPallet::submit_attestation(
                RuntimeOrigin::signed(ALICE),
                [1u8; 32],
                [2u8; 32],
                1,
                50,
            );
            assert_eq!(result, Err(Error::<Test>::InsufficientCompute.into()));
            assert!(Attestations::<Test>::get(ALICE).is_none());
        });
    }

    #[test]
    fn test_gflops_thresholds_per_tier() {
        new_test_ext().execute_with(|| {
            // Bronze needs >=100; exactly 100 passes.
            AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 500).unwrap();
            AttestPallet::submit_attestation(
                RuntimeOrigin::signed(ALICE),
                [1u8; 32],
                [2u8; 32],
                1,
                100,
            )
            .unwrap();
            assert_eq!(
                Attestations::<Test>::get(ALICE).unwrap().tier,
                AttestationTier::Bronze
            );

            // 4 GPUs claim Silver (needs >=1000) but deliver 500 → rejected.
            AttestPallet::deposit_stake(RuntimeOrigin::signed(BOB), 500).unwrap();
            let result = AttestPallet::submit_attestation(
                RuntimeOrigin::signed(BOB),
                [1u8; 32],
                [2u8; 32],
                4,
                500,
            );
            assert_eq!(result, Err(Error::<Test>::InsufficientCompute.into()));

            // 4 GPUs with 1500 GFLOPS → Silver (needs a verified challenge too).
            AttestPallet::confirm_challenge(RuntimeOrigin::signed(VERIFIER), BOB).unwrap();
            AttestPallet::submit_attestation(
                RuntimeOrigin::signed(BOB),
                [1u8; 32],
                [2u8; 32],
                4,
                1500,
            )
            .unwrap();
            assert_eq!(
                Attestations::<Test>::get(BOB).unwrap().tier,
                AttestationTier::Silver
            );
        });
    }

    #[test]
    fn test_below_eligibility_floor_cannot_attest() {
        // item 19: a device measuring below the eligibility floor (a CPU/APU
        // integrated GPU, ≈1–2 TFLOPS) is rejected outright — it cannot attest,
        // hence cannot earn — even when its GPU count would otherwise qualify.
        new_test_ext().execute_with(|| {
            AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 500).unwrap();
            let result = AttestPallet::submit_attestation(
                RuntimeOrigin::signed(ALICE),
                [1u8; 32],
                [2u8; 32],
                1,
                40, // < TestMinEligibilityGflops (50)
            );
            assert_eq!(result, Err(Error::<Test>::BelowEligibilityFloor.into()));
            assert!(Attestations::<Test>::get(ALICE).is_none());
        });
        // At/above the floor an otherwise-valid submission attests normally.
        new_test_ext().execute_with(|| {
            let result = AttestPallet::submit_attestation(
                RuntimeOrigin::signed(BOB),
                [1u8; 32],
                [2u8; 32],
                1,
                100, // >= floor (50) and >= Bronze (100)
            );
            assert!(result.is_ok());
            assert_eq!(
                Attestations::<Test>::get(BOB).unwrap().tier,
                AttestationTier::Bronze
            );
        });
    }

    #[test]
    fn test_effective_stake_multiplier() {
        new_test_ext().execute_with(|| {
            stake_and_attest(ALICE, 500, 16); // Gold = 2.5x → 500 * 2500/10000 = 125
            assert_eq!(AttestPallet::effective_stake(&ALICE), 125);

            // Unattested account has zero effective stake
            assert_eq!(AttestPallet::effective_stake(&BOB), 0);
        });
    }

    #[test]
    fn test_inactive_fullnode_capped_at_bronze() {
        new_test_ext().execute_with(|| {
            // Not an active full node → even a 16-GPU box is capped at Bronze.
            MOCK_ACTIVE.with(|c| c.set(false));
            stake_and_attest(ALICE, 500, 16);
            assert_eq!(
                Attestations::<Test>::get(ALICE).unwrap().tier,
                AttestationTier::Bronze
            );
        });
    }

    #[test]
    fn test_low_uptime_fullnode_capped_at_bronze() {
        new_test_ext().execute_with(|| {
            // Active but low uptime (below Silver's 90%) → capped at Bronze.
            MOCK_ACTIVE.with(|c| c.set(true));
            MOCK_UPTIME.with(|c| c.set(5_000)); // 50%
            stake_and_attest(ALICE, 500, 4); // would-be Silver
            assert_eq!(
                Attestations::<Test>::get(ALICE).unwrap().tier,
                AttestationTier::Bronze
            );
        });
    }

    #[test]
    fn test_bronze_not_fullnode_gated() {
        new_test_ext().execute_with(|| {
            // Bronze stays open to non-full-nodes (entry tier preserved).
            MOCK_ACTIVE.with(|c| c.set(false));
            stake_and_attest(ALICE, 500, 1);
            assert_eq!(
                Attestations::<Test>::get(ALICE).unwrap().tier,
                AttestationTier::Bronze
            );
        });
    }

    #[test]
    fn test_withdraw_stake_removes_record() {
        new_test_ext().execute_with(|| {
            stake_and_attest(ALICE, 500, 2);
            assert_eq!(AttestedProviderCount::<Test>::get(), 1);
            let before = pallet_balances::Pallet::<Test>::free_balance(ALICE);

            AttestPallet::withdraw_stake(RuntimeOrigin::signed(ALICE)).unwrap();
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(ALICE),
                before + 500
            );
            assert!(Attestations::<Test>::get(ALICE).is_none());
            assert!(ProviderStake::<Test>::get(ALICE).is_none());
            assert_eq!(AttestedProviderCount::<Test>::get(), 0);
            assert_eq!(TotalStaked::<Test>::get(), 0);
        });
    }

    #[test]
    fn test_withdraw_stake_rejects_when_none() {
        new_test_ext().execute_with(|| {
            let result = AttestPallet::withdraw_stake(RuntimeOrigin::signed(ALICE));
            assert_eq!(result, Err(Error::<Test>::NoStakeToWithdraw.into()));
        });
    }

    #[test]
    fn test_challenge_rotates_on_interval() {
        new_test_ext().execute_with(|| {
            assert_eq!(AttestationChallenge::<Test>::get(), INITIAL_CHALLENGE);
            // Rotate at block 10 (interval = 10)
            System::set_block_number(10);
            AttestPallet::on_initialize(10);
            let rotated = AttestationChallenge::<Test>::get();
            assert_ne!(rotated, INITIAL_CHALLENGE, "challenge should rotate");
            // No rotation at non-interval block
            System::set_block_number(11);
            AttestPallet::on_initialize(11);
            assert_eq!(AttestationChallenge::<Test>::get(), rotated);
        });
    }

    #[test]
    fn test_challenge_is_bound_to_parent_hash_not_just_block_number() {
        // Red-team finding #16: the rotation seed was `blake2_256(block_number)`
        // ⇒ fully predictable (the next rotation block is known in advance), so
        // future challenges could be pre-computed. The seed must be bound to the
        // parent block hash, which is unknown until the parent is produced.
        new_test_ext().execute_with(|| {
            System::set_block_number(10);
            System::set_parent_hash(H256::from([0xAAu8; 32]));
            AttestPallet::on_initialize(10);
            let a = AttestationChallenge::<Test>::get();

            System::set_block_number(10);
            System::set_parent_hash(H256::from([0xBBu8; 32]));
            AttestPallet::on_initialize(10);
            let b = AttestationChallenge::<Test>::get();

            assert_ne!(
                a, b,
                "challenge must depend on the parent block hash, not only the block number"
            );

            // And it is not the old, fully-predictable block-number seed.
            let old_predictable_seed =
                sp_io::hashing::blake2_256(&frame_system::Pallet::<Test>::block_number().encode());
            assert_ne!(a, old_predictable_seed);
            assert_ne!(b, old_predictable_seed);
        });
    }

    #[test]
    fn test_expired_attestation_slashes_after_missed_limit() {
        new_test_ext().execute_with(|| {
            stake_and_attest(ALICE, 1_000, 2);
            let initial_stake = ProviderStake::<Test>::get(ALICE).unwrap();

            // Advance beyond max age (50 blocks) repeatedly. Misses are only
            // counted once per `MaxAttestationAge` window (red-team fix), so
            // space the calls by a full window to accumulate the miss limit.
            for i in 0..TestMaxMissed::get() {
                System::set_block_number(100 + i as u64 * (TestMaxAge::get() + 1));
                AttestPallet::check_expired(RuntimeOrigin::signed(BOB), ALICE).unwrap();
            }

            // After reaching missed limit, 10% slashed (exactly once).
            let stake_after = ProviderStake::<Test>::get(ALICE).unwrap();
            let penalty = initial_stake / 10;
            assert_eq!(stake_after, initial_stake - penalty);
            assert_eq!(TotalStaked::<Test>::get(), initial_stake - penalty);
        });
    }

    /// REGRESSION (red-team 2026-10-09, finding #23, gpu-attestation /
    /// liveness-incentive evasion): a re-submitted attestation must NOT reset the
    /// accumulated miss history. Previously every `submit_attestation` set
    /// `missed_count = 0` and cleared `LastMissBlock`, so a provider that had gone
    /// idle could zero its miss count by simply re-attesting (the open Bronze tier
    /// needs no verified challenge) and dodge the `MaxMissedAttestations` slash
    /// forever. A re-submission must preserve the miss state; only a genuinely
    /// fresh attestation starts clean.
    #[test]
    fn test_resubmit_preserves_miss_history_and_does_not_evade_slash() {
        new_test_ext().execute_with(|| {
            stake_and_attest(ALICE, 1_000, 2);

            // Count one miss for ALICE.
            System::set_block_number(100 + TestMaxAge::get() + 1);
            AttestPallet::check_expired(RuntimeOrigin::signed(BOB), ALICE).unwrap();
            assert_eq!(Attestations::<Test>::get(ALICE).unwrap().missed_count, 1);

            // An idle provider tries to zero its miss history by re-attesting.
            AttestPallet::submit_attestation(
                RuntimeOrigin::signed(ALICE),
                [1u8; 32],
                [2u8; 32],
                2,
                10_000,
            )
            .unwrap();
            assert_eq!(
                Attestations::<Test>::get(ALICE).unwrap().missed_count,
                1,
                "re-submission must not reset the miss counter (slash evasion)"
            );

            // Two more misses reach the limit -> the 10% slash still fires, i.e.
            // the evasion is closed.
            let initial_stake = ProviderStake::<Test>::get(ALICE).unwrap();
            for blk in [300u64, 400u64] {
                System::set_block_number(blk);
                AttestPallet::check_expired(RuntimeOrigin::signed(BOB), ALICE).unwrap();
            }
            assert_eq!(
                ProviderStake::<Test>::get(ALICE).unwrap(),
                initial_stake - initial_stake / 10,
                "the missed-attestation slash must still be reachable after a re-attest"
            );

            // A genuinely fresh attestation, by contrast, starts at zero.
            AttestPallet::confirm_challenge(RuntimeOrigin::signed(VERIFIER), BOB).unwrap();
            AttestPallet::submit_attestation(
                RuntimeOrigin::signed(BOB),
                [3u8; 32],
                [4u8; 32],
                1,
                10_000,
            )
            .unwrap();
            assert_eq!(Attestations::<Test>::get(BOB).unwrap().missed_count, 0);
        });
    }

    /// REGRESSION (red-team 2026-10-08, finding #3, fund lock / D4 violation):
    /// a slash must actually BURN the penalty — remove it from total issuance —
    /// not merely shrink the accounting while the currency stays stranded in the
    /// pallet account (where `withdraw_stake` can never return it). The pallet
    /// account's free balance must drop by exactly `penalty` and end equal to the
    /// remaining (unslashed) stake.
    #[test]
    fn test_expired_slash_burns_currency() {
        new_test_ext().execute_with(|| {
            stake_and_attest(ALICE, 1_000, 2);
            let acct = AttestPallet::account_id();
            let before = pallet_balances::Pallet::<Test>::free_balance(acct.clone());
            assert_eq!(before, 1_000);

            for i in 0..TestMaxMissed::get() {
                System::set_block_number(100 + i as u64 * (TestMaxAge::get() + 1));
                AttestPallet::check_expired(RuntimeOrigin::signed(BOB), ALICE).unwrap();
            }

            let penalty = 1_000u128 / 10;
            let after = pallet_balances::Pallet::<Test>::free_balance(acct);
            assert_eq!(
                after,
                before - penalty,
                "slash must burn the penalty out of the pallet account"
            );
            // The pallet now holds exactly the remaining (unslashed) stake.
            assert_eq!(after, ProviderStake::<Test>::get(ALICE).unwrap());
        });
    }

    /// REGRESSION (red-team 2026-10-08): `check_expired` is permissionless and
    /// used to count one miss + apply one slash per *call*. Because the slash is
    /// a percentage of the *current* remaining stake, any signed account could
    /// call it repeatedly in a single block and drain an overdue provider's
    /// entire stake (1000 -> 900 -> 810 -> ...). A miss must only be counted once
    /// per `MaxAttestationAge` window, so repeated calls in the same window are a
    /// no-op.
    #[test]
    fn test_repeated_check_expired_does_not_drain_stake() {
        new_test_ext().execute_with(|| {
            stake_and_attest(ALICE, 1_000, 2);
            let initial = ProviderStake::<Test>::get(ALICE).unwrap();
            assert_eq!(initial, 1_000);

            // ALICE is now overdue (submitted_at = 1, max_age = 50). Hammer
            // `check_expired` in the SAME block, permissionlessly, as BOB.
            System::set_block_number(100);
            for _ in 0..10 {
                AttestPallet::check_expired(RuntimeOrigin::signed(BOB), ALICE).unwrap();
            }
            assert_eq!(
                ProviderStake::<Test>::get(ALICE).unwrap(),
                initial,
                "repeated same-window checks must not slash (drain) the stake"
            );
            // And the single overdue window counts at most one miss.
            assert!(
                Attestations::<Test>::get(ALICE).unwrap().missed_count <= 1,
                "at most one miss per attestation window"
            );
        });
    }

    #[test]
    fn test_expired_below_missed_limit_no_slash() {
        new_test_ext().execute_with(|| {
            stake_and_attest(ALICE, 1_000, 2);
            let initial_stake = ProviderStake::<Test>::get(ALICE).unwrap();

            // Only 1 missed — below limit of 3 — no slash.
            System::set_block_number(100);
            AttestPallet::check_expired(RuntimeOrigin::signed(BOB), ALICE).unwrap();
            assert_eq!(ProviderStake::<Test>::get(ALICE).unwrap(), initial_stake);

            // Record tracks the miss.
            assert_eq!(Attestations::<Test>::get(ALICE).unwrap().missed_count, 1);
        });
    }

    #[test]
    fn test_check_expired_no_attestation_error() {
        new_test_ext().execute_with(|| {
            let result = AttestPallet::check_expired(RuntimeOrigin::signed(BOB), ALICE);
            assert_eq!(result, Err(Error::<Test>::NoExistingAttestation.into()));
        });
    }

    // ── Part 2: node-verified capacity challenge ──────────────────────────

    #[test]
    fn test_confirm_challenge_records_current_challenge() {
        new_test_ext().execute_with(|| {
            assert!(!AttestPallet::challenge_verified(&ALICE));
            AttestPallet::confirm_challenge(RuntimeOrigin::signed(VERIFIER), ALICE).unwrap();
            assert_eq!(
                ChallengeVerified::<Test>::get(ALICE),
                Some(INITIAL_CHALLENGE)
            );
            assert!(AttestPallet::challenge_verified(&ALICE));
        });
    }

    #[test]
    fn test_confirm_challenge_verifier_only() {
        new_test_ext().execute_with(|| {
            // A random signed account cannot confirm challenges.
            let result = AttestPallet::confirm_challenge(RuntimeOrigin::signed(ALICE), BOB);
            assert_eq!(result, Err(Error::<Test>::NotChallengeVerifier.into()));
            assert!(ChallengeVerified::<Test>::get(BOB).is_none());
        });
    }

    #[test]
    fn test_confirm_challenge_requires_active_challenge() {
        new_test_ext().execute_with(|| {
            AttestationChallenge::<Test>::put([0u8; 32]);
            let result = AttestPallet::confirm_challenge(RuntimeOrigin::signed(VERIFIER), ALICE);
            assert_eq!(result, Err(Error::<Test>::NoActiveChallenge.into()));
        });
    }

    #[test]
    fn test_unverified_high_tier_capped_to_bronze() {
        new_test_ext().execute_with(|| {
            // 16-GPU, high-GFLOPS box, but no node-verified challenge → Bronze.
            AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 500).unwrap();
            AttestPallet::submit_attestation(
                RuntimeOrigin::signed(ALICE),
                [1u8; 32],
                [2u8; 32],
                16,
                10_000,
            )
            .unwrap();
            assert_eq!(
                Attestations::<Test>::get(ALICE).unwrap().tier,
                AttestationTier::Bronze
            );
        });
    }

    #[test]
    fn test_verified_high_tier_allowed() {
        new_test_ext().execute_with(|| {
            AttestPallet::confirm_challenge(RuntimeOrigin::signed(VERIFIER), ALICE).unwrap();
            AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 500).unwrap();
            AttestPallet::submit_attestation(
                RuntimeOrigin::signed(ALICE),
                [1u8; 32],
                [2u8; 32],
                4,
                10_000,
            )
            .unwrap();
            assert_eq!(
                Attestations::<Test>::get(ALICE).unwrap().tier,
                AttestationTier::Silver
            );
        });
    }

    #[test]
    fn test_challenge_rotation_invalidates_verification() {
        new_test_ext().execute_with(|| {
            AttestPallet::confirm_challenge(RuntimeOrigin::signed(VERIFIER), ALICE).unwrap();
            assert!(AttestPallet::challenge_verified(&ALICE));
            // Rotate the challenge: the stored solution no longer matches.
            System::set_block_number(10);
            AttestPallet::on_initialize(10);
            assert!(!AttestPallet::challenge_verified(&ALICE));
        });
    }

    #[test]
    fn test_legacy_record_not_demoted_on_expiry_sweep() {
        new_test_ext().execute_with(|| {
            // Simulate a pre-upgrade Silver record: tier stored, no
            // ChallengeVerified entry. The expiry sweep must leave the tier
            // alone (challenge gate is submission-only).
            AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 1_000).unwrap();
            AttestPallet::confirm_challenge(RuntimeOrigin::signed(VERIFIER), ALICE).unwrap();
            AttestPallet::submit_attestation(
                RuntimeOrigin::signed(ALICE),
                [1u8; 32],
                [2u8; 32],
                4,
                10_000,
            )
            .unwrap();
            assert_eq!(
                Attestations::<Test>::get(ALICE).unwrap().tier,
                AttestationTier::Silver
            );
            // Drop the verification, then run a sweep past max age.
            ChallengeVerified::<Test>::remove(ALICE);
            System::set_block_number(100);
            AttestPallet::check_expired(RuntimeOrigin::signed(BOB), ALICE).unwrap();
            assert_eq!(
                Attestations::<Test>::get(ALICE).unwrap().tier,
                AttestationTier::Silver,
                "legacy/unverified record must not be demoted by the sweep"
            );
        });
    }
}
