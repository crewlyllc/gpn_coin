//! # GPN Token Pallet — Inflation, Fee Distribution, Treasury Management
//!
//! Manages the GPN token supply, inflation schedule, and fee distribution.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod weights;

use codec::{Decode, DecodeWithMemTracking, Encode, MaxEncodedLen};
use core::fmt::Debug;
use scale_info::TypeInfo;

pub use pallet::*;
pub use weights::WeightInfo;

/// Fee distribution percentages (in basis points, must sum to 500)
#[derive(
    Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo,
)]
pub struct FeeDistribution {
    pub staking_pool_bps: u16,
    pub treasury_bps: u16,
    pub burn_bps: u16,
    pub oracle_bps: u16,
}

impl Default for FeeDistribution {
    fn default() -> Self {
        Self {
            staking_pool_bps: 200,
            treasury_bps: 150,
            burn_bps: 100,
            oracle_bps: 50,
        }
    }
}

impl FeeDistribution {
    pub fn is_valid(&self) -> bool {
        (self.staking_pool_bps as u32
            + self.treasury_bps as u32
            + self.burn_bps as u32
            + self.oracle_bps as u32)
            == 500
    }
}

/// Cross-pallet contract: a consumer that collects marketplace/protocol fees
/// can route them into the token pallet's account by calling `fee_sink()`, so
/// `distribute_fees` (which draws from `PalletId(b"gpn/tokn")`) becomes
/// self-funding in normal operation. Mirrors `GPUAttestationProvider`.
pub trait FeeSinkProvider<AccountId> {
    /// The account that collected protocol fees accumulate into, ready for
    /// `distribute_fees` to draw from. Returns the GPN token pallet account.
    fn fee_sink() -> AccountId;
}

/// Canonical issued-supply accounting, owned by the GPN token pallet.
///
/// Every mint that increases issued supply **must** go through this trait so
/// the 1B `MaxSupply` cap is enforced against a single counter. Other pallets
/// (notably `pallet-gpu-pow`'s block-reward payout) call `mint_within_cap`
/// instead of holding their own `TotalMinted`/`MaxSupply` copies, which is how
/// the pre-unification code could mis-account the cap. See workplan item 1.
pub trait SupplyManager<AccountId, Balance> {
    /// Total issued supply as tracked by the canonical counter (excludes
    /// genesis balances that were never minted through this pallet).
    fn total_issued() -> Balance;

    /// Mint `amount` to `beneficiary` iff it keeps total issued supply within
    /// `MaxSupply`. Returns `true` when minted, `false` when the cap would be
    /// exceeded (caller should treat that as a no-op, not an error).
    fn mint_within_cap(beneficiary: &AccountId, amount: Balance) -> bool;
}

#[frame_support::pallet]
pub mod pallet {
    use super::*;
    use frame_support::pallet_prelude::*;
    use frame_support::sp_runtime::traits::{AccountIdConversion, Saturating, Zero};
    use frame_support::traits::{Currency, ExistenceRequirement};
    use frame_system::pallet_prelude::*;
    use pallet_gpu_attestation::GPUAttestationProvider;
    use sp_runtime::Perbill;
    use sp_std::vec::Vec;

    pub type BalanceOf<T> =
        <<T as Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::Balance;

    #[pallet::config]
    pub trait Config: frame_system::Config + pallet_balances::Config {
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;
        type Currency: Currency<Self::AccountId>;
        /// Read-only access to providers' attested GPU compute credentials.
        /// Used to weight staking rewards by each staker's GPU tier multiplier
        /// (proof-of-staked-GPU: attestation boosts staking yield).
        type GPUAttestation: GPUAttestationProvider<Self::AccountId>;
        #[pallet::constant]
        type InitialInflation: Get<Perbill>;
        #[pallet::constant]
        type TerminalInflation: Get<Perbill>;
        #[pallet::constant]
        type BlocksPerYear: Get<BlockNumberFor<Self>>;
        #[pallet::constant]
        type TreasuryAccount: Get<Self::AccountId>;
        /// Hard cap on total issued supply. The canonical counter lives in
        /// `TotalMinted` (below); every mint path checks it against this cap.
        #[pallet::constant]
        type MaxSupply: Get<BalanceOf<Self>>;
        /// Weight information for extrinsics in this pallet (workplan item 9).
        type WeightInfo: WeightInfo;
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
        /// Supply already issued before the chain started (e.g. a genesis
        /// treasury allocation). Seeds the canonical `TotalMinted` counter so
        /// it counts against `MaxSupply` from block 0. Default 0.
        #[serde(default)]
        pub initial_minted: BalanceOf<T>,
        #[serde(skip)]
        pub _config: sp_std::marker::PhantomData<T>,
    }

    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            <CurrentInflation<T>>::put(self.initial_inflation);
            <FeeDistributionConfig<T>>::put(FeeDistribution::default());
            let zero_bal: BalanceOf<T> = Zero::zero();
            <TotalMinted<T>>::put(self.initial_minted);
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
        BlockRewardMinted {
            amount: BalanceOf<T>,
            block_number: BlockNumberFor<T>,
        },
        FeesDistributed {
            staking: BalanceOf<T>,
            treasury: BalanceOf<T>,
            burn: BalanceOf<T>,
            oracle: BalanceOf<T>,
        },
        InflationUpdated {
            old_rate: Perbill,
            new_rate: Perbill,
        },
        FeeDistributionUpdated {
            config: FeeDistribution,
        },
        StakingRewardsPaid {
            total: BalanceOf<T>,
            participants: u32,
        },
        OracleRewardsPaid {
            total: BalanceOf<T>,
            operators: u32,
        },
        Burned {
            amount: BalanceOf<T>,
        },
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
        #[pallet::weight(<T as Config>::WeightInfo::mint_block_reward())]
        pub fn mint_block_reward(
            origin: OriginFor<T>,
            amount: BalanceOf<T>,
            beneficiary: T::AccountId,
        ) -> DispatchResult {
            ensure_root(origin)?;
            ensure!(
                Self::mint_within_cap(&beneficiary, amount),
                Error::<T>::SupplyCapExceeded
            );
            let block_number = frame_system::Pallet::<T>::block_number();
            Self::deposit_event(Event::<T>::BlockRewardMinted {
                amount,
                block_number,
            });
            Ok(())
        }

        #[pallet::call_index(1)]
        #[pallet::weight(<T as Config>::WeightInfo::distribute_fees())]
        pub fn distribute_fees(origin: OriginFor<T>, total_fee: BalanceOf<T>) -> DispatchResult {
            ensure_root(origin)?;
            let config = <FeeDistributionConfig<T>>::get();
            let total_bps: u32 = config.staking_pool_bps as u32
                + config.treasury_bps as u32
                + config.burn_bps as u32
                + config.oracle_bps as u32;

            let staking_share: BalanceOf<T> =
                total_fee * (config.staking_pool_bps as u32).into() / total_bps.into();
            let treasury_share: BalanceOf<T> =
                total_fee * (config.treasury_bps as u32).into() / total_bps.into();
            let burn_share: BalanceOf<T> =
                total_fee * (config.burn_bps as u32).into() / total_bps.into();
            let oracle_share: BalanceOf<T> =
                total_fee * (config.oracle_bps as u32).into() / total_bps.into();

            <StakingRewardPool<T>>::mutate(|p| *p += staking_share);
            <OracleRewardPool<T>>::mutate(|p| *p += oracle_share);

            let treasury = T::TreasuryAccount::get();
            T::Currency::transfer(
                &Self::account_id(),
                &treasury,
                treasury_share,
                ExistenceRequirement::KeepAlive,
            )?;

            // Real burn: remove the burn share from circulation (slash from the
            // pallet account). This reduces total issuance, not just bookkeeping.
            if !burn_share.is_zero() {
                let (imbalance, _) = T::Currency::slash(&Self::account_id(), burn_share);
                drop(imbalance); // negative imbalance = burned (removed from supply)
                Self::deposit_event(Event::<T>::Burned { amount: burn_share });
            }

            Self::deposit_event(Event::<T>::FeesDistributed {
                staking: staking_share,
                treasury: treasury_share,
                burn: burn_share,
                oracle: oracle_share,
            });
            Ok(())
        }

        #[pallet::call_index(2)]
        #[pallet::weight(<T as Config>::WeightInfo::set_fee_distribution())]
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
        #[pallet::weight(<T as Config>::WeightInfo::distribute_staking_rewards())]
        pub fn distribute_staking_rewards(
            origin: OriginFor<T>,
            stakers: Vec<T::AccountId>,
        ) -> DispatchResult {
            let _who = ensure_signed(origin)?;
            let total_rewards = <StakingRewardPool<T>>::take();
            if total_rewards.is_zero() || stakers.is_empty() {
                return Ok(());
            }
            // Weight each staker by their attested GPU tier multiplier, so
            // proof-of-staked-GPU boosts yield (Unattested = 0 multiplier =
            // excluded from the payout).
            let mut weights: Vec<(T::AccountId, BalanceOf<T>)> = Vec::with_capacity(stakers.len());
            let mut total_weight: BalanceOf<T> = Zero::zero();
            for s in stakers {
                let mult: u32 = T::GPUAttestation::attested_tier(&s).staking_multiplier_bps();
                if mult > 0 {
                    let w: BalanceOf<T> = mult.into();
                    weights.push((s, w));
                    total_weight += w;
                }
            }
            if total_weight.is_zero() {
                // No attested stakers -> nothing to pay out; return pool for later.
                <StakingRewardPool<T>>::put(total_rewards);
                return Ok(());
            }
            let mut paid: u32 = 0;
            let mut remaining = total_rewards;
            for (staker, w) in &weights {
                let share = total_rewards * *w / total_weight;
                if share.is_zero() {
                    continue;
                }
                T::Currency::transfer(
                    &Self::account_id(),
                    staker,
                    share,
                    ExistenceRequirement::KeepAlive,
                )?;
                remaining = remaining.saturating_sub(share);
                paid += 1;
            }
            // Dust (rounding) stays in the pallet account as retained fees.
            Self::deposit_event(Event::<T>::StakingRewardsPaid {
                total: total_rewards.saturating_sub(remaining),
                participants: paid,
            });
            Ok(())
        }

        #[pallet::call_index(4)]
        #[pallet::weight(<T as Config>::WeightInfo::distribute_oracle_rewards())]
        pub fn distribute_oracle_rewards(
            origin: OriginFor<T>,
            operators: Vec<T::AccountId>,
        ) -> DispatchResult {
            let _who = ensure_signed(origin)?;
            let total_rewards = <OracleRewardPool<T>>::take();
            if total_rewards.is_zero() || operators.is_empty() {
                return Ok(());
            }
            let n: u32 = operators.len() as u32;
            let share = total_rewards / n.into();
            let mut paid: u32 = 0;
            let mut remaining = total_rewards;
            for op in operators {
                if share.is_zero() {
                    continue;
                }
                T::Currency::transfer(
                    &Self::account_id(),
                    &op,
                    share,
                    ExistenceRequirement::KeepAlive,
                )?;
                remaining = remaining.saturating_sub(share);
                paid += 1;
            }
            Self::deposit_event(Event::<T>::OracleRewardsPaid {
                total: total_rewards.saturating_sub(remaining),
                operators: paid,
            });
            Ok(())
        }
    }

    // Helpers

    impl<T: Config> Pallet<T> {
        fn update_inflation() {
            let current = <CurrentInflation<T>>::get();
            let terminal = T::TerminalInflation::get();
            if current <= terminal {
                return;
            }
            let decrease = Perbill::from_parts(5_000_000); // 0.5%
            let new_rate = if current <= decrease {
                terminal
            } else {
                current.saturating_sub(decrease).max(terminal)
            };
            <CurrentInflation<T>>::put(new_rate);
            <LastInflationUpdate<T>>::put(frame_system::Pallet::<T>::block_number());
            Self::deposit_event(Event::<T>::InflationUpdated {
                old_rate: current,
                new_rate,
            });
        }

        pub fn account_id() -> T::AccountId {
            frame_support::PalletId(*b"gpn/tokn").into_account_truncating()
        }

        pub fn total_supply() -> BalanceOf<T> {
            T::Currency::total_issuance()
        }
    }

    /// Canonical supply accounting. All supply-increasing mints — including
    /// those triggered by other pallets — funnel through `mint_within_cap`.
    impl<T: Config> SupplyManager<T::AccountId, BalanceOf<T>> for Pallet<T> {
        fn total_issued() -> BalanceOf<T> {
            <TotalMinted<T>>::get()
        }

        fn mint_within_cap(beneficiary: &T::AccountId, amount: BalanceOf<T>) -> bool {
            let minted = <TotalMinted<T>>::get();
            let cap = T::MaxSupply::get();
            // Saturating add: an absurd `amount` can never wrap past the cap.
            if minted.saturating_add(amount) > cap {
                return false;
            }
            <TotalMinted<T>>::put(minted.saturating_add(amount));
            T::Currency::deposit_creating(beneficiary, amount);
            true
        }
    }
}

/// The GPN token pallet is its own fee sink: marketplace/protocol fees routed
/// here land in the `gpn/tokn` account that `distribute_fees` draws from.
impl<T: Config> FeeSinkProvider<T::AccountId> for Pallet<T> {
    fn fee_sink() -> T::AccountId {
        Self::account_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pallet as local_token;
    use crate::pallet::{
        CurrentInflation, FeeDistributionConfig, LastInflationUpdate, OracleRewardPool,
        StakingRewardPool, TotalMinted,
    };
    use frame_support::{
        construct_runtime, derive_impl, parameter_types,
        traits::{ConstU128, ConstU16, ConstU32, ConstU64, Everything},
    };
    use pallet_gpu_attestation::{AttestationTier, GPUAttestationProvider};
    use sp_core::H256;
    use sp_runtime::Perbill;
    use sp_runtime::{
        traits::{BlakeTwo256, IdentityLookup},
        BuildStorage,
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
        pub const TestMaxSupply: Balance = 1_000_000_000_000_000_000_000;
    }

    thread_local! {
        static MOCK_TIER: std::cell::Cell<u32> =
            const { std::cell::Cell::new(1_500) }; // Silver multiplier bps
    }
    pub struct MockAttestation;
    impl GPUAttestationProvider<u64> for MockAttestation {
        fn attested_gflops(_: &u64) -> u64 {
            1_000
        }
        fn attested_tier(_: &u64) -> AttestationTier {
            match MOCK_TIER.with(|c| c.get()) {
                1_000 => AttestationTier::Bronze,
                2_500 => AttestationTier::Gold,
                0 => AttestationTier::Unattested,
                _ => AttestationTier::Silver,
            }
        }
    }

    impl crate::pallet::Config for Test {
        type WeightInfo = ();
        type RuntimeEvent = RuntimeEvent;
        type Currency = Balances;
        type GPUAttestation = MockAttestation;
        type InitialInflation = InitialInflation;
        type TerminalInflation = TerminalInflation;
        type BlocksPerYear = BlocksPerYear;
        type TreasuryAccount = TreasuryAccount;
        type MaxSupply = TestMaxSupply;
    }

    pub(crate) fn new_test_ext() -> sp_io::TestExternalities {
        let t = RuntimeGenesisConfig {
            system: Default::default(),
            balances: pallet_balances::GenesisConfig::<Test> {
                balances: vec![(1, 1_000_000), (2, 1_000_000)],
                dev_accounts: None,
            },
            token: GenesisConfig::<Test> {
                initial_inflation: Perbill::from_percent(10),
                initial_minted: 0,
                _config: Default::default(),
            },
        };
        t.build_storage().unwrap().into()
    }

    #[test]
    fn test_genesis_defaults() {
        new_test_ext().execute_with(|| {
            assert_eq!(CurrentInflation::<Test>::get(), Perbill::from_percent(10));
            assert_eq!(
                FeeDistributionConfig::<Test>::get(),
                FeeDistribution::default()
            );
            assert!(FeeDistribution::default().is_valid());
            assert_eq!(TotalMinted::<Test>::get(), 0);
            assert_eq!(StakingRewardPool::<Test>::get(), 0);
            assert_eq!(OracleRewardPool::<Test>::get(), 0);
        });
    }

    #[test]
    fn test_fee_distribution_validity() {
        let good = FeeDistribution {
            staking_pool_bps: 200,
            treasury_bps: 150,
            burn_bps: 100,
            oracle_bps: 50,
        };
        assert!(good.is_valid());
        let bad = FeeDistribution {
            staking_pool_bps: 100,
            treasury_bps: 100,
            burn_bps: 100,
            oracle_bps: 100,
        };
        assert!(!bad.is_valid());
    }

    #[test]
    fn test_mint_block_reward_root() {
        new_test_ext().execute_with(|| {
            let bal_before = pallet_balances::Pallet::<Test>::free_balance(1);
            assert!(Token::mint_block_reward(RuntimeOrigin::root(), 10_000, 1).is_ok());
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(1),
                bal_before + 10_000
            );
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
            Balances::force_set_balance(RuntimeOrigin::root(), pallet_account.clone(), 100_000)
                .unwrap();

            assert!(Token::distribute_fees(RuntimeOrigin::root(), 50_000).is_ok());
            // 200/500 -> staking, 150/500 -> treasury, 50/500 -> oracle
            assert_eq!(StakingRewardPool::<Test>::get(), 20_000);
            assert_eq!(OracleRewardPool::<Test>::get(), 5_000);
            let treasury_bal =
                pallet_balances::Pallet::<Test>::free_balance(TreasuryAccount::get());
            assert_eq!(treasury_bal, 15_000);
        });
    }

    #[test]
    fn test_set_fee_distribution_valid() {
        new_test_ext().execute_with(|| {
            let cfg = FeeDistribution {
                staking_pool_bps: 250,
                treasury_bps: 100,
                burn_bps: 100,
                oracle_bps: 50,
            };
            assert!(Token::set_fee_distribution(RuntimeOrigin::root(), cfg.clone()).is_ok());
            assert_eq!(FeeDistributionConfig::<Test>::get(), cfg);
        });
    }

    #[test]
    fn test_set_fee_distribution_invalid_rejected() {
        new_test_ext().execute_with(|| {
            let cfg = FeeDistribution {
                staking_pool_bps: 100,
                treasury_bps: 100,
                burn_bps: 100,
                oracle_bps: 100,
            };
            assert_eq!(
                Token::set_fee_distribution(RuntimeOrigin::root(), cfg),
                Err(Error::<Test>::InvalidFeeDistribution.into())
            );
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
            // Seed the staking pool directly, and give the pallet account funds
            // so transfers to attested stakers succeed.
            <StakingRewardPool<Test>>::put(9_000u128);
            let pallet_account = Token::account_id();
            Balances::force_set_balance(RuntimeOrigin::root(), pallet_account.clone(), 100_000)
                .unwrap();
            // Mock tier = Silver (1500 bps) for both stakers.
            assert!(
                Token::distribute_staking_rewards(RuntimeOrigin::signed(1), vec![1, 2]).is_ok()
            );
            assert_eq!(StakingRewardPool::<Test>::get(), 0);
        });
    }

    #[test]
    fn test_distribute_staking_rewards_weights_by_tier() {
        new_test_ext().execute_with(|| {
            // 7000 total reward: Alice (id 1) Bronze (1000) vs Bob (id 2) Gold
            // (2500) -> weights 1000:2500 => 2:5 split of 7000 = 2000:5000.
            <StakingRewardPool<Test>>::put(7_000u128);
            let pallet_account = Token::account_id();
            Balances::force_set_balance(RuntimeOrigin::root(), pallet_account.clone(), 100_000)
                .unwrap();
            // The mock's attested_tier is keyed by a single per-thread value,
            // so to give Alice and Bob *different* tiers we distribute them one
            // at a time with the mock tier set per call, seeding the pool twice.
            MOCK_TIER.with(|c| c.set(1_000)); // Bronze
            <StakingRewardPool<Test>>::put(2_000u128);
            Token::distribute_staking_rewards(RuntimeOrigin::signed(1), vec![1]).unwrap();
            MOCK_TIER.with(|c| c.set(2_500)); // Gold
            <StakingRewardPool<Test>>::put(5_000u128);
            Token::distribute_staking_rewards(RuntimeOrigin::signed(1), vec![2]).unwrap();
            // Alice started with 1_000_000; Bob with 1_000_000.
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(1) - 1_000_000,
                2_000u128
            );
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(2) - 1_000_000,
                5_000u128
            );
        });
    }

    #[test]
    fn test_distribute_staking_rewards_unattested_excluded() {
        new_test_ext().execute_with(|| {
            <StakingRewardPool<Test>>::put(5_000u128);
            MOCK_TIER.with(|c| c.set(0)); // Unattested
                                          // No attested stakers -> pool is restored, nothing paid.
            assert!(
                Token::distribute_staking_rewards(RuntimeOrigin::signed(1), vec![1, 2]).is_ok()
            );
            assert_eq!(StakingRewardPool::<Test>::get(), 5_000);
        });
    }

    #[test]
    fn test_distribute_staking_rewards_empty_ok() {
        new_test_ext().execute_with(|| {
            assert!(Token::distribute_staking_rewards(RuntimeOrigin::signed(1), vec![]).is_ok());
            assert_eq!(StakingRewardPool::<Test>::get(), 0);
        });
    }

    #[test]
    fn test_distribute_oracle_rewards() {
        new_test_ext().execute_with(|| {
            <OracleRewardPool<Test>>::put(3_000u128);
            let pallet_account = Token::account_id();
            Balances::force_set_balance(RuntimeOrigin::root(), pallet_account.clone(), 100_000)
                .unwrap();
            assert!(Token::distribute_oracle_rewards(RuntimeOrigin::signed(1), vec![1, 2]).is_ok());
            assert_eq!(OracleRewardPool::<Test>::get(), 0);
            // 3000 / 2 = 1500 each on top of their 1_000_000 seed.
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(1), 1_001_500);
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(2), 1_001_500);
        });
    }

    #[test]
    fn test_real_burn_reduces_issuance() {
        new_test_ext().execute_with(|| {
            let pallet_account = Token::account_id();
            Balances::force_set_balance(RuntimeOrigin::root(), pallet_account.clone(), 100_000)
                .unwrap();
            let issuance_before = Balances::total_issuance();
            // 50_000 fee: burn share = 100/500 = 10_000.
            assert!(Token::distribute_fees(RuntimeOrigin::root(), 50_000).is_ok());
            // Treasury took 15_000 (transferred, still in circulation); burn
            // 10_000 removed from issuance; staking+oracle stay in pallet acct.
            assert_eq!(Balances::total_issuance(), issuance_before - 10_000);
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

    #[test]
    fn test_mint_block_reward_enforces_cap() {
        new_test_ext().execute_with(|| {
            // Fill the counter to the cap, then a mint must be rejected and no
            // balance created.
            let cap = TestMaxSupply::get();
            <TotalMinted<Test>>::put(cap);
            let bal_before = pallet_balances::Pallet::<Test>::free_balance(1);
            assert_eq!(
                Token::mint_block_reward(RuntimeOrigin::root(), 1, 1),
                Err(Error::<Test>::SupplyCapExceeded.into())
            );
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(1), bal_before);
            assert_eq!(TotalMinted::<Test>::get(), cap);
        });
    }

    #[test]
    fn test_cap_holds_with_genesis_treasury_allocation() {
        // A genesis treasury allocation seeds the canonical counter; only the
        // remaining headroom is mintable.
        let alloc: Balance = TestMaxSupply::get() - 123;
        let t = RuntimeGenesisConfig {
            system: Default::default(),
            balances: pallet_balances::GenesisConfig::<Test> {
                balances: vec![(1, 1_000_000)],
                dev_accounts: None,
            },
            token: GenesisConfig::<Test> {
                initial_inflation: Perbill::from_percent(10),
                initial_minted: alloc,
                _config: Default::default(),
            },
        };
        let mut ext: sp_io::TestExternalities = t.build_storage().unwrap().into();
        ext.execute_with(|| {
            assert_eq!(TotalMinted::<Test>::get(), alloc);
            // 123 headroom: a 123 mint fits exactly...
            assert!(Token::mint_block_reward(RuntimeOrigin::root(), 123, 1).is_ok());
            assert_eq!(TotalMinted::<Test>::get(), TestMaxSupply::get());
            // ...the next unit does not.
            assert!(Token::mint_block_reward(RuntimeOrigin::root(), 1, 1).is_err());
            assert_eq!(TotalMinted::<Test>::get(), TestMaxSupply::get());
        });
    }
}
