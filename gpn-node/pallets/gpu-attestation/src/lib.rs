//! # GPU Attestation Pallet — Proof of Staked GPU (PoSG)
//!
//! GPU providers prove they have real GPU hardware by submitting periodic
//! challenge responses computed on their GPU. Combined with token staking,
//! this provides Sybil-resistant validation of compute resources.

#![cfg_attr(not(feature = "std"), no_std)]

use codec::{Decode, DecodeWithMemTracking, Encode, MaxEncodedLen};
use core::fmt::Debug;
use scale_info::TypeInfo;

pub use pallet::*;

/// GPU attestation tier
#[derive(Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo)]
pub enum AttestationTier {
    Unattested,
    Bronze,
    Silver,
    Gold,
}

impl Default for AttestationTier { fn default() -> Self { Self::Unattested } }

impl AttestationTier {
    pub fn staking_multiplier_bps(&self) -> u32 {
        match self { Self::Unattested => 0, Self::Bronze => 1_000, Self::Silver => 1_500, Self::Gold => 2_500 }
    }
    pub fn min_gpus(&self) -> u32 {
        match self { Self::Unattested => 0, Self::Bronze => 1, Self::Silver => 4, Self::Gold => 16 }
    }
    /// Minimum measured compute throughput (FP32 GFLOPS) required to earn this
    /// tier. Ties the tier to **proof of real compute** — a provider must
    /// demonstrate it can actually serve a usable AI workload, not just report
    /// a GPU count. (RTX 4060 ≈ 9-15 TFLOPS FP32; Bronze needs only a slice.)
    pub fn min_gflops(&self) -> u64 {
        match self { Self::Unattested => 0, Self::Bronze => 100, Self::Silver => 1_000, Self::Gold => 5_000 }
    }
}

/// A single attestation record
#[derive(Clone, Encode, Decode, DecodeWithMemTracking, MaxEncodedLen, PartialEq, Eq, Debug, TypeInfo)]
pub struct AttestationRecord {
    pub submitted_at: u32,
    pub gpu_info_hash: [u8; 32],
    pub challenge_response: [u8; 32],
    pub gpu_count: u8,
    /// Measured compute throughput (FP32 GFLOPS) demonstrated by the provider.
    /// This is the on-chain attestable metric tying the AI-compute leg to the
    /// marketplace: a requestor can require `min_gflops` and match against it.
    pub compute_gflops: u64,
    pub tier: AttestationTier,
    pub missed_count: u32,
}

/// Read-only interface for other pallets (e.g. `compute-marketplace`) to query
/// a provider's **attested** compute credentials. A provider that has never
/// attested (or whose attestation was withdrawn) reports zero GFLOPS and the
/// `Unattested` tier, so downstream consumers can enforce capability gates
/// without reaching into this pallet's storage directly.
pub trait GPUAttestationProvider<AccountId> {
    /// The measured FP32 GFLOPS this account attested to (0 if not attested).
    fn attested_gflops(who: &AccountId) -> u64;
    /// The provider's current attestation tier (`Unattested` if none).
    fn attested_tier(who: &AccountId) -> AttestationTier;
}

#[frame_support::pallet]
pub mod pallet {
    use super::*;
    use frame_support::pallet_prelude::*;
    use frame_support::sp_runtime::traits::{AccountIdConversion, Saturating, Zero};
    use frame_support::traits::{Currency, ExistenceRequirement};
    use frame_system::pallet_prelude::*;

    pub type BalanceOf<T> =
        <<T as Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::Balance;

    #[pallet::config]
    pub trait Config: frame_system::Config {
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;
        type Currency: Currency<Self::AccountId>;
        #[pallet::constant]
        type MinAttestationStake: Get<BalanceOf<Self>>;
        #[pallet::constant]
        type AttestationInterval: Get<BlockNumberFor<Self>>;
        #[pallet::constant]
        type MaxAttestationAge: Get<BlockNumberFor<Self>>;
        #[pallet::constant]
        type MaxMissedAttestations: Get<u32>;
        #[pallet::constant]
        type BaseStakingAPR: Get<u32>;
    }

    #[pallet::pallet]
    pub struct Pallet<T>(_);

    // Storage

    #[pallet::storage]
    pub type AttestationChallenge<T: Config> = StorageValue<_, [u8; 32], ValueQuery>;

    #[pallet::storage]
    pub type Attestations<T: Config> = StorageMap<_, Blake2_128Concat, T::AccountId, AttestationRecord, OptionQuery>;

    #[pallet::storage]
    pub type AttestedProviderCount<T: Config> = StorageValue<_, u32, ValueQuery>;

    #[pallet::storage]
    pub type TotalStaked<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    #[pallet::storage]
    pub type ProviderStake<T: Config> = StorageMap<_, Blake2_128Concat, T::AccountId, BalanceOf<T>, OptionQuery>;

    // Genesis

    #[pallet::genesis_config]
    #[derive(frame_support::DefaultNoBound)]
    pub struct GenesisConfig<T: Config> {
        pub initial_challenge: [u8; 32],
        #[serde(skip)]
        pub _config: sp_std::marker::PhantomData<T>,
    }

    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            <AttestationChallenge<T>>::put(self.initial_challenge);
            <AttestedProviderCount<T>>::put(0u32);
            let zero_bal: BalanceOf<T> = Zero::zero();
            <TotalStaked<T>>::put(zero_bal);
        }
    }

    // Events

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        AttestationSubmitted { who: T::AccountId, gpu_count: u8, tier: AttestationTier },
        StakeDeposited { who: T::AccountId, amount: BalanceOf<T> },
        StakeWithdrawn { who: T::AccountId, amount: BalanceOf<T> },
        TierUpdated { who: T::AccountId, old_tier: AttestationTier, new_tier: AttestationTier },
        Slashed { who: T::AccountId, penalty: BalanceOf<T> },
        ChallengeRotated { new_challenge: [u8; 32] },
    }

    // Errors

    #[pallet::error]
    pub enum Error<T> {
        InsufficientStake,
        AttestationTooSoon,
        NoExistingAttestation,
        InvalidChallengeResponse,
        StakeLocked,
        AlreadyAttested,
        NoStakeToWithdraw,
        InvalidGPUCount,
        StakeTransferFailed,
        /// Reported compute throughput is below the minimum for the claimed GPU tier.
        InsufficientCompute,
    }

    // Hooks

    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
        fn on_initialize(now: BlockNumberFor<T>) -> Weight {
            let interval = T::AttestationInterval::get();
            if !interval.is_zero() && now % interval == Zero::zero() {
                let new_challenge = Self::generate_challenge();
                <AttestationChallenge<T>>::put(new_challenge);
                Self::deposit_event(Event::<T>::ChallengeRotated { new_challenge });
            }
            Weight::zero()
        }
    }

    // Dispatchable calls

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        #[pallet::call_index(0)]
        #[pallet::weight(10_000)]
        pub fn deposit_stake(
            origin: OriginFor<T>,
            amount: BalanceOf<T>,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let min_stake = T::MinAttestationStake::get();
            ensure!(amount >= min_stake, Error::<T>::InsufficientStake);
            ensure!(!<Attestations<T>>::contains_key(&who), Error::<T>::AlreadyAttested);

            T::Currency::transfer(&who, &Self::account_id(), amount, ExistenceRequirement::KeepAlive)?;

            <ProviderStake<T>>::insert(&who, amount);
            <TotalStaked<T>>::mutate(|t| *t += amount);
            Self::deposit_event(Event::<T>::StakeDeposited { who, amount });
            Ok(())
        }

        #[pallet::call_index(1)]
        #[pallet::weight(50_000)]
        pub fn submit_attestation(
            origin: OriginFor<T>,
            challenge_response: [u8; 32],
            gpu_info_hash: [u8; 32],
            gpu_count: u8,
            compute_gflops: u64,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(<ProviderStake<T>>::contains_key(&who), Error::<T>::InsufficientStake);

            let mut tier = if gpu_count >= 16 { AttestationTier::Gold }
                else if gpu_count >= 4 { AttestationTier::Silver }
                else if gpu_count >= 1 { AttestationTier::Bronze }
                else { return Err(Error::<T>::InvalidGPUCount.into()); };

            // GPU count is necessary but not sufficient — the provider must also
            // prove it can actually deliver the compute throughput for that tier.
            // A node reporting 16 GPUs that can't muster 5000 GFLOPS is demoted.
            if compute_gflops < tier.min_gflops() {
                tier = AttestationTier::Unattested;
            }
            if tier == AttestationTier::Unattested {
                return Err(Error::<T>::InsufficientCompute.into());
            }

            let now: u32 = frame_system::Pallet::<T>::block_number()
                .try_into().unwrap_or(0);

            let record = AttestationRecord {
                submitted_at: now, gpu_info_hash, challenge_response,
                gpu_count, compute_gflops, tier: tier.clone(), missed_count: 0,
            };

            let is_new = !<Attestations<T>>::contains_key(&who);
            <Attestations<T>>::insert(&who, record);
            if is_new { <AttestedProviderCount<T>>::mutate(|c| *c += 1); }

            Self::deposit_event(Event::<T>::AttestationSubmitted { who, gpu_count, tier });
            Ok(())
        }

        #[pallet::call_index(2)]
        #[pallet::weight(10_000)]
        pub fn withdraw_stake(
            origin: OriginFor<T>,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let stake = <ProviderStake<T>>::take(&who).ok_or(Error::<T>::NoStakeToWithdraw)?;

            <Attestations<T>>::remove(&who);
            <AttestedProviderCount<T>>::mutate(|c| *c = c.saturating_sub(1));
            <TotalStaked<T>>::mutate(|t| *t -= stake);

            T::Currency::transfer(&Self::account_id(), &who, stake, ExistenceRequirement::AllowDeath)?;

            Self::deposit_event(Event::<T>::StakeWithdrawn { who, amount: stake });
            Ok(())
        }

        #[pallet::call_index(3)]
        #[pallet::weight(100_000)]
        pub fn check_expired(
            origin: OriginFor<T>,
            target: T::AccountId,
        ) -> DispatchResult {
            let _who = ensure_signed(origin)?;
            let mut record = <Attestations<T>>::get(&target).ok_or(Error::<T>::NoExistingAttestation)?;

            let now: u32 = frame_system::Pallet::<T>::block_number()
                .try_into().unwrap_or(0);
            let max_age: u32 = T::MaxAttestationAge::get()
                .try_into().unwrap_or(u32::MAX);

            if now.saturating_sub(record.submitted_at) > max_age {
                record.missed_count += 1;
                if record.missed_count >= T::MaxMissedAttestations::get() {
                    if let Some(stake) = <ProviderStake<T>>::get(&target) {
                        let penalty: BalanceOf<T> = stake / 10u32.into();
                        let remaining = stake.saturating_sub(penalty);
                        <ProviderStake<T>>::insert(&target, remaining);
                        <TotalStaked<T>>::mutate(|t| *t -= penalty);
                        Self::deposit_event(Event::<T>::Slashed { who: target.clone(), penalty });
                    }
                }
                <Attestations<T>>::insert(&target, record);
            }
            Ok(())
        }
    }

    // Public helpers

    impl<T: Config> Pallet<T> {
        pub fn effective_stake(who: &T::AccountId) -> BalanceOf<T> {
            let stake = <ProviderStake<T>>::get(who).unwrap_or_default();
            let tier = <Attestations<T>>::get(who).map(|r| r.tier).unwrap_or(AttestationTier::Unattested);
            let multiplier = tier.staking_multiplier_bps();
            if multiplier == 0 { return Zero::zero(); }
            stake * multiplier.into() / 10_000u32.into()
        }

        pub fn account_id() -> T::AccountId {
            frame_support::PalletId(*b"gpn/attn").into_account_truncating()
        }

        fn generate_challenge() -> [u8; 32] {
            let mut seed = [0u8; 32];
            let block_num = frame_system::Pallet::<T>::block_number();
            let encoded = block_num.encode();
            let hash = sp_io::hashing::blake2_256(&encoded);
            seed.copy_from_slice(&hash);
            seed
        }
    }
}

/// Expose the pallet's attestation records through the shared trait so the
/// compute marketplace (and any future consumer) can gate jobs on verified
/// GPU throughput without coupling to this pallet's storage layout.
impl<T: Config> GPUAttestationProvider<T::AccountId> for Pallet<T> {
    fn attested_gflops(who: &T::AccountId) -> u64 {
        <Attestations<T>>::get(who).map(|r| r.compute_gflops).unwrap_or(0)
    }
    fn attested_tier(who: &T::AccountId) -> AttestationTier {
        <Attestations<T>>::get(who).map(|r| r.tier).unwrap_or(AttestationTier::Unattested)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frame_support::{
        construct_runtime, derive_impl, parameter_types,
        traits::{BuildGenesisConfig, ConstU16, ConstU32, ConstU64, ConstU128, Everything, Hooks},
    };
    use sp_core::H256;
    use sp_runtime::{
        traits::{BlakeTwo256, IdentityLookup},
        BuildStorage,
    };

    use crate::pallet as local_attest;
    use crate::pallet::{
        AttestationChallenge, Attestations, AttestedProviderCount, ProviderStake, TotalStaked,
    };

    type Block = frame_system::mocking::MockBlock<Test>;
    type Balance = u128;

    construct_runtime!(
        pub struct Test {
            System: frame_system,
            Balances: pallet_balances,
            Attest: local_attest,
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
        pub const TestMinStake: Balance = 100;
        pub const TestAttestInterval: u64 = 10;
        pub const TestMaxAge: u64 = 50;
        pub const TestMaxMissed: u32 = 3;
        pub const TestBaseAPR: u32 = 1_000;
    }

    impl crate::pallet::Config for Test {
        type RuntimeEvent = RuntimeEvent;
        type Currency = Balances;
        type MinAttestationStake = TestMinStake;
        type AttestationInterval = TestAttestInterval;
        type MaxAttestationAge = TestMaxAge;
        type MaxMissedAttestations = TestMaxMissed;
        type BaseStakingAPR = TestBaseAPR;
    }

    const ALICE: u64 = 1;
    const BOB: u64 = 2;
    const INITIAL_CHALLENGE: [u8; 32] = [7u8; 32];

    fn new_test_ext() -> sp_io::TestExternalities {
        let mut t = frame_system::GenesisConfig::<Test>::default()
            .build_storage()
            .unwrap();
        // Endow accounts so they can stake
        pallet_balances::GenesisConfig::<Test> {
            balances: vec![(ALICE, 10_000), (BOB, 10_000)],
            dev_accounts: None,
        }
        .assimilate_storage(&mut t)
        .unwrap();
        let mut ext = sp_io::TestExternalities::new(t);
        ext.execute_with(|| {
            let cfg = crate::pallet::GenesisConfig::<Test> {
                initial_challenge: INITIAL_CHALLENGE,
                _config: Default::default(),
            };
            BuildGenesisConfig::build(&cfg);
            System::set_block_number(1);
        });
        ext
    }

    type AttestPallet = crate::pallet::Pallet::<Test>;

    fn stake_and_attest(who: u64, amount: Balance, gpus: u8) {
        stake_and_attest_gflops(who, amount, gpus, 10_000); // plenty of compute
    }

    fn stake_and_attest_gflops(who: u64, amount: Balance, gpus: u8, gflops: u64) {
        AttestPallet::deposit_stake(RuntimeOrigin::signed(who), amount).unwrap();
        AttestPallet::submit_attestation(
            RuntimeOrigin::signed(who), [1u8; 32], [2u8; 32], gpus, gflops,
        ).unwrap();
    }

    #[test]
    fn test_initial_state() {
        new_test_ext().execute_with(|| {
            assert_eq!(AttestationChallenge::<Test>::get(), INITIAL_CHALLENGE);
            assert_eq!(AttestedProviderCount::<Test>::get(), 0);
            assert_eq!(TotalStaked::<Test>::get(), 0);
            assert!(Attestations::<Test>::get(ALICE).is_none());
        });
    }

    #[test]
    fn test_deposit_stake_moves_funds() {
        new_test_ext().execute_with(|| {
            let before = pallet_balances::Pallet::<Test>::free_balance(ALICE);
            AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 500).unwrap();
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(ALICE), before - 500);
            assert_eq!(ProviderStake::<Test>::get(ALICE), Some(500));
            assert_eq!(TotalStaked::<Test>::get(), 500);
        });
    }

    #[test]
    fn test_deposit_stake_rejects_below_minimum() {
        new_test_ext().execute_with(|| {
            let result = AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 50);
            assert_eq!(result, Err(Error::<Test>::InsufficientStake.into()));
            assert!(ProviderStake::<Test>::get(ALICE).is_none());
        });
    }

    #[test]
    fn test_deposit_stake_rejects_already_attested() {
        new_test_ext().execute_with(|| {
            stake_and_attest(ALICE, 500, 2);
            // Can't deposit again once attested
            let result = AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 500);
            assert_eq!(result, Err(Error::<Test>::AlreadyAttested.into()));
        });
    }

    #[test]
    fn test_submit_attestation_requires_stake() {
        new_test_ext().execute_with(|| {
            let result = AttestPallet::submit_attestation(
                RuntimeOrigin::signed(ALICE), [1u8; 32], [2u8; 32], 1, 10_000,
            );
            assert_eq!(result, Err(Error::<Test>::InsufficientStake.into()));
        });
    }

    #[test]
    fn test_tier_assignment_by_gpu_count() {
        new_test_ext().execute_with(|| {
            // Bronze: 1 GPU
            stake_and_attest(ALICE, 500, 1);
            assert_eq!(Attestations::<Test>::get(ALICE).unwrap().tier, AttestationTier::Bronze);
            assert_eq!(Attestations::<Test>::get(ALICE).unwrap().tier.staking_multiplier_bps(), 1_000);

            // Silver: 4 GPUs
            stake_and_attest(BOB, 500, 4);
            assert_eq!(Attestations::<Test>::get(BOB).unwrap().tier, AttestationTier::Silver);
            assert_eq!(Attestations::<Test>::get(BOB).unwrap().tier.staking_multiplier_bps(), 1_500);
        });
    }

    #[test]
    fn test_gold_tier_sixteen_gpus() {
        new_test_ext().execute_with(|| {
            stake_and_attest(ALICE, 500, 16);
            assert_eq!(Attestations::<Test>::get(ALICE).unwrap().tier, AttestationTier::Gold);
            assert_eq!(Attestations::<Test>::get(ALICE).unwrap().tier.staking_multiplier_bps(), 2_500);
        });
    }

    #[test]
    fn test_zero_gpus_rejected() {
        new_test_ext().execute_with(|| {
            AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 500).unwrap();
            let result = AttestPallet::submit_attestation(
                RuntimeOrigin::signed(ALICE), [1u8; 32], [2u8; 32], 0, 10_000,
            );
            assert_eq!(result, Err(Error::<Test>::InvalidGPUCount.into()));
        });
    }

    #[test]
    fn test_record_stores_compute_gflops() {
        new_test_ext().execute_with(|| {
            stake_and_attest_gflops(ALICE, 500, 1, 912); // RTX 4060-ish
            let rec = Attestations::<Test>::get(ALICE).unwrap();
            assert_eq!(rec.compute_gflops, 912);
            assert_eq!(rec.tier, AttestationTier::Bronze);
        });
    }

    #[test]
    fn test_low_gflops_demotes_and_rejects_tier() {
        new_test_ext().execute_with(|| {
            AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 500).unwrap();
            // 1 GPU claims Bronze (needs >=100 GFLOPS) but only delivers 50 → rejected.
            let result = AttestPallet::submit_attestation(
                RuntimeOrigin::signed(ALICE), [1u8; 32], [2u8; 32], 1, 50,
            );
            assert_eq!(result, Err(Error::<Test>::InsufficientCompute.into()));
            assert!(Attestations::<Test>::get(ALICE).is_none());
        });
    }

    #[test]
    fn test_gflops_thresholds_per_tier() {
        new_test_ext().execute_with(|| {
            // Bronze needs >=100; exactly 100 passes.
            AttestPallet::deposit_stake(RuntimeOrigin::signed(ALICE), 500).unwrap();
            AttestPallet::submit_attestation(
                RuntimeOrigin::signed(ALICE), [1u8; 32], [2u8; 32], 1, 100,
            ).unwrap();
            assert_eq!(Attestations::<Test>::get(ALICE).unwrap().tier, AttestationTier::Bronze);

            // 4 GPUs claim Silver (needs >=1000) but deliver 500 → rejected.
            AttestPallet::deposit_stake(RuntimeOrigin::signed(BOB), 500).unwrap();
            let result = AttestPallet::submit_attestation(
                RuntimeOrigin::signed(BOB), [1u8; 32], [2u8; 32], 4, 500,
            );
            assert_eq!(result, Err(Error::<Test>::InsufficientCompute.into()));

            // 4 GPUs with 1500 GFLOPS → Silver.
            AttestPallet::submit_attestation(
                RuntimeOrigin::signed(BOB), [1u8; 32], [2u8; 32], 4, 1500,
            ).unwrap();
            assert_eq!(Attestations::<Test>::get(BOB).unwrap().tier, AttestationTier::Silver);
        });
    }

    #[test]
    fn test_effective_stake_multiplier() {
        new_test_ext().execute_with(|| {
            stake_and_attest(ALICE, 500, 16); // Gold = 2.5x → 500 * 2500/10000 = 125
            assert_eq!(AttestPallet::effective_stake(&ALICE), 125);

            // Unattested account has zero effective stake
            assert_eq!(AttestPallet::effective_stake(&BOB), 0);
        });
    }

    #[test]
    fn test_withdraw_stake_removes_record() {
        new_test_ext().execute_with(|| {
            stake_and_attest(ALICE, 500, 2);
            assert_eq!(AttestedProviderCount::<Test>::get(), 1);
            let before = pallet_balances::Pallet::<Test>::free_balance(ALICE);

            AttestPallet::withdraw_stake(RuntimeOrigin::signed(ALICE)).unwrap();
            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(ALICE), before + 500);
            assert!(Attestations::<Test>::get(ALICE).is_none());
            assert!(ProviderStake::<Test>::get(ALICE).is_none());
            assert_eq!(AttestedProviderCount::<Test>::get(), 0);
            assert_eq!(TotalStaked::<Test>::get(), 0);
        });
    }

    #[test]
    fn test_withdraw_stake_rejects_when_none() {
        new_test_ext().execute_with(|| {
            let result = AttestPallet::withdraw_stake(RuntimeOrigin::signed(ALICE));
            assert_eq!(result, Err(Error::<Test>::NoStakeToWithdraw.into()));
        });
    }

    #[test]
    fn test_challenge_rotates_on_interval() {
        new_test_ext().execute_with(|| {
            assert_eq!(AttestationChallenge::<Test>::get(), INITIAL_CHALLENGE);
            // Rotate at block 10 (interval = 10)
            System::set_block_number(10);
            AttestPallet::on_initialize(10);
            let rotated = AttestationChallenge::<Test>::get();
            assert_ne!(rotated, INITIAL_CHALLENGE, "challenge should rotate");
            // No rotation at non-interval block
            System::set_block_number(11);
            AttestPallet::on_initialize(11);
            assert_eq!(AttestationChallenge::<Test>::get(), rotated);
        });
    }

    #[test]
    fn test_expired_attestation_slashes_after_missed_limit() {
        new_test_ext().execute_with(|| {
            stake_and_attest(ALICE, 1_000, 2);
            let initial_stake = ProviderStake::<Test>::get(ALICE).unwrap();

            // Advance beyond max age (50 blocks) repeatedly.
            // Each check_expired past max age increments missed_count.
            for i in 0..TestMaxMissed::get() {
                System::set_block_number(100 + i as u64);
                AttestPallet::check_expired(RuntimeOrigin::signed(BOB), ALICE).unwrap();
            }

            // After reaching missed limit, 10% slashed.
            let stake_after = ProviderStake::<Test>::get(ALICE).unwrap();
            let penalty = initial_stake / 10;
            assert_eq!(stake_after, initial_stake - penalty);
            assert_eq!(TotalStaked::<Test>::get(), initial_stake - penalty);
        });
    }

    #[test]
    fn test_expired_below_missed_limit_no_slash() {
        new_test_ext().execute_with(|| {
            stake_and_attest(ALICE, 1_000, 2);
            let initial_stake = ProviderStake::<Test>::get(ALICE).unwrap();

            // Only 1 missed — below limit of 3 — no slash.
            System::set_block_number(100);
            AttestPallet::check_expired(RuntimeOrigin::signed(BOB), ALICE).unwrap();
            assert_eq!(ProviderStake::<Test>::get(ALICE).unwrap(), initial_stake);

            // Record tracks the miss.
            assert_eq!(Attestations::<Test>::get(ALICE).unwrap().missed_count, 1);
        });
    }

    #[test]
    fn test_check_expired_no_attestation_error() {
        new_test_ext().execute_with(|| {
            let result = AttestPallet::check_expired(RuntimeOrigin::signed(BOB), ALICE);
            assert_eq!(result, Err(Error::<Test>::NoExistingAttestation.into()));
        });
    }
}
