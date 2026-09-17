//! # GPN Bridge pallet (Hyperlane Warp Route — GPN side)
//!
//! Locks/unlocks native GPN against a wrapped `wGPN` ERC-20 on Ethereum so GPN
//! can be traded on a DEX at mainnet launch. This pallet is the **GPN-side
//! adapter** only; the actual cross-chain transport, relayer and Ethereum-side
//! `wGPN` + Warp Route are inherited from Hyperlane (see
//! `design-crosschain-bridge.md`, workplan item 10).
//!
//! ## Model (lock / mint, unlock / burn)
//!
//! - **`bridge_out`** (GPN → Ethereum): caller transfers `amount` GPN into the
//!   keyless bridge treasury and emits an outbound message. Off-chain, a relayer
//!   observes this and mints `wGPN` on Ethereum 1:1.
//! - **`bridge_in`** (Ethereum → GPN): after `wGPN` is burned on Ethereum, a
//!   relayer submits the corresponding inbound message. The pallet verifies it
//!   against the configured **ISM** and releases `amount` from the treasury to
//!   the GPN recipient.
//!
//! ## Supply conservation (the core safety property)
//!
//! The bridge **never** mints native GPN. It only moves existing supply into and
//! out of the treasury account, so:
//!
//!   `wGPN outstanding == TotalLocked == (sum of bridge_out) - (sum of bridge_in)`
//!
//! and the canonical 1B cap owned by `pallet-gpn-token` is untouched.
//!
//! ## Security posture (mirrors Hyperlane's "sovereign ISM")
//!
//! The pallet's message-authenticity check is a pluggable ISM. This prototype
//! ships a **mock ISM** (a trusted-relayer allowlist) — the conservative
//! N-of-M validator-set posture we'd use at launch, upgradeable to a more
//! decentralized ISM later. Replay protection (message-hash dedupe), a
//! rate-limit and a pause circuit-breaker bound the blast radius.

#![cfg_attr(not(feature = "std"), no_std)]

pub use pallet::*;

#[frame_support::pallet]
pub mod pallet {
    use frame_support::pallet_prelude::*;
    use frame_support::traits::{Currency, ExistenceRequirement};
    use frame_system::pallet_prelude::*;
    use sp_runtime::traits::{Hash, Saturating, Zero};
    use sp_std::prelude::*;

    pub type BalanceOf<T> =
        <<T as Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::Balance;

    #[pallet::pallet]
    #[pallet::without_storage_info]
    pub struct Pallet<T>(_);

    #[pallet::config]
    pub trait Config: frame_system::Config {
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;
        type Currency: Currency<Self::AccountId>;
        /// The keyless bridge treasury account (holds locked GPN 1:1 backing wGPN).
        #[pallet::constant]
        type BridgeTreasuryAccount: Get<Self::AccountId>;
        /// Rate-limit window length in blocks.
        #[pallet::constant]
        type RateLimitWindow: Get<BlockNumberFor<Self>>;
        /// Max total volume across the bridge (either direction) per window.
        #[pallet::constant]
        type MaxWindowVolume: Get<BalanceOf<Self>>;
        /// Max single bridge transfer in either direction.
        #[pallet::constant]
        type MaxSingleTransfer: Get<BalanceOf<Self>>;
        /// Max length of the destination address bytes (EVM = 20, Substrate = 32).
        #[pallet::constant]
        type MaxDestAddressLength: Get<u32>;
        type WeightInfo: WeightInfo;
    }

    // ── Storage ────────────────────────────────────────────────────────────

    /// Mock ISM: the trusted relayer/validator allowlist. Only these accounts may
    /// call `bridge_in`. This is where a real Hyperlane ISM (signature/threshold
    /// verification) plugs in.
    #[pallet::storage]
    pub type Relayers<T: Config> = StorageMap<_, Blake2_128Concat, T::AccountId, (), OptionQuery>;

    /// Total native GPN currently locked in the treasury (== wGPN outstanding).
    #[pallet::storage]
    pub type TotalLocked<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    /// Monotonic outbound nonce (ids the GPN → Ethereum messages).
    #[pallet::storage]
    pub type OutboundNonce<T: Config> = StorageValue<_, u64, ValueQuery>;

    /// Replay guard: set of already-processed inbound message hashes.
    #[pallet::storage]
    pub type ProcessedMessages<T: Config> = StorageMap<_, Blake2_128Concat, T::Hash, (), OptionQuery>;

    /// Circuit-breaker: when `true`, both directions are halted.
    #[pallet::storage]
    pub type Paused<T: Config> = StorageValue<_, bool, ValueQuery>;

    /// Rate-limit window start block.
    #[pallet::storage]
    pub type WindowStart<T: Config> = StorageValue<_, BlockNumberFor<T>, ValueQuery>;

    /// Volume locked outbound (GPN → Ethereum) within the current window.
    #[pallet::storage]
    pub type WindowOutbound<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    /// Volume unlocked inbound (Ethereum → GPN) within the current window.
    #[pallet::storage]
    pub type WindowInbound<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    // ── Genesis ────────────────────────────────────────────────────────────

    #[pallet::genesis_config]
    #[derive(frame_support::DefaultNoBound)]
    pub struct GenesisConfig<T: Config> {
        /// Initial trusted relayer/validator set (mock ISM).
        pub relayers: Vec<T::AccountId>,
        #[serde(skip)]
        pub _config: sp_std::marker::PhantomData<T>,
    }

    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            for r in &self.relayers {
                Relayers::<T>::insert(r, ());
            }
            let zero: BalanceOf<T> = Zero::zero();
            TotalLocked::<T>::put(zero);
            OutboundNonce::<T>::put(0u64);
            Paused::<T>::put(false);
            WindowStart::<T>::put(frame_system::Pallet::<T>::block_number());
            WindowOutbound::<T>::put(zero);
            WindowInbound::<T>::put(zero);
        }
    }

    // ── Events ─────────────────────────────────────────────────────────────

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        BridgeOut {
            who: T::AccountId,
            nonce: u64,
            dest: BoundedVec<u8, T::MaxDestAddressLength>,
            amount: BalanceOf<T>,
        },
        BridgeIn {
            relayer: T::AccountId,
            source_chain: u32,
            recipient: T::AccountId,
            amount: BalanceOf<T>,
            message_hash: T::Hash,
        },
        RelayerUpdated {
            relayer: T::AccountId,
            allowed: bool,
        },
        Paused,
        Resumed,
    }

    // ── Errors ─────────────────────────────────────────────────────────────

    #[pallet::error]
    pub enum Error<T> {
        /// Bridge is paused (circuit-breaker open).
        BridgePaused,
        /// Caller is not a trusted relayer (mock ISM rejection).
        NotRelayer,
        /// Inbound message already processed (replay).
        AlreadyProcessed,
        /// Rate-limit exceeded for this window.
        RateLimitExceeded,
        /// Single transfer exceeds `MaxSingleTransfer`.
        TransferExceedsSingleLimit,
        /// Amount is zero.
        AmountTooLow,
        /// Would unlock more than is currently locked (insolvent bridge).
        InsufficientLockedBalance,
        /// Destination address is empty.
        EmptyDestination,
    }

    // ── Calls ──────────────────────────────────────────────────────────────

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// Lock native GPN into the bridge treasury to mint `wGPN` on Ethereum.
        ///
        /// Permissionless: any account with a GPN balance may bridge out.
        #[pallet::call_index(0)]
        #[pallet::weight(T::WeightInfo::bridge_out())]
        pub fn bridge_out(
            origin: OriginFor<T>,
            dest: BoundedVec<u8, T::MaxDestAddressLength>,
            amount: BalanceOf<T>,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            Self::ensure_live()?;
            ensure!(!dest.is_empty(), Error::<T>::EmptyDestination);
            ensure!(!amount.is_zero(), Error::<T>::AmountTooLow);
            ensure!(
                amount <= T::MaxSingleTransfer::get(),
                Error::<T>::TransferExceedsSingleLimit
            );

            Self::bump_rate_limit(false, amount)?;

            // Move native GPN caller → treasury. Total issuance is unchanged.
            let treasury = T::BridgeTreasuryAccount::get();
            T::Currency::transfer(
                &who,
                &treasury,
                amount,
                ExistenceRequirement::AllowDeath,
            )?;

            TotalLocked::<T>::mutate(|l| *l = l.saturating_add(amount));

            let nonce = OutboundNonce::<T>::get();
            OutboundNonce::<T>::put(nonce.saturating_add(1));

            Self::deposit_event(Event::<T>::BridgeOut {
                who,
                nonce,
                dest,
                amount,
            });
            Ok(())
        }

        /// Release native GPN after `wGPN` is burned on Ethereum.
        ///
        /// Caller must be a trusted relayer (mock ISM). The message is
        /// deduplicated by its content hash (computed from the fields, so a
        /// relayer cannot forge a colliding dedupe key) and bounded by
        /// `TotalLocked`.
        #[pallet::call_index(1)]
        #[pallet::weight(T::WeightInfo::bridge_in())]
        pub fn bridge_in(
            origin: OriginFor<T>,
            source_chain: u32,
            recipient: T::AccountId,
            amount: BalanceOf<T>,
            nonce: u64,
        ) -> DispatchResult {
            let relayer = ensure_signed(origin)?;
            Self::ensure_live()?;
            ensure!(Relayers::<T>::contains_key(&relayer), Error::<T>::NotRelayer);

            ensure!(!amount.is_zero(), Error::<T>::AmountTooLow);
            ensure!(
                amount <= T::MaxSingleTransfer::get(),
                Error::<T>::TransferExceedsSingleLimit
            );

            // Replay guard: each inbound message processed exactly once.
            let message_hash = Self::message_hash(source_chain, &recipient, amount, nonce);
            ensure!(
                !ProcessedMessages::<T>::contains_key(message_hash),
                Error::<T>::AlreadyProcessed
            );

            // Solvency: can never release more than was locked.
            ensure!(
                amount <= TotalLocked::<T>::get(),
                Error::<T>::InsufficientLockedBalance
            );

            Self::bump_rate_limit(true, amount)?;

            // Release native GPN treasury → recipient. Total issuance unchanged.
            let treasury = T::BridgeTreasuryAccount::get();
            T::Currency::transfer(
                &treasury,
                &recipient,
                amount,
                ExistenceRequirement::AllowDeath,
            )?;

            TotalLocked::<T>::mutate(|l| *l = l.saturating_sub(amount));
            ProcessedMessages::<T>::insert(message_hash, ());

            Self::deposit_event(Event::<T>::BridgeIn {
                relayer,
                source_chain,
                recipient,
                amount,
                message_hash,
            });
            Ok(())
        }

        /// Root/admin: add or remove a trusted relayer (mock ISM membership).
        #[pallet::call_index(2)]
        #[pallet::weight(T::WeightInfo::set_relayer())]
        pub fn set_relayer(
            origin: OriginFor<T>,
            relayer: T::AccountId,
            allowed: bool,
        ) -> DispatchResult {
            ensure_root(origin)?;
            if allowed {
                Relayers::<T>::insert(relayer.clone(), ());
            } else {
                Relayers::<T>::remove(relayer.clone());
            }
            Self::deposit_event(Event::<T>::RelayerUpdated { relayer, allowed });
            Ok(())
        }

        /// Root/admin: halt both directions (circuit-breaker).
        #[pallet::call_index(3)]
        #[pallet::weight(T::WeightInfo::pause())]
        pub fn pause(origin: OriginFor<T>) -> DispatchResult {
            ensure_root(origin)?;
            Paused::<T>::put(true);
            Self::deposit_event(Event::<T>::Paused);
            Ok(())
        }

        /// Root/admin: resume both directions.
        #[pallet::call_index(4)]
        #[pallet::weight(T::WeightInfo::resume())]
        pub fn resume(origin: OriginFor<T>) -> DispatchResult {
            ensure_root(origin)?;
            Paused::<T>::put(false);
            Self::deposit_event(Event::<T>::Resumed);
            Ok(())
        }
    }

    // ── Helpers ────────────────────────────────────────────────────────────

    impl<T: Config> Pallet<T> {
        /// Content hash of an inbound message — the canonical dedupe/replay key.
        pub fn message_hash(
            source_chain: u32,
            recipient: &T::AccountId,
            amount: BalanceOf<T>,
            nonce: u64,
        ) -> T::Hash {
            T::Hashing::hash_of(&(source_chain, recipient, amount, nonce))
        }

        fn ensure_live() -> DispatchResult {
            ensure!(!Paused::<T>::get(), Error::<T>::BridgePaused);
            Ok(())
        }

        /// Advance the rate-limit window if it has elapsed, then check and
        /// accumulate volume for the current window. `inbound` selects which
        /// counter (outbound = GPN→Eth, inbound = Eth→GPN) to bump.
        fn bump_rate_limit(inbound: bool, amount: BalanceOf<T>) -> DispatchResult {
            let now = frame_system::Pallet::<T>::block_number();
            let window = T::RateLimitWindow::get();
            let start = WindowStart::<T>::get();
            if now.saturating_sub(start) >= window {
                // Window elapsed → reset counters for a fresh window.
                let zero: BalanceOf<T> = Zero::zero();
                WindowStart::<T>::put(now);
                WindowOutbound::<T>::put(zero);
                WindowInbound::<T>::put(zero);
            }

            let (current, other) = if inbound {
                (WindowInbound::<T>::get(), WindowOutbound::<T>::get())
            } else {
                (WindowOutbound::<T>::get(), WindowInbound::<T>::get())
            };

            let next = current.saturating_add(amount);
            let total = next.saturating_add(other);
            ensure!(
                next <= T::MaxWindowVolume::get() && total <= T::MaxWindowVolume::get(),
                Error::<T>::RateLimitExceeded
            );

            if inbound {
                WindowInbound::<T>::put(next);
            } else {
                WindowOutbound::<T>::put(next);
            }
            Ok(())
        }

        /// GPN currently locked in the treasury (== wGPN outstanding).
        pub fn total_locked() -> BalanceOf<T> {
            TotalLocked::<T>::get()
        }

        /// Free balance held by the bridge treasury account.
        pub fn treasury_balance() -> BalanceOf<T> {
            T::Currency::free_balance(&T::BridgeTreasuryAccount::get())
        }

        /// Next outbound nonce (ids GPN → Ethereum messages).
        pub fn outbound_nonce() -> u64 {
            OutboundNonce::<T>::get()
        }

        /// Whether the bridge is currently paused.
        pub fn is_paused() -> bool {
            Paused::<T>::get()
        }

        /// Whether `who` is a trusted relayer (mock ISM membership).
        pub fn is_relayer(who: &T::AccountId) -> bool {
            Relayers::<T>::contains_key(who)
        }
    }

    /// Weight functions (unbenchmarked for now — `()` returns zero).
    pub trait WeightInfo {
        fn bridge_out() -> Weight;
        fn bridge_in() -> Weight;
        fn set_relayer() -> Weight;
        fn pause() -> Weight;
        fn resume() -> Weight;
    }

    impl WeightInfo for () {
        fn bridge_out() -> Weight {
            Weight::zero()
        }
        fn bridge_in() -> Weight {
            Weight::zero()
        }
        fn set_relayer() -> Weight {
            Weight::zero()
        }
        fn pause() -> Weight {
            Weight::zero()
        }
        fn resume() -> Weight {
            Weight::zero()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate as pallet_gpn_bridge;
        use frame_support::{
            assert_noop, assert_ok, construct_runtime, derive_impl, parameter_types,
            traits::{ConstU16, ConstU32, ConstU64, ConstU128, Everything},
        };
        use sp_runtime::{
            traits::IdentityLookup,
            BuildStorage,
        };

        type Block = frame_system::mocking::MockBlock<Test>;
        type Balance = u128;
        type AccountId = u64;

        construct_runtime!(
            pub enum Test {
                System: frame_system,
                Balances: pallet_balances,
                Bridge: pallet_gpn_bridge,
            }
        );

        #[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
        impl frame_system::Config for Test {
            type BaseCallFilter = Everything;
            type Block = Block;
            type AccountId = AccountId;
            type BlockHashCount = ConstU64<250>;
            type Lookup = IdentityLookup<AccountId>;
            type SS58Prefix = ConstU16<42>;
            type AccountData = pallet_balances::AccountData<Balance>;
        }

        impl pallet_balances::Config for Test {
            type Balance = Balance;
            type DustRemoval = ();
            type RuntimeEvent = RuntimeEvent;
            type ExistentialDeposit = ConstU128<1>;
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
            pub const BridgeTreasuryAccount: AccountId = 99;
            pub const RateLimitWindow: u64 = 10;
            pub const MaxWindowVolume: Balance = 10_000;
            pub const MaxSingleTransfer: Balance = 1_000;
            pub const MaxDestAddressLength: u32 = 32;
        }

        impl pallet_gpn_bridge::Config for Test {
            type RuntimeEvent = RuntimeEvent;
            type Currency = Balances;
            type BridgeTreasuryAccount = BridgeTreasuryAccount;
            type RateLimitWindow = RateLimitWindow;
            type MaxWindowVolume = MaxWindowVolume;
            type MaxSingleTransfer = MaxSingleTransfer;
            type MaxDestAddressLength = MaxDestAddressLength;
            type WeightInfo = ();
        }

        const RELAYER: AccountId = 7;
        const ALICE: AccountId = 1;
        const ETH_ADDR: [u8; 20] = [0xaa; 20];

        fn new_test_ext() -> sp_io::TestExternalities {
            let mut t = frame_system::GenesisConfig::<Test>::default()
                .build_storage()
                .unwrap();
            pallet_balances::GenesisConfig::<Test> {
                balances: vec![(ALICE, 1_000_000), (RELAYER, 1_000_000)],
                dev_accounts: None,
            }
            .assimilate_storage(&mut t)
            .unwrap();
            pallet_gpn_bridge::GenesisConfig::<Test> {
                relayers: vec![RELAYER],
                _config: Default::default(),
            }
            .assimilate_storage(&mut t)
            .unwrap();
            let mut ext = sp_io::TestExternalities::new(t);
            ext.execute_with(|| System::set_block_number(1));
            ext
        }

        fn dest(bytes: &[u8]) -> BoundedVec<u8, MaxDestAddressLength> {
            bytes.to_vec().try_into().unwrap()
        }

        #[test]
        fn bridge_out_locks_into_treasury_and_preserves_issuance() {
            new_test_ext().execute_with(|| {
                let issuance_before = Balances::total_issuance();
                assert_ok!(Bridge::bridge_out(
                    RuntimeOrigin::signed(ALICE),
                    dest(&ETH_ADDR),
                    500
                ));
                assert_eq!(Bridge::treasury_balance(), 500);
                assert_eq!(Bridge::total_locked(), 500);
                assert_eq!(Balances::free_balance(ALICE), 999_500);
                // Locking moves supply; it does not mint.
                assert_eq!(Balances::total_issuance(), issuance_before);
            });
        }

        #[test]
        fn bridge_out_increments_nonce() {
            new_test_ext().execute_with(|| {
                assert_eq!(Bridge::outbound_nonce(), 0);
                assert_ok!(Bridge::bridge_out(
                    RuntimeOrigin::signed(ALICE),
                    dest(&ETH_ADDR),
                    100
                ));
                assert_eq!(Bridge::outbound_nonce(), 1);
                assert_ok!(Bridge::bridge_out(
                    RuntimeOrigin::signed(ALICE),
                    dest(&ETH_ADDR),
                    100
                ));
                assert_eq!(Bridge::outbound_nonce(), 2);
            });
        }

        #[test]
        fn bridge_out_rejects_zero_and_empty_dest() {
            new_test_ext().execute_with(|| {
                assert_noop!(
                    Bridge::bridge_out(RuntimeOrigin::signed(ALICE), dest(&ETH_ADDR), 0),
                    Error::<Test>::AmountTooLow
                );
                assert_noop!(
                    Bridge::bridge_out(RuntimeOrigin::signed(ALICE), dest(&[]), 100),
                    Error::<Test>::EmptyDestination
                );
            });
        }

        #[test]
        fn bridge_out_rejects_over_single_limit() {
            new_test_ext().execute_with(|| {
                assert_noop!(
                    Bridge::bridge_out(RuntimeOrigin::signed(ALICE), dest(&ETH_ADDR), 1_001),
                    Error::<Test>::TransferExceedsSingleLimit
                );
            });
        }

        #[test]
        fn bridge_in_requires_trusted_relayer() {
            new_test_ext().execute_with(|| {
                assert_ok!(Bridge::bridge_out(
                    RuntimeOrigin::signed(ALICE),
                    dest(&ETH_ADDR),
                    500
                ));
                assert_noop!(
                    Bridge::bridge_in(RuntimeOrigin::signed(ALICE), 1, 2, 100, 0),
                    Error::<Test>::NotRelayer
                );
            });
        }

        #[test]
        fn bridge_in_unlocks_and_preserves_issuance() {
            new_test_ext().execute_with(|| {
                assert_ok!(Bridge::bridge_out(
                    RuntimeOrigin::signed(ALICE),
                    dest(&ETH_ADDR),
                    500
                ));
                let issuance_before = Balances::total_issuance();
                assert_ok!(Bridge::bridge_in(
                    RuntimeOrigin::signed(RELAYER),
                    1,
                    2,
                    300,
                    0
                ));
                assert_eq!(Balances::free_balance(2), 300);
                assert_eq!(Bridge::treasury_balance(), 200);
                assert_eq!(Bridge::total_locked(), 200);
                assert_eq!(Balances::total_issuance(), issuance_before);
            });
        }

        #[test]
        fn bridge_in_rejects_replay() {
            new_test_ext().execute_with(|| {
                assert_ok!(Bridge::bridge_out(
                    RuntimeOrigin::signed(ALICE),
                    dest(&ETH_ADDR),
                    500
                ));
                assert_ok!(Bridge::bridge_in(
                    RuntimeOrigin::signed(RELAYER),
                    1,
                    2,
                    100,
                    0
                ));
                assert_noop!(
                    Bridge::bridge_in(RuntimeOrigin::signed(RELAYER), 1, 2, 100, 0),
                    Error::<Test>::AlreadyProcessed
                );
            });
        }

        #[test]
        fn bridge_in_rejects_insolvent_unlock() {
            new_test_ext().execute_with(|| {
                assert_ok!(Bridge::bridge_out(
                    RuntimeOrigin::signed(ALICE),
                    dest(&ETH_ADDR),
                    500
                ));
                assert_noop!(
                    Bridge::bridge_in(RuntimeOrigin::signed(RELAYER), 1, 2, 501, 0),
                    Error::<Test>::InsufficientLockedBalance
                );
            });
        }

        #[test]
        fn rate_limit_enforced_across_window() {
            new_test_ext().execute_with(|| {
                // 10 transfers of 1000 = 10_000, exactly at the window cap.
                for _ in 0..10 {
                    assert_ok!(Bridge::bridge_out(
                        RuntimeOrigin::signed(ALICE),
                        dest(&ETH_ADDR),
                        1_000
                    ));
                }
                // 11th would exceed MaxWindowVolume (10_000).
                assert_noop!(
                    Bridge::bridge_out(RuntimeOrigin::signed(ALICE), dest(&ETH_ADDR), 1),
                    Error::<Test>::RateLimitExceeded
                );
            });
        }

        #[test]
        fn rate_limit_resets_after_window() {
            new_test_ext().execute_with(|| {
                for _ in 0..10 {
                    assert_ok!(Bridge::bridge_out(
                        RuntimeOrigin::signed(ALICE),
                        dest(&ETH_ADDR),
                        1_000
                    ));
                }
                // Advance past the window (10 blocks).
                System::set_block_number(11);
                assert_ok!(Bridge::bridge_out(
                    RuntimeOrigin::signed(ALICE),
                    dest(&ETH_ADDR),
                    1_000
                ));
            });
        }

        #[test]
        fn pause_halts_both_directions() {
            new_test_ext().execute_with(|| {
                assert_ok!(Bridge::bridge_out(
                    RuntimeOrigin::signed(ALICE),
                    dest(&ETH_ADDR),
                    100
                ));
                assert_ok!(Bridge::pause(RuntimeOrigin::root()));
                assert!(Bridge::is_paused());
                assert_noop!(
                    Bridge::bridge_out(RuntimeOrigin::signed(ALICE), dest(&ETH_ADDR), 1),
                    Error::<Test>::BridgePaused
                );
                assert_noop!(
                    Bridge::bridge_in(RuntimeOrigin::signed(RELAYER), 1, 2, 1, 0),
                    Error::<Test>::BridgePaused
                );
                assert_ok!(Bridge::resume(RuntimeOrigin::root()));
                assert!(!Bridge::is_paused());
                assert_ok!(Bridge::bridge_out(
                    RuntimeOrigin::signed(ALICE),
                    dest(&ETH_ADDR),
                    1
                ));
            });
        }

        #[test]
        fn pause_and_set_relayer_require_root() {
            new_test_ext().execute_with(|| {
                assert_noop!(
                    Bridge::pause(RuntimeOrigin::signed(ALICE)),
                    sp_runtime::DispatchError::BadOrigin
                );
                assert_noop!(
                    Bridge::set_relayer(RuntimeOrigin::signed(ALICE), 8, true),
                    sp_runtime::DispatchError::BadOrigin
                );
            });
        }

        #[test]
        fn set_relayer_updates_mock_ism() {
            new_test_ext().execute_with(|| {
                assert!(!Bridge::is_relayer(&8));
                assert_ok!(Bridge::set_relayer(RuntimeOrigin::root(), 8, true));
                assert!(Bridge::is_relayer(&8));
                assert_ok!(Bridge::set_relayer(RuntimeOrigin::root(), 8, false));
                assert!(!Bridge::is_relayer(&8));
            });
        }
    }
}
