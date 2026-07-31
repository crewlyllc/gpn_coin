//! # GPN Token Pallet — Inflation, Fee Distribution, Treasury Management
//!
//! Manages the GPN token supply, inflation schedule, and fee distribution.

#![cfg_attr(not(feature = "std"), no_std)]

use codec::{Decode, DecodeWithMemTracking, Encode, MaxEncodedLen};
use core::fmt::Debug;
use scale_info::TypeInfo;

pub use pallet::*;

/// Fee distribution percentages (in basis points, must sum to 500)
#[derive(Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo)]
pub struct FeeDistribution {
    pub staking_pool_bps: u16,
    pub treasury_bps: u16,
    pub burn_bps: u16,
    pub oracle_bps: u16,
}

impl Default for FeeDistribution {
    fn default() -> Self {
        Self { staking_pool_bps: 200, treasury_bps: 150, burn_bps: 100, oracle_bps: 50 }
    }
}

impl FeeDistribution {
    pub fn is_valid(&self) -> bool {
        (self.staking_pool_bps as u32
            + self.treasury_bps as u32
            + self.burn_bps as u32
            + self.oracle_bps as u32) == 500
    }
}

#[frame_support::pallet]
pub mod pallet {
    use super::*;
    use frame_support::pallet_prelude::*;
    use frame_support::sp_runtime::traits::{AccountIdConversion, Saturating};
    use frame_support::traits::{Currency, ExistenceRequirement};
    use frame_system::pallet_prelude::*;
    use sp_runtime::Perbill;

    pub type BalanceOf<T> =
        <<T as Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::Balance;

    #[pallet::config]
    pub trait Config: frame_system::Config + pallet_balances::Config {
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;
        type Currency: Currency<Self::AccountId>;
        #[pallet::constant]
        type InitialInflation: Get<Perbill>;
        #[pallet::constant]
        type TerminalInflation: Get<Perbill>;
        #[pallet::constant]
        type BlocksPerYear: Get<BlockNumberFor<Self>>;
        #[pallet::constant]
        type TreasuryAccount: Get<Self::AccountId>;
    }

    #[pallet::pallet]
    pub struct Pallet<T>(_);

    // Storage

    #[pallet::storage]
    pub type CurrentInflation<T: Config> = StorageValue<_, Perbill, ValueQuery>;

    #[pallet::storage]
    pub type TotalMinted<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    #[pallet::storage]
    pub type FeeDistributionConfig<T: Config> = StorageValue<_, FeeDistribution, ValueQuery>;

    #[pallet::storage]
    pub type LastInflationUpdate<T: Config> = StorageValue<_, BlockNumberFor<T>, ValueQuery>;

    #[pallet::storage]
    pub type StakingRewardPool<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    #[pallet::storage]
    pub type OracleRewardPool<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    // Genesis

    #[pallet::genesis_config]
    #[derive(frame_support::DefaultNoBound)]
    pub struct GenesisConfig<T: Config> {
        pub initial_inflation: Perbill,
        #[serde(skip)]
        pub _config: sp_std::marker::PhantomData<T>,
    }

    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            <CurrentInflation<T>>::put(self.initial_inflation);
            <FeeDistributionConfig<T>>::put(FeeDistribution::default());
            let zero_bal: BalanceOf<T> = Zero::zero();
            <TotalMinted<T>>::put(zero_bal);
            <StakingRewardPool<T>>::put(zero_bal);
            <OracleRewardPool<T>>::put(zero_bal);
            let zero_block: BlockNumberFor<T> = Zero::zero();
            <LastInflationUpdate<T>>::put(zero_block);
        }
    }

    // Events

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        BlockRewardMinted { amount: BalanceOf<T>, block_number: BlockNumberFor<T> },
        FeesDistributed { staking: BalanceOf<T>, treasury: BalanceOf<T>, burn: BalanceOf<T>, oracle: BalanceOf<T> },
        InflationUpdated { old_rate: Perbill, new_rate: Perbill },
        FeeDistributionUpdated { config: FeeDistribution },
        StakingRewardsPaid { total: BalanceOf<T>, participants: u32 },
    }

    // Errors

    #[pallet::error]
    pub enum Error<T> {
        InvalidFeeDistribution,
        InvalidInflationRate,
        SupplyCapExceeded,
        NoUpdateNeeded,
    }

    // Hooks

    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
        fn on_initialize(now: BlockNumberFor<T>) -> Weight {
            let last_update = <LastInflationUpdate<T>>::get();
            let blocks_per_year = T::BlocksPerYear::get();
            if !blocks_per_year.is_zero() && now >= last_update + blocks_per_year {
                Self::update_inflation();
            }
            Weight::zero()
        }
    }

    // Dispatchable calls

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        #[pallet::call_index(0)]
        #[pallet::weight(10_000)]
        pub fn mint_block_reward(
            origin: OriginFor<T>,
            amount: BalanceOf<T>,
            beneficiary: T::AccountId,
        ) -> DispatchResult {
            ensure_root(origin)?;
            T::Currency::deposit_creating(&beneficiary, amount);
            <TotalMinted<T>>::mutate(|m| *m += amount);
            let block_number = frame_system::Pallet::<T>::block_number();
            Self::deposit_event(Event::<T>::BlockRewardMinted { amount, block_number });
            Ok(())
        }

        #[pallet::call_index(1)]
        #[pallet::weight(20_000)]
        pub fn distribute_fees(
            origin: OriginFor<T>,
            total_fee: BalanceOf<T>,
        ) -> DispatchResult {
            ensure_root(origin)?;
            let config = <FeeDistributionConfig<T>>::get();
            let total_bps: u32 = config.staking_pool_bps as u32
                + config.treasury_bps as u32
                + config.burn_bps as u32
                + config.oracle_bps as u32;

            let staking_share: BalanceOf<T> = total_fee * (config.staking_pool_bps as u32).into() / total_bps.into();
            let treasury_share: BalanceOf<T> = total_fee * (config.treasury_bps as u32).into() / total_bps.into();
            let burn_share: BalanceOf<T> = total_fee * (config.burn_bps as u32).into() / total_bps.into();
            let oracle_share: BalanceOf<T> = total_fee * (config.oracle_bps as u32).into() / total_bps.into();

            <StakingRewardPool<T>>::mutate(|p| *p += staking_share);
            <OracleRewardPool<T>>::mutate(|p| *p += oracle_share);

            let treasury = T::TreasuryAccount::get();
            T::Currency::transfer(&Self::account_id(), &treasury, treasury_share, ExistenceRequirement::KeepAlive)?;

            Self::deposit_event(Event::<T>::FeesDistributed {
                staking: staking_share, treasury: treasury_share,
                burn: burn_share, oracle: oracle_share,
            });
            Ok(())
        }

        #[pallet::call_index(2)]
        #[pallet::weight(10_000)]
        pub fn set_fee_distribution(
            origin: OriginFor<T>,
            new_config: FeeDistribution,
        ) -> DispatchResult {
            ensure_root(origin)?;
            ensure!(new_config.is_valid(), Error::<T>::InvalidFeeDistribution);
            <FeeDistributionConfig<T>>::put(new_config.clone());
            Self::deposit_event(Event::<T>::FeeDistributionUpdated { config: new_config });
            Ok(())
        }

        #[pallet::call_index(3)]
        #[pallet::weight(100_000)]
        pub fn distribute_staking_rewards(
            origin: OriginFor<T>,
            participant_count: u32,
        ) -> DispatchResult {
            let _who = ensure_signed(origin)?;
            let total_rewards = <StakingRewardPool<T>>::take();
            if total_rewards.is_zero() || participant_count == 0 {
                return Ok(());
            }
            Self::deposit_event(Event::<T>::StakingRewardsPaid {
                total: total_rewards,
                participants: participant_count,
            });
            Ok(())
        }
    }

    // Helpers

    impl<T: Config> Pallet<T> {
        fn update_inflation() {
            let current = <CurrentInflation<T>>::get();
            let terminal = T::TerminalInflation::get();
            if current <= terminal { return; }
            let decrease = Perbill::from_parts(5_000_000); // 0.5%
            let new_rate = if current <= decrease { terminal } else { current.saturating_sub(decrease).max(terminal) };
            <CurrentInflation<T>>::put(new_rate);
            <LastInflationUpdate<T>>::put(frame_system::Pallet::<T>::block_number());
            Self::deposit_event(Event::<T>::InflationUpdated { old_rate: current, new_rate });
        }

        pub fn account_id() -> T::AccountId {
            frame_support::PalletId(*b"gpn/tokn").into_account_truncating()
        }

        pub fn total_supply() -> BalanceOf<T> { T::Currency::total_issuance() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frame_support::{
        construct_runtime, derive_impl, parameter_types,
        traits::{ConstU16, ConstU32, ConstU64, ConstU128, Everything},
    };
    use sp_core::H256;
    use sp_runtime::{
        traits::{BlakeTwo256, IdentityLookup},
        BuildStorage,
    };
    use sp_runtime::Perbill;
    use crate::pallet as local_token;
    use crate::pallet::{
        CurrentInflation, FeeDistributionConfig, LastInflationUpdate, OracleRewardPool,
        StakingRewardPool, TotalMinted,
    };

    type Block = frame_system::mocking::MockBlock<Test>;
    type Balance = u128;
    type AccountId = u64;

    construct_runtime!(
        pub struct Test {
            System: frame_system,
            Balances: pallet_balances,
            Token: local_token,
        }
    );

    #[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
    impl frame_system::Config for Test {
        type Block = Block;
        type AccountId = AccountId;
        type BlockHashCount = ConstU64<250>;
        type Lookup = IdentityLookup<AccountId>;
        type AccountData = pallet_balances::AccountData<Balance>;
        type SS58Prefix = ConstU16<42>;
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
        pub const InitialInflation: sp_runtime::Perbill = sp_runtime::Perbill::from_percent(10);
        pub const TerminalInflation: sp_runtime::Perbill = sp_runtime::Perbill::from_percent(1);
        pub const BlocksPerYear: u64 = 1000;
        pub const TreasuryAccount: AccountId = 99;
    }

    impl crate::pallet::Config for Test {
        type RuntimeEvent = RuntimeEvent;
        type Currency = Balances;
        type InitialInflation = InitialInflation;
        type TerminalInflation = TerminalInflation;
        type BlocksPerYear = BlocksPerYear;
        type TreasuryAccount = TreasuryAccount;
    }

    pub(crate) fn new_test_ext() -> sp_io::TestExternalities {
        let t = RuntimeGenesisConfig {
            system: Default::default(),
            balances: pallet_balances::GenesisConfig::<Test> {
                balances: vec![(1, 1_000_000), (2, 1_000_000)],
                dev_accounts: None,
            },
            token: GenesisConfig::<Test> { initial_inflation: Perbill::from_percent(10), _config: Default::default() },
        };
        t.build_storage().unwrap().into()
    }

    #[test]
    fn test_genesis_defaults() {
        new_test_ext().execute_with(|| {
            assert_eq!(CurrentInflation::<Test>::get(), Perbill::from_percent(10));
            assert_eq!(FeeDistributionConfig::<Test>::get(), FeeDistribution::default());
            assert!(FeeDistribution::default().is_valid());
            assert_eq!(TotalMinted::<Test>::get(), 0);
            assert_eq!(StakingRewardPool::<Test>::get(), 0);
            assert_eq!(OracleRewardPool::<Test>::get(), 0);
        });
    }

    #[test]
    fn test_fee_distribution_validity() {
        let good = FeeDistribution { staking_pool_bps: 200, treasury_bps: 150, burn_bps: 100, oracle_bps: 50 };
        assert!(good.is_valid());
        let bad = FeeDistribution { staking_pool_bps: 100, treasury_bps: 100, burn_bps: 100, oracle_bps: 100 };
        assert!(!bad.is_valid());
    }

    #[test]
    fn test_mint_block_reward_root() {
        new_test_ext().execute_with(|| {
            let bal_before = pallet_balances::Pallet::<Test>::free_balance(1);
            assert!(Token::mint_block_reward(RuntimeOrigin::root(), 10_000, 1).is_ok());
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(1), bal_before + 10_000);
            assert_eq!(TotalMinted::<Test>::get(), 10_000);
        });
    }

    #[test]
    fn test_mint_block_reward_requires_root() {
        new_test_ext().execute_with(|| {
            assert!(Token::mint_block_reward(RuntimeOrigin::signed(1), 10_000, 1).is_err());
            assert_eq!(TotalMinted::<Test>::get(), 0);
        });
    }

    #[test]
    fn test_distribute_fees() {
        new_test_ext().execute_with(|| {
            // Fund the pallet account so the treasury transfer succeeds.
            let pallet_account = Token::account_id();
            Balances::force_set_balance(RuntimeOrigin::root(), pallet_account.clone(), 100_000).unwrap();

            assert!(Token::distribute_fees(RuntimeOrigin::root(), 50_000).is_ok());
            // 200/500 -> staking, 150/500 -> treasury, 50/500 -> oracle
            assert_eq!(StakingRewardPool::<Test>::get(), 20_000);
            assert_eq!(OracleRewardPool::<Test>::get(), 5_000);
            let treasury_bal = pallet_balances::Pallet::<Test>::free_balance(TreasuryAccount::get());
            assert_eq!(treasury_bal, 15_000);
        });
    }

    #[test]
    fn test_set_fee_distribution_valid() {
        new_test_ext().execute_with(|| {
            let cfg = FeeDistribution { staking_pool_bps: 250, treasury_bps: 100, burn_bps: 100, oracle_bps: 50 };
            assert!(Token::set_fee_distribution(RuntimeOrigin::root(), cfg.clone()).is_ok());
            assert_eq!(FeeDistributionConfig::<Test>::get(), cfg);
        });
    }

    #[test]
    fn test_set_fee_distribution_invalid_rejected() {
        new_test_ext().execute_with(|| {
            let cfg = FeeDistribution { staking_pool_bps: 100, treasury_bps: 100, burn_bps: 100, oracle_bps: 100 };
            assert_eq!(Token::set_fee_distribution(RuntimeOrigin::root(), cfg), Err(Error::<Test>::InvalidFeeDistribution.into()));
        });
    }

    #[test]
    fn test_set_fee_distribution_requires_root() {
        new_test_ext().execute_with(|| {
            let cfg = FeeDistribution::default();
            assert!(Token::set_fee_distribution(RuntimeOrigin::signed(1), cfg).is_err());
        });
    }

    #[test]
    fn test_distribute_staking_rewards_drains_pool() {
        new_test_ext().execute_with(|| {
            // Seed the staking pool directly.
            <StakingRewardPool<Test>>::put(9_000u128);
            assert!(Token::distribute_staking_rewards(RuntimeOrigin::signed(1), 3).is_ok());
            assert_eq!(StakingRewardPool::<Test>::get(), 0);
        });
    }

    #[test]
    fn test_distribute_staking_rewards_empty_ok() {
        new_test_ext().execute_with(|| {
            assert!(Token::distribute_staking_rewards(RuntimeOrigin::signed(1), 3).is_ok());
            assert_eq!(StakingRewardPool::<Test>::get(), 0);
        });
    }

    #[test]
    fn test_inflation_decays_to_terminal() {
        new_test_ext().execute_with(|| {
            assert_eq!(CurrentInflation::<Test>::get(), Perbill::from_percent(10));
            // Advance blocks to trigger inflation update.
            System::set_block_number(1001);
            <Token as frame_support::traits::Hooks<u64>>::on_initialize(1001);
            let new_inflation = CurrentInflation::<Test>::get();
            assert!(new_inflation < Perbill::from_percent(10));
            assert!(new_inflation >= Perbill::from_percent(1));
            assert_eq!(LastInflationUpdate::<Test>::get(), 1001);
        });
    }

    #[test]
    fn test_inflation_does_not_update_before_year() {
        new_test_ext().execute_with(|| {
            System::set_block_number(500);
            <Token as frame_support::traits::Hooks<u64>>::on_initialize(500);
            assert_eq!(CurrentInflation::<Test>::get(), Perbill::from_percent(10));
        });
    }

    #[test]
    fn test_total_supply() {
        new_test_ext().execute_with(|| {
            let before = Token::total_supply();
            Token::mint_block_reward(RuntimeOrigin::root(), 5_000, 1).unwrap();
            assert_eq!(Token::total_supply(), before + 5_000);
        });
    }
}
