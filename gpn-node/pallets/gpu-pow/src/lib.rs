//! # GPU PoW Pallet
//!
//! Ethash-compatible proof-of-work for GPU mining.
//! Handles difficulty adjustment, block reward distribution, and PoW verification.

#![cfg_attr(not(feature = "std"), no_std)]

use codec::{Decode, DecodeWithMemTracking, Encode};
use core::fmt::Debug;
use scale_info::TypeInfo;

use sp_runtime::SaturatedConversion;

#[cfg(feature = "std")]
use serde::{Deserialize, Serialize};

pub use pallet::*;

/// A 256-bit unsigned integer for difficulty.
pub type U256 = [u64; 4];

/// Ethash proof-of-work solution.
#[derive(Clone, Encode, Decode, DecodeWithMemTracking, PartialEq, Eq, Debug, TypeInfo)]
pub struct EthashSolution {
    pub nonce: u64,
    pub mix_hash: [u8; 32],
    pub solution_hash: [u8; 32],
    pub algorithm_version: u32,
}

/// Mining information for RPC.
#[derive(Clone, Encode, Decode, PartialEq, Eq, Debug, TypeInfo)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
pub struct MiningInfo {
    pub difficulty: U256,
    pub epoch: u64,
    pub dag_size: u64,
    pub algorithm_version: u32,
    pub block_number: u64,
    pub block_reward: u128,
    pub network_hashrate_estimate: u64,
}

#[frame_support::pallet]
pub mod pallet {
    use super::*;
    use frame_support::pallet_prelude::*;
    use frame_support::traits::Currency;
    use frame_system::pallet_prelude::*;
    use pallet_gpn_token::SupplyManager;

    pub type BalanceOf<T> =
        <<T as Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::Balance;

    #[pallet::config]
    pub trait Config: frame_system::Config + pallet_timestamp::Config {
        type Currency: Currency<Self::AccountId>;
        #[pallet::constant]
        type MinDifficulty: Get<U256>;
        #[pallet::constant]
        type MaxDifficulty: Get<U256>;
        #[pallet::constant]
        type InitialBlockReward: Get<BalanceOf<Self>>;
        #[pallet::constant]
        type HalvingInterval: Get<BlockNumberFor<Self>>;
        #[pallet::constant]
        type TargetBlockTime: Get<u64>;
        #[pallet::constant]
        type DifficultyAdjustmentWindow: Get<BlockNumberFor<Self>>;
        #[pallet::constant]
        type PowMaxWeight: Get<u64>;
        #[pallet::constant]
        type AlgorithmVersion: Get<u32>;
        /// Canonical issued-supply accounting lives in `pallet-gpn-token`.
        /// Block rewards are minted through it so the 1B cap is enforced
        /// against one counter (workplan item 1).
        type Supply: SupplyManager<Self::AccountId, BalanceOf<Self>>;
    }

    #[pallet::pallet]
    pub struct Pallet<T>(_);

    #[pallet::storage]
    pub type Difficulty<T: Config> = StorageValue<_, U256, ValueQuery>;

    #[pallet::storage]
    /// Unix timestamp (seconds) of the last block that ran the difficulty
    /// adjustment. Guards against adjusting on block 1 and provides the
    /// `actual_time` window for `adjust_difficulty`.
    pub type LastBlockTimestamp<T: Config> = StorageValue<_, u64, ValueQuery>;

    #[pallet::storage]
    /// Block number of the most recent difficulty adjustment. Difficulty is
    /// recomputed once every `DifficultyAdjustmentWindow` blocks (using the
    /// average block time over that window) to keep the adjustment stable.
    pub type LastAdjustmentBlock<T: Config> = StorageValue<_, BlockNumberFor<T>, ValueQuery>;

    #[pallet::storage]
    pub type CurrentEpoch<T: Config> = StorageValue<_, u64, ValueQuery>;

    #[pallet::storage]
    /// The block reward pending distribution, earned by `PendingRewardBeneficiary`.
    pub type PendingBlockRewards<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    #[pallet::storage]
    /// The miner who earned the pending block reward.
    pub type PendingRewardBeneficiary<T: Config> = StorageValue<_, T::AccountId, OptionQuery>;

    #[pallet::storage]
    /// Miner account that automatically receives each block's reward when the
    /// block carries no manual `submit_solution` claim (workplan item 4:
    /// block-reward auto-wiring). Seeded at genesis and mutable by root.
    pub type BlockRewardBeneficiary<T: Config> = StorageValue<_, T::AccountId, OptionQuery>;

    #[pallet::storage]
    pub type LastHalvingBlock<T: Config> = StorageValue<_, BlockNumberFor<T>, ValueQuery>;

    #[pallet::storage]
    pub type CurrentBlockReward<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    #[pallet::genesis_config]
    #[derive(frame_support::DefaultNoBound)]
    pub struct GenesisConfig<T: Config> {
        pub initial_difficulty: U256,
        pub initial_block_reward: BalanceOf<T>,
        /// Account credited automatically with each block reward (item 4).
        /// `None` disables auto-emission until root sets one.
        pub block_reward_beneficiary: Option<T::AccountId>,
        #[serde(skip)]
        pub _config: sp_std::marker::PhantomData<T>,
    }

    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            <Difficulty<T>>::put(self.initial_difficulty);
            <CurrentBlockReward<T>>::put(self.initial_block_reward);
            <CurrentEpoch<T>>::put(0);
            let zero_bal: BalanceOf<T> = Zero::zero();
            <PendingBlockRewards<T>>::put(zero_bal);
            let zero_block: BlockNumberFor<T> = Zero::zero();
            <LastHalvingBlock<T>>::put(zero_block);
            <PendingRewardBeneficiary<T>>::kill();
            match &self.block_reward_beneficiary {
                Some(beneficiary) => <BlockRewardBeneficiary<T>>::put(beneficiary),
                None => <BlockRewardBeneficiary<T>>::kill(),
            }
            <LastBlockTimestamp<T>>::put(0u64);
            <LastAdjustmentBlock<T>>::put(zero_block);
        }
    }

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        BlockMined {
            miner: T::AccountId,
            difficulty: U256,
            reward: BalanceOf<T>,
        },
        /// A block reward was minted to `miner` (auto-wired accrual or a
        /// manual `submit_solution` claim, item 4).
        BlockRewardPaid {
            miner: T::AccountId,
            reward: BalanceOf<T>,
        },
        /// The auto-reward beneficiary was set/changed (item 4).
        BlockRewardBeneficiarySet {
            beneficiary: T::AccountId,
        },
        DifficultyAdjusted {
            old_difficulty: U256,
            new_difficulty: U256,
        },
        RewardHalved {
            old_reward: BalanceOf<T>,
            new_reward: BalanceOf<T>,
            halving_number: u32,
        },
        AlgorithmUpgraded {
            old_version: u32,
            new_version: u32,
        },
    }

    #[pallet::error]
    pub enum Error<T> {
        InvalidSolution,
        SolutionHashMismatch,
        UnsupportedAlgorithmVersion,
        DifficultyTooLow,
        DifficultyTooHigh,
        SolutionTooEarly,
    }

    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
        fn on_initialize(now: BlockNumberFor<T>) -> Weight {
            // Self-tune block time: every `DifficultyAdjustmentWindow` blocks,
            // recompute the difficulty target from the *average* block time over
            // that window (target semantics: higher = easier). Windowed + averaged
            // adjustment is stable, unlike per-block adjustment (which oscillates
            // due to the lag between difficulty read and block import).
            let window = T::DifficultyAdjustmentWindow::get();
            let last_adj = <LastAdjustmentBlock<T>>::get();
            let last_ts = <LastBlockTimestamp<T>>::get();
            let current_ts: u64 = pallet_timestamp::Pallet::<T>::get().saturated_into::<u64>();
            if last_adj.is_zero() {
                // First block: just seed the window start, no adjustment yet.
                <LastAdjustmentBlock<T>>::put(now);
                <LastBlockTimestamp<T>>::put(current_ts);
            } else if !window.is_zero() && now >= last_adj + window {
                let blocks: u64 = (now - last_adj).saturated_into::<u64>();
                let elapsed_ms = current_ts.saturating_sub(last_ts);
                // `last_ts`/`current_ts` are in MILLISECONDS (pallet_timestamp
                // Moment); `TargetBlockTime` is in SECONDS. Convert to seconds
                // before comparing. Guard against a missing window start
                // (last_ts == 0, e.g. block 1 seeded before a timestamp was set).
                let elapsed_s = elapsed_ms / 1000;
                if last_ts != 0 && blocks > 0 && elapsed_s > 0 {
                    let avg_block_time = elapsed_s / blocks;
                    let stored = <Difficulty<T>>::get();
                    let new = Self::adjust_difficulty(stored, avg_block_time);
                    log::info!(
                        target: "runtime::gpu-pow",
                        "ADJUST now={} last_adj={} last_ts={} cur_ts={} elapsed_s={} blocks={} avg_s={} target={} stored_limb3={} new_limb3={}",
                        now.saturated_into::<u64>(), last_adj.saturated_into::<u64>(), last_ts, current_ts, elapsed_s, blocks, avg_block_time,
                        T::TargetBlockTime::get(), stored[3], new[3],
                    );
                }
                <LastAdjustmentBlock<T>>::put(now);
                <LastBlockTimestamp<T>>::put(current_ts);
            }

            let halving_interval = T::HalvingInterval::get();
            let last_halving = <LastHalvingBlock<T>>::get();
            if !halving_interval.is_zero()
                && !last_halving.is_zero()
                && now - last_halving >= halving_interval
            {
                Self::perform_halving();
            }
            Self::distribute_block_rewards();
            Weight::zero()
        }
    }

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// Submit a valid GPU PoW solution to claim the block reward.
        ///
        /// The reward is paid to `origin` in the next block's `on_initialize`.
        #[pallet::call_index(0)]
        #[pallet::weight(10_000)]
        pub fn submit_solution(
            origin: OriginFor<T>,
            solution: EthashSolution,
            difficulty: U256,
        ) -> DispatchResult {
            let miner = ensure_signed(origin)?;
            let computed_hash = Self::compute_solution_hash(&solution);
            ensure!(
                computed_hash == solution.solution_hash,
                Error::<T>::SolutionHashMismatch
            );
            ensure!(
                Self::meets_difficulty(&solution.solution_hash, &difficulty),
                Error::<T>::InvalidSolution
            );
            ensure!(
                solution.algorithm_version == T::AlgorithmVersion::get(),
                Error::<T>::UnsupportedAlgorithmVersion
            );
            let reward = <CurrentBlockReward<T>>::get();
            <PendingBlockRewards<T>>::put(reward);
            <PendingRewardBeneficiary<T>>::put(miner.clone());
            Self::deposit_event(Event::<T>::BlockMined {
                miner,
                difficulty,
                reward,
            });
            Ok(())
        }

        /// Set the account that automatically receives each block reward when
        /// the block carries no manual `submit_solution` claim (item 4).
        ///
        /// Root only: prevents a stranger from redirecting the chain's block
        /// emission to themselves. Root may rotate the beneficiary at any time.
        #[pallet::call_index(1)]
        #[pallet::weight(10_000)]
        pub fn set_block_beneficiary(
            origin: OriginFor<T>,
            beneficiary: T::AccountId,
        ) -> DispatchResult {
            ensure_root(origin)?;
            <BlockRewardBeneficiary<T>>::put(&beneficiary);
            Self::deposit_event(Event::<T>::BlockRewardBeneficiarySet { beneficiary });
            Ok(())
        }
    }

    impl<T: Config> Pallet<T> {
        /// Pay this block's emission once (item 4).
        ///
        /// Precedence: a manual claim recorded by `submit_solution` in the
        /// *previous* block is honoured first; otherwise the configured
        /// [`BlockRewardBeneficiary`] is auto-credited. Exactly one reward is
        /// minted per block, so the manual and automatic paths can never
        /// double-pay.
        fn distribute_block_rewards() {
            if !Self::distribute_pending_rewards() {
                Self::accrue_block_reward();
            }
        }

        /// Auto-wired block-reward accrual: mint the current block reward to
        /// the configured beneficiary. No-op when no beneficiary is set, the
        /// reward is zero, or the canonical supply cap is reached.
        pub(crate) fn accrue_block_reward() {
            let beneficiary = match <BlockRewardBeneficiary<T>>::get() {
                Some(b) => b,
                None => return,
            };
            let reward = <CurrentBlockReward<T>>::get();
            if reward.is_zero() {
                return;
            }
            // Mint through the canonical supply manager (enforces the 1B cap).
            if T::Supply::mint_within_cap(&beneficiary, reward) {
                Self::deposit_event(Event::<T>::BlockRewardPaid {
                    miner: beneficiary,
                    reward,
                });
            }
        }

        /// Distribute a pending reward recorded by `submit_solution`.
        /// Returns `true` when a manual claim was present (regardless of
        /// whether it minted), so the caller can skip auto-accrual.
        pub(crate) fn distribute_pending_rewards() -> bool {
            let miner = match <PendingRewardBeneficiary<T>>::take() {
                Some(m) => m,
                None => return false,
            };
            let reward = <PendingBlockRewards<T>>::take();
            if !reward.is_zero() {
                // Mint through the canonical supply manager; it enforces the
                // global cap and updates the single `TotalMinted` counter.
                // Returns false (no-op) when the cap would be exceeded.
                if T::Supply::mint_within_cap(&miner, reward) {
                    Self::deposit_event(Event::<T>::BlockRewardPaid { miner, reward });
                }
            }
            true
        }

        pub fn adjust_difficulty(parent_difficulty: U256, actual_time: u64) -> U256 {
            let target_time = T::TargetBlockTime::get();
            if actual_time == 0 {
                return parent_difficulty;
            }
            // `Difficulty` is a *target* (hash < target = valid, higher = EASIER).
            // Ratio = 2*actual/(actual+target):
            //   - blocks too slow (actual > target) -> ratio > 1 -> target up (easier)
            //   - blocks too fast (actual < target) -> ratio < 1 -> target down (harder)
            //   - on time (actual == target)       -> ratio == 1 -> unchanged
            // The (actual+target) averaging smooths noise vs a raw actual/target
            // ratio. Scaled by 1000 to avoid truncation.
            let ratio_numer = actual_time * 1000;
            let ratio_denom = (target_time + actual_time) * 1000 / 2;
            let ratio_denom = ratio_denom.max(1);
            let new_diff =
                Self::apply_difficulty_ratio(parent_difficulty, ratio_numer, ratio_denom);
            let min_d = T::MinDifficulty::get();
            let max_d = T::MaxDifficulty::get();
            let clamped = if Self::u256_lt(&new_diff, &min_d) {
                min_d
            } else if Self::u256_gt(&new_diff, &max_d) {
                max_d
            } else {
                new_diff
            };
            <Difficulty<T>>::put(clamped);
            clamped
        }

        fn compute_solution_hash(solution: &EthashSolution) -> [u8; 32] {
            // Hash only the proof-relevant fields (exclude solution_hash to avoid circular dependency)
            let work_data = (
                solution.nonce,
                solution.mix_hash,
                solution.algorithm_version,
            );
            sp_io::hashing::blake2_256(&work_data.encode())
        }

        fn perform_halving() {
            let old_reward: BalanceOf<T> = <CurrentBlockReward<T>>::get();
            let new_reward = old_reward / 2u32.into();
            <CurrentBlockReward<T>>::put(new_reward);
            <LastHalvingBlock<T>>::put(frame_system::Pallet::<T>::block_number());
            <CurrentEpoch<T>>::mutate(|e| *e += 1);
            Self::deposit_event(Event::<T>::RewardHalved {
                old_reward,
                new_reward,
                halving_number: frame_system::Pallet::<T>::block_number()
                    .try_into()
                    .unwrap_or(0),
            });
        }

        pub(crate) fn u256_lt(a: &U256, b: &U256) -> bool {
            for i in (0..4).rev() {
                if a[i] != b[i] {
                    return a[i] < b[i];
                }
            }
            false
        }

        pub(crate) fn u256_gt(a: &U256, b: &U256) -> bool {
            for i in (0..4).rev() {
                if a[i] != b[i] {
                    return a[i] > b[i];
                }
            }
            false
        }

        pub(crate) fn meets_difficulty(hash: &[u8; 32], difficulty: &U256) -> bool {
            let h = [
                u64::from_le_bytes(hash[0..8].try_into().unwrap()),
                u64::from_le_bytes(hash[8..16].try_into().unwrap()),
                u64::from_le_bytes(hash[16..24].try_into().unwrap()),
                u64::from_le_bytes(hash[24..32].try_into().unwrap()),
            ];
            Self::u256_lt(&h, difficulty)
        }

        pub(crate) fn apply_difficulty_ratio(current: U256, numer: u64, denom: u64) -> U256 {
            let mut result = [0u64; 4];
            let mut carry = 0u128;
            for i in 0..4 {
                let v = current[i] as u128 * numer as u128 + carry;
                result[i] = (v / denom as u128) as u64;
                carry = v % denom as u128;
            }
            result
        }

        #[allow(dead_code)]
        pub(crate) fn get_epoch(block_height: u64) -> u64 {
            block_height / 30000
        }

        pub fn get_mining_info() -> MiningInfo {
            let difficulty = <Difficulty<T>>::get();
            let epoch = <CurrentEpoch<T>>::get();
            let block_number: u64 = frame_system::Pallet::<T>::block_number()
                .try_into()
                .unwrap_or(0);
            let block_reward: u128 = <CurrentBlockReward<T>>::get().try_into().unwrap_or(0);
            MiningInfo {
                difficulty,
                epoch,
                dag_size: 2u64 << (epoch + 23).min(63),
                algorithm_version: T::AlgorithmVersion::get(),
                block_number,
                block_reward,
                network_hashrate_estimate: 0,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frame_support::traits::Currency;
    use frame_support::{
        construct_runtime, derive_impl, parameter_types,
        traits::{BuildGenesisConfig, ConstU128, ConstU16, ConstU32, ConstU64, Everything, Hooks},
    };
    use sp_core::H256;
    use sp_runtime::{
        traits::{BlakeTwo256, IdentityLookup},
        BuildStorage,
    };

    use crate::pallet as local_gpu_pow;
    use crate::pallet::{
        BlockRewardBeneficiary, CurrentBlockReward, CurrentEpoch, Difficulty, PendingBlockRewards,
        PendingRewardBeneficiary,
    };
    use pallet_gpn_token::SupplyManager;
    use sp_runtime::SaturatedConversion;

    thread_local! {
        static MOCK_MINTED: std::cell::Cell<u128> = const { std::cell::Cell::new(0) };
        static MOCK_CAP: std::cell::Cell<u128> = const { std::cell::Cell::new(u128::MAX) };
    }

    /// Test double for the canonical `pallet-gpn-token` supply manager: tracks a
    /// single issued-supply counter and enforces `MOCK_CAP`.
    pub struct MockSupply;
    impl SupplyManager<u64, Balance> for MockSupply {
        fn total_issued() -> Balance {
            MOCK_MINTED.with(|c| c.get())
        }
        fn mint_within_cap(beneficiary: &u64, amount: Balance) -> bool {
            let minted = MOCK_MINTED.with(|c| c.get());
            let cap = MOCK_CAP.with(|c| c.get());
            if minted.saturating_add(amount) > cap {
                return false;
            }
            MOCK_MINTED.with(|c| c.set(minted.saturating_add(amount)));
            let _ = pallet_balances::Pallet::<Test>::deposit_creating(beneficiary, amount);
            true
        }
    }

    type Block = frame_system::mocking::MockBlock<Test>;
    type Balance = u128;

    construct_runtime!(
        pub struct Test {
            System: frame_system,
            Timestamp: pallet_timestamp,
            Balances: pallet_balances,
            GpuPow: local_gpu_pow,
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

    impl pallet_timestamp::Config for Test {
        type Moment = u64;
        type OnTimestampSet = ();
        type MinimumPeriod = ConstU64<5000>;
        type WeightInfo = ();
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
        pub const TestMinDifficulty: U256 = [0, 0, 0, 0x1_0000];
        pub const TestMaxDifficulty: U256 = [0xFFFF_FFFF_FFFF_FFFF; 4];
        pub const TestInitialReward: Balance = 50_000_000_000_000; // 50 GPN
        pub const TestHalvingInterval: u64 = 100;
        pub const TestTargetBlockTime: u64 = 15;
        pub const TestDifficultyWindow: u64 = 10;
        pub const TestPowMaxWeight: u64 = 200_000;
        pub const TestAlgoVersion: u32 = 1;
    }

    impl crate::pallet::Config for Test {
        type Currency = Balances;
        type MinDifficulty = TestMinDifficulty;
        type MaxDifficulty = TestMaxDifficulty;
        type InitialBlockReward = TestInitialReward;
        type HalvingInterval = TestHalvingInterval;
        type TargetBlockTime = TestTargetBlockTime;
        type DifficultyAdjustmentWindow = TestDifficultyWindow;
        type PowMaxWeight = TestPowMaxWeight;
        type AlgorithmVersion = TestAlgoVersion;
        type Supply = MockSupply;
    }

    fn new_test_ext() -> sp_io::TestExternalities {
        MOCK_MINTED.with(|c| c.set(0));
        MOCK_CAP.with(|c| c.set(u128::MAX));
        let t = frame_system::GenesisConfig::<Test>::default()
            .build_storage()
            .unwrap();
        let mut ext = sp_io::TestExternalities::new(t);
        ext.execute_with(|| {
            let gp_config = crate::pallet::GenesisConfig::<Test> {
                initial_difficulty: TestMinDifficulty::get(),
                initial_block_reward: TestInitialReward::get(),
                block_reward_beneficiary: None,
                _config: Default::default(),
            };
            BuildGenesisConfig::build(&gp_config);
            System::set_block_number(1);
        });
        ext
    }

    type GpuPowPallet = crate::pallet::Pallet<Test>;

    #[test]
    fn test_initial_state() {
        new_test_ext().execute_with(|| {
            assert_eq!(Difficulty::<Test>::get(), TestMinDifficulty::get());
            assert_eq!(CurrentBlockReward::<Test>::get(), TestInitialReward::get());
            assert_eq!(CurrentEpoch::<Test>::get(), 0);
            assert_eq!(PendingBlockRewards::<Test>::get(), 0);
            assert!(PendingRewardBeneficiary::<Test>::get().is_none());
        });
    }

    #[test]
    fn test_u256_comparison() {
        new_test_ext().execute_with(|| {
            let small = [0u64; 4];
            let medium = [0, 0, 0, 0x1000];
            let large = [0, 0, 0, 0x2000];
            assert!(GpuPowPallet::u256_lt(&small, &medium));
            assert!(!GpuPowPallet::u256_lt(&medium, &small));
            assert!(GpuPowPallet::u256_gt(&large, &medium));
            assert!(!GpuPowPallet::u256_gt(&medium, &large));
        });
    }

    #[test]
    fn test_meets_difficulty() {
        new_test_ext().execute_with(|| {
            let zero_hash = [0u8; 32];
            assert!(GpuPowPallet::meets_difficulty(
                &zero_hash,
                &[0, 0, 0, 0x1000]
            ));
            let big_hash = [0xFFu8; 32];
            assert!(!GpuPowPallet::meets_difficulty(
                &big_hash,
                &[0, 0, 0, 0x1000]
            ));
        });
    }

    #[test]
    #[test]
    fn test_difficulty_adjustment_slower_easier() {
        new_test_ext().execute_with(|| {
            // Blocks too slow (actual 30s > target 15s) -> target must rise (easier).
            let parent = [0, 0, 0, 0x1000_0000_0000_0000];
            let adjusted = GpuPowPallet::adjust_difficulty(parent, 30);
            assert!(GpuPowPallet::u256_gt(&adjusted, &parent));
        });
    }

    #[test]
    fn test_difficulty_adjustment_faster_harder() {
        new_test_ext().execute_with(|| {
            // Blocks too fast (actual 5s < target 15s) -> target must fall (harder).
            let parent = [0, 0, 0, 0x1000_0000_0000_0000];
            let adjusted = GpuPowPallet::adjust_difficulty(parent, 5);
            assert!(GpuPowPallet::u256_lt(&adjusted, &parent));
        });
    }

    #[test]
    fn test_difficulty_adjustment_on_time_stable() {
        new_test_ext().execute_with(|| {
            // On-time (actual 15s == target 15s) -> unchanged.
            let parent = [0, 0, 0, 0x1000_0000_0000_0000];
            let adjusted = GpuPowPallet::adjust_difficulty(parent, 15);
            assert_eq!(adjusted, parent);
        });
    }

    #[test]
    fn test_on_initialize_tunes_difficulty_windowed() {
        new_test_ext().execute_with(|| {
            // Window = 10. Start from an easy target (above MinDifficulty) so
            // hardening is observable. Timestamps are in MILLISECONDS.
            // Blocks 1-11 are fast (avg 5s < target 15s) -> the target falls.
            Difficulty::<Test>::put([0, 0, 0, 0x1000_0000_0000]); // 2^236, easy
            let before = Difficulty::<Test>::get();
            assert!(GpuPowPallet::u256_gt(&before, &TestMinDifficulty::get()));

            pallet_timestamp::Pallet::<Test>::set_timestamp(1_000_000_000); // ms
            System::set_block_number(1);
            GpuPowPallet::on_initialize(1); // seeds window start, no change

            // 5s (5000ms) per block for blocks 2..11; window completes at block
            // 11: 10 blocks over 50s = 5s avg -> harder.
            for b in 2..=11 {
                pallet_timestamp::Pallet::<Test>::set_timestamp(1_000_000_000 + (b as u64) * 5000);
                System::set_block_number(b);
                GpuPowPallet::on_initialize(b);
            }
            let after = Difficulty::<Test>::get();
            assert!(GpuPowPallet::u256_lt(&after, &before));
        });
    }

    #[test]
    fn test_submit_solution() {
        new_test_ext().execute_with(|| {
            let miner = 42u64;
            let sol = EthashSolution {
                nonce: 0,
                mix_hash: [0u8; 32],
                solution_hash: [0u8; 32],
                algorithm_version: 1,
            };
            // Compute the correct solution_hash (hash of nonce + mix_hash + version)
            let work_data = (sol.nonce, sol.mix_hash, sol.algorithm_version);
            let correct_hash = sp_io::hashing::blake2_256(&work_data.encode());
            let valid_solution = EthashSolution {
                solution_hash: correct_hash,
                ..sol
            };
            // Use MaxDifficulty (easiest) so any valid hash passes
            let result = GpuPowPallet::submit_solution(
                RuntimeOrigin::signed(miner),
                valid_solution,
                TestMaxDifficulty::get(),
            );
            assert!(result.is_ok());
            assert_eq!(PendingBlockRewards::<Test>::get(), TestInitialReward::get());
            assert_eq!(PendingRewardBeneficiary::<Test>::get(), Some(miner));
        });
    }

    #[test]
    fn test_reward_distribution() {
        new_test_ext().execute_with(|| {
            let miner = 42u64;
            let initial_balance = pallet_balances::Pallet::<Test>::free_balance(miner);
            let sol = EthashSolution {
                nonce: 1,
                mix_hash: [0u8; 32],
                solution_hash: [0u8; 32],
                algorithm_version: 1,
            };
            let work_data = (sol.nonce, sol.mix_hash, sol.algorithm_version);
            let correct_hash = sp_io::hashing::blake2_256(&work_data.encode());
            let valid_solution = EthashSolution {
                solution_hash: correct_hash,
                ..sol
            };
            GpuPowPallet::submit_solution(
                RuntimeOrigin::signed(miner),
                valid_solution,
                TestMaxDifficulty::get(),
            )
            .unwrap();
            GpuPowPallet::distribute_pending_rewards();
            let new_balance = pallet_balances::Pallet::<Test>::free_balance(miner);
            assert_eq!(new_balance, initial_balance + TestInitialReward::get());
            assert_eq!(PendingBlockRewards::<Test>::get(), 0);
            assert!(PendingRewardBeneficiary::<Test>::get().is_none());
        });
    }

    #[test]
    fn test_get_epoch() {
        new_test_ext().execute_with(|| {
            assert_eq!(GpuPowPallet::get_epoch(0), 0);
            assert_eq!(GpuPowPallet::get_epoch(29999), 0);
            assert_eq!(GpuPowPallet::get_epoch(30000), 1);
            assert_eq!(GpuPowPallet::get_epoch(60000), 2);
        });
    }

    #[test]
    fn test_u256_arithmetic() {
        new_test_ext().execute_with(|| {
            let current = [100u64, 0, 0, 0];
            let result = GpuPowPallet::apply_difficulty_ratio(current, 2000, 1000);
            assert_eq!(result[0], 200);
        });
    }

    #[test]
    fn test_total_minted_tracking() {
        new_test_ext().execute_with(|| {
            assert_eq!(MockSupply::total_issued(), 0);
            let miner = 42u64;
            let sol = EthashSolution {
                nonce: 1,
                mix_hash: [0u8; 32],
                solution_hash: [0u8; 32],
                algorithm_version: 1,
            };
            let work_data = (sol.nonce, sol.mix_hash, sol.algorithm_version);
            let correct_hash = sp_io::hashing::blake2_256(&work_data.encode());
            let valid = EthashSolution {
                solution_hash: correct_hash,
                ..sol
            };
            GpuPowPallet::submit_solution(
                RuntimeOrigin::signed(miner),
                valid,
                TestMaxDifficulty::get(),
            )
            .unwrap();
            GpuPowPallet::distribute_pending_rewards();
            assert_eq!(MockSupply::total_issued(), TestInitialReward::get());
        });
    }

    #[test]
    fn test_supply_cap_halts_mining() {
        new_test_ext().execute_with(|| {
            // Set the canonical counter to the cap so new rewards are rejected.
            MOCK_CAP.with(|c| c.set(1_000_000_000_000_000_000_000));
            MOCK_MINTED.with(|c| c.set(1_000_000_000_000_000_000_000));

            let miner = 42u64;
            let initial_balance = pallet_balances::Pallet::<Test>::free_balance(miner);
            let sol = EthashSolution {
                nonce: 2,
                mix_hash: [0u8; 32],
                solution_hash: [0u8; 32],
                algorithm_version: 1,
            };
            let work_data = (sol.nonce, sol.mix_hash, sol.algorithm_version);
            let correct_hash = sp_io::hashing::blake2_256(&work_data.encode());
            let valid = EthashSolution {
                solution_hash: correct_hash,
                ..sol
            };
            GpuPowPallet::submit_solution(
                RuntimeOrigin::signed(miner),
                valid,
                TestMaxDifficulty::get(),
            )
            .unwrap();
            // Distribute should silently skip due to supply cap
            GpuPowPallet::distribute_pending_rewards();
            // Balance unchanged
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(miner),
                initial_balance
            );
            // Canonical issued supply unchanged
            assert_eq!(MockSupply::total_issued(), 1_000_000_000_000_000_000_000);
        });
    }

    // ── Item 4: block-reward auto-wiring ────────────────────────────────────

    /// Build an `EthashSolution` whose `solution_hash` matches the pallet's
    /// recomputation, so `submit_solution` accepts it.
    fn valid_solution(nonce: u64) -> EthashSolution {
        let sol = EthashSolution {
            nonce,
            mix_hash: [0u8; 32],
            solution_hash: [0u8; 32],
            algorithm_version: TestAlgoVersion::get(),
        };
        let work_data = (sol.nonce, sol.mix_hash, sol.algorithm_version);
        let correct_hash = sp_io::hashing::blake2_256(&work_data.encode());
        EthashSolution {
            solution_hash: correct_hash,
            ..sol
        }
    }

    #[test]
    fn test_genesis_can_seed_block_reward_beneficiary() {
        MOCK_MINTED.with(|c| c.set(0));
        MOCK_CAP.with(|c| c.set(u128::MAX));
        let t = frame_system::GenesisConfig::<Test>::default()
            .build_storage()
            .unwrap();
        let mut ext = sp_io::TestExternalities::new(t);
        ext.execute_with(|| {
            let gp_config = crate::pallet::GenesisConfig::<Test> {
                initial_difficulty: TestMinDifficulty::get(),
                initial_block_reward: TestInitialReward::get(),
                block_reward_beneficiary: Some(7u64),
                _config: Default::default(),
            };
            BuildGenesisConfig::build(&gp_config);
            assert_eq!(BlockRewardBeneficiary::<Test>::get(), Some(7u64));
        });
    }

    #[test]
    fn test_set_block_beneficiary_requires_root() {
        new_test_ext().execute_with(|| {
            assert!(GpuPowPallet::set_block_beneficiary(RuntimeOrigin::signed(1), 9).is_err());
            assert_eq!(BlockRewardBeneficiary::<Test>::get(), None);
            assert!(GpuPowPallet::set_block_beneficiary(RuntimeOrigin::root(), 9).is_ok());
            assert_eq!(BlockRewardBeneficiary::<Test>::get(), Some(9));
        });
    }

    /// A configured beneficiary is credited automatically on each block, with
    /// no manual `submit_solution` needed (the auto-wiring deliverable).
    #[test]
    fn test_block_reward_auto_wired_per_block() {
        new_test_ext().execute_with(|| {
            let miner = 7u64;
            GpuPowPallet::set_block_beneficiary(RuntimeOrigin::root(), miner).unwrap();
            let before = pallet_balances::Pallet::<Test>::free_balance(miner);

            pallet_timestamp::Pallet::<Test>::set_timestamp(1_000_000_000);
            for b in 1..=5u64 {
                System::set_block_number(b);
                GpuPowPallet::on_initialize(b);
            }

            let reward = TestInitialReward::get();
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(miner),
                before + reward * 5,
                "each produced block must credit the miner"
            );
            assert_eq!(MockSupply::total_issued(), reward * 5);
        });
    }

    /// With no beneficiary configured, blocks mint nothing (safe default).
    #[test]
    fn test_auto_reward_no_beneficiary_is_noop() {
        new_test_ext().execute_with(|| {
            System::set_block_number(1);
            GpuPowPallet::on_initialize(1);
            GpuPowPallet::on_initialize(2);
            assert_eq!(MockSupply::total_issued(), 0);
            assert_eq!(BlockRewardBeneficiary::<Test>::get(), None);
        });
    }

    /// A manual claim takes precedence: the block is paid exactly once, not
    /// twice (app + auto).
    #[test]
    fn test_manual_claim_takes_precedence_over_auto() {
        new_test_ext().execute_with(|| {
            let auto_miner = 7u64;
            let manual_miner = 42u64;
            GpuPowPallet::set_block_beneficiary(RuntimeOrigin::root(), auto_miner).unwrap();

            // Manual claim recorded during block 1...
            GpuPowPallet::submit_solution(
                RuntimeOrigin::signed(manual_miner),
                valid_solution(1),
                TestMaxDifficulty::get(),
            )
            .unwrap();

            // ...paid at the next block, and the auto path is skipped.
            System::set_block_number(2);
            GpuPowPallet::on_initialize(2);

            let reward = TestInitialReward::get();
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(manual_miner),
                reward,
                "manual claimant paid exactly once"
            );
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(auto_miner),
                0,
                "auto beneficiary must not also be paid"
            );
            assert_eq!(MockSupply::total_issued(), reward);
        });
    }

    /// Auto-accrual still respects the canonical supply cap.
    #[test]
    fn test_auto_reward_respects_supply_cap() {
        new_test_ext().execute_with(|| {
            MOCK_CAP.with(|c| c.set(1_000_000_000_000_000_000_000));
            MOCK_MINTED.with(|c| c.set(1_000_000_000_000_000_000_000));
            let miner = 7u64;
            GpuPowPallet::set_block_beneficiary(RuntimeOrigin::root(), miner).unwrap();
            System::set_block_number(1);
            GpuPowPallet::on_initialize(1);
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(miner), 0);
            assert_eq!(MockSupply::total_issued(), 1_000_000_000_000_000_000_000);
        });
    }
}
