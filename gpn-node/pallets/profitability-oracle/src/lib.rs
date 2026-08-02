//! # Profitability Oracle Pallet
//!
//! On-chain data feeds for GPU profitability calculations.

#![cfg_attr(not(feature = "std"), no_std)]

use codec::{Decode, Encode};
use scale_info::TypeInfo;
use core::fmt::Debug;

pub use pallet::*;

#[frame_support::pallet]
pub mod pallet {
    use super::*;
    use frame_support::{pallet_prelude::*, traits::{Currency, ExistenceRequirement}, BoundedVec};
    use frame_support::sp_runtime::traits::AccountIdConversion;
    use frame_system::pallet_prelude::*;
    use sp_runtime::traits::Saturating;
    use sp_std::vec::Vec;

    /// Balance type backed by the configured `Currency` (Phase C).
    pub type BalanceOf<T> =
        <<T as Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::Balance;

    #[derive(Clone, Encode, Decode, PartialEq, Eq, Debug, TypeInfo, MaxEncodedLen)]
    pub struct ProfitabilityFeed {
        pub token_price_usd: u64,
        pub avg_gpu_hour_price: u64,
        pub utilization_rate_bps: u32,
        pub staking_apr_bps: u32,
        pub updated_at: u64,
    }

    /// A pending operator-admission proposal (oracle-governance §6 gap #4,
    /// Phase B). A committee member proposes an operator and self-approves;
    /// other committee members `approve_operator` until `AdmissionThreshold`
    /// approvals are reached, at which point the operator is admitted. This
    /// removes the single-root capture point for the risky direction (adding
    /// operators to the trusted feed-writer set) without a new pallet.
    #[derive(Clone, Encode, Decode, PartialEq, Eq, Debug, TypeInfo, MaxEncodedLen)]
    #[scale_info(skip_type_params(T))]
    pub struct PendingAdmission<T: Config> {
        pub proposer: T::AccountId,
        pub approvals: BoundedVec<T::AccountId, T::CommitteeSize>,
    }

    #[pallet::config]
    pub trait Config: frame_system::Config {
        #[pallet::constant]
        type MaxOracleOperators: Get<u32>;
        #[pallet::constant]
        type OracleUpdateInterval: Get<BlockNumberFor<Self>>;
        /// Max size of the admission committee (bounds the approvals BoundedVec).
        #[pallet::constant]
        type CommitteeSize: Get<u32>;
        /// Number of committee approvals required to admit a new operator.
        /// Must be >= 1. With threshold 1 the proposer admits immediately.
        #[pallet::constant]
        type AdmissionThreshold: Get<u32>;
        /// Bond required to become a permissionless oracle operator (Phase C).
        #[pallet::constant]
        type OperatorBond: Get<BalanceOf<Self>>;
        /// Bond required to file a slashing challenge (Phase C).
        #[pallet::constant]
        type ChallengeBond: Get<BalanceOf<Self>>;
        /// Deviation (in basis points) above which an operator's feed is an
        /// "outlier" and slashable (Phase C).
        #[pallet::constant]
        type OutlierThresholdBps: Get<u32>;
        /// Blocks without a feed after which an operator is "stale" and
        /// slashable (Phase C).
        #[pallet::constant]
        type StaleOperatorBlocks: Get<BlockNumberFor<Self>>;
        /// Portion (in basis points) of the operator bond slashed on a
        /// successful challenge (Phase C). Decision D4 defaults to BURN
        /// (deflation, aligns with the 20% burn in `gpn-token`); configurable
        /// via `SlashBps`.
        #[pallet::constant]
        type SlashBps: Get<u32>;
        /// The currency used to hold operator/challenge bonds (Phase C).
        type Currency: Currency<Self::AccountId>;
    }

    #[pallet::pallet]
    pub struct Pallet<T>(_);

    #[pallet::storage]
    pub type CurrentFeed<T: Config> = StorageValue<_, ProfitabilityFeed, OptionQuery>;

    /// Latest feed **per operator** (oracle-governance §6 gap #3: enables
    /// client-side median aggregation across operators). `CurrentFeed` remains
    /// the effective / latest value; this map lets consumers read each
    /// operator's own latest feed independently and aggregate (e.g. take the
    /// median) so one outlier operator can't unilaterally move the effective
    /// feed. Additive + backward-compatible: populated on every `update_feed`;
    /// never read by existing consumers.
    #[pallet::storage]
    pub type OperatorFeeds<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, ProfitabilityFeed, OptionQuery>;

    #[pallet::storage]
    pub type LastUpdateBlock<T: Config> = StorageValue<_, BlockNumberFor<T>, ValueQuery>;

    #[pallet::storage]
    pub type OracleOperators<T: Config> = StorageMap<_, Blake2_128Concat, T::AccountId, (), OptionQuery>;

    #[pallet::storage]
    pub type OperatorCount<T: Config> = StorageValue<_, u32, ValueQuery>;

    /// Named admission committee (oracle-governance §6 gap #4, Phase B).
    /// Genesis-seeded; `propose_operator`/`approve_operator` require the signer
    /// to be a committee member. Removes the single-root capture point for
    /// adding operators. Mainnet seeds this EMPTY (inert by default).
    #[pallet::storage]
    pub type Committee<T: Config> = StorageMap<_, Blake2_128Concat, T::AccountId, (), OptionQuery>;

    /// Pending operator-admission proposals awaiting committee approvals.
    #[pallet::storage]
    pub type PendingAdmissions<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, PendingAdmission<T>, OptionQuery>;

    /// GPN bonded by a permissionless oracle operator (Phase C, §6 gap #5).
    /// Presence here = a bonded operator who may call `update_feed`; the value
    /// is the bond size, which is the operator's weight in the on-chain
    /// bond-weighted median `effective_feed()`. Sybil resistance comes from the
    /// bond cost + slashing (mirrors the 1,000 GPN attestation stake).
    #[pallet::storage]
    pub type StakedOperators<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, BalanceOf<T>, ValueQuery>;

    /// Per-operator staleness: the last block each operator submitted a feed.
    /// Independent of the global `LastUpdateBlock` (which Phase A/B uses for
    /// the anti-spam throttle); used to detect stale operators for slashing.
    #[pallet::storage]
    pub type OperatorLastFeedBlock<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, BlockNumberFor<T>, OptionQuery>;

    /// A pending slashing challenge against a bonded operator (Phase C).
    #[derive(Clone, Encode, Decode, PartialEq, Eq, Debug, TypeInfo, MaxEncodedLen)]
    #[scale_info(skip_type_params(T))]
    pub struct Challenge<T: Config> {
        /// Who submitted the challenge (must post `ChallengeBond`).
        pub challenger: T::AccountId,
        /// The operator being challenged.
        pub target: T::AccountId,
        /// Block at which the challenge was opened.
        pub opened_at: BlockNumberFor<T>,
        /// Bond posted by the challenger (returned if the challenge succeeds,
        /// slashed if the challenge is dismissed as frivolous).
        pub bond: BalanceOf<T>,
    }

    /// The verdict a resolver issues against a challenged operator.
    #[derive(Clone, Encode, Decode, PartialEq, Eq, Debug, TypeInfo, MaxEncodedLen)]
    pub enum ChallengeVerdict {
        /// Operator stopped posting feeds past `StaleOperatorBlocks`.
        SlashStale,
        /// Operator's latest feed deviates > `OutlierThresholdBps` from the
        /// on-chain bond-weighted median on some field.
        SlashOutlier,
        /// Challenge is unfounded (within threshold + not stale): the
        /// challenger's bond is slashed as a frivolous-challenge deterrent.
        Dismiss,
    }

    impl ChallengeVerdict {
        /// Map a `u8` discriminant (the extrinsic wire format for this enum,
        /// avoiding `DecodeWithMemTracking` on the call arg in this Substrate
        /// version) back to a verdict. 0/1/2 → the three variants.
        pub fn from_u8(v: u8) -> Option<Self> {
            match v {
                0 => Some(Self::SlashStale),
                1 => Some(Self::SlashOutlier),
                _ => Some(Self::Dismiss),
            }
        }

        /// The u8 wire discriminant of this verdict.
        pub fn discriminant(&self) -> u8 {
            match self {
                Self::SlashStale => 0,
                Self::SlashOutlier => 1,
                Self::Dismiss => 2,
            }
        }
    }

    /// Open challenges awaiting resolution, keyed by the challenged operator.
    #[pallet::storage]
    pub type OpenChallenges<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, Challenge<T>, OptionQuery>;

    #[pallet::genesis_config]
    #[derive(frame_support::DefaultNoBound)]
    pub struct GenesisConfig<T: Config> {
        pub initial_operators: Vec<T::AccountId>,
        /// Genesis-seeded admission committee (Phase B).
        pub initial_committee: Vec<T::AccountId>,
        #[serde(skip)]
        pub _config: sp_std::marker::PhantomData<T>,
    }

    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            let zero_block: BlockNumberFor<T> = Zero::zero();
            <LastUpdateBlock<T>>::put(zero_block);
            // Insert initial operators
            for op in &self.initial_operators {
                <OracleOperators<T>>::insert(op, ());
            }
            <OperatorCount<T>>::put(self.initial_operators.len() as u32);
            // Insert the admission committee (Phase B).
            for c in &self.initial_committee {
                <Committee<T>>::insert(c, ());
            }
        }
    }

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        FeedUpdated { token_price_usd: u64, staking_apr_bps: u32 },
        OracleFeedUpdated(u64),
        OperatorAdded { who: T::AccountId },
        OperatorRemoved { who: T::AccountId },
        OperatorProposed { operator: T::AccountId, proposer: T::AccountId },
        OperatorApproved { operator: T::AccountId, approver: T::AccountId },
        // ── Phase C (staking-backed slashing oracle) ──
        OperatorBonded { who: T::AccountId, bond: BalanceOf<T> },
        OperatorBondIncreased { who: T::AccountId, additional: BalanceOf<T> },
        OperatorUnbonded { who: T::AccountId, bond: BalanceOf<T> },
        ChallengeOpened { target: T::AccountId, challenger: T::AccountId },
        /// `verdict` is the u8 discriminant (0=SlashStale, 1=SlashOutlier, 2=Dismiss).
        ChallengeResolved { target: T::AccountId, verdict: u8 },
    }

    #[pallet::error]
    pub enum Error<T> {
        UpdateTooSoon,
        InvalidPrice,
        NotAuthorized,
        AlreadyOperator,
        NotOperator,
        MaxOperatorsReached,
        NotCommitteeMember,
        NoPendingAdmission,
        AlreadyApproved,
        AlreadyPending,
        ApprovalLimitReached,
        // ── Phase C (staking-backed slashing oracle) ──
        AlreadyStakedOperator,
        NotStakedOperator,
        AlreadyChallenged,
        NoOpenChallenge,
        NotStaleYet,
        NotOutlier,
        NoFeed,
        ZeroAmount,
    }

    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {}

    // ════════════════════════════════════════════
    // Dispatchable calls
    // ════════════════════════════════════════════

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// Submit a profitability feed update.
        /// Only authorized oracle operators may call this.
        #[pallet::call_index(0)]
        #[pallet::weight(10_000)]
        pub fn update_feed(
            origin: OriginFor<T>,
            token_price_usd: u64,
            avg_gpu_hour_price: u64,
            utilization_rate_bps: u32,
            staking_apr_bps: u32,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;

            // Phase C: a bonded (permissionless) operator is authorized to post
            // in addition to the committee-approved set (Phase A/B).
            let is_committee = <OracleOperators<T>>::contains_key(&who);
            let is_staked = <StakedOperators<T>>::contains_key(&who);
            ensure!(is_committee || is_staked, Error::<T>::NotAuthorized);

            let block_number = frame_system::Pallet::<T>::block_number();
            // Per-operator anti-spam throttle (Phase C): each operator is gated
            // by its OWN last-posted block, so independent (committee or bonded)
            // operators can each post their own feed without blocking each
            // other. The global `LastUpdateBlock` is still updated as the
            // "latest post" marker (backward-compatible) but no longer gates.
            let last = <OperatorLastFeedBlock<T>>::get(&who).unwrap_or_else(Zero::zero);
            ensure!(block_number >= last + T::OracleUpdateInterval::get() || last.is_zero(), Error::<T>::UpdateTooSoon);
            ensure!(token_price_usd > 0, Error::<T>::InvalidPrice);

            let feed = ProfitabilityFeed {
                token_price_usd, avg_gpu_hour_price,
                utilization_rate_bps, staking_apr_bps,
                updated_at: block_number.try_into().unwrap_or(0),
            };
            <CurrentFeed<T>>::put(feed.clone());
            // Per-operator latest feed (oracle-governance §6 gap #3).
            <OperatorFeeds<T>>::insert(&who, feed);
            // Per-operator staleness bookkeeping (Phase C).
            <OperatorLastFeedBlock<T>>::insert(&who, block_number);
            <LastUpdateBlock<T>>::put(block_number);
            Self::deposit_event(Event::<T>::FeedUpdated { token_price_usd, staking_apr_bps });
            Ok(())
        }

        /// Propose a new oracle operator (Phase B committee admission, §6 gap
        /// #4). The signer must be a committee member; the proposer
        /// self-approves. If `AdmissionThreshold == 1` the operator is admitted
        /// immediately; otherwise a pending proposal is stored for other
        /// committee members to `approve_operator`.
        #[pallet::call_index(1)]
        #[pallet::weight(10_000)]
        pub fn propose_operator(
            origin: OriginFor<T>,
            operator: T::AccountId,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(<Committee<T>>::contains_key(&who), Error::<T>::NotCommitteeMember);
            ensure!(!<OracleOperators<T>>::contains_key(&operator), Error::<T>::AlreadyOperator);
            ensure!(!<PendingAdmissions<T>>::contains_key(&operator), Error::<T>::AlreadyPending);
            let count = <OperatorCount<T>>::get();
            ensure!(count < T::MaxOracleOperators::get(), Error::<T>::MaxOperatorsReached);

            let mut approvals = BoundedVec::new();
            approvals.try_push(who.clone()).map_err(|_| Error::<T>::ApprovalLimitReached)?;

            let threshold = T::AdmissionThreshold::get().max(1);
            if (approvals.len() as u32) >= threshold {
                // Single-approval threshold: admit immediately.
                Self::admit_operator(operator, count);
            } else {
                let pending = PendingAdmission { proposer: who.clone(), approvals };
                <PendingAdmissions<T>>::insert(&operator, pending);
                Self::deposit_event(Event::<T>::OperatorProposed { operator, proposer: who });
            }
            Ok(())
        }

        /// Approve a pending operator-admission proposal (Phase B). The signer
        /// must be a committee member. When approvals reach
        /// `AdmissionThreshold`, the operator is admitted and the proposal is
        /// cleared.
        #[pallet::call_index(2)]
        #[pallet::weight(10_000)]
        pub fn approve_operator(
            origin: OriginFor<T>,
            operator: T::AccountId,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(<Committee<T>>::contains_key(&who), Error::<T>::NotCommitteeMember);
            let mut pending = <PendingAdmissions<T>>::get(&operator)
                .ok_or(Error::<T>::NoPendingAdmission)?;
            ensure!(!pending.approvals.contains(&who), Error::<T>::AlreadyApproved);

            pending.approvals.try_push(who.clone())
                .map_err(|_| Error::<T>::ApprovalLimitReached)?;

            let threshold = T::AdmissionThreshold::get().max(1);
            if (pending.approvals.len() as u32) >= threshold {
                // Threshold reached: admit and clear the proposal.
                <PendingAdmissions<T>>::remove(&operator);
                let count = <OperatorCount<T>>::get();
                Self::admit_operator(operator, count);
            } else {
                <PendingAdmissions<T>>::insert(&operator, pending);
                Self::deposit_event(Event::<T>::OperatorApproved { operator, approver: who });
            }
            Ok(())
        }

        /// Remove an oracle operator. Allowed for any **committee member**
        /// (normal governance removal) or **root** (emergency break-glass; on
        /// mainnet root is inert so only the committee path is live). Removal
        /// only shrinks the trusted set, so it is intentionally easier than
        /// addition (which requires the committee threshold).
        #[pallet::call_index(3)]
        #[pallet::weight(10_000)]
        pub fn remove_operator(
            origin: OriginFor<T>,
            operator: T::AccountId,
        ) -> DispatchResult {
            let is_committee = match ensure_signed(origin.clone()) {
                Ok(who) => <Committee<T>>::contains_key(&who),
                Err(_) => false,
            };
            if !is_committee {
                ensure_root(origin)?;
            }
            ensure!(<OracleOperators<T>>::contains_key(&operator), Error::<T>::NotOperator);
            <OracleOperators<T>>::remove(&operator);
            <OperatorCount<T>>::mutate(|c| *c = c.saturating_sub(1));
            Self::deposit_event(Event::<T>::OperatorRemoved { who: operator });
            Ok(())
        }

        /// Bond GPN to become a permissionless oracle operator (Phase C). Any
        /// signed account may call; the bond is transferred into the pallet
        /// account and the caller becomes a bonded operator who may post feeds
        /// and whose weight in `effective_feed()` equals the bond size.
        #[pallet::call_index(4)]
        #[pallet::weight(10_000)]
        pub fn bond_operator(origin: OriginFor<T>) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(!<StakedOperators<T>>::contains_key(&who), Error::<T>::AlreadyStakedOperator);
            let total = <OracleOperators<T>>::iter().count() as u32 + <StakedOperators<T>>::iter().count() as u32;
            ensure!(total < T::MaxOracleOperators::get(), Error::<T>::MaxOperatorsReached);

            let bond = T::OperatorBond::get();
            T::Currency::transfer(&who, &Self::account_id(), bond, ExistenceRequirement::KeepAlive)?;
            <StakedOperators<T>>::insert(&who, bond);
            Self::deposit_event(Event::<T>::OperatorBonded { who, bond });
            Ok(())
        }

        /// Increase an existing bonded operator's bond (Phase C), raising their
        /// weight in the bond-weighted median.
        #[pallet::call_index(5)]
        #[pallet::weight(10_000)]
        pub fn bond_more(origin: OriginFor<T>, additional: BalanceOf<T>) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(<StakedOperators<T>>::contains_key(&who), Error::<T>::NotStakedOperator);
            ensure!(!additional.is_zero(), Error::<T>::ZeroAmount);
            T::Currency::transfer(&who, &Self::account_id(), additional, ExistenceRequirement::KeepAlive)?;
            <StakedOperators<T>>::mutate(&who, |b| *b = b.saturating_add(additional));
            Self::deposit_event(Event::<T>::OperatorBondIncreased { who, additional });
            Ok(())
        }

        /// Exit as a bonded operator: return the full bond (Phase C). Blocked
        /// while a challenge is open against the caller (prevents slashing
        /// evasion, decision D4 refinement).
        #[pallet::call_index(6)]
        #[pallet::weight(10_000)]
        pub fn unbond_operator(origin: OriginFor<T>) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(<StakedOperators<T>>::contains_key(&who), Error::<T>::NotStakedOperator);
            ensure!(!<OpenChallenges<T>>::contains_key(&who), Error::<T>::AlreadyChallenged);
            let bond = <StakedOperators<T>>::take(&who);
            // AllowDeath: the pallet account may be drained below ED when the
            // last bond is returned.
            T::Currency::transfer(&Self::account_id(), &who, bond, ExistenceRequirement::AllowDeath)?;
            <OperatorLastFeedBlock<T>>::remove(&who);
            Self::deposit_event(Event::<T>::OperatorUnbonded { who, bond });
            Ok(())
        }

        /// Open a slashing challenge against a bonded operator (Phase C). The
        /// challenger posts `ChallengeBond`. The target may not unbond until
        /// the challenge is resolved.
        #[pallet::call_index(7)]
        #[pallet::weight(10_000)]
        pub fn challenge_operator(origin: OriginFor<T>, target: T::AccountId) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(<StakedOperators<T>>::contains_key(&target), Error::<T>::NotStakedOperator);
            ensure!(!<OpenChallenges<T>>::contains_key(&target), Error::<T>::AlreadyChallenged);

            let bond = T::ChallengeBond::get();
            T::Currency::transfer(&who, &Self::account_id(), bond, ExistenceRequirement::KeepAlive)?;
            let challenge = Challenge {
                challenger: who.clone(),
                target: target.clone(),
                opened_at: frame_system::Pallet::<T>::block_number(),
                bond,
            };
            <OpenChallenges<T>>::insert(&target, challenge);
            Self::deposit_event(Event::<T>::ChallengeOpened { target, challenger: who });
            Ok(())
        }

        /// Resolve an open challenge against a bonded operator (Phase C).
        ///
        /// - `SlashStale`: operator hasn't posted in `StaleOperatorBlocks` →
        ///   `SlashBps` of their bond is slashed (burned per D4 default).
        /// - `SlashOutlier`: operator's feed deviates > `OutlierThresholdBps`
        ///   from the bond-weighted median on some field → same slash.
        /// - `Dismiss`: unfounded challenge → the challenger's bond is slashed
        ///   (frivolous-challenge deterrent), target untouched.
        ///
        /// In all cases the challenge is closed. On success the challenger's
        /// bond is returned.
        #[pallet::call_index(8)]
        #[pallet::weight(10_000)]
        pub fn resolve_challenge(
            origin: OriginFor<T>,
            target: T::AccountId,
            verdict: u8,
        ) -> DispatchResult {
            ensure_signed(origin)?;
            let verdict = ChallengeVerdict::from_u8(verdict).ok_or(Error::<T>::InvalidPrice)?;
            let verdict_disc = verdict.clone().discriminant();
            let challenge = <OpenChallenges<T>>::get(&target).ok_or(Error::<T>::NoOpenChallenge)?;
            let bond = <StakedOperators<T>>::get(&target);
            let now = frame_system::Pallet::<T>::block_number();

            match verdict {
                ChallengeVerdict::SlashStale => {
                    let last = <OperatorLastFeedBlock<T>>::get(&target).ok_or(Error::<T>::NoFeed)?;
                    ensure!(now >= last + T::StaleOperatorBlocks::get(), Error::<T>::NotStaleYet);
                    Self::slash_operator(&target, bond);
                    Self::return_challenger_bond(&challenge);
                }
                ChallengeVerdict::SlashOutlier => {
                    ensure!(Self::is_outlier(&target), Error::<T>::NotOutlier);
                    Self::slash_operator(&target, bond);
                    Self::return_challenger_bond(&challenge);
                }
                ChallengeVerdict::Dismiss => {
                    // Frivolous-challenge deterrent: burn the challenger's bond.
                    let (imbalance, _) = T::Currency::slash(&Self::account_id(), challenge.bond);
                    drop(imbalance);
                }
            }
            <OpenChallenges<T>>::remove(&target);
            Self::deposit_event(Event::<T>::ChallengeResolved { target, verdict: verdict_disc });
            Ok(())
        }
    }

    impl<T: Config> Pallet<T> {
        /// The pallet's account, which holds operator/challenge bonds (Phase C).
        pub fn account_id() -> T::AccountId {
            frame_support::PalletId(*b"gpn/orcl").into_account_truncating()
        }

        /// Insert an operator into the trusted set and bump the count.
        /// Shared by the immediate-admit (threshold 1) and threshold-reached
        /// paths so admission semantics stay in one place.
        fn admit_operator(operator: T::AccountId, current_count: u32) {
            <OracleOperators<T>>::insert(&operator, ());
            <OperatorCount<T>>::put(current_count + 1);
            Self::deposit_event(Event::<T>::OperatorAdded { who: operator });
        }

        // ── Phase C (staking-backed slashing oracle) helpers ──

        /// Slash `SlashBps` of the operator's bond (burn per D4 default).
        /// If the remaining bond is zero the operator is removed from the
        /// permissionless set; otherwise their weight is reduced.
        fn slash_operator(target: &T::AccountId, bond: BalanceOf<T>) {
            if bond.is_zero() {
                return;
            }
            let slash_amount = bond * T::SlashBps::get().into() / 10_000u32.into();
            if !slash_amount.is_zero() {
                let (imbalance, _) = T::Currency::slash(&Self::account_id(), slash_amount);
                drop(imbalance); // negative imbalance = burned (removed from supply)
            }
            let remaining = bond.saturating_sub(slash_amount);
            if remaining.is_zero() {
                <StakedOperators<T>>::remove(target);
                <OperatorLastFeedBlock<T>>::remove(target);
            } else {
                <StakedOperators<T>>::insert(target, remaining);
            }
        }

        /// Return a successful challenger's bond from the pallet account.
        fn return_challenger_bond(challenge: &Challenge<T>) {
            // AllowDeath: the pallet account may be drained below ED.
            let _ = T::Currency::transfer(
                &Self::account_id(),
                &challenge.challenger,
                challenge.bond,
                ExistenceRequirement::AllowDeath,
            );
        }

        /// True if `target`'s latest feed deviates > `OutlierThresholdBps` from
        /// the on-chain bond-weighted median on any field (and is thus
        /// slashable as an outlier).
        fn is_outlier(target: &T::AccountId) -> bool {
            let Some(target_feed) = <OperatorFeeds<T>>::get(target) else {
                return false;
            };
            let Some(median) = Self::effective_feed() else {
                return false;
            };
            let threshold = T::OutlierThresholdBps::get() as u64;
            Self::deviation_bps(target_feed.token_price_usd, median.token_price_usd) > threshold
                || Self::deviation_bps(target_feed.avg_gpu_hour_price, median.avg_gpu_hour_price) > threshold
                || Self::deviation_bps(target_feed.utilization_rate_bps as u64, median.utilization_rate_bps as u64) > threshold
                || Self::deviation_bps(target_feed.staking_apr_bps as u64, median.staking_apr_bps as u64) > threshold
        }

        /// Relative deviation (in basis points) between two values.
        fn deviation_bps(a: u64, b: u64) -> u64 {
            if a == b {
                return 0;
            }
            let (big, small) = if a > b { (a, b) } else { (b, a) };
            if small == 0 {
                return u64::MAX;
            }
            (big - small) * 10_000 / small
        }

        /// The on-chain **bond-weighted median** feed across all bonded
        /// operators who have posted a feed (Phase C aggregation). Drops stale
        /// entries. Returns `None` if no bonded operator has posted. Each field
        /// is aggregated independently; `updated_at` is the max of the entries.
        ///
        /// `pub` so the runtime API (and thus the node RPC + agent read tool)
        /// can surface the authoritative on-chain median, instead of only a
        /// client-side reimplementation.
        pub fn effective_feed() -> Option<ProfitabilityFeed> {
            let mut entries: Vec<(BalanceOf<T>, ProfitabilityFeed)> = Vec::new();
            for (who, bond) in <StakedOperators<T>>::iter() {
                if let Some(feed) = <OperatorFeeds<T>>::get(&who) {
                    entries.push((bond, feed));
                }
            }
            if entries.is_empty() {
                return None;
            }
            let token: Vec<(u64, BalanceOf<T>)> =
                entries.iter().map(|(b, f)| (f.token_price_usd, *b)).collect();
            let gpu: Vec<(u64, BalanceOf<T>)> =
                entries.iter().map(|(b, f)| (f.avg_gpu_hour_price, *b)).collect();
            let util: Vec<(u64, BalanceOf<T>)> =
                entries.iter().map(|(b, f)| (f.utilization_rate_bps as u64, *b)).collect();
            let apr: Vec<(u64, BalanceOf<T>)> =
                entries.iter().map(|(b, f)| (f.staking_apr_bps as u64, *b)).collect();
            let updated_at = entries.iter().map(|(_, f)| f.updated_at).max().unwrap_or(0);
            Some(ProfitabilityFeed {
                token_price_usd: Self::bond_weighted_median(&token),
                avg_gpu_hour_price: Self::bond_weighted_median(&gpu),
                utilization_rate_bps: Self::bond_weighted_median(&util) as u32,
                staking_apr_bps: Self::bond_weighted_median(&apr) as u32,
                updated_at,
            })
        }

        /// Bond-weighted median of `(value, weight)` pairs: sort by value, walk
        /// cumulative weight, take the value at the 50%-weight point. Falls back
        /// to a plain median if total weight is zero.
        fn bond_weighted_median(values: &[(u64, BalanceOf<T>)]) -> u64 {
            if values.is_empty() {
                return 0;
            }
            let mut v: Vec<(u64, BalanceOf<T>)> = values.to_vec();
            v.sort_by_key(|(val, _)| *val);
            let total: BalanceOf<T> =
                v.iter().fold(Zero::zero(), |acc, (_, w)| acc.saturating_add(*w));
            if total.is_zero() {
                return v[v.len() / 2].0;
            }
            let half = total / 2u32.into();
            let mut cum: BalanceOf<T> = Zero::zero();
            for (val, w) in &v {
                cum = cum.saturating_add(*w);
                if cum >= half {
                    return *val;
                }
            }
            v[v.len() - 1].0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frame_support::{
        construct_runtime, derive_impl, parameter_types,
        traits::{BuildGenesisConfig, ConstU16, ConstU32, ConstU64, Everything},
    };
    use sp_core::H256;
    use sp_runtime::{
        traits::{BlakeTwo256, IdentityLookup},
        BuildStorage,
    };

    use crate::pallet as local_oracle;
    use crate::pallet::{
        CurrentFeed, LastUpdateBlock, OperatorCount, OracleOperators, Committee, PendingAdmissions,
        StakedOperators, OperatorLastFeedBlock, OpenChallenges, ChallengeVerdict,
    };

    type Block = frame_system::mocking::MockBlock<Test>;
    type Balance = u64;

    construct_runtime!(
        pub struct Test {
            System: frame_system,
            Balances: pallet_balances,
            Oracle: local_oracle,
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

    parameter_types! {
        pub const ExistentialDeposit: Balance = 1;
    }
    impl pallet_balances::Config for Test {
        type Balance = Balance;
        type DustRemoval = ();
        type RuntimeEvent = RuntimeEvent;
        type ExistentialDeposit = ExistentialDeposit;
        type AccountStore = System;
        type WeightInfo = ();
        type MaxLocks = ConstU32<50>;
        type MaxReserves = ConstU32<50>;
        type ReserveIdentifier = [u8; 8];
        type FreezeIdentifier = ();
        type RuntimeHoldReason = ();
        type RuntimeFreezeReason = ();
        type MaxFreezes = ConstU32<0>;
        type DoneSlashHandler = ();
    }

    parameter_types! {
        pub const TestMaxOperators: u32 = 10;
        pub const TestOracleInterval: u64 = 5;
        pub const TestCommitteeSize: u32 = 5;
        pub const TestAdmissionThreshold: u32 = 2;
        // ── Phase C ──
        pub const TestOperatorBond: Balance = 1_000;
        pub const TestChallengeBond: Balance = 100;
        pub const TestOutlierThresholdBps: u32 = 2_000; // 20%
        pub const TestStaleOperatorBlocks: u64 = 100;
        pub const TestSlashBps: u32 = 1_000; // 10%
    }

    impl crate::pallet::Config for Test {
        type MaxOracleOperators = TestMaxOperators;
        type OracleUpdateInterval = TestOracleInterval;
        type CommitteeSize = TestCommitteeSize;
        type AdmissionThreshold = TestAdmissionThreshold;
        type OperatorBond = TestOperatorBond;
        type ChallengeBond = TestChallengeBond;
        type OutlierThresholdBps = TestOutlierThresholdBps;
        type StaleOperatorBlocks = TestStaleOperatorBlocks;
        type SlashBps = TestSlashBps;
        type Currency = Balances;
    }

    const ALICE: u64 = 1;
    const BOB: u64 = 2;
    const CHARLIE: u64 = 3;
    const DAVE: u64 = 4;

    fn new_test_ext() -> sp_io::TestExternalities {
        let t = RuntimeGenesisConfig {
            system: Default::default(),
            balances: pallet_balances::GenesisConfig::<Test> {
                balances: vec![
                    (ALICE, 1_000_000),
                    (BOB, 1_000_000),
                    (CHARLIE, 1_000_000),
                    (DAVE, 1_000_000),
                ],
                dev_accounts: None,
            },
            oracle: crate::pallet::GenesisConfig::<Test> {
                // Alice + Bob are genesis operators AND committee members.
                initial_operators: vec![ALICE, BOB],
                initial_committee: vec![ALICE, BOB, CHARLIE],
                _config: Default::default(),
            },
        };
        let mut ext: sp_io::TestExternalities = t.build_storage().unwrap().into();
        ext.execute_with(|| {
            System::set_block_number(1);
        });
        ext
    }

    type OraclePallet = crate::pallet::Pallet::<Test>;

    #[test]
    fn test_genesis_operators_loaded() {
        new_test_ext().execute_with(|| {
            assert!(OracleOperators::<Test>::contains_key(ALICE));
            assert!(OracleOperators::<Test>::contains_key(BOB));
            assert!(!OracleOperators::<Test>::contains_key(CHARLIE));
            assert_eq!(OperatorCount::<Test>::get(), 2);
            assert_eq!(LastUpdateBlock::<Test>::get(), 0);
            assert!(CurrentFeed::<Test>::get().is_none());
        });
    }

    #[test]
    fn test_update_feed_by_operator() {
        new_test_ext().execute_with(|| {
            let result = OraclePallet::update_feed(
                RuntimeOrigin::signed(ALICE),
                25_000,   // token_price_usd ($0.025 in 6-decimals units)
                150,      // avg_gpu_hour_price ($1.50/hr)
                60_00,    // utilization_rate_bps (60%)
                1_200,    // staking_apr_bps (12%)
            );
            assert!(result.is_ok());

            let feed = CurrentFeed::<Test>::get().expect("feed should be set");
            assert_eq!(feed.token_price_usd, 25_000);
            assert_eq!(feed.avg_gpu_hour_price, 150);
            assert_eq!(feed.utilization_rate_bps, 60_00);
            assert_eq!(feed.staking_apr_bps, 1_200);
            assert_eq!(LastUpdateBlock::<Test>::get(), 1);
        });
    }

    #[test]
    fn test_update_feed_rejected_for_non_operator() {
        new_test_ext().execute_with(|| {
            let result = OraclePallet::update_feed(
                RuntimeOrigin::signed(CHARLIE),
                25_000, 150, 60_00, 1_200,
            );
            assert_eq!(result, Err(Error::<Test>::NotAuthorized.into()));
            assert!(CurrentFeed::<Test>::get().is_none());
        });
    }

    #[test]
    fn test_update_feed_rejected_when_too_soon() {
        new_test_ext().execute_with(|| {
            // First update at block 1
            OraclePallet::update_feed(
                RuntimeOrigin::signed(ALICE), 25_000, 150, 60_00, 1_200,
            ).unwrap();
            // Second update in same block must fail (interval = 5)
            let result = OraclePallet::update_feed(
                RuntimeOrigin::signed(ALICE), 26_000, 150, 60_00, 1_200,
            );
            assert_eq!(result, Err(Error::<Test>::UpdateTooSoon.into()));
        });
    }

    #[test]
    fn test_update_feed_allowed_after_interval() {
        new_test_ext().execute_with(|| {
            OraclePallet::update_feed(
                RuntimeOrigin::signed(ALICE), 25_000, 150, 60_00, 1_200,
            ).unwrap();
            // Advance past the interval
            System::set_block_number(6);
            let result = OraclePallet::update_feed(
                RuntimeOrigin::signed(BOB), 26_000, 160, 70_00, 1_300,
            );
            assert!(result.is_ok());
            assert_eq!(CurrentFeed::<Test>::get().unwrap().token_price_usd, 26_000);
            assert_eq!(LastUpdateBlock::<Test>::get(), 6);
        });
    }

    #[test]
    fn test_update_feed_per_operator_throttle_two_operators() {
        // Phase-C fix: the anti-spam throttle is PER-OPERATOR, so two
        // independent (committee or bonded) operators can each post a feed
        // within the same interval without blocking each other. Previously the
        // global LastUpdateBlock gate rejected the second operator.
        new_test_ext().execute_with(|| {
            // ALICE posts at block 1
            OraclePallet::update_feed(
                RuntimeOrigin::signed(ALICE), 25_000, 150, 60_00, 1_200,
            ).unwrap();
            // BOB posts at the SAME block — allowed (own throttle is zero)
            let result = OraclePallet::update_feed(
                RuntimeOrigin::signed(BOB), 26_000, 160, 70_00, 1_300,
            );
            assert!(result.is_ok());
            // CurrentFeed is the latest writer's feed; both stored per-operator
            assert_eq!(CurrentFeed::<Test>::get().unwrap().token_price_usd, 26_000);
            assert_eq!(OperatorLastFeedBlock::<Test>::get(ALICE), Some(1));
            assert_eq!(OperatorLastFeedBlock::<Test>::get(BOB), Some(1));
        });
    }

    #[test]
    fn test_update_feed_per_operator_throttle_same_operator() {
        // Same operator is still rate-limited by its own last-posted block.
        new_test_ext().execute_with(|| {
            OraclePallet::update_feed(
                RuntimeOrigin::signed(ALICE), 25_000, 150, 60_00, 1_200,
            ).unwrap();
            // ALICE again in the same block must fail (interval = 5)
            let result = OraclePallet::update_feed(
                RuntimeOrigin::signed(ALICE), 26_000, 150, 60_00, 1_200,
            );
            assert_eq!(result, Err(Error::<Test>::UpdateTooSoon.into()));
            // Advance 5 blocks and ALICE may post again
            System::set_block_number(6);
            let result = OraclePallet::update_feed(
                RuntimeOrigin::signed(ALICE), 27_000, 170, 80_00, 1_400,
            );
            assert!(result.is_ok());
            assert_eq!(OperatorLastFeedBlock::<Test>::get(ALICE), Some(6));
        });
    }

    #[test]
    fn test_update_feed_rejects_zero_price() {
        new_test_ext().execute_with(|| {
            let result = OraclePallet::update_feed(
                RuntimeOrigin::signed(ALICE), 0, 150, 60_00, 1_200,
            );
            assert_eq!(result, Err(Error::<Test>::InvalidPrice.into()));
        });
    }

    // ── Phase B committee admission (oracle-governance §6 gap #4) ──
    //
    // Committee members: ALICE, BOB, CHARLIE (genesis). Threshold = 2, so a
    // single member's proposal needs one more approval to admit. DAVE is not on
    // the committee.

    #[test]
    fn test_genesis_committee_loaded() {
        new_test_ext().execute_with(|| {
            assert!(Committee::<Test>::contains_key(ALICE));
            assert!(Committee::<Test>::contains_key(BOB));
            assert!(Committee::<Test>::contains_key(CHARLIE));
            assert!(!Committee::<Test>::contains_key(DAVE));
            assert!(PendingAdmissions::<Test>::iter().count() == 0);
        });
    }

    #[test]
    fn test_propose_operator_requires_committee_member() {
        new_test_ext().execute_with(|| {
            // DAVE is not on the committee -> rejected.
            let result = OraclePallet::propose_operator(
                RuntimeOrigin::signed(DAVE), 99,
            );
            assert_eq!(result, Err(Error::<Test>::NotCommitteeMember.into()));
            assert!(!OracleOperators::<Test>::contains_key(99));
            assert!(PendingAdmissions::<Test>::get(99).is_none());
        });
    }

    #[test]
    fn test_propose_operator_creates_pending_until_threshold() {
        new_test_ext().execute_with(|| {
            // ALICE (committee) proposes CHARLIE-as-operator (id 99).
            let result = OraclePallet::propose_operator(
                RuntimeOrigin::signed(ALICE), 99,
            );
            assert!(result.is_ok());
            // Threshold is 2 -> NOT admitted yet, pending stored.
            assert!(!OracleOperators::<Test>::contains_key(99));
            let pending = PendingAdmissions::<Test>::get(99).expect("pending");
            assert_eq!(pending.proposer, ALICE);
            assert_eq!(pending.approvals.len(), 1);
            assert_eq!(OperatorCount::<Test>::get(), 2);
        });
    }

    #[test]
    fn test_approve_operator_admits_at_threshold() {
        new_test_ext().execute_with(|| {
            // ALICE proposes id 99 (self-approval = 1).
            OraclePallet::propose_operator(RuntimeOrigin::signed(ALICE), 99).unwrap();
            // BOB (committee) approves -> reaches threshold 2 -> admitted.
            let result = OraclePallet::approve_operator(RuntimeOrigin::signed(BOB), 99);
            assert!(result.is_ok());
            assert!(OracleOperators::<Test>::contains_key(99));
            assert_eq!(OperatorCount::<Test>::get(), 3);
            // Proposal cleared.
            assert!(PendingAdmissions::<Test>::get(99).is_none());
        });
    }

    #[test]
    fn test_approve_operator_requires_committee_member() {
        new_test_ext().execute_with(|| {
            OraclePallet::propose_operator(RuntimeOrigin::signed(ALICE), 99).unwrap();
            // DAVE is not a committee member -> rejected.
            let result = OraclePallet::approve_operator(RuntimeOrigin::signed(DAVE), 99);
            assert_eq!(result, Err(Error::<Test>::NotCommitteeMember.into()));
            assert!(!OracleOperators::<Test>::contains_key(99));
        });
    }

    #[test]
    fn test_approve_operator_no_pending_rejected() {
        new_test_ext().execute_with(|| {
            // No proposal exists for 99.
            let result = OraclePallet::approve_operator(RuntimeOrigin::signed(BOB), 99);
            assert_eq!(result, Err(Error::<Test>::NoPendingAdmission.into()));
        });
    }

    #[test]
    fn test_approve_operator_cannot_double_approve() {
        new_test_ext().execute_with(|| {
            OraclePallet::propose_operator(RuntimeOrigin::signed(ALICE), 99).unwrap();
            // ALICE already self-approved as proposer; can't approve again.
            let result = OraclePallet::approve_operator(RuntimeOrigin::signed(ALICE), 99);
            assert_eq!(result, Err(Error::<Test>::AlreadyApproved.into()));
        });
    }

    #[test]
    fn test_propose_operator_already_operator_rejected() {
        new_test_ext().execute_with(|| {
            // ALICE is already an operator (genesis).
            let result = OraclePallet::propose_operator(RuntimeOrigin::signed(ALICE), ALICE);
            assert_eq!(result, Err(Error::<Test>::AlreadyOperator.into()));
        });
    }

    #[test]
    fn test_propose_operator_duplicate_pending_rejected() {
        new_test_ext().execute_with(|| {
            OraclePallet::propose_operator(RuntimeOrigin::signed(ALICE), 99).unwrap();
            // BOB proposes the same operator -> AlreadyPending.
            let result = OraclePallet::propose_operator(RuntimeOrigin::signed(BOB), 99);
            assert_eq!(result, Err(Error::<Test>::AlreadyPending.into()));
        });
    }

    #[test]
    fn test_remove_operator_committee_member() {
        new_test_ext().execute_with(|| {
            // CHARLIE (committee member, not an operator) can remove BOB.
            let result = OraclePallet::remove_operator(
                RuntimeOrigin::signed(CHARLIE), BOB,
            );
            assert!(result.is_ok());
            assert!(!OracleOperators::<Test>::contains_key(BOB));
            assert_eq!(OperatorCount::<Test>::get(), 1);

            // Removing a non-operator fails.
            let miss = OraclePallet::remove_operator(
                RuntimeOrigin::signed(CHARLIE), 99,
            );
            assert_eq!(miss, Err(Error::<Test>::NotOperator.into()));
        });
    }

    #[test]
    fn test_remove_operator_root_break_glass() {
        new_test_ext().execute_with(|| {
            // Root (emergency break-glass) can still remove.
            let result = OraclePallet::remove_operator(
                RuntimeOrigin::root(), BOB,
            );
            assert!(result.is_ok());
            assert!(!OracleOperators::<Test>::contains_key(BOB));
        });
    }

    #[test]
    fn test_remove_operator_rejects_non_committee_signed() {
        new_test_ext().execute_with(|| {
            // DAVE is not a committee member and not root -> rejected.
            let result = OraclePallet::remove_operator(
                RuntimeOrigin::signed(DAVE), BOB,
            );
            assert!(result.is_err());
            assert!(OracleOperators::<Test>::contains_key(BOB));
        });
    }

    // Threshold-1 immediate admission with its own runtime config.
    mod threshold_one {
        use super::*;
        use frame_support::parameter_types;
        use sp_runtime::BuildStorage;

        construct_runtime!(
            pub struct TestT1 {
                System: frame_system,
                Balances: pallet_balances,
                Oracle: local_oracle,
            }
        );

        #[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
        impl frame_system::Config for TestT1 {
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

        parameter_types! {
            pub const T1ExistentialDeposit: Balance = 1;
        }
        impl pallet_balances::Config for TestT1 {
            type Balance = Balance;
            type DustRemoval = ();
            type RuntimeEvent = RuntimeEvent;
            type ExistentialDeposit = T1ExistentialDeposit;
            type AccountStore = System;
            type WeightInfo = ();
            type MaxLocks = ConstU32<50>;
            type MaxReserves = ConstU32<50>;
            type ReserveIdentifier = [u8; 8];
            type FreezeIdentifier = ();
            type RuntimeHoldReason = ();
            type RuntimeFreezeReason = ();
            type MaxFreezes = ConstU32<0>;
            type DoneSlashHandler = ();
        }

        parameter_types! {
            pub const T1MaxOperators: u32 = 10;
            pub const T1Interval: u64 = 5;
            pub const T1CommitteeSize: u32 = 5;
            pub const T1AdmissionThreshold: u32 = 1;
            pub const T1OperatorBond: Balance = 1_000;
            pub const T1ChallengeBond: Balance = 100;
            pub const T1OutlierThresholdBps: u32 = 2_000;
            pub const T1StaleOperatorBlocks: u64 = 100;
            pub const T1SlashBps: u32 = 1_000;
        }

        impl crate::pallet::Config for TestT1 {
            type MaxOracleOperators = T1MaxOperators;
            type OracleUpdateInterval = T1Interval;
            type CommitteeSize = T1CommitteeSize;
            type AdmissionThreshold = T1AdmissionThreshold;
            type OperatorBond = T1OperatorBond;
            type ChallengeBond = T1ChallengeBond;
            type OutlierThresholdBps = T1OutlierThresholdBps;
            type StaleOperatorBlocks = T1StaleOperatorBlocks;
            type SlashBps = T1SlashBps;
            type Currency = Balances;
        }

        #[test]
        fn test_threshold_one_admits_immediately() {
            let t = RuntimeGenesisConfig {
                system: Default::default(),
                balances: pallet_balances::GenesisConfig::<TestT1> {
                    balances: vec![(ALICE, 1_000_000), (BOB, 1_000_000)],
                    dev_accounts: None,
                },
                oracle: crate::pallet::GenesisConfig::<TestT1> {
                    initial_operators: vec![ALICE, BOB],
                    initial_committee: vec![ALICE, BOB],
                    _config: Default::default(),
                },
            };
            let mut ext: sp_io::TestExternalities = t.build_storage().unwrap().into();
            ext.execute_with(|| {
                // ALICE proposes id 99; threshold 1 -> admitted immediately.
                let r = crate::pallet::Pallet::<TestT1>::propose_operator(
                    RuntimeOrigin::signed(ALICE), 99,
                );
                assert!(r.is_ok());
                assert!(OracleOperators::<TestT1>::contains_key(99));
                assert_eq!(OperatorCount::<TestT1>::get(), 3);
                assert!(PendingAdmissions::<TestT1>::get(99).is_none());
            });
        }
    }

    #[test]
    fn test_feed_roundtrip_after_operator_change() {
        new_test_ext().execute_with(|| {
            // Bob loses operator status
            OraclePallet::remove_operator(RuntimeOrigin::root(), BOB).unwrap();
            // Bob can no longer update
            let result = OraclePallet::update_feed(
                RuntimeOrigin::signed(BOB), 25_000, 150, 60_00, 1_200,
            );
            assert_eq!(result, Err(Error::<Test>::NotAuthorized.into()));
        });
    }

    // ── Oracle-governance §6 gap #3: per-operator feeds (median aggregation) ──

    #[test]
    fn test_operator_feeds_stored_per_operator() {
        new_test_ext().execute_with(|| {
            // ALICE (genesis operator) writes at block 1.
            OraclePallet::update_feed(
                RuntimeOrigin::signed(ALICE), 25_000, 150, 60_00, 1_200,
            ).unwrap();
            // BOB (genesis operator) writes later (after the interval) with
            // a different value.
            System::set_block_number(6);
            OraclePallet::update_feed(
                RuntimeOrigin::signed(BOB), 26_000, 160, 70_00, 1_300,
            ).unwrap();

            // Each operator's OWN latest feed is preserved independently.
            let alice = OperatorFeeds::<Test>::get(ALICE).expect("alice feed");
            let bob = OperatorFeeds::<Test>::get(BOB).expect("bob feed");
            assert_eq!(alice.token_price_usd, 25_000);
            assert_eq!(alice.avg_gpu_hour_price, 150);
            assert_eq!(bob.token_price_usd, 26_000);
            assert_eq!(bob.avg_gpu_hour_price, 160);

            // CurrentFeed still holds the latest writer (BOB) — unchanged
            // semantics for existing consumers.
            assert_eq!(CurrentFeed::<Test>::get().unwrap().token_price_usd, 26_000);

            // Operators who never posted have no entry.
            assert!(OperatorFeeds::<Test>::get(CHARLIE).is_none());
        });
    }

    #[test]
    fn test_operator_feed_overwritten_by_same_operator() {
        new_test_ext().execute_with(|| {
            OraclePallet::update_feed(
                RuntimeOrigin::signed(ALICE), 25_000, 150, 60_00, 1_200,
            ).unwrap();
            System::set_block_number(6);
            OraclePallet::update_feed(
                RuntimeOrigin::signed(ALICE), 30_000, 200, 80_00, 1_500,
            ).unwrap();
            // ALICE's per-operator entry is REPLACED (only the latest kept).
            let alice = OperatorFeeds::<Test>::get(ALICE).expect("alice feed");
            assert_eq!(alice.token_price_usd, 30_000);
            assert_eq!(alice.avg_gpu_hour_price, 200);
            assert_eq!(alice.utilization_rate_bps, 80_00);
        });
    }

    // ── Phase C (staking-backed slashing oracle, §6 gap #5) ──
    //
    // DAVE is not a genesis operator or committee member; he becomes a
    // permissionless operator by bonding. CHARLIE is a committee member but not
    // an operator; he funds challenges. Accounts 50/51 are fresh (0 balance,
    // funded on demand) for multi-operator scenarios.

    #[test]
    fn test_bond_operator_transfers_bond_and_registers() {
        new_test_ext().execute_with(|| {
            let before = pallet_balances::Pallet::<Test>::free_balance(DAVE);
            let r = OraclePallet::bond_operator(RuntimeOrigin::signed(DAVE));
            assert!(r.is_ok());
            // Bond transferred into the pallet account.
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(DAVE), before - 1_000);
            assert_eq!(StakedOperators::<Test>::get(DAVE), 1_000);

            // A bonded (permissionless) operator may now post a feed.
            let u = OraclePallet::update_feed(
                RuntimeOrigin::signed(DAVE), 25_000, 150, 60_00, 1_200,
            );
            assert!(u.is_ok());
            assert_eq!(CurrentFeed::<Test>::get().unwrap().token_price_usd, 25_000);
        });
    }

    #[test]
    fn test_bond_operator_insufficient_balance() {
        new_test_ext().execute_with(|| {
            // Account 50 has no funds -> can't post the 1,000 bond.
            let r = OraclePallet::bond_operator(RuntimeOrigin::signed(50));
            assert!(r.is_err());
            assert!(!StakedOperators::<Test>::contains_key(50));
        });
    }

    #[test]
    fn test_bond_more_increases_weight() {
        new_test_ext().execute_with(|| {
            OraclePallet::bond_operator(RuntimeOrigin::signed(DAVE)).unwrap();
            let r = OraclePallet::bond_more(RuntimeOrigin::signed(DAVE), 500);
            assert!(r.is_ok());
            assert_eq!(StakedOperators::<Test>::get(DAVE), 1_500);

            // Zero additional is rejected.
            let z = OraclePallet::bond_more(RuntimeOrigin::signed(DAVE), 0);
            assert_eq!(z, Err(Error::<Test>::ZeroAmount.into()));
        });
    }

    #[test]
    fn test_unbond_operator_returns_bond() {
        new_test_ext().execute_with(|| {
            OraclePallet::bond_operator(RuntimeOrigin::signed(DAVE)).unwrap();
            let before = pallet_balances::Pallet::<Test>::free_balance(DAVE);
            let r = OraclePallet::unbond_operator(RuntimeOrigin::signed(DAVE));
            assert!(r.is_ok());
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(DAVE), before + 1_000);
            assert!(!StakedOperators::<Test>::contains_key(DAVE));
        });
    }

    #[test]
    fn test_unbond_blocked_while_challenged() {
        new_test_ext().execute_with(|| {
            OraclePallet::bond_operator(RuntimeOrigin::signed(DAVE)).unwrap();
            // CHARLIE opens a challenge against DAVE.
            OraclePallet::challenge_operator(RuntimeOrigin::signed(CHARLIE), DAVE).unwrap();
            // DAVE cannot unbond while challenged (slashing-evasion guard).
            let r = OraclePallet::unbond_operator(RuntimeOrigin::signed(DAVE));
            assert_eq!(r, Err(Error::<Test>::AlreadyChallenged.into()));
            assert!(StakedOperators::<Test>::contains_key(DAVE));
        });
    }

    #[test]
    fn test_challenge_opens_with_bond() {
        new_test_ext().execute_with(|| {
            OraclePallet::bond_operator(RuntimeOrigin::signed(DAVE)).unwrap();
            let before = pallet_balances::Pallet::<Test>::free_balance(CHARLIE);
            let r = OraclePallet::challenge_operator(RuntimeOrigin::signed(CHARLIE), DAVE);
            assert!(r.is_ok());
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(CHARLIE), before - 100);
            let ch = OpenChallenges::<Test>::get(DAVE).expect("open challenge");
            assert_eq!(ch.challenger, CHARLIE);
            assert_eq!(ch.target, DAVE);
            assert_eq!(ch.bond, 100);
        });
    }

    #[test]
    fn test_challenge_not_operator_or_already_challenged() {
        new_test_ext().execute_with(|| {
            // Challenging a non-bonded account fails.
            let r = OraclePallet::challenge_operator(RuntimeOrigin::signed(CHARLIE), 50);
            assert_eq!(r, Err(Error::<Test>::NotStakedOperator.into()));

            OraclePallet::bond_operator(RuntimeOrigin::signed(DAVE)).unwrap();
            OraclePallet::challenge_operator(RuntimeOrigin::signed(CHARLIE), DAVE).unwrap();
            // A second open challenge against the same target fails.
            let r2 = OraclePallet::challenge_operator(RuntimeOrigin::signed(ALICE), DAVE);
            assert_eq!(r2, Err(Error::<Test>::AlreadyChallenged.into()));
        });
    }

    #[test]
    fn test_resolve_slash_stale() {
        new_test_ext().execute_with(|| {
            // DAVE bonds + posts a feed at block 1 (staleness clock starts).
            OraclePallet::bond_operator(RuntimeOrigin::signed(DAVE)).unwrap();
            OraclePallet::update_feed(
                RuntimeOrigin::signed(DAVE), 25_000, 150, 60_00, 1_200,
            ).unwrap();
            assert_eq!(OperatorLastFeedBlock::<Test>::get(DAVE), Some(1));

            // CHARLIE opens a challenge.
            OraclePallet::challenge_operator(RuntimeOrigin::signed(CHARLIE), DAVE).unwrap();
            let charlie_before = pallet_balances::Pallet::<Test>::free_balance(CHARLIE);

            // Not stale yet at block 50 (< 1 + 100).
            System::set_block_number(50);
            let early = OraclePallet::resolve_challenge(
                RuntimeOrigin::signed(ALICE), DAVE, 0, // SlashStale
            );
            assert_eq!(early, Err(Error::<Test>::NotStaleYet.into()));

            // Stale past 1 + 100 = 101.
            System::set_block_number(102);
            let r = OraclePallet::resolve_challenge(
                RuntimeOrigin::signed(ALICE), DAVE, 0, // SlashStale
            );
            assert!(r.is_ok());
            // 10% of 1,000 slashed (burned) -> bond 900; challenge closed.
            assert_eq!(StakedOperators::<Test>::get(DAVE), 900);
            assert!(OpenChallenges::<Test>::get(DAVE).is_none());
            // Challenger's bond returned.
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(CHARLIE), charlie_before + 100);
        });
    }

    #[test]
    fn test_resolve_slash_outlier() {
        new_test_ext().execute_with(|| {
            // Two bonded operators post divergent token prices at block 1 / 6.
            OraclePallet::bond_operator(RuntimeOrigin::signed(DAVE)).unwrap();
            OraclePallet::update_feed(
                RuntimeOrigin::signed(DAVE), 10_000, 150, 60_00, 1_200,
            ).unwrap();
            // Fund + bond a second operator, then post a wildly higher price.
            pallet_balances::Pallet::<Test>::force_set_balance(RuntimeOrigin::root(), 50, 1_000_000).unwrap();
            System::set_block_number(6);
            OraclePallet::bond_operator(RuntimeOrigin::signed(50)).unwrap();
            OraclePallet::update_feed(
                RuntimeOrigin::signed(50), 50_000, 150, 60_00, 1_200,
            ).unwrap();

            // 50's price (50,000) deviates far from the median (10,000) -> outlier.
            OraclePallet::challenge_operator(RuntimeOrigin::signed(CHARLIE), 50).unwrap();
            let r = OraclePallet::resolve_challenge(
                RuntimeOrigin::signed(ALICE), 50, 1, // SlashOutlier
            );
            assert!(r.is_ok());
            assert_eq!(StakedOperators::<Test>::get(50), 900); // 10% slashed
            assert!(OpenChallenges::<Test>::get(50).is_none());
        });
    }

    #[test]
    fn test_resolve_dismiss_slashes_challenger() {
        new_test_ext().execute_with(|| {
            // DAVE bonds + posts an in-range feed.
            OraclePallet::bond_operator(RuntimeOrigin::signed(DAVE)).unwrap();
            OraclePallet::update_feed(
                RuntimeOrigin::signed(DAVE), 25_000, 150, 60_00, 1_200,
            ).unwrap();

            // A frivolous challenge is opened, then dismissed.
            OraclePallet::challenge_operator(RuntimeOrigin::signed(CHARLIE), DAVE).unwrap();
            let charlie_before = pallet_balances::Pallet::<Test>::free_balance(CHARLIE);
            let r = OraclePallet::resolve_challenge(
                RuntimeOrigin::signed(ALICE), DAVE, 2, // Dismiss
            );
            assert!(r.is_ok());
            // Challenger's bond burned (not returned); target untouched.
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(CHARLIE), charlie_before);
            assert_eq!(StakedOperators::<Test>::get(DAVE), 1_000);
            assert!(OpenChallenges::<Test>::get(DAVE).is_none());
        });
    }

    #[test]
    fn test_effective_feed_bond_weighted_median() {
        new_test_ext().execute_with(|| {
            // Three bonded operators, equal bonds; one is a high outlier.
            OraclePallet::bond_operator(RuntimeOrigin::signed(DAVE)).unwrap();
            OraclePallet::update_feed(RuntimeOrigin::signed(DAVE), 2_000, 100, 50_00, 1_000).unwrap();
            pallet_balances::Pallet::<Test>::force_set_balance(RuntimeOrigin::root(), 50, 1_000_000).unwrap();
            System::set_block_number(6);
            OraclePallet::bond_operator(RuntimeOrigin::signed(50)).unwrap();
            OraclePallet::update_feed(RuntimeOrigin::signed(50), 3_000, 100, 50_00, 1_000).unwrap();
            pallet_balances::Pallet::<Test>::force_set_balance(RuntimeOrigin::root(), 51, 1_000_000).unwrap();
            System::set_block_number(11);
            OraclePallet::bond_operator(RuntimeOrigin::signed(51)).unwrap();
            OraclePallet::update_feed(RuntimeOrigin::signed(51), 9_999, 100, 50_00, 1_000).unwrap();

            // Equal weights -> median of [2000, 3000, 9999] = 3000; the 9999
            // outlier cannot move it.
            let feed = OraclePallet::effective_feed().expect("effective feed");
            assert_eq!(feed.token_price_usd, 3_000);

            // Now give 51 (the outlier) a tiny bond and 50 a large bond: the
            // large-bond operator dominates.
            OraclePallet::bond_more(RuntimeOrigin::signed(50), 100_000).unwrap();
            let feed2 = OraclePallet::effective_feed().expect("effective feed");
            // 50's bond (101,000) >> others -> its value (3000) is the median.
            assert_eq!(feed2.token_price_usd, 3_000);
        });
    }

    #[test]
    fn test_update_feed_sets_operator_last_block() {
        new_test_ext().execute_with(|| {
            OraclePallet::bond_operator(RuntimeOrigin::signed(DAVE)).unwrap();
            OraclePallet::update_feed(
                RuntimeOrigin::signed(DAVE), 25_000, 150, 60_00, 1_200,
            ).unwrap();
            assert_eq!(OperatorLastFeedBlock::<Test>::get(DAVE), Some(1));
            // Committee operator's staleness is tracked too.
            System::set_block_number(6);
            OraclePallet::update_feed(
                RuntimeOrigin::signed(ALICE), 26_000, 160, 70_00, 1_300,
            ).unwrap();
            assert_eq!(OperatorLastFeedBlock::<Test>::get(ALICE), Some(6));
        });
    }

    #[test]
    fn test_committee_path_still_works_without_bonds() {
        new_test_ext().execute_with(|| {
            // No bonds exist; the committee (Phase A/B) path is unchanged.
            assert!(StakedOperators::<Test>::iter().count() == 0);
            let r = OraclePallet::update_feed(
                RuntimeOrigin::signed(ALICE), 25_000, 150, 60_00, 1_200,
            );
            assert!(r.is_ok());
            assert_eq!(CurrentFeed::<Test>::get().unwrap().token_price_usd, 25_000);
            // No bonded operators -> no effective feed (agent falls back).
            assert!(OraclePallet::effective_feed().is_none());
        });
    }
}
