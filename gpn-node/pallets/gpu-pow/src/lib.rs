//! # GPU PoW Pallet
//!
//! Ethash-compatible proof-of-work for GPU mining.
//! Handles difficulty adjustment, block reward distribution, and PoW verification.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "runtime-benchmarks")]
pub mod benchmarking;
pub mod weights;

use codec::{Decode, DecodeWithMemTracking, Encode};
use core::fmt::Debug;
use scale_info::TypeInfo;

use sp_runtime::SaturatedConversion;

#[cfg(feature = "std")]
use serde::{Deserialize, Serialize};

pub use pallet::*;
use pallet_gpu_attestation::GPUAttestationProvider;
pub use weights::WeightInfo;

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
        /// Upper bound on one retarget step, as a percent of the current target
        /// (200 ⇒ the target may at most **double** in one window, i.e. the
        /// window mean was ≥ 2× the target block time).
        ///
        /// See `MinRetargetStepPercent` for why the step is clamped at all.
        #[pallet::constant]
        type MaxRetargetStepPercent: Get<u32>;
        /// Lower bound on one retarget step, as a percent of the current target
        /// (50 ⇒ the target may at most **halve** in one window).
        ///
        /// The retarget law is `new_target = old_target × clamp(actual/target,
        /// min_pct, max_pct)`. `actual/target` is the *symmetric* control law:
        /// 2× too slow doubles the target, 2× too fast halves it, and
        /// `min × max == 100 × 100` makes the clamp symmetric in log space, so
        /// the controller cannot ratchet tighter or looser than it can recover.
        ///
        /// The law this replaces was `2x/(1+x)`, which is asymmetric
        /// (`x = 2 ⇒ +33 %`, `x = 0.5 ⇒ −33 %`, and `x → 0 ⇒ r → 0` with no
        /// floor). Measured on this chain it collapsed the target by 50.6 % in
        /// one 10-block window (85.51B → 42.27B) and then needed a whole window
        /// to recover only +37 % — easy to tighten, slow to relax, which is
        /// exactly what put 86–96 s blocks behind a 15 s target (#52).
        #[pallet::constant]
        type MinRetargetStepPercent: Get<u32>;
        #[pallet::constant]
        type PowMaxWeight: Get<u64>;
        #[pallet::constant]
        type AlgorithmVersion: Get<u32>;
        /// Canonical issued-supply accounting lives in `pallet-gpn-token`.
        /// Block rewards are minted through it so the 1B cap is enforced
        /// against one counter (workplan item 1).
        type Supply: SupplyManager<Self::AccountId, BalanceOf<Self>>;
        /// Cross-pallet read of attested GPU throughput — the block-reward path
        /// requires the producer to be attested, so **only GPU miners can earn**
        /// (the "GPU-only" eligibility gate). This is NOT an equalizer: payouts
        /// stay hashrate-proportional / Bitcoin-style — the fastest GPU wins the
        /// most blocks. The gate only decides who is allowed to keep a reward.
        type GPUAttestation: GPUAttestationProvider<Self::AccountId>;
        /// Weight information for extrinsics in this pallet (workplan item 9).
        type WeightInfo: WeightInfo;
        /// Reads the block PRODUCER's account from the PoW seal digest, so the
        /// block reward is paid to the miner who produced the block (Bitcoin-style
        /// coinbase) instead of a separate claim. `None` = unknown/legacy seal →
        /// fall back to `BlockRewardBeneficiary`.
        type FindAuthor: frame_support::traits::FindAuthor<Self::AccountId>;
        /// Minimum number of blocks an account must have authored (proven, seal-verified
        /// PoW work) before its block rewards are paid to it directly. This is the
        /// **stake-free, PoW-verified "GPU-only" gate**: producing a seal-verified block
        /// IS proof of real GPU work, so no stake or self-reported attestation is needed.
        /// See design-verifiable-gpu-proof-2026-10-06.md.
        #[pallet::constant]
        type MiningAttestationThreshold: Get<u32>;
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

    #[pallet::storage]
    /// Per-account count of blocks authored. The author is stamped by the node in the
    /// pre-runtime digest and the block's Ethash seal is verified on import, so this
    /// count is *proven* GPU work — the basis of the stake-free mining gate.
    pub type BlocksAuthored<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, u32, ValueQuery>;

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
        /// The block reward was changed via `set_block_reward` (item 5).
        BlockRewardSet {
            old_reward: BalanceOf<T>,
            new_reward: BalanceOf<T>,
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
        /// The caller is not an attested GPU provider.
        NotAttested,
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
                // Millisecond-precise retarget. `last_ts`/`current_ts` are the
                // pallet_timestamp `Moment` (MILLISECONDS). Averaging in *whole
                // seconds* truncates to 0 once blocks are faster than 1s each,
                // which made `adjust_difficulty` a no-op and FROZE the difficulty
                // (a fast GPU could then run away). Average in ms and compare
                // against the target converted ms→target. Guard against a missing
                // window start (last_ts == 0, e.g. block 1 seeded before any
                // timestamp was set).
                if last_ts != 0 && blocks > 0 && elapsed_ms > 0 {
                    let avg_block_time_ms = elapsed_ms / blocks;
                    let stored = <Difficulty<T>>::get();
                    let new = Self::adjust_difficulty(stored, avg_block_time_ms);
                    log::info!(
                        target: "runtime::gpu-pow",
                        "ADJUST now={} last_adj={} last_ts={} cur_ts={} elapsed_ms={} blocks={} avg_ms={} target_ms={} stored_limb3={} new_limb3={}",
                        now.saturated_into::<u64>(), last_adj.saturated_into::<u64>(), last_ts, current_ts, elapsed_ms, blocks, avg_block_time_ms,
                        T::TargetBlockTime::get().saturating_mul(1000), stored[3], new[3],
                    );
                }
                <LastAdjustmentBlock<T>>::put(now);
                <LastBlockTimestamp<T>>::put(current_ts);
            }

            let halving_interval = T::HalvingInterval::get();
            let last_halving = <LastHalvingBlock<T>>::get();
            // A zero `LastHalvingBlock` is the genesis baseline (block 0): the
            // field is ONLY ever written by `perform_halving` itself
            // (`genesis_build` seeds it to zero), so the old
            // `!last_halving.is_zero()` guard made this branch UNREACHABLE and
            // the block reward NEVER halved (red-team finding 2026-10-09,
            // chain-consensus / monetary policy: emission stayed at 50 GPN/block
            // indefinitely, over-minting until the supply cap — ~2.1x the
            // documented `2 x reward x HalvingInterval` course). Treat zero as the
            // genesis baseline so the first halving fires at `now == interval`
            // and each subsequent one every interval. `last_halving` is always
            // `<= now`, so the subtraction cannot underflow.
            if !halving_interval.is_zero() && now - last_halving >= halving_interval {
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
        #[pallet::weight(<T as Config>::WeightInfo::submit_solution())]
        pub fn submit_solution(
            origin: OriginFor<T>,
            solution: EthashSolution,
            difficulty: U256,
        ) -> DispatchResult {
            let miner = ensure_signed(origin)?;
            ensure!(
                T::GPUAttestation::attested_gflops(&miner) > 0,
                Error::<T>::NotAttested
            );
            let computed_hash = Self::compute_solution_hash(&solution);
            ensure!(
                computed_hash == solution.solution_hash,
                Error::<T>::SolutionHashMismatch
            );
            // Validate the proof-of-work claim against the *chain's* difficulty
            // target, never the caller-supplied one (red-team finding 2026-10-09,
            // chain-consensus/difficulty): `difficulty` is attacker-chosen, so
            // trusting it let any attested account pass `meets_difficulty` with a
            // trivial target (e.g. U256::MAX = easiest) and be credited as having
            // mined — a PoW-bypass / (latent) reward-theft. Anchor the check to the
            // live target and additionally reject a claimed target weaker (larger)
            // than the chain's. `DifficultyTooHigh` already existed, unused, for
            // exactly this guard; `Difficulty` is a *target* (higher = easier).
            let chain_difficulty = <Difficulty<T>>::get();
            ensure!(
                !Self::u256_gt(&difficulty, &chain_difficulty),
                Error::<T>::DifficultyTooHigh
            );
            ensure!(
                Self::meets_difficulty(&solution.solution_hash, &chain_difficulty),
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
        #[pallet::weight(<T as Config>::WeightInfo::set_block_beneficiary())]
        pub fn set_block_beneficiary(
            origin: OriginFor<T>,
            beneficiary: T::AccountId,
        ) -> DispatchResult {
            ensure_root(origin)?;
            <BlockRewardBeneficiary<T>>::put(&beneficiary);
            Self::deposit_event(Event::<T>::BlockRewardBeneficiarySet { beneficiary });
            Ok(())
        }

        /// Root only: adjust the block reward (the mining emission rate).
        ///
        /// This is the *runtime* knob for "change the mining rate in future
        /// without touching the genesis". It sets `CurrentBlockReward`, which
        /// every block's emission reads. Reachable by the 2-of-3 committee via
        /// `gpn-governance` propose/execute (Root dispatch), or by sudo.
        /// emission_rate = new_reward / TargetBlockTime.
        #[pallet::call_index(2)]
        #[pallet::weight(<T as Config>::WeightInfo::set_block_reward())]
        pub fn set_block_reward(origin: OriginFor<T>, new_reward: BalanceOf<T>) -> DispatchResult {
            ensure_root(origin)?;
            let old_reward: BalanceOf<T> = <CurrentBlockReward<T>>::get();
            <CurrentBlockReward<T>>::put(new_reward);
            Self::deposit_event(Event::<T>::BlockRewardSet {
                old_reward,
                new_reward,
            });
            Ok(())
        }
    }

    impl<T: Config> Pallet<T> {
        /// Pay this block's emission once, to the block's PRODUCER.
        ///
        /// Bitcoin-style: the miner that produced the block earns its reward. The
        /// author is read from the PoW seal digest via `FindAuthor`; the payout is
        /// gated on GPU attestation so **only attested GPU miners are paid** (the
        /// "GPU-only" rule; an unattested author can't be paid and the reward
        /// falls back to `BlockRewardBeneficiary`). This gate is *eligibility*, not
        /// equalization — payout weight remains hashrate-proportional.
        pub(crate) fn distribute_block_rewards() {
            let reward = <CurrentBlockReward<T>>::get();
            if reward.is_zero() {
                return;
            }
            let author =
                <T::FindAuthor as frame_support::traits::FindAuthor<T::AccountId>>::find_author(
                    frame_system::Pallet::<T>::digest()
                        .logs
                        .iter()
                        .filter_map(|d| d.as_pre_runtime()),
                );
            let payee = match author {
                Some(a) => {
                    // Stake-free, PoW-verified "GPU-only" gate: count proven block
                    // production. A seal-verified block authored IS proof of real GPU
                    // work, so an account becomes pay-eligible after authoring
                    // `MiningAttestationThreshold` blocks (or is grandfathered in via an
                    // existing attestation). No stake is required.
                    let authored = <BlocksAuthored<T>>::mutate(&a, |c| {
                        *c = c.saturating_add(1);
                        *c
                    });
                    if authored >= T::MiningAttestationThreshold::get()
                        || T::GPUAttestation::attested_gflops(&a) > 0
                    {
                        Some(a)
                    } else {
                        <BlockRewardBeneficiary<T>>::get()
                    }
                }
                None => <BlockRewardBeneficiary<T>>::get(),
            };
            if let Some(p) = payee {
                // Mint through the canonical supply manager (enforces the 1B cap).
                if T::Supply::mint_within_cap(&p, reward) {
                    Self::deposit_event(Event::<T>::BlockRewardPaid { miner: p, reward });
                }
            }
        }

        /// `actual_time_ms` is the average block time over the adjustment window,
        /// in MILLISECONDS. `TargetBlockTime` is configured in whole SECONDS, so
        /// it is converted to ms here — this keeps sub-second precision and means
        /// a sub-1s average still hardens the target instead of truncating to 0
        /// and freezing (the runaway bug). See design item 7.
        pub fn adjust_difficulty(parent_difficulty: U256, actual_time_ms: u64) -> U256 {
            let target_time_ms = T::TargetBlockTime::get().saturating_mul(1000);
            if actual_time_ms == 0 || target_time_ms == 0 {
                return parent_difficulty;
            }
            // `Difficulty` is a *target* (hash < target = valid, higher = EASIER).
            // Ratio = actual/target, clamped to [min_pct, max_pct]:
            //   - blocks too slow (actual > target) -> ratio > 1 -> target up (easier)
            //   - blocks too fast (actual < target) -> ratio < 1 -> target down (harder)
            //   - on time (actual == target)       -> ratio == 1 -> unchanged
            //
            // Deliberately NOT `2*actual/(actual+target)`. That law is
            // asymmetric: it can only relax by <2× per window while it can
            // harden without bound (actual→0 ⇒ ratio→0), so a burst of fast
            // blocks ratchets the target down and the chain then crawls for
            // several windows (#52, measured −50.6 % then +37 %).
            let (ratio_numer, ratio_denom) = Self::clamp_retarget_ratio(
                actual_time_ms,
                target_time_ms,
                T::MinRetargetStepPercent::get(),
                T::MaxRetargetStepPercent::get(),
            );
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

        /// Bound one retarget step to `[min_pct, max_pct]` percent of the current
        /// target, returning `(numer, denom)` for `apply_difficulty_ratio`.
        ///
        /// Inside the band the raw ratio is returned untouched (no rounding and
        /// no re-quantisation, so the controller is continuous at the boundary);
        /// outside it the step is exactly the bound. `max_pct` is floored at 100
        /// (never harder than "hold") and `min_pct` is capped at `max_pct`, so a
        /// mis-set config degrades to "pin the target", never to an inverted or
        /// zero-length step (a zero denominator would be a divide-by-zero in
        /// `apply_difficulty_ratio`).
        ///
        /// Why a clamp at all: a window mean of a memoryless PoW solve is noisy
        /// (CV = 1/√n), and with a short window that noise is larger than any
        /// real hashrate drift the loop is meant to track. Unbounded, the
        /// controller converts sampling noise into multi-minute block times; the
        /// clamp is what keeps a burst from becoming a cadence swing.
        pub(crate) fn clamp_retarget_ratio(
            numer: u64,
            denom: u64,
            min_pct: u32,
            max_pct: u32,
        ) -> (u64, u64) {
            let hi = max_pct.max(100) as u128;
            let lo = (min_pct as u128).min(hi).max(1);
            if denom == 0 {
                return (numer, denom);
            }
            let n = numer as u128;
            let d = denom as u128;
            // n/d < lo/100 ⇒ the window was faster than the clamp allows.
            if n.saturating_mul(100) < d.saturating_mul(lo) {
                return (lo as u64, 100);
            }
            // n/d > hi/100 ⇒ the window was slower than the clamp allows.
            if n.saturating_mul(100) > d.saturating_mul(hi) {
                return (hi as u64, 100);
            }
            (numer, denom)
        }

        pub(crate) fn apply_difficulty_ratio(current: U256, numer: u64, denom: u64) -> U256 {
            let mut result = [0u64; 4];
            let mut carry = 0u128;
            for i in 0..4 {
                let v = current[i] as u128 * numer as u128 + carry;
                let q = v / denom as u128;
                // A target near `MaxDifficulty` scaled by ratio > 1 overflows the
                // limb. Truncating (`as u64`) would wrap it into a *much smaller*
                // number — i.e. a huge accidental HARDENING that could stall the
                // chain, since the max-bound check below would then never fire.
                // Saturate instead: the caller's `MaxDifficulty` clamp then keeps
                // the target at the ceiling.
                if q > u64::MAX as u128 {
                    return [u64::MAX; 4];
                }
                result[i] = q as u64;
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
        pub const TestDifficultyWindow: u64 = 30;
        pub const TestMinRetargetStepPercent: u32 = 50;
        pub const TestMaxRetargetStepPercent: u32 = 200;
        pub const TestPowMaxWeight: u64 = 200_000;
        pub const TestAlgoVersion: u32 = 1;
        pub const TestMiningAttestationThreshold: u32 = 3;
    }

    // Mock hardware attestation so the capability gate can be exercised in tests.
    pub struct MockAttestation;
    impl GPUAttestationProvider<u64> for MockAttestation {
        fn attested_gflops(_: &u64) -> u64 {
            1_000
        }
        fn attested_tier(_: &u64) -> pallet_gpu_attestation::AttestationTier {
            pallet_gpu_attestation::AttestationTier::Silver
        }
    }

    // Mock block-author source: returns whatever MOCK_AUTHOR is set to.
    thread_local! {
        pub static MOCK_AUTHOR: std::cell::Cell<Option<u64>> = std::cell::Cell::new(None);
    }
    pub struct MockAuthor;
    impl frame_support::traits::FindAuthor<u64> for MockAuthor {
        fn find_author<'a, I>(_digests: I) -> Option<u64>
        where
            I: 'a + IntoIterator<Item = (sp_runtime::ConsensusEngineId, &'a [u8])>,
        {
            MOCK_AUTHOR.with(|c| c.get())
        }
    }

    impl crate::pallet::Config for Test {
        type Currency = Balances;
        type GPUAttestation = MockAttestation;
        type MinDifficulty = TestMinDifficulty;
        type MaxDifficulty = TestMaxDifficulty;
        type InitialBlockReward = TestInitialReward;
        type HalvingInterval = TestHalvingInterval;
        type TargetBlockTime = TestTargetBlockTime;
        type DifficultyAdjustmentWindow = TestDifficultyWindow;
        type MinRetargetStepPercent = TestMinRetargetStepPercent;
        type MaxRetargetStepPercent = TestMaxRetargetStepPercent;
        type PowMaxWeight = TestPowMaxWeight;
        type AlgorithmVersion = TestAlgoVersion;
        type Supply = MockSupply;
        type WeightInfo = ();
        type FindAuthor = MockAuthor;
        type MiningAttestationThreshold = TestMiningAttestationThreshold;
    }

    pub fn new_test_ext() -> sp_io::TestExternalities {
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
    fn test_difficulty_adjustment_slower_easier() {
        new_test_ext().execute_with(|| {
            // Blocks too slow (actual 30s = 30000ms > target 15s) -> target rises (easier).
            let parent = [0, 0, 0, 0x1000_0000_0000_0000];
            let adjusted = GpuPowPallet::adjust_difficulty(parent, 30_000);
            assert!(GpuPowPallet::u256_gt(&adjusted, &parent));
        });
    }

    #[test]
    fn test_difficulty_adjustment_faster_harder() {
        new_test_ext().execute_with(|| {
            // Blocks too fast (actual 5s = 5000ms < target 15s) -> target falls (harder).
            let parent = [0, 0, 0, 0x1000_0000_0000_0000];
            let adjusted = GpuPowPallet::adjust_difficulty(parent, 5_000);
            assert!(GpuPowPallet::u256_lt(&adjusted, &parent));
        });
    }

    #[test]
    fn test_difficulty_adjustment_on_time_stable() {
        new_test_ext().execute_with(|| {
            // On-time (actual 15s == target 15s) -> unchanged.
            let parent = [0, 0, 0, 0x1000_0000_0000_0000];
            let adjusted = GpuPowPallet::adjust_difficulty(parent, 15_000);
            assert_eq!(adjusted, parent);
        });
    }

    /// Regression (item 7, runaway bug): a sub-second average must still
    /// HARDEN the target. In the old whole-second averaging this case
    /// truncated to 0 and left the target frozen.
    #[test]
    fn test_difficulty_adjustment_sub_second_hardens() {
        new_test_ext().execute_with(|| {
            let parent = [0, 0, 0, 0x1000_0000_0000_0000];
            let adjusted = GpuPowPallet::adjust_difficulty(parent, 500); // 0.5s/block
            assert!(
                GpuPowPallet::u256_lt(&adjusted, &parent),
                "sub-1s must harden"
            );
            assert_ne!(adjusted, parent, "must not be a no-op (frozen)");
        });
    }

    #[test]
    fn test_on_initialize_tunes_difficulty_windowed() {
        new_test_ext().execute_with(|| {
            // Window = 30. Start from an easy target (above MinDifficulty) so
            // hardening is observable. Timestamps are in MILLISECONDS.
            // Blocks 2-31 are fast (avg 5s < target 15s) -> the target falls.
            Difficulty::<Test>::put([0, 0, 0, 0x1000_0000_0000]); // 2^236, easy
            let before = Difficulty::<Test>::get();
            assert!(GpuPowPallet::u256_gt(&before, &TestMinDifficulty::get()));

            pallet_timestamp::Pallet::<Test>::set_timestamp(1_000_000_000); // ms
            System::set_block_number(1);
            GpuPowPallet::on_initialize(1); // seeds window start, no change

            // 5s (5000ms) per block for blocks 2..=31; the window completes at
            // block 31: 30 blocks over 150s = 5s avg -> harder (clamped to 0.5x).
            for b in 2..=31 {
                pallet_timestamp::Pallet::<Test>::set_timestamp(1_000_000_000 + (b as u64) * 5000);
                System::set_block_number(b);
                GpuPowPallet::on_initialize(b);
            }
            let after = Difficulty::<Test>::get();
            assert!(GpuPowPallet::u256_lt(&after, &before));
        });
    }

    /// Regression (item 7): previously a sub-1s window average truncated to 0
    /// seconds and the retarget was a no-op (difficulty frozen → runaway risk).
    /// With ms averaging the same window hardens the target.
    #[test]
    fn test_on_initialize_sub_second_window_hardens() {
        new_test_ext().execute_with(|| {
            Difficulty::<Test>::put([0, 0, 0, 0x1000_0000_0000]); // 2^236, easy
            let before = Difficulty::<Test>::get();

            pallet_timestamp::Pallet::<Test>::set_timestamp(1_000_000_000); // ms
            System::set_block_number(1);
            GpuPowPallet::on_initialize(1); // seed window start

            // 500ms per block for blocks 2..=31 (avg 0.5s << target 15s).
            for b in 2..=31 {
                pallet_timestamp::Pallet::<Test>::set_timestamp(1_000_000_000 + (b as u64) * 500);
                System::set_block_number(b);
                GpuPowPallet::on_initialize(b);
            }
            let after = Difficulty::<Test>::get();
            assert!(
                GpuPowPallet::u256_lt(&after, &before),
                "sub-1s must harden, not freeze"
            );
        });
    }

    // ---- symmetric retarget (item #52) --------------------------------------
    //
    // Law: `new_target = old_target × clamp(actual/target, min_pct, max_pct)`.
    // What must hold:
    //   * inside the band the raw ratio is applied EXACTLY (no rounding shim)
    //   * exactly at a bound it passes through unchanged (`<` / `>`, not `<=`),
    //     so the controller is continuous at the boundary
    //   * outside the band the step is exactly the bound
    //   * the clamp is symmetric in log space: clamp(x) × clamp(1/x) == 1, so a
    //     too-fast window is undone by one equally-too-slow window (no ratchet)
    //   * min/max difficulty bounds still apply AFTER the step clamp
    //   * window boundaries: no adjustment before the window completes

    /// u256 → f64 exponent (log2) so tests can compare *magnitudes* without
    /// hand-computing 256-bit products.
    fn log2_of(v: U256) -> f64 {
        let limb3 = v[3] as f64;
        if limb3 > 0.0 {
            return 192.0 + limb3.log2();
        }
        for (i, limb) in v.iter().enumerate().rev() {
            if *limb > 0 {
                return (i as f64) * 64.0 + (*limb as f64).log2();
            }
        }
        f64::NEG_INFINITY
    }

    #[test]
    fn test_clamp_passes_through_on_time_ratio() {
        // x = 1 ⇒ hold the target.
        assert_eq!(
            GpuPowPallet::clamp_retarget_ratio(15_000, 15_000, 50, 200),
            (15_000, 15_000)
        );
    }

    #[test]
    fn test_clamp_preserves_ratio_inside_band() {
        // x = 1.2 and x = 0.8 are inside [0.5, 2.0] ⇒ untouched, not rescaled.
        assert_eq!(
            GpuPowPallet::clamp_retarget_ratio(18_000, 15_000, 50, 200),
            (18_000, 15_000)
        );
        assert_eq!(
            GpuPowPallet::clamp_retarget_ratio(12_000, 15_000, 50, 200),
            (12_000, 15_000)
        );
    }

    #[test]
    fn test_clamp_boundaries_are_exact_and_inclusive() {
        // Exactly 0.5× and exactly 2× must pass through as the SAME fraction
        // (not be rewritten to 50/100 or 200/100): the step is identical, but a
        // rewrite would mean the controller is discontinuous at the bound.
        assert_eq!(
            GpuPowPallet::clamp_retarget_ratio(7_500, 15_000, 50, 200),
            (7_500, 15_000)
        );
        assert_eq!(
            GpuPowPallet::clamp_retarget_ratio(30_000, 15_000, 50, 200),
            (30_000, 15_000)
        );
        // One part in 100_000 outside each bound is clamped.
        assert_eq!(
            GpuPowPallet::clamp_retarget_ratio(29_999, 15_000, 50, 200),
            (29_999, 15_000)
        );
        assert_eq!(
            GpuPowPallet::clamp_retarget_ratio(30_001, 15_000, 50, 200),
            (200, 100)
        );
        assert_eq!(
            GpuPowPallet::clamp_retarget_ratio(7_499, 15_000, 50, 200),
            (50, 100)
        );
    }

    #[test]
    fn test_clamp_beyond_both_bounds() {
        // Absurdly fast (x → 0) and absurdly slow (x = 100): exactly the bounds.
        assert_eq!(
            GpuPowPallet::clamp_retarget_ratio(1, 15_000, 50, 200),
            (50, 100)
        );
        assert_eq!(
            GpuPowPallet::clamp_retarget_ratio(1_500_000, 15_000, 50, 200),
            (200, 100)
        );
    }

    /// The whole point of the fix: the clamp must be symmetric in log space, so
    /// an error in one direction costs exactly one window in the other. The old
    /// `2x/(1+x)` law violated this (x=4 ⇒ +60 %, x=0.25 ⇒ −40 %), which is the
    /// ratchet that produced 86–96 s block tails (#52).
    #[test]
    fn test_clamp_is_log_symmetric_no_ratchet() {
        for x_pct in [1u64, 10, 25, 49, 51, 500, 4_000, 100_000] {
            let (n_hi, d_hi) = GpuPowPallet::clamp_retarget_ratio(x_pct, 100, 50, 200);
            // the reciprocal error: x' = 1/x
            let (n_lo, d_lo) = GpuPowPallet::clamp_retarget_ratio(100, x_pct.max(1), 50, 200);
            let hi = n_hi as f64 / d_hi as f64;
            let lo = n_lo as f64 / d_lo as f64;
            assert!(
                (hi * lo - 1.0).abs() < 1e-9,
                "x={}% : step up {:.4} × step down {:.4} = {:.4} (must be 1.0)",
                x_pct,
                hi,
                lo,
                hi * lo
            );
        }
    }

    #[test]
    fn test_clamp_misconfig_degrades_to_pin_not_division_by_zero() {
        // max < 100 is floored at 100 (never "harden when told to hold"), and
        // min > max is capped at max, so the result is always a valid fraction
        // with a non-zero denominator.
        let (n, d) = GpuPowPallet::clamp_retarget_ratio(300, 100, 10, 90);
        assert_eq!((n, d), (100, 100));
        let (n, d) = GpuPowPallet::clamp_retarget_ratio(300, 100, 400, 200);
        assert_eq!((n, d), (200, 100));
        let (n, d) = GpuPowPallet::clamp_retarget_ratio(0, 100, 0, 0);
        assert_eq!((n, d), (100, 100));
        // a zero denominator is inert, never a divide-by-zero
        assert_eq!(GpuPowPallet::clamp_retarget_ratio(7, 0, 50, 200), (7, 0));
    }

    #[test]
    fn test_adjust_difficulty_is_exactly_symmetric() {
        new_test_ext().execute_with(|| {
            let parent = [0, 0, 0, 0x1000_0000_0000_0000];
            // 60 s vs a 15 s target = 4× too slow ⇒ clamped at 2×.
            let slower = GpuPowPallet::adjust_difficulty(parent, 60_000);
            assert_eq!(slower, [0, 0, 0, 0x2000_0000_0000_0000], "must double");
            // 3.75 s = 4× too fast ⇒ clamped at 0.5× — the exact inverse.
            let faster = GpuPowPallet::adjust_difficulty(parent, 3_750);
            assert_eq!(faster, [0, 0, 0, 0x0800_0000_0000_0000], "must halve");
            // and one round trip through both returns to the parent exactly
            let round = GpuPowPallet::adjust_difficulty(slower, 3_750);
            assert_eq!(round, parent, "one 4x-fast window must undo one 4x-slow");
        });
    }

    #[test]
    fn test_adjust_difficulty_inside_band_proportional() {
        new_test_ext().execute_with(|| {
            let parent = [0, 0, 0, 0x1000_0000_0000_0000];
            // 30 s = 2× target ⇒ exactly 2× (the bound, not "2x/(1+x)" = 1.33)
            assert_eq!(
                GpuPowPallet::adjust_difficulty(parent, 30_000),
                [0, 0, 0, 0x2000_0000_0000_0000]
            );
            // 7.5 s = 0.5× target ⇒ exactly half
            assert_eq!(
                GpuPowPallet::adjust_difficulty(parent, 7_500),
                [0, 0, 0, 0x0800_0000_0000_0000]
            );
            // 22.5 s = 1.5× ⇒ 1.5× (interior of the band, no clamp)
            let l = log2_of(GpuPowPallet::adjust_difficulty(parent, 22_500)) - log2_of(parent);
            assert!((l - 1.5f64.log2()).abs() < 1e-9, "interior step not 1.5x");
        });
    }

    #[test]
    fn test_adjust_difficulty_step_never_exceeds_bounds() {
        new_test_ext().execute_with(|| {
            let parent = [0, 0, 0, 0x1000_0000_0000_0000];
            let before = log2_of(parent);
            for actual_ms in [1u64, 200, 1_000, 5_000, 14_000, 15_000, 16_000, 45_000,
                              90_000, 300_000, 3_600_000, u64::MAX / 2_000] {
                let after = GpuPowPallet::adjust_difficulty(parent, actual_ms);
                let factor = 2f64.powf(log2_of(after) - before);
                assert!(
                    factor >= 0.5 - 1e-9 && factor <= 2.0 + 1e-9,
                    "actual_ms={} moved the target {:.4}x (outside [0.5, 2.0])",
                    actual_ms, factor
                );
            }
        });
    }

    /// Recovery speed, the metric #52 is about: ONE too-fast window (10× fast,
    /// would be −90 % unclamped) must be undone by ONE equally-too-slow window.
    /// Under the old law this took three windows and never fully returned.
    #[test]
    fn test_recovery_is_one_window() {
        new_test_ext().execute_with(|| {
            let parent = [0, 0, 0, 0x1000_0000_0000_0000];
            let dipped = GpuPowPallet::adjust_difficulty(parent, 1_500); // 10x fast
            let back = GpuPowPallet::adjust_difficulty(dipped, 150_000); // 10x slow
            assert_eq!(back, parent, "did not fully recover in one window");
            // The old law's arithmetic, kept here as the regression witness:
            // 2*0.1/1.1 = 0.1818 then 2*10/11 = 1.818 ⇒ 0.33 of the original.
            let old_fast = 2.0 * 0.1 / 1.1;
            let old_slow = 2.0 * 10.0 / 11.0;
            assert!(
                old_fast * old_slow < 0.34,
                "old-law witness changed; re-derive before deleting this test"
            );
        });
    }

    #[test]
    fn test_adjust_difficulty_is_monotonic() {
        new_test_ext().execute_with(|| {
            let parent = [0, 0, 0, 0x1000_0000_0000_0000];
            let mut prev = 0.0f64;
            for actual_ms in [1_000u64, 5_000, 12_000, 15_000, 20_000, 40_000, 120_000] {
                let after = GpuPowPallet::adjust_difficulty(parent, actual_ms);
                let l = log2_of(after);
                assert!(l >= prev && l.is_finite(), "not monotonic at {}", actual_ms);
                prev = l;
            }
        });
    }

    #[test]
    fn test_adjust_difficulty_min_max_bounds_apply_after_clamp() {
        new_test_ext().execute_with(|| {
            // At MinDifficulty and 10x too fast: the step clamp would take us
            // BELOW the floor, so the floor wins.
            assert_eq!(
                GpuPowPallet::adjust_difficulty(TestMinDifficulty::get(), 1_500),
                TestMinDifficulty::get()
            );
            // At MaxDifficulty and 10x too slow: the ceiling wins.
            assert_eq!(
                GpuPowPallet::adjust_difficulty(TestMaxDifficulty::get(), 150_000),
                TestMaxDifficulty::get()
            );
        });
    }

    #[test]
    fn test_zero_actual_time_is_a_no_op() {
        new_test_ext().execute_with(|| {
            let parent = [0, 0, 0, 0x1000_0000_0000_0000];
            assert_eq!(GpuPowPallet::adjust_difficulty(parent, 0), parent);
        });
    }

    #[test]
    fn test_window_does_not_adjust_before_it_completes() {
        new_test_ext().execute_with(|| {
            Difficulty::<Test>::put([0, 0, 0, 0x1000_0000_0000]);
            let before = Difficulty::<Test>::get();
            pallet_timestamp::Pallet::<Test>::set_timestamp(1_000_000_000);
            System::set_block_number(1);
            GpuPowPallet::on_initialize(1); // seeds the window at block 1
            // window = 30 ⇒ the retarget fires at block 31, not before.
            for b in 2..=30 {
                pallet_timestamp::Pallet::<Test>::set_timestamp(1_000_000_000 + (b as u64) * 5_000);
                System::set_block_number(b);
                GpuPowPallet::on_initialize(b);
            }
            assert_eq!(Difficulty::<Test>::get(), before, "adjusted mid-window");
        });
    }

    /// One stalled block must not move the cadence. At window = 30 a single 90 s
    /// gap is one of 30 intervals: mean = (29×15 + 90)/30 = 17.5 s ⇒ +16.7 %.
    /// The old 10-block window made the same event +50 %.
    #[test]
    fn test_window_dilutes_a_single_stalled_block() {
        new_test_ext().execute_with(|| {
            Difficulty::<Test>::put([0, 0, 0, 0x8000_0000_0000]);
            let before = log2_of(Difficulty::<Test>::get());
            pallet_timestamp::Pallet::<Test>::set_timestamp(1_000_000_000);
            System::set_block_number(1);
            GpuPowPallet::on_initialize(1);
            let mut ts = 1_000_000_000u64;
            for b in 2..=31 {
                ts += if b == 31 { 90_000 } else { 15_000 };
                pallet_timestamp::Pallet::<Test>::set_timestamp(ts);
                System::set_block_number(b);
                GpuPowPallet::on_initialize(b);
            }
            let factor = 2f64.powf(log2_of(Difficulty::<Test>::get()) - before);
            assert!(
                (factor - 17.5 / 15.0).abs() < 0.01,
                "single stall moved the target {:.4}x, expected 1.1667x",
                factor
            );
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
            // Easiest chain target so a valid (but unground) hash still meets it;
            // `submit_solution` now anchors the check to the chain difficulty.
            Difficulty::<Test>::put(TestMaxDifficulty::get());
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

    /// Red-team finding 2026-10-09 (chain-consensus/difficulty): a PoW claim must
    /// be validated against the *chain's* difficulty, not the caller-supplied one.
    /// Before the fix an attested account could pass `meets_difficulty` with a
    /// trivial (easiest) target and be credited as having mined.
    #[test]
    fn test_submit_solution_rejects_weak_difficulty() {
        new_test_ext().execute_with(|| {
            let miner = 42u64;
            let sol = EthashSolution {
                nonce: 0,
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

            // Chain difficulty is the hardest test target. Claiming the easiest
            // target (weaker than the chain's) must be rejected outright...
            let weak = GpuPowPallet::submit_solution(
                RuntimeOrigin::signed(miner),
                valid_solution.clone(),
                TestMaxDifficulty::get(),
            );
            assert!(
                weak.is_err(),
                "a weaker-than-chain difficulty must be rejected"
            );
            assert_eq!(PendingBlockRewards::<Test>::get(), 0);

            // ...and even sitting on the chain target, the unground hash must fail
            // the real (chain-anchored) difficulty check.
            let unground = GpuPowPallet::submit_solution(
                RuntimeOrigin::signed(miner),
                valid_solution,
                TestMinDifficulty::get(),
            );
            assert!(
                unground.is_err(),
                "a hash above the chain target must be rejected"
            );
            assert_eq!(PendingBlockRewards::<Test>::get(), 0);
            assert!(PendingRewardBeneficiary::<Test>::get().is_none());
        });
    }

    #[test]
    fn test_reward_distribution() {
        // Bitcoin-style: the block PRODUCER (FindAuthor) is paid, gated on attestation.
        new_test_ext().execute_with(|| {
            let miner = 42u64;
            let initial_balance = pallet_balances::Pallet::<Test>::free_balance(miner);
            MOCK_AUTHOR.with(|c| c.set(Some(miner)));
            GpuPowPallet::distribute_block_rewards();
            let new_balance = pallet_balances::Pallet::<Test>::free_balance(miner);
            assert_eq!(new_balance, initial_balance + TestInitialReward::get());
        });
    }

    #[test]
    fn test_unattested_author_falls_back_to_beneficiary() {
        new_test_ext().execute_with(|| {
            // No author known → fall back to the configured beneficiary.
            let ben = 7u64;
            BlockRewardBeneficiary::<Test>::put(ben);
            MOCK_AUTHOR.with(|c| c.set(None));
            let before = pallet_balances::Pallet::<Test>::free_balance(ben);
            GpuPowPallet::distribute_block_rewards();
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(ben),
                before + TestInitialReward::get()
            );
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
            MOCK_AUTHOR.with(|c| c.set(Some(miner)));
            GpuPowPallet::distribute_block_rewards();
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
            // Anchor a valid submission to an easy chain target (see
            // `submit_solution`'s chain-difficulty check).
            Difficulty::<Test>::put(TestMaxDifficulty::get());
            GpuPowPallet::submit_solution(
                RuntimeOrigin::signed(miner),
                valid,
                TestMaxDifficulty::get(),
            )
            .unwrap();
            // Distribute should silently skip due to supply cap
            MOCK_AUTHOR.with(|c| c.set(Some(miner)));
            GpuPowPallet::distribute_block_rewards();
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

    /// `set_block_reward` is Root-only and mutates the emission knob
    /// (`CurrentBlockReward`) without touching the genesis.
    #[test]
    fn test_set_block_reward_requires_root() {
        new_test_ext().execute_with(|| {
            let before = CurrentBlockReward::<Test>::get();
            assert!(
                GpuPowPallet::set_block_reward(RuntimeOrigin::signed(1), 12_000_000_000_000)
                    .is_err()
            );
            assert_eq!(CurrentBlockReward::<Test>::get(), before);
            assert!(
                GpuPowPallet::set_block_reward(RuntimeOrigin::root(), 12_000_000_000_000).is_ok()
            );
            assert_eq!(CurrentBlockReward::<Test>::get(), 12_000_000_000_000);
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

    /// Bitcoin-style: the block PRODUCER (author) is paid; the configured
    /// beneficiary is NOT (no double-pay).
    #[test]
    fn test_author_paid_over_beneficiary() {
        new_test_ext().execute_with(|| {
            let beneficiary = 7u64;
            let author = 42u64;
            GpuPowPallet::set_block_beneficiary(RuntimeOrigin::root(), beneficiary).unwrap();
            MOCK_AUTHOR.with(|c| c.set(Some(author)));
            System::set_block_number(2);
            GpuPowPallet::on_initialize(2);

            let reward = TestInitialReward::get();
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(author),
                reward,
                "block author paid exactly once"
            );
            assert_eq!(
                pallet_balances::Pallet::<Test>::free_balance(beneficiary),
                0,
                "beneficiary must not also be paid"
            );
            assert_eq!(MockSupply::total_issued(), reward);
        });
    }

    /// Red-team finding 2026-10-09 (chain-consensus / monetary policy): the
    /// block-reward halving never fired. `on_initialize` only halved when
    /// `LastHalvingBlock != 0`, but that field is written *only* by the halving
    /// itself (`genesis_build` seeds it to zero) — so the branch was unreachable
    /// and the reward stayed at the initial value forever, over-minting until the
    /// supply cap (~2.1x the documented `2 x reward x HalvingInterval` course).
    #[test]
    fn test_block_reward_halving_fires_at_interval() {
        new_test_ext().execute_with(|| {
            let interval = TestHalvingInterval::get(); // 100
            let r0 = CurrentBlockReward::<Test>::get();

            pallet_timestamp::Pallet::<Test>::set_timestamp(1_000_000_000);
            System::set_block_number(1);
            GpuPowPallet::on_initialize(1);

            // Not before the interval.
            System::set_block_number(interval - 1);
            GpuPowPallet::on_initialize(interval - 1);
            assert_eq!(
                CurrentBlockReward::<Test>::get(),
                r0,
                "no halving before the interval elapses"
            );

            // Exactly at the interval the reward halves once and the epoch ticks.
            System::set_block_number(interval);
            GpuPowPallet::on_initialize(interval);
            assert_eq!(
                CurrentBlockReward::<Test>::get(),
                r0 / 2,
                "reward must halve at the interval"
            );
            assert_eq!(CurrentEpoch::<Test>::get(), 1);

            // And only once per interval.
            System::set_block_number(interval + 1);
            GpuPowPallet::on_initialize(interval + 1);
            assert_eq!(CurrentBlockReward::<Test>::get(), r0 / 2);

            // Second halving at 2x the interval.
            System::set_block_number(2 * interval);
            GpuPowPallet::on_initialize(2 * interval);
            assert_eq!(CurrentBlockReward::<Test>::get(), r0 / 4);
            assert_eq!(CurrentEpoch::<Test>::get(), 2);
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
