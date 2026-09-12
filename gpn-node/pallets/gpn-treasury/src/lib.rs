//! # GPN Treasury Pallet — vested, governance-controlled treasury
//!
//! Implements the disclosed treasury allocation decided by the project owner:
//!
//! - **Allocation:** 20% of the 1B cap (200,000,000 GPN) held by the treasury
//!   account (`PalletId(*b"gpn/trsy")`), funded at genesis, counted against the
//!   1B supply cap (via `gpn-token`'s canonical `TotalMinted`, seeded by the
//!   chain spec).
//! - **Vesting:** a discrete **tranche** schedule (6-month cadence). At launch
//!   **0 GPN is unlocked**; each tranche unlocks at its block. The treasury can
//!   only ever spend its *unlocked* balance.
//! - **Governance:** a fixed roster of **25 addresses** seeded at genesis.
//!   Spending requires **13-of-25** roster approvals.
//!
//! Default schedule (Reading A, 200M total):
//!   Phase 1 — 100M over 48 months  → 8 tranches × 12,500,000 GPN every 6 months
//!   Phase 2 — 100M over 120 months → 20 tranches ×  5,000,000 GPN every 6 months

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "runtime-benchmarks")]
pub mod benchmarking;
pub mod weights;

use codec::{Decode, DecodeWithMemTracking, Encode, MaxEncodedLen};
use core::fmt::Debug;
use scale_info::TypeInfo;

pub use pallet::*;
pub use weights::WeightInfo;

/// A single unlock tranche: `amount` becomes spendable at `unlock_block`.
#[derive(
    Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo,
)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "std",
    serde(bound(
        serialize = "BlockNumber: serde::Serialize, Balance: serde::Serialize",
        deserialize = "BlockNumber: serde::Deserialize<'de>, Balance: serde::Deserialize<'de>"
    ))
)]
pub struct Tranche<BlockNumber, Balance> {
    pub unlock_block: BlockNumber,
    pub amount: Balance,
}

#[frame_support::pallet]
pub mod pallet {
    use super::*;
    use frame_support::pallet_prelude::*;
    use frame_support::sp_runtime::traits::{AccountIdConversion, Saturating, Zero};
    use frame_support::traits::{Currency, ExistenceRequirement};
    use frame_system::pallet_prelude::*;
    use sp_std::vec::Vec;

    pub type BalanceOf<T> =
        <<T as Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::Balance;
    pub type TrancheOf<T> = Tranche<BlockNumberFor<T>, BalanceOf<T>>;

    #[pallet::config]
    pub trait Config: frame_system::Config {
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;
        type Currency: Currency<Self::AccountId>;
        /// Maximum roster size (governance token holders). 25 for GPN.
        #[pallet::constant]
        type MaxRoster: Get<u32>;
        /// Maximum number of vesting tranches (sanity bound on the schedule).
        #[pallet::constant]
        type MaxTranches: Get<u32>;
        /// Weight information for extrinsics in this pallet (workplan item 9).
        type WeightInfo: WeightInfo;
    }

    #[pallet::pallet]
    pub struct Pallet<T>(_);

    // ── Storage ────────────────────────────────────────────────────────────

    /// The fixed governance roster (25 addresses at genesis).
    #[pallet::storage]
    pub type Roster<T: Config> =
        StorageValue<_, BoundedVec<T::AccountId, T::MaxRoster>, ValueQuery>;

    /// Number of roster approvals required to release funds (13 for GPN).
    #[pallet::storage]
    pub type Threshold<T: Config> = StorageValue<_, u32, ValueQuery>;

    /// The vesting schedule (tranches in ascending unlock order).
    #[pallet::storage]
    pub type Schedule<T: Config> =
        StorageValue<_, BoundedVec<TrancheOf<T>, T::MaxTranches>, ValueQuery>;

    /// Total GPN already released from the treasury (spent).
    #[pallet::storage]
    pub type TotalReleased<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    /// Monotonic spend-proposal id.
    #[pallet::storage]
    pub type NextProposalId<T: Config> = StorageValue<_, u64, ValueQuery>;

    /// A spend proposal: once it gathers `Threshold` approvals it may be
    /// executed (transferring `amount` to `dest` from the unlocked balance).
    /// Stored as parallel maps keyed by proposal id (avoids a generic struct).
    #[pallet::storage]
    pub type ProposalDest<T: Config> =
        StorageMap<_, Blake2_128Concat, u64, T::AccountId, OptionQuery>;

    #[pallet::storage]
    pub type ProposalAmount<T: Config> =
        StorageMap<_, Blake2_128Concat, u64, BalanceOf<T>, OptionQuery>;

    #[pallet::storage]
    pub type ProposalApprovals<T: Config> =
        StorageMap<_, Blake2_128Concat, u64, BoundedVec<T::AccountId, T::MaxRoster>, ValueQuery>;

    #[pallet::storage]
    pub type ProposalExecuted<T: Config> = StorageMap<_, Blake2_128Concat, u64, bool, ValueQuery>;

    // ── Genesis ────────────────────────────────────────────────────────────

    #[pallet::genesis_config]
    #[derive(frame_support::DefaultNoBound)]
    pub struct GenesisConfig<T: Config> {
        /// Governance roster (e.g. 25 addresses). Empty disables treasury spending.
        pub roster: Vec<T::AccountId>,
        /// Approvals required to spend (e.g. 13).
        pub threshold: u32,
        /// Vesting tranches as `(unlock_block, amount)`. Empty = nothing unlocks.
        pub schedule: Vec<(BlockNumberFor<T>, BalanceOf<T>)>,
        #[serde(skip)]
        pub _config: sp_std::marker::PhantomData<T>,
    }

    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            let roster: BoundedVec<T::AccountId, T::MaxRoster> = self
                .roster
                .clone()
                .try_into()
                .expect("roster within MaxRoster");
            Roster::<T>::put(roster);
            let schedule: BoundedVec<TrancheOf<T>, T::MaxTranches> = self
                .schedule
                .iter()
                .map(|(b, a)| Tranche {
                    unlock_block: *b,
                    amount: *a,
                })
                .collect::<Vec<_>>()
                .try_into()
                .expect("schedule within MaxTranches");
            Schedule::<T>::put(schedule);
            Threshold::<T>::put(self.threshold);
            let zero: BalanceOf<T> = Zero::zero();
            TotalReleased::<T>::put(zero);
            NextProposalId::<T>::put(0u64);
        }
    }

    // ── Events ─────────────────────────────────────────────────────────────

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        SpendProposed {
            id: u64,
            dest: T::AccountId,
            amount: BalanceOf<T>,
        },
        SpendApproved {
            id: u64,
            approver: T::AccountId,
            approvals: u32,
        },
        SpendExecuted {
            id: u64,
            dest: T::AccountId,
            amount: BalanceOf<T>,
        },
        RosterUpdated {
            size: u32,
        },
        ThresholdUpdated {
            threshold: u32,
        },
    }

    // ── Errors ─────────────────────────────────────────────────────────────

    #[pallet::error]
    pub enum Error<T> {
        NotInRoster,
        NoProposal,
        AlreadyExecuted,
        AlreadyApproved,
        NotEnoughApprovals,
        InsufficientUnlocked,
        RosterEmpty,
        TransferFailed,
    }

    // ── Calls ──────────────────────────────────────────────────────────────

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// Propose a spend of `amount` to `dest`. Caller must be a roster member.
        #[pallet::call_index(0)]
        #[pallet::weight(<T as Config>::WeightInfo::propose_spend())]
        pub fn propose_spend(
            origin: OriginFor<T>,
            dest: T::AccountId,
            amount: BalanceOf<T>,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(Self::is_member(&who), Error::<T>::NotInRoster);
            ensure!(!Roster::<T>::get().is_empty(), Error::<T>::RosterEmpty);

            let id = NextProposalId::<T>::get();
            NextProposalId::<T>::put(id.saturating_add(1));

            let mut approvals: BoundedVec<T::AccountId, T::MaxRoster> = BoundedVec::default();
            let _ = approvals.try_push(who.clone());

            ProposalDest::<T>::insert(id, dest.clone());
            ProposalAmount::<T>::insert(id, amount);
            ProposalApprovals::<T>::insert(id, approvals);
            ProposalExecuted::<T>::insert(id, false);
            Self::deposit_event(Event::<T>::SpendProposed { id, dest, amount });
            Ok(())
        }

        /// Approve a pending spend. Caller must be a roster member; each member
        /// may approve once.
        #[pallet::call_index(1)]
        #[pallet::weight(<T as Config>::WeightInfo::approve_spend())]
        pub fn approve_spend(origin: OriginFor<T>, id: u64) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(Self::is_member(&who), Error::<T>::NotInRoster);

            ensure!(ProposalDest::<T>::contains_key(id), Error::<T>::NoProposal);
            ensure!(!ProposalExecuted::<T>::get(id), Error::<T>::AlreadyExecuted);

            let mut approvals = ProposalApprovals::<T>::get(id);
            ensure!(!approvals.contains(&who), Error::<T>::AlreadyApproved);
            approvals
                .try_push(who.clone())
                .map_err(|_| Error::<T>::NotInRoster)?;
            let n = approvals.len() as u32;
            ProposalApprovals::<T>::insert(id, approvals);
            Self::deposit_event(Event::<T>::SpendApproved {
                id,
                approver: who,
                approvals: n,
            });
            Ok(())
        }

        /// Execute a spend once it has at least `Threshold` approvals and the
        /// amount is within the currently-unlocked treasury balance.
        #[pallet::call_index(2)]
        #[pallet::weight(<T as Config>::WeightInfo::execute_spend())]
        pub fn execute_spend(origin: OriginFor<T>, id: u64) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(Self::is_member(&who), Error::<T>::NotInRoster);

            let dest = ProposalDest::<T>::get(id).ok_or(Error::<T>::NoProposal)?;
            ensure!(!ProposalExecuted::<T>::get(id), Error::<T>::AlreadyExecuted);
            let amount = ProposalAmount::<T>::get(id).ok_or(Error::<T>::NoProposal)?;
            let approvals = ProposalApprovals::<T>::get(id);
            ensure!(
                approvals.len() as u32 >= Threshold::<T>::get(),
                Error::<T>::NotEnoughApprovals
            );

            // Amount must fit within the unlocked-but-not-yet-released balance.
            let unlocked = Self::unlocked_amount();
            let released = TotalReleased::<T>::get();
            let available = unlocked.saturating_sub(released);
            ensure!(amount <= available, Error::<T>::InsufficientUnlocked);

            T::Currency::transfer(
                &Self::account_id(),
                &dest,
                amount,
                ExistenceRequirement::AllowDeath,
            )?;
            TotalReleased::<T>::mutate(|r| *r = r.saturating_add(amount));
            ProposalExecuted::<T>::insert(id, true);
            Self::deposit_event(Event::<T>::SpendExecuted { id, dest, amount });
            Ok(())
        }

        /// Root: replace the governance roster (fixed 25 by default). Emergency/
        /// ops only; use sparingly and publicly.
        #[pallet::call_index(3)]
        #[pallet::weight(<T as Config>::WeightInfo::set_roster())]
        pub fn set_roster(origin: OriginFor<T>, roster: Vec<T::AccountId>) -> DispatchResult {
            ensure_root(origin)?;
            let bounded: BoundedVec<T::AccountId, T::MaxRoster> =
                roster.try_into().map_err(|_| Error::<T>::RosterEmpty)?;
            let size = bounded.len() as u32;
            Roster::<T>::put(bounded);
            ensure!(size > 0, Error::<T>::RosterEmpty);
            Self::deposit_event(Event::<T>::RosterUpdated { size });
            Ok(())
        }

        /// Root: update the approval threshold.
        #[pallet::call_index(4)]
        #[pallet::weight(<T as Config>::WeightInfo::set_threshold())]
        pub fn set_threshold(origin: OriginFor<T>, threshold: u32) -> DispatchResult {
            ensure_root(origin)?;
            Threshold::<T>::put(threshold);
            Self::deposit_event(Event::<T>::ThresholdUpdated { threshold });
            Ok(())
        }
    }

    // ── Helpers ────────────────────────────────────────────────────────────

    impl<T: Config> Pallet<T> {
        /// The treasury account (holds the vested allocation).
        pub fn account_id() -> T::AccountId {
            frame_support::PalletId(*b"gpn/trsy").into_account_truncating()
        }

        fn is_member(who: &T::AccountId) -> bool {
            Roster::<T>::get().contains(who)
        }

        /// Total amount unlocked as of the current block.
        pub fn unlocked_amount() -> BalanceOf<T> {
            let now: u64 = frame_system::Pallet::<T>::block_number()
                .try_into()
                .unwrap_or(0);
            let mut total: BalanceOf<T> = Zero::zero();
            for t in Schedule::<T>::get().into_iter() {
                let unlock: u64 = t.unlock_block.try_into().unwrap_or(u64::MAX);
                if now >= unlock {
                    total = total.saturating_add(t.amount);
                } else {
                    // Schedule is ascending; safe to stop early.
                    break;
                }
            }
            total
        }

        /// Amount currently spendable (unlocked minus already released).
        pub fn spendable() -> BalanceOf<T> {
            Self::unlocked_amount().saturating_sub(TotalReleased::<T>::get())
        }

        /// Roster size (number of governance members).
        pub fn roster_size() -> u32 {
            Roster::<T>::get().len() as u32
        }

        pub fn roster() -> Vec<T::AccountId> {
            Roster::<T>::get().into_iter().collect()
        }

        pub fn threshold() -> u32 {
            Threshold::<T>::get()
        }

        pub fn schedule() -> Vec<TrancheOf<T>> {
            Schedule::<T>::get().into_iter().collect()
        }

        /// Total GPN ever scheduled into the vesting tranches (the disclosed
        /// 200M allocation). Independent of how much has unlocked so far.
        pub fn total_allocation() -> BalanceOf<T> {
            Schedule::<T>::get()
                .into_iter()
                .fold(Zero::zero(), |acc, t| acc.saturating_add(t.amount))
        }

        /// Total GPN already released (spent) from the treasury.
        pub fn released() -> BalanceOf<T> {
            TotalReleased::<T>::get()
        }

        /// Free balance currently held by the treasury account.
        pub fn balance() -> BalanceOf<T> {
            T::Currency::free_balance(&Self::account_id())
        }

        /// Current monotonic spend-proposal id (next id to be assigned).
        pub fn next_proposal_id() -> u64 {
            NextProposalId::<T>::get()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frame_support::{
        construct_runtime, derive_impl, parameter_types,
        traits::{BuildGenesisConfig, ConstU128, ConstU16, ConstU32, ConstU64, Everything},
    };
    use sp_core::H256;
    use sp_runtime::{
        traits::{BlakeTwo256, IdentityLookup},
        BuildStorage,
    };

    use crate::pallet as local_treasury;
    use crate::pallet::{ProposalApprovals, Roster, Schedule, Threshold, TotalReleased};

    type Block = frame_system::mocking::MockBlock<Test>;
    type Balance = u128;

    construct_runtime!(
        pub struct Test {
            System: frame_system,
            Balances: pallet_balances,
            Treasury: local_treasury,
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
        pub const TestMaxRoster: u32 = 25;
        pub const TestMaxTranches: u32 = 32;
    }

    impl crate::pallet::Config for Test {
        type WeightInfo = ();
        type RuntimeEvent = RuntimeEvent;
        type Currency = Balances;
        type MaxRoster = TestMaxRoster;
        type MaxTranches = TestMaxTranches;
    }

    const ALLOC: Balance = 200; // scaled-down 200 "GPN" for the test
    const PHASE1: Balance = 8; // tranches
    const PHASE2: Balance = 20;

    fn roster25() -> Vec<u64> {
        (1..=25).collect()
    }

    fn default_schedule() -> Vec<(u64, Balance)> {
        let mut s = Vec::new();
        // Phase 1: 8 tranches, unlock at 10, 20, ... 80
        for i in 1..=PHASE1 {
            s.push(((i * 10) as u64, 10));
        }
        // Phase 2: 20 tranches, unlock at 90, 100, ... 280
        for i in 1..=PHASE2 {
            s.push(((80 + i * 10) as u64, 6));
        }
        s
    }

    pub fn new_test_ext() -> sp_io::TestExternalities {
        let mut t = frame_system::GenesisConfig::<Test>::default()
            .build_storage()
            .unwrap();
        pallet_balances::GenesisConfig::<Test> {
            balances: vec![(
                crate::pallet::Pallet::<Test>::account_id(),
                (PHASE1 * 10 + PHASE2 * 6) as Balance,
            )],
            dev_accounts: None,
        }
        .assimilate_storage(&mut t)
        .unwrap();
        let mut ext = sp_io::TestExternalities::new(t);
        ext.execute_with(|| {
            let cfg = crate::pallet::GenesisConfig::<Test> {
                roster: roster25(),
                threshold: 13,
                schedule: default_schedule(),
                _config: Default::default(),
            };
            BuildGenesisConfig::build(&cfg);
            System::set_block_number(1);
        });
        ext
    }

    type T = crate::pallet::Pallet<Test>;

    #[test]
    fn test_genesis_roster_and_threshold() {
        new_test_ext().execute_with(|| {
            assert_eq!(Roster::<Test>::get().len(), 25);
            assert_eq!(Threshold::<Test>::get(), 13);
            assert_eq!(Schedule::<Test>::get().len(), 28);
        });
    }

    #[test]
    fn test_nothing_unlocked_at_launch() {
        new_test_ext().execute_with(|| {
            System::set_block_number(1);
            assert_eq!(T::unlocked_amount(), 0);
            assert_eq!(T::spendable(), 0);
        });
    }

    #[test]
    fn test_unlocks_stepwise() {
        new_test_ext().execute_with(|| {
            System::set_block_number(15); // past tranche 1 (block 10)
            assert_eq!(T::unlocked_amount(), 10);
            System::set_block_number(85); // past all 8 phase-1 tranches (80)
            assert_eq!(T::unlocked_amount(), 80);
            System::set_block_number(285); // past all tranches (280)
            assert_eq!(T::unlocked_amount(), 80 + 20 * 6);
        });
    }

    #[test]
    fn test_spend_requires_13_of_25() {
        new_test_ext().execute_with(|| {
            System::set_block_number(90); // 8 tranches = 80 unlocked
                                          // member 1 proposes
            T::propose_spend(RuntimeOrigin::signed(1), 99, 50).unwrap();
            // 12 approvals < 13 -> cannot execute
            for m in 2..=12 {
                T::approve_spend(RuntimeOrigin::signed(m), 0).unwrap();
            }
            // proposer counts as 1 -> approvals = 1 + 11 = 12
            let approvals = ProposalApprovals::<Test>::get(0);
            assert_eq!(approvals.len(), 12);
            assert_eq!(
                T::execute_spend(RuntimeOrigin::signed(1), 0),
                Err(Error::<Test>::NotEnoughApprovals.into())
            );
            // 13th approval
            T::approve_spend(RuntimeOrigin::signed(13), 0).unwrap();
            T::execute_spend(RuntimeOrigin::signed(1), 0).unwrap();
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(99), 50);
            assert_eq!(TotalReleased::<Test>::get(), 50);
        });
    }

    #[test]
    fn test_cannot_spend_more_than_unlocked() {
        new_test_ext().execute_with(|| {
            System::set_block_number(15); // only 10 unlocked
            T::propose_spend(RuntimeOrigin::signed(1), 99, 50).unwrap();
            for m in 2..=13 {
                T::approve_spend(RuntimeOrigin::signed(m), 0).unwrap();
            }
            assert_eq!(
                T::execute_spend(RuntimeOrigin::signed(1), 0),
                Err(Error::<Test>::InsufficientUnlocked.into())
            );
        });
    }

    #[test]
    fn test_non_member_cannot_propose_or_approve() {
        new_test_ext().execute_with(|| {
            System::set_block_number(90);
            assert_eq!(
                T::propose_spend(RuntimeOrigin::signed(999), 99, 1),
                Err(Error::<Test>::NotInRoster.into())
            );
            T::propose_spend(RuntimeOrigin::signed(1), 99, 1).unwrap();
            assert_eq!(
                T::approve_spend(RuntimeOrigin::signed(999), 0),
                Err(Error::<Test>::NotInRoster.into())
            );
        });
    }

    #[test]
    fn test_double_approve_rejected() {
        new_test_ext().execute_with(|| {
            System::set_block_number(90);
            T::propose_spend(RuntimeOrigin::signed(1), 99, 1).unwrap();
            assert_eq!(
                T::approve_spend(RuntimeOrigin::signed(1), 0),
                Err(Error::<Test>::AlreadyApproved.into())
            );
        });
    }

    #[test]
    fn test_typed_getters_for_rpc() {
        new_test_ext().execute_with(|| {
            System::set_block_number(1);
            // Schedule totals / balances read by the `gpn_treasuryStatus` RPC.
            assert_eq!(T::total_allocation(), ALLOC); // 80 + 120
            assert_eq!(T::balance(), ALLOC);
            assert_eq!(T::released(), 0);
            assert_eq!(T::roster_size(), 25);
            assert_eq!(T::next_proposal_id(), 0);
            assert_eq!(T::threshold(), 13);

            // After a spend the released/balance getters track it.
            System::set_block_number(285);
            T::propose_spend(RuntimeOrigin::signed(1), 99, 7).unwrap();
            for m in 2..=13 {
                T::approve_spend(RuntimeOrigin::signed(m), 0).unwrap();
            }
            T::execute_spend(RuntimeOrigin::signed(1), 0).unwrap();
            assert_eq!(T::released(), 7);
            assert_eq!(T::balance(), ALLOC - 7);
            assert_eq!(T::spendable(), T::unlocked_amount() - 7);
        });
    }
}
