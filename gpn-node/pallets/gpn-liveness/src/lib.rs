//! # GPN Liveness Pallet — Self-Organizing Full-Node / Bootnode Layer
//!
//! Rewards always-on, publicly-reachable full nodes and penalizes
//! idle/hidden/offline ones, so the bootnode layer emerges organically instead
//! of being a founder-funded VPS. Also exposes full-node status + uptime so the
//! attestation pallet can gate Silver/Gold tiers behind real network service.
//!
//! **Model (Proof-of-Work compatible):** there is no validator authority set,
//! so reachability is proven **permissionlessly** via a stake + peer-witness
//! scheme, mirroring the attestation pallet's `check_expired` finalization:
//!
//! 1. `register_fullnode(endpoint)` — operator bonds and declares a public
//!    libp2p endpoint.
//! 2. Each [`LivenessWindow`] blocks, other bonded full nodes submit
//!    `witness_reachable(target)` — "I, a full node, dialed target's declared
//!    endpoint and got a valid handshake."
//! 3. `submit_liveness(target)` (anyone may call, like `check_expired`)
//!    finalizes the *most recent completed window*: if target accumulated
//!    [`MinWitnesses`] distinct witnesses that window, it's confirmed-online;
//!    otherwise `missed_count` rises, uptime decays, and sustained absence
//!    slashes [`SlashBps`] of the bond.
//!
//! Sybil resistance: witnesses must themselves be bonded full nodes and cannot
//! witness themselves; rewards scale with honest peer diversity, so a lone
//! Sybil cluster earns nothing.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "runtime-benchmarks")]
pub mod benchmarking;
pub mod weights;

use codec::{Decode, DecodeWithMemTracking, Encode, MaxEncodedLen};
use core::fmt::Debug;
use scale_info::TypeInfo;

pub use pallet::*;
pub use weights::WeightInfo;

/// Read-only interface for other pallets (e.g. `gpu-attestation`) to query a
/// node's full-node standing when awarding tiers.
pub trait LivenessProvider<AccountId> {
    /// Whether `who` is a currently-active, publicly-reachable full node
    /// (registered, bonded, confirmed online in the last completed window).
    fn is_active_full_node(who: &AccountId) -> bool;
    /// Rolling uptime over the recent [`LivenessProvider`] lookback, in basis
    /// points (10_000 = 100% of recent windows confirmed online).
    fn window_uptime_bps(who: &AccountId) -> u32;
}

/// A registered full node's on-chain record. (The declared endpoint lives in
/// the [`Endpoints`] storage map; this record tracks liveness only.)
#[derive(
    Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo,
)]
pub struct FullNodeRecord {
    /// Window id of the last window in which this node was confirmed online.
    pub last_confirmed_window: u64,
    /// Bitmask of recent windows confirmed online (bit 0 = most recent).
    /// Used to compute rolling uptime. Bounded to [`UptimeLookback`] bits.
    pub uptime_bits: u32,
    /// Consecutive windows missed (drives slashing after [`MaxMissedLiveness`]).
    pub missed_count: u32,
    /// Marked inactive once slashed / unregistered.
    pub active: bool,
}

#[frame_support::pallet]
pub mod pallet {
    use super::*;
    use frame_support::pallet_prelude::*;
    use frame_support::sp_runtime::traits::{AccountIdConversion, Saturating, Zero};
    use frame_support::traits::{Currency, ExistenceRequirement};
    use frame_system::pallet_prelude::*;
    use sp_std::vec;
    use sp_std::vec::Vec;

    pub type BalanceOf<T> =
        <<T as Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::Balance;

    #[pallet::config]
    pub trait Config: frame_system::Config {
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;
        type Currency: Currency<Self::AccountId>;
        /// Bond required to register as a full node (separate, smaller than the
        /// 1,000 GPN attestation stake so running a full node is cheap to try).
        #[pallet::constant]
        type LivenessBond: Get<BalanceOf<Self>>;
        /// Blocks per liveness window (~60h @15s = 14,400, matching attestation).
        #[pallet::constant]
        type LivenessWindow: Get<u64>;
        /// Distinct bonded witnesses required per window to confirm online.
        #[pallet::constant]
        type MinWitnesses: Get<u32>;
        /// Consecutive missed windows before slashing.
        #[pallet::constant]
        type MaxMissedLiveness: Get<u32>;
        /// Portion of the bond slashed on sustained absence (bps, 1_000 = 10%).
        #[pallet::constant]
        type SlashBps: Get<u32>;
        /// Number of recent windows used for the rolling uptime score.
        #[pallet::constant]
        type UptimeLookback: Get<u32>;
        /// Max endpoint length bytes.
        #[pallet::constant]
        type MaxEndpointLen: Get<u32>;
        /// Weight information for extrinsics in this pallet (workplan item 9).
        type WeightInfo: WeightInfo;
    }

    #[pallet::pallet]
    pub struct Pallet<T>(_);

    // ── Storage ────────────────────────────────────────────────────────────

    /// Declared endpoints for registered full nodes.
    #[pallet::storage]
    pub type Endpoints<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        T::AccountId,
        BoundedVec<u8, T::MaxEndpointLen>,
        OptionQuery,
    >;

    /// Bonded amount per registered full node.
    #[pallet::storage]
    pub type Bonds<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, BalanceOf<T>, OptionQuery>;

    /// Full-node liveness records.
    #[pallet::storage]
    pub type FullNodes<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, FullNodeRecord, OptionQuery>;

    /// Witness counts per (window, target). Map key: (window_id, target).
    #[pallet::storage]
    pub type WitnessCounts<T: Config> =
        StorageDoubleMap<_, Twox64Concat, u64, Blake2_128Concat, T::AccountId, u32, ValueQuery>;

    /// Set of witnesses that already witnessed a (window, target) — dedup.
    #[pallet::storage]
    pub type Witnesses<T: Config> = StorageNMap<
        _,
        (
            NMapKey<Twox64Concat, u64>,
            NMapKey<Blake2_128Concat, T::AccountId>,
            NMapKey<Blake2_128Concat, T::AccountId>,
        ),
        (),
        OptionQuery,
    >;

    /// Running total of registered full nodes (for stats / reward routing).
    #[pallet::storage]
    pub type FullNodeCount<T: Config> = StorageValue<_, u32, ValueQuery>;

    // ── Genesis ────────────────────────────────────────────────────────────

    #[pallet::genesis_config]
    #[derive(frame_support::DefaultNoBound)]
    pub struct GenesisConfig<T: Config> {
        #[serde(skip)]
        pub _config: sp_std::marker::PhantomData<T>,
    }

    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            <FullNodeCount<T>>::put(0u32);
        }
    }

    // ── Events ─────────────────────────────────────────────────────────────

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        FullNodeRegistered {
            who: T::AccountId,
        },
        FullNodeUnregistered {
            who: T::AccountId,
        },
        WitnessSubmitted {
            target: T::AccountId,
            witness: T::AccountId,
            window: u64,
        },
        WindowConfirmed {
            who: T::AccountId,
            window: u64,
        },
        WindowMissed {
            who: T::AccountId,
            window: u64,
            missed_count: u32,
        },
        BondSlashed {
            who: T::AccountId,
            penalty: BalanceOf<T>,
        },
    }

    // ── Errors ─────────────────────────────────────────────────────────────

    #[pallet::error]
    pub enum Error<T> {
        InsufficientBond,
        AlreadyRegistered,
        NotRegistered,
        CannotWitnessSelf,
        WitnessMustBeFullNode,
        EndpointTooLong,
        NotEnoughWitnesses,
        NoExistingRecord,
        BondTransferFailed,
    }

    // ── Calls ──────────────────────────────────────────────────────────────

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// Bond + register as a full node with a public endpoint.
        #[pallet::call_index(0)]
        #[pallet::weight(<T as Config>::WeightInfo::register_fullnode())]
        pub fn register_fullnode(origin: OriginFor<T>, endpoint: Vec<u8>) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(
                !<Endpoints<T>>::contains_key(&who),
                Error::<T>::AlreadyRegistered
            );
            let max_len = T::MaxEndpointLen::get() as usize;
            ensure!(endpoint.len() <= max_len, Error::<T>::EndpointTooLong);
            ensure!(!endpoint.is_empty(), Error::<T>::EndpointTooLong);

            let bond = T::LivenessBond::get();
            T::Currency::transfer(
                &who,
                &Self::account_id(),
                bond,
                ExistenceRequirement::KeepAlive,
            )?;

            let now = Self::window_id();
            let bounded: BoundedVec<u8, T::MaxEndpointLen> = endpoint
                .try_into()
                .map_err(|_| Error::<T>::EndpointTooLong)?;
            <Endpoints<T>>::insert(&who, bounded);
            <Bonds<T>>::insert(&who, bond);
            // A node registered in window R is first finalizable in window R+1
            // (its registration window is partial). last_confirmed_window starts
            // at R so window R is skipped and R+1 onward are finalizable.
            <FullNodes<T>>::insert(
                &who,
                FullNodeRecord {
                    last_confirmed_window: now,
                    uptime_bits: 0,
                    missed_count: 0,
                    active: true,
                },
            );
            <FullNodeCount<T>>::mutate(|c| *c = c.saturating_add(1));
            Self::deposit_event(Event::<T>::FullNodeRegistered { who });
            Ok(())
        }

        /// Withdraw the bond and stop being a full node.
        #[pallet::call_index(1)]
        #[pallet::weight(<T as Config>::WeightInfo::unregister_fullnode())]
        pub fn unregister_fullnode(origin: OriginFor<T>) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let bond = <Bonds<T>>::take(&who).ok_or(Error::<T>::NotRegistered)?;
            <Endpoints<T>>::remove(&who);
            <FullNodes<T>>::remove(&who);
            <FullNodeCount<T>>::mutate(|c| *c = c.saturating_sub(1));
            T::Currency::transfer(
                &Self::account_id(),
                &who,
                bond,
                ExistenceRequirement::AllowDeath,
            )?;
            Self::deposit_event(Event::<T>::FullNodeUnregistered { who });
            Ok(())
        }

        /// A bonded full node witnesses that it successfully reached `target`'s
        /// declared endpoint this window. Dedup per (window, target, witness).
        #[pallet::call_index(2)]
        #[pallet::weight(<T as Config>::WeightInfo::witness_reachable())]
        pub fn witness_reachable(origin: OriginFor<T>, target: T::AccountId) -> DispatchResult {
            let witness = ensure_signed(origin)?;
            ensure!(witness != target, Error::<T>::CannotWitnessSelf);
            // Witness must itself be a bonded, active full node.
            ensure!(
                <Bonds<T>>::contains_key(&witness),
                Error::<T>::WitnessMustBeFullNode
            );
            ensure!(
                <Endpoints<T>>::contains_key(&target),
                Error::<T>::NotRegistered
            );

            let window = Self::window_id();
            let key = (window, target.clone(), witness.clone());
            ensure!(
                !<Witnesses<T>>::contains_key(key.clone()),
                Error::<T>::NoExistingRecord
            ); // already witnessed
            <Witnesses<T>>::insert(key, ());
            <WitnessCounts<T>>::mutate(window, &target, |c| *c = c.saturating_add(1));
            Self::deposit_event(Event::<T>::WitnessSubmitted {
                target,
                witness,
                window,
            });
            Ok(())
        }

        /// Finalize the most recent completed window for `target` (anyone may
        /// call, mirroring attestation's `check_expired`). If target gathered
        /// enough distinct witnesses that window it's confirmed online; else the
        /// miss is recorded and sustained absence slashes the bond.
        #[pallet::call_index(3)]
        #[pallet::weight(<T as Config>::WeightInfo::submit_liveness())]
        pub fn submit_liveness(origin: OriginFor<T>, target: T::AccountId) -> DispatchResult {
            let _caller = ensure_signed(origin)?;
            let mut record = <FullNodes<T>>::get(&target).ok_or(Error::<T>::NoExistingRecord)?;

            let window = Self::window_id();
            // Finalize the window that just ended (window - 1).
            if window == 0 {
                return Ok(());
            }
            let finalizing = window - 1;

            // If already finalized at/after this window, no-op (idempotent).
            if record.last_confirmed_window >= finalizing {
                return Ok(());
            }

            let min_w = T::MinWitnesses::get();
            let got = <WitnessCounts<T>>::get(finalizing, &target);
            let confirmed = got >= min_w;

            if confirmed {
                // Shift uptime bits left by windows elapsed since last record,
                // set bit0 = confirmed.
                let elapsed = finalizing.saturating_sub(record.last_confirmed_window);
                let lookback = T::UptimeLookback::get() as u64;
                let shift = elapsed.min(lookback);
                let bits = if shift >= lookback as u64 {
                    0
                } else {
                    record.uptime_bits << shift as u32
                };
                record.uptime_bits = (bits | 1u32) & Self::bitmask(lookback as u32);
                record.last_confirmed_window = finalizing;
                record.missed_count = 0;
                record.active = true;
                Self::deposit_event(Event::<T>::WindowConfirmed {
                    who: target.clone(),
                    window: finalizing,
                });
            } else {
                // Missed: decay uptime (clear bit0 region) and count the miss.
                record.missed_count = record.missed_count.saturating_add(1);
                // A miss shifts history and leaves bit0 = 0.
                let lookback = T::UptimeLookback::get() as u64;
                record.uptime_bits = (record.uptime_bits << 1u32) & Self::bitmask(lookback as u32);
                record.last_confirmed_window = finalizing;
                let missed = record.missed_count;
                Self::deposit_event(Event::<T>::WindowMissed {
                    who: target.clone(),
                    window: finalizing,
                    missed_count: missed,
                });

                if missed >= T::MaxMissedLiveness::get() {
                    // Slash the bond.
                    if let Some(bond) = <Bonds<T>>::get(&target) {
                        let slash_bps = T::SlashBps::get();
                        let penalty: BalanceOf<T> = bond * slash_bps.into() / 10_000u32.into();
                        let remaining = bond.saturating_sub(penalty);
                        if remaining.is_zero() {
                            <Bonds<T>>::remove(&target);
                            <Endpoints<T>>::remove(&target);
                            record.active = false;
                        } else {
                            <Bonds<T>>::insert(&target, remaining);
                        }
                        Self::deposit_event(Event::<T>::BondSlashed {
                            who: target.clone(),
                            penalty,
                        });
                    }
                }
            }

            <FullNodes<T>>::insert(&target, record);
            Ok(())
        }
    }

    // ── Helpers ────────────────────────────────────────────────────────────

    impl<T: Config> Pallet<T> {
        pub fn account_id() -> T::AccountId {
            frame_support::PalletId(*b"gpn/live").into_account_truncating()
        }

        fn window_id() -> u64 {
            let now: u64 = frame_system::Pallet::<T>::block_number()
                .try_into()
                .unwrap_or(0);
            let window = T::LivenessWindow::get();
            if window == 0 {
                0
            } else {
                now / window
            }
        }

        fn bitmask(n: u32) -> u32 {
            if n >= 32 {
                u32::MAX
            } else {
                (1u32 << n).saturating_sub(1)
            }
        }

        fn popcount(mut x: u32) -> u32 {
            let mut c = 0u32;
            while x != 0 {
                c += x & 1;
                x >>= 1;
            }
            c
        }

        pub fn is_active(who: &T::AccountId) -> bool {
            <FullNodes<T>>::get(who).map(|r| r.active).unwrap_or(false)
        }

        pub fn uptime_bps(who: &T::AccountId) -> u32 {
            let lookback = T::UptimeLookback::get();
            if lookback == 0 {
                return 0;
            }
            match <FullNodes<T>>::get(who) {
                Some(r) => Self::popcount(r.uptime_bits) * 10_000u32 / lookback,
                None => 0,
            }
        }

        /// Whether `who` is currently registered as a full node.
        pub fn is_registered(who: &T::AccountId) -> bool {
            <FullNodes<T>>::get(who).is_some()
        }

        /// The raw liveness record for `who`, if registered.
        pub fn record(who: &T::AccountId) -> Option<FullNodeRecord> {
            <FullNodes<T>>::get(who)
        }

        /// Consecutive liveness windows missed by `who` (0 if unregistered).
        pub fn missed_count(who: &T::AccountId) -> u32 {
            <FullNodes<T>>::get(who)
                .map(|r| r.missed_count)
                .unwrap_or(0)
        }

        /// The bond currently held for `who` (0 if none / unregistered).
        pub fn bond(who: &T::AccountId) -> BalanceOf<T> {
            <Bonds<T>>::get(who).unwrap_or_else(Zero::zero)
        }

        /// Running total of registered full nodes.
        pub fn full_node_count() -> u32 {
            FullNodeCount::<T>::get()
        }
    }
}

/// Implement the shared trait so the attestation pallet can gate tiers.
impl<T: Config> LivenessProvider<T::AccountId> for Pallet<T> {
    fn is_active_full_node(who: &T::AccountId) -> bool {
        Pallet::<T>::is_active(who)
    }
    fn window_uptime_bps(who: &T::AccountId) -> u32 {
        Pallet::<T>::uptime_bps(who)
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

    use crate::pallet as local_liveness;
    use crate::pallet::{Bonds, Endpoints, FullNodeCount, FullNodes};

    type Block = frame_system::mocking::MockBlock<Test>;
    type Balance = u128;

    construct_runtime!(
        pub struct Test {
            System: frame_system,
            Balances: pallet_balances,
            Liveness: local_liveness,
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
        pub const TestBond: Balance = 500;
        pub const TestWindow: u64 = 10;
        pub const TestMinWitnesses: u32 = 2;
        pub const TestMaxMissed: u32 = 3;
        pub const TestSlashBps: u32 = 1_000;
        pub const TestLookback: u32 = 10;
        pub const TestMaxEndpointLen: u32 = 256;
    }

    impl crate::pallet::Config for Test {
        type WeightInfo = ();
        type RuntimeEvent = RuntimeEvent;
        type Currency = Balances;
        type LivenessBond = TestBond;
        type LivenessWindow = TestWindow;
        type MinWitnesses = TestMinWitnesses;
        type MaxMissedLiveness = TestMaxMissed;
        type SlashBps = TestSlashBps;
        type UptimeLookback = TestLookback;
        type MaxEndpointLen = TestMaxEndpointLen;
    }

    const ALICE: u64 = 1;
    const BOB: u64 = 2;
    const CHARLIE: u64 = 3;

    pub fn new_test_ext() -> sp_io::TestExternalities {
        let mut t = frame_system::GenesisConfig::<Test>::default()
            .build_storage()
            .unwrap();
        pallet_balances::GenesisConfig::<Test> {
            balances: vec![(ALICE, 10_000), (BOB, 10_000), (CHARLIE, 10_000)],
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

    type LivenessPallet = crate::pallet::Pallet<Test>;

    fn register(who: u64) {
        LivenessPallet::register_fullnode(
            RuntimeOrigin::signed(who),
            format!("/ip4/1.2.3.4/tcp/30333/p2p/{}", who).into_bytes(),
        )
        .unwrap();
    }

    #[test]
    fn test_register_moves_bond() {
        new_test_ext().execute_with(|| {
            let before = pallet_balances::Pallet::<Test>::free_balance(ALICE);
            register(ALICE);
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(ALICE),
                before - TestBond::get()
            );
            assert_eq!(Bonds::<Test>::get(ALICE), Some(TestBond::get()));
            assert_eq!(FullNodeCount::<Test>::get(), 1);
            assert!(Endpoints::<Test>::get(ALICE).is_some());
        });
    }

    #[test]
    fn test_register_rejects_duplicate() {
        new_test_ext().execute_with(|| {
            register(ALICE);
            let r =
                LivenessPallet::register_fullnode(RuntimeOrigin::signed(ALICE), b"/ip4/x".to_vec());
            assert_eq!(r, Err(Error::<Test>::AlreadyRegistered.into()));
        });
    }

    #[test]
    fn test_cannot_witness_self_or_non_fullnode() {
        new_test_ext().execute_with(|| {
            register(ALICE);
            // Self witness rejected.
            let r = LivenessPallet::witness_reachable(RuntimeOrigin::signed(ALICE), ALICE);
            assert_eq!(r, Err(Error::<Test>::CannotWitnessSelf.into()));
            // Non-registered BOB cannot witness.
            let r = LivenessPallet::witness_reachable(RuntimeOrigin::signed(BOB), ALICE);
            assert_eq!(r, Err(Error::<Test>::WitnessMustBeFullNode.into()));
        });
    }

    #[test]
    fn test_confirmed_with_enough_witnesses() {
        new_test_ext().execute_with(|| {
            register(ALICE); // window 0 (registration, partial)
            register(BOB);
            register(CHARLIE);
            // Advance to window 1 (block 11) — witnesses here count for window 1.
            System::set_block_number(11);
            LivenessPallet::witness_reachable(RuntimeOrigin::signed(BOB), ALICE).unwrap();
            LivenessPallet::witness_reachable(RuntimeOrigin::signed(CHARLIE), ALICE).unwrap();
            // Advance to window 2 (block 21) — submit_liveness finalizes window 1.
            System::set_block_number(21);
            LivenessPallet::submit_liveness(RuntimeOrigin::signed(CHARLIE), ALICE).unwrap();
            let rec = FullNodes::<Test>::get(ALICE).unwrap();
            assert!(rec.active);
            assert!(crate::pallet::Pallet::<Test>::uptime_bps(&ALICE) > 0);
        });
    }

    #[test]
    fn test_missed_window_then_slash_after_limit() {
        new_test_ext().execute_with(|| {
            register(ALICE); // window 0 (registration, partial — not finalizable)
                             // Windows 1,2,3 are finalizable; nobody witnesses ALICE in any of them.
                             // Finalize window 1 (miss 1): submit while in window 2.
            System::set_block_number(21);
            LivenessPallet::submit_liveness(RuntimeOrigin::signed(ALICE), ALICE).unwrap();
            // Finalize window 2 (miss 2): submit while in window 3.
            System::set_block_number(31);
            LivenessPallet::submit_liveness(RuntimeOrigin::signed(ALICE), ALICE).unwrap();
            // Finalize window 3 (miss 3 = limit): submit while in window 4 → slash.
            System::set_block_number(41);
            LivenessPallet::submit_liveness(RuntimeOrigin::signed(ALICE), ALICE).unwrap();
            let rec = FullNodes::<Test>::get(ALICE).unwrap();
            assert_eq!(rec.missed_count, 3);
            // After 3rd miss the bond was slashed 10% (remaining > 0 so not removed).
            let bond = Bonds::<Test>::get(ALICE).unwrap();
            assert_eq!(bond, TestBond::get() - TestBond::get() / 10);
        });
    }

    #[test]
    fn test_typed_getters_for_rpc() {
        new_test_ext().execute_with(|| {
            // Unknown account: all getters return neutral values (the RPC never
            // errors on an unknown account; it reports registered = false).
            assert!(!LivenessPallet::is_registered(&ALICE));
            assert!(!LivenessPallet::is_active(&ALICE));
            assert!(LivenessPallet::record(&ALICE).is_none());
            assert_eq!(LivenessPallet::missed_count(&ALICE), 0);
            assert_eq!(LivenessPallet::bond(&ALICE), 0);
            assert_eq!(LivenessPallet::uptime_bps(&ALICE), 0);
            assert_eq!(LivenessPallet::full_node_count(), 0);

            register(ALICE);
            assert!(LivenessPallet::is_registered(&ALICE));
            assert!(LivenessPallet::is_active(&ALICE));
            assert_eq!(LivenessPallet::bond(&ALICE), TestBond::get());
            assert_eq!(LivenessPallet::full_node_count(), 1);
            let rec = LivenessPallet::record(&ALICE).unwrap();
            assert!(rec.active);
            assert_eq!(rec.missed_count, 0);
        });
    }
}
