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
    use frame_support::{pallet_prelude::*, BoundedVec};
    use frame_system::pallet_prelude::*;
    use sp_std::vec::Vec;

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
            ensure!(<OracleOperators<T>>::contains_key(&who), Error::<T>::NotAuthorized);

            let block_number = frame_system::Pallet::<T>::block_number();
            let last = <LastUpdateBlock<T>>::get();
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
    }

    impl<T: Config> Pallet<T> {
        /// Insert an operator into the trusted set and bump the count.
        /// Shared by the immediate-admit (threshold 1) and threshold-reached
        /// paths so admission semantics stay in one place.
        fn admit_operator(operator: T::AccountId, current_count: u32) {
            <OracleOperators<T>>::insert(&operator, ());
            <OperatorCount<T>>::put(current_count + 1);
            Self::deposit_event(Event::<T>::OperatorAdded { who: operator });
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
    };

    type Block = frame_system::mocking::MockBlock<Test>;

    construct_runtime!(
        pub struct Test {
            System: frame_system,
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
        type AccountData = ();
        type OnNewAccount = ();
        type OnKilledAccount = ();
        type SystemWeightInfo = ();
        type SS58Prefix = ConstU16<42>;
        type OnSetCode = ();
        type MaxConsumers = ConstU32<16>;
    }

    parameter_types! {
        pub const TestMaxOperators: u32 = 10;
        pub const TestOracleInterval: u64 = 5;
        pub const TestCommitteeSize: u32 = 5;
        pub const TestAdmissionThreshold: u32 = 2;
    }

    impl crate::pallet::Config for Test {
        type MaxOracleOperators = TestMaxOperators;
        type OracleUpdateInterval = TestOracleInterval;
        type CommitteeSize = TestCommitteeSize;
        type AdmissionThreshold = TestAdmissionThreshold;
    }

    const ALICE: u64 = 1;
    const BOB: u64 = 2;
    const CHARLIE: u64 = 3;
    const DAVE: u64 = 4;

    fn new_test_ext() -> sp_io::TestExternalities {
        let t = frame_system::GenesisConfig::<Test>::default()
            .build_storage()
            .unwrap();
        let mut ext = sp_io::TestExternalities::new(t);
        ext.execute_with(|| {
            let cfg = crate::pallet::GenesisConfig::<Test> {
                // Alice + Bob are genesis operators AND committee members.
                initial_operators: vec![ALICE, BOB],
                initial_committee: vec![ALICE, BOB, CHARLIE],
                _config: Default::default(),
            };
            BuildGenesisConfig::build(&cfg);
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
            type AccountData = ();
            type OnNewAccount = ();
            type OnKilledAccount = ();
            type SystemWeightInfo = ();
            type SS58Prefix = ConstU16<42>;
            type OnSetCode = ();
            type MaxConsumers = ConstU32<16>;
        }

        parameter_types! {
            pub const T1MaxOperators: u32 = 10;
            pub const T1Interval: u64 = 5;
            pub const T1CommitteeSize: u32 = 5;
            pub const T1AdmissionThreshold: u32 = 1;
        }

        impl crate::pallet::Config for TestT1 {
            type MaxOracleOperators = T1MaxOperators;
            type OracleUpdateInterval = T1Interval;
            type CommitteeSize = T1CommitteeSize;
            type AdmissionThreshold = T1AdmissionThreshold;
        }

        #[test]
        fn test_threshold_one_admits_immediately() {
            let t = frame_system::GenesisConfig::<TestT1>::default()
                .build_storage()
                .unwrap();
            let mut ext = sp_io::TestExternalities::new(t);
            ext.execute_with(|| {
                let cfg = crate::pallet::GenesisConfig::<TestT1> {
                    initial_operators: vec![ALICE, BOB],
                    initial_committee: vec![ALICE, BOB],
                    _config: Default::default(),
                };
                BuildGenesisConfig::build(&cfg);
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
}
