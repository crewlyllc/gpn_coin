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
    use frame_support::pallet_prelude::*;
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

    #[pallet::config]
    pub trait Config: frame_system::Config {
        #[pallet::constant]
        type MaxOracleOperators: Get<u32>;
        #[pallet::constant]
        type OracleUpdateInterval: Get<BlockNumberFor<Self>>;
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

    #[pallet::genesis_config]
    #[derive(frame_support::DefaultNoBound)]
    pub struct GenesisConfig<T: Config> {
        pub initial_operators: Vec<T::AccountId>,
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
        }
    }

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        FeedUpdated { token_price_usd: u64, staking_apr_bps: u32 },
        OracleFeedUpdated(u64),
        OperatorAdded { who: T::AccountId },
        OperatorRemoved { who: T::AccountId },
    }

    #[pallet::error]
    pub enum Error<T> {
        UpdateTooSoon,
        InvalidPrice,
        NotAuthorized,
        AlreadyOperator,
        NotOperator,
        MaxOperatorsReached,
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

        /// Add a new oracle operator (governance-only).
        #[pallet::call_index(1)]
        #[pallet::weight(10_000)]
        pub fn add_operator(
            origin: OriginFor<T>,
            operator: T::AccountId,
        ) -> DispatchResult {
            ensure_root(origin)?;
            ensure!(!<OracleOperators<T>>::contains_key(&operator), Error::<T>::AlreadyOperator);
            let count = <OperatorCount<T>>::get();
            ensure!(count < T::MaxOracleOperators::get(), Error::<T>::MaxOperatorsReached);
            <OracleOperators<T>>::insert(&operator, ());
            <OperatorCount<T>>::put(count + 1);
            Self::deposit_event(Event::<T>::OperatorAdded { who: operator });
            Ok(())
        }

        /// Remove an oracle operator (governance-only).
        #[pallet::call_index(2)]
        #[pallet::weight(10_000)]
        pub fn remove_operator(
            origin: OriginFor<T>,
            operator: T::AccountId,
        ) -> DispatchResult {
            ensure_root(origin)?;
            ensure!(<OracleOperators<T>>::contains_key(&operator), Error::<T>::NotOperator);
            <OracleOperators<T>>::remove(&operator);
            <OperatorCount<T>>::mutate(|c| *c = c.saturating_sub(1));
            Self::deposit_event(Event::<T>::OperatorRemoved { who: operator });
            Ok(())
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
        CurrentFeed, LastUpdateBlock, OperatorCount, OracleOperators,
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
    }

    impl crate::pallet::Config for Test {
        type MaxOracleOperators = TestMaxOperators;
        type OracleUpdateInterval = TestOracleInterval;
    }

    const ALICE: u64 = 1;
    const BOB: u64 = 2;
    const CHARLIE: u64 = 3;

    fn new_test_ext() -> sp_io::TestExternalities {
        let t = frame_system::GenesisConfig::<Test>::default()
            .build_storage()
            .unwrap();
        let mut ext = sp_io::TestExternalities::new(t);
        ext.execute_with(|| {
            let cfg = crate::pallet::GenesisConfig::<Test> {
                initial_operators: vec![ALICE, BOB],
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

    #[test]
    fn test_add_operator_root_only() {
        new_test_ext().execute_with(|| {
            // Root can add
            let result = OraclePallet::add_operator(
                RuntimeOrigin::root(), CHARLIE,
            );
            assert!(result.is_ok());
            assert!(OracleOperators::<Test>::contains_key(CHARLIE));
            assert_eq!(OperatorCount::<Test>::get(), 3);

            // Adding again fails
            let dup = OraclePallet::add_operator(
                RuntimeOrigin::root(), CHARLIE,
            );
            assert_eq!(dup, Err(Error::<Test>::AlreadyOperator.into()));
        });
    }

    #[test]
    fn test_add_operator_rejects_signed() {
        new_test_ext().execute_with(|| {
            // Non-root (even an existing operator) cannot add
            let result = OraclePallet::add_operator(
                RuntimeOrigin::signed(ALICE), CHARLIE,
            );
            assert!(result.is_err());
            assert!(!OracleOperators::<Test>::contains_key(CHARLIE));
        });
    }

    #[test]
    fn test_add_operator_max_limit() {
        new_test_ext().execute_with(|| {
            // Current count is 2; max is 10. Add until limit.
            for id in 10..20u64 {
                let _ = OraclePallet::add_operator(RuntimeOrigin::root(), id);
            }
            // Try to exceed limit (should fail once count == max)
            let result = OraclePallet::add_operator(RuntimeOrigin::root(), 99);
            assert_eq!(result, Err(Error::<Test>::MaxOperatorsReached.into()));
            assert_eq!(OperatorCount::<Test>::get(), TestMaxOperators::get());
        });
    }

    #[test]
    fn test_remove_operator() {
        new_test_ext().execute_with(|| {
            let result = OraclePallet::remove_operator(
                RuntimeOrigin::root(), BOB,
            );
            assert!(result.is_ok());
            assert!(!OracleOperators::<Test>::contains_key(BOB));
            assert_eq!(OperatorCount::<Test>::get(), 1);

            // Removing a non-operator fails
            let miss = OraclePallet::remove_operator(
                RuntimeOrigin::root(), CHARLIE,
            );
            assert_eq!(miss, Err(Error::<Test>::NotOperator.into()));
        });
    }

    #[test]
    fn test_remove_operator_rejects_signed() {
        new_test_ext().execute_with(|| {
            let result = OraclePallet::remove_operator(
                RuntimeOrigin::signed(ALICE), BOB,
            );
            assert!(result.is_err());
            assert!(OracleOperators::<Test>::contains_key(BOB));
        });
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
