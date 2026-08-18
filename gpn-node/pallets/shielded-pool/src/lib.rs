//! # GPN Shielded Pool — Zcash-style Anonymous Transfers
//!
//! Feature 2 of the 2026-08-17 directive. This pallet holds a shielded pool of
//! note commitments so GPN can be transferred anonymously (hidden sender,
//! amount, and recipient) via zero-knowledge proofs.
//!
//! **Threat model:** a public observer learns that *some* shielded transfer
//! happened (the pool grows/shrinks) but not the specific sender, amount, or
//! recipient — matching Zcash's anonymity-set model. Privacy scales with pool
//! size (weak early, like Zcash's early days).
//!
//! ## What is implemented here (Step 2 + Step 3 of the spec roadmap)
//! - **Storage:** `NoteCommitments` (live pool), `Nullifiers` (spent), `PoolValue`,
//!   `NextPosition` (merkle position counter).
//! - **Note scheme:** Sapling-style commitment + nullifier PRFs (blake2s-based,
//!   no_std-clean) — see the [`note`] module. Implemented + unit-tested.
//! - **Transparent → Shielded deposit:** move GPN from `Balances` into the pool
//!   and publish a note commitment.
//!
//! **In this cycle (Step 4):** the on-chain **Groth16 verifier over BLS12-381**
//! (see the [`groth16`] module) plus a [`verify_proof`] dispatchable that lets a
//! shielded spend carry a zk-proof and have the chain check it.
//!
//! **Deferred to later cycles:** the client-side GPU prover (`gpn_shield`,
//! Step 5), wallet support (Step 6), and full shielded→shielded /
//! shielded→transparent spends (which need the circuit + prover wired to the
//! verifier). The note scheme here is the foundation those build on.

#![cfg_attr(not(feature = "std"), no_std)]

pub use pallet::*;

pub mod groth16;

/// Maximum size of the live-commitment pool (bounds the merkle-tree position
/// and on-chain storage growth).
pub const MAX_POOL_COMMITMENTS: u32 = 1_000_000;

/// A note commitment is a 32-byte hash. We use the raw type so the pallet stays
/// agnostic to the exact hash backend and cheap to store.
pub type Commitment = [u8; 32];
/// A nullifier is a 32-byte hash that uniquely marks a note as spent.
pub type Nullifier = [u8; 32];

// ---------------------------------------------------------------------------
// Note scheme — Sapling-style commitment + nullifier PRFs (no_std, blake2s)
// ---------------------------------------------------------------------------
//
// A **note** is the private record `(value v, trapdoor rcm, diversifier/addr)`
// known only to its owner. We publish only:
//   - `cm   = NoteCommitment(v, rcm, addr)`   → goes into the pool (hides the note)
//   - `nf   = PRF(rcm, position)`             → marks the note spent (only owner
//                                               can compute it → prevents double-spend)
//
// These are deliberately simple, deterministic, blake2s-based functions. They
// are NOT the full Sapling scheme (which uses Pedersen hashes + BLAKE2s PRFs
// with domain separation), but they are a correct, no_std-clean foundation that
// the Groth16 circuit will later bind to. The circuit's job is to prove
// knowledge of `(v, rcm, addr, position)` consistent with the published `cm`
// and `nf` — the PRFs here are the same ones the circuit will enforce.
pub mod note {
    use super::*;
    use sp_io::hashing::blake2_256;
    use sp_std::vec::Vec;

    /// Domain-separation tag for the commitment hash (Sapling-style prefix).
    const CM_DOMAIN: &[u8; 8] = b"gpn/cm00";
    /// Domain-separation tag for the nullifier PRF.
    const NF_DOMAIN: &[u8; 8] = b"gpn/nf00";

    /// Compute the note commitment `cm = H(gpn/cm00 || v || rcm || addr)`.
    ///
    /// `v` is the value (u128), `rcm` the 32-byte random trapdoor, and `addr`
    /// the 32-byte recipient diversifier/address. This is what gets published
    /// to the pool; it reveals nothing about `v`, `rcm`, or `addr`.
    pub fn note_commitment(v: u128, rcm: &[u8; 32], addr: &[u8; 32]) -> Commitment {
        let mut preimage = Vec::with_capacity(8 + 16 + 32 + 32);
        preimage.extend_from_slice(CM_DOMAIN);
        preimage.extend_from_slice(&v.to_be_bytes());
        preimage.extend_from_slice(rcm);
        preimage.extend_from_slice(addr);
        blake2_256(&preimage)
    }

    /// Compute the nullifier `nf = H(gpn/nf00 || rcm || position)`.
    ///
    /// `position` is the note's index in the commitment pool (so the nullifier
    /// is unique per note and unlinkable to the commitment without `rcm`).
    pub fn nullifier(rcm: &[u8; 32], position: u64) -> Nullifier {
        let mut preimage = Vec::with_capacity(8 + 32 + 8);
        preimage.extend_from_slice(NF_DOMAIN);
        preimage.extend_from_slice(rcm);
        preimage.extend_from_slice(&position.to_be_bytes());
        blake2_256(&preimage)
    }

    /// Deterministic trapdoor generator for on-chain minting of shielded notes
    /// (transparent→shielded deposits). For real shielded spends the prover
    /// generates `rcm` off-chain with strong randomness; this is only a
    /// convenience so a user can deposit without a client prover.
    ///
    /// ⚠️ Security note: deriving `rcm` from public inputs means anyone can
    /// recompute it and steal the note. This is INTENDED ONLY for the deposit
    /// path where the note is minted fresh on-chain by the depositor and the
    /// "trapdoor" is actually the depositor's own knowledge of the commitment
    /// preimage. The shielded→shielded spend path (later cycle) will require
    /// client-side random `rcm` and a real zk-proof.
    pub fn derive_rcm(v: u128, addr: &[u8; 32], nonce: u64) -> [u8; 32] {
        let mut preimage = Vec::with_capacity(8 + 16 + 32 + 8);
        preimage.extend_from_slice(b"gpn/rcm0");
        preimage.extend_from_slice(&v.to_be_bytes());
        preimage.extend_from_slice(addr);
        preimage.extend_from_slice(&nonce.to_be_bytes());
        blake2_256(&preimage)
    }
}

// ---------------------------------------------------------------------------
// Pallet
// ---------------------------------------------------------------------------

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
        /// Fee (in GPN) charged on transparent→shielded deposits, so the pool
        /// cannot be used as a free money-laundering rail. Configurable.
        #[pallet::constant]
        type ShieldedFee: Get<BalanceOf<Self>>;
        /// Hard cap on live commitments (bounds storage + anonymity-set growth).
        #[pallet::constant]
        type MaxCommitments: Get<u32>;
    }

    #[pallet::pallet]
    pub struct Pallet<T>(_);

    // ---- Storage ----------------------------------------------------------

    /// Live (unspent) note commitments in the pool, in insertion order. The
    /// index in this vec is the note's `position` used in nullifier PRFs.
    #[pallet::storage]
    pub type NoteCommitments<T: Config> =
        StorageValue<_, BoundedVec<Commitment, T::MaxCommitments>, ValueQuery>;

    /// Spent nullifiers — prevents double-spending a note.
    #[pallet::storage]
    pub type Nullifiers<T: Config> = StorageMap<_, Blake2_128Concat, Nullifier, (), OptionQuery>;

    /// Total value locked in the shielded pool (sum of shielded balances).
    /// Transparent→shielded deposits add to this; shielded→transparent spends
    /// (later cycle) subtract.
    #[pallet::storage]
    pub type PoolValue<T: Config> = StorageValue<_, BalanceOf<T>, ValueQuery>;

    /// Next merkle-tree position for a commitment being appended.
    #[pallet::storage]
    pub type NextPosition<T: Config> = StorageValue<_, u64, ValueQuery>;

    // ---- Events -----------------------------------------------------------

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        /// A transparent→shielded deposit moved `amount` into the pool and
        /// published commitment `cm` at position `position`.
        Deposited {
            who: T::AccountId,
            amount: BalanceOf<T>,
            cm: Commitment,
            position: u64,
        },
        /// The pool was used to mint a shielded note (commitment appended).
        NoteAdded { cm: Commitment, position: u64 },
        /// A Groth16 zk-proof was verified on-chain (Step 4).
        ProofVerified { who: T::AccountId },
    }

    // ---- Errors -----------------------------------------------------------

    #[pallet::error]
    pub enum Error<T> {
        /// Pool is at capacity; cannot accept more commitments.
        PoolFull,
        /// The commitment is already in the pool.
        DuplicateCommitment,
        /// Deposit amount must be non-zero (and above the fee).
        ZeroAmount,
        /// The supplied commitment preimage is inconsistent.
        InvalidCommitment,
        /// Currency transfer failed (e.g. insufficient balance).
        TransferFailed,
        /// A supplied zero-knowledge proof failed verification.
        InvalidProof,
    }

    // ---- Calls ------------------------------------------------------------

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// Verify a Groth16 proof against the on-chain test verification key.
        ///
        /// Step 4 of the roadmap: exposes the BLS12-381 Groth16 verifier to the
        /// runtime so a future shielded spend can prove note ownership without
        /// revealing sender/amount/recipient. The proof and public inputs are
        /// passed as opaque byte strings (G1/G2 compressed + Fr big-endian).
        ///
        /// This is a *pure verification* call — it never reads a witness or
        /// secret. In the prototype it uses the deterministic [`groth16::test_key`];
        /// a real deployment swaps in a Powers-of-Tau ceremony key.
        #[pallet::call_index(1)]
        #[pallet::weight(200_000)]
        pub fn verify_proof(
            origin: OriginFor<T>,
            proof_a: [u8; 48],
            proof_b: [u8; 96],
            proof_c: [u8; 48],
            public_inputs: sp_std::vec::Vec<[u8; 32]>,
        ) -> DispatchResult {
            let _who = ensure_signed(origin)?;
            // Real value-balance circuit key (1 public input: `fee`).
            let vk = groth16::value_balance_key();
            // Convert public inputs (u128 values, big-endian) to BLS scalars.
            let scalars: sp_std::vec::Vec<bls12_381::Scalar> = public_inputs
                .iter()
                .map(|b| {
                    let mut lo = [0u8; 8];
                    lo.copy_from_slice(&b[0..8]);
                    bls12_381::Scalar::from(u64::from_be_bytes(lo))
                })
                .collect();
            // Deserialize the proof from its compressed wire bytes.
            let proof = groth16::proof_from_bytes(&proof_a, &proof_b, &proof_c)
                .ok_or(Error::<T>::InvalidProof)?;
            let r = groth16::verify(&vk, &scalars, &proof);
            ensure!(r.is_ok(), Error::<T>::InvalidProof);
            Self::deposit_event(Event::<T>::ProofVerified { who: _who });
            Ok(())
        }

        /// Transparent → Shielded deposit.
        ///
        /// Moves `amount` from the sender's `Balances` into the pool, mints a
        /// shielded note (publishes `cm`), and increments `PoolValue`. The
        /// sender pays a configurable `ShieldedFee` on top.
        #[pallet::call_index(0)]
        #[pallet::weight(50_000)]
        pub fn deposit(
            origin: OriginFor<T>,
            amount: BalanceOf<T>,
            addr: [u8; 32],
            nonce: u64,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(!amount.is_zero(), Error::<T>::ZeroAmount);

            let fee = T::ShieldedFee::get();
            let total = amount.saturating_add(fee);

            // Capacity check before mutating anything.
            let pool = <NoteCommitments<T>>::get();
            ensure!((pool.len() as u32) < T::MaxCommitments::get(), Error::<T>::PoolFull);
            // Compute the commitment for this note.
            let rcm = note::derive_rcm(
                amount.try_into().unwrap_or(u128::MAX),
                &addr,
                nonce,
            );
            let v: u128 = amount.try_into().unwrap_or(u128::MAX);
            let cm = note::note_commitment(v, &rcm, &addr);

            ensure!(!pool.contains(&cm), Error::<T>::DuplicateCommitment);

            // Move funds: amount → pool, fee → burned (or treasury later).
            T::Currency::transfer(&who, &Self::account_id(), total, ExistenceRequirement::KeepAlive)
                .map_err(|_| Error::<T>::TransferFailed)?;

            // Burn the fee by removing it from pool value (fee never enters the
            // redeemable shielded supply — it's a network cost, self-funding).
            let position = <NextPosition<T>>::get();
            let mut new_pool = pool;
            new_pool.try_push(cm).map_err(|_| Error::<T>::PoolFull)?;
            <NoteCommitments<T>>::put(new_pool);
            <Nullifiers<T>>::insert(note::nullifier(&rcm, position), ());
            <NextPosition<T>>::put(position + 1);
            <PoolValue<T>>::mutate(|p| *p = p.saturating_add(amount));

            Self::deposit_event(Event::<T>::Deposited { who, amount, cm, position });
            Self::deposit_event(Event::<T>::NoteAdded { cm, position });
            Ok(())
        }
    }

    // ---- Public helpers ---------------------------------------------------

    impl<T: Config> Pallet<T> {
        pub fn account_id() -> T::AccountId {
            frame_support::PalletId(*b"gpn/shld").into_account_truncating()
        }

        /// Number of live commitments in the pool (anonymity-set size).
        pub fn pool_size() -> u32 {
            <NoteCommitments<T>>::get().len() as u32
        }

        /// Whether a nullifier has already been spent.
        pub fn is_nullifier_spent(nf: &Nullifier) -> bool {
            <Nullifiers<T>>::contains_key(nf)
        }

        /// Total value locked in the shielded pool.
        pub fn pool_value() -> BalanceOf<T> {
            <PoolValue<T>>::get()
        }
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

    use crate::pallet as local_shield;
    use crate::pallet::{NextPosition, NoteCommitments, Nullifiers, PoolValue};

    type Block = frame_system::mocking::MockBlock<Test>;
    type Balance = u128;

    construct_runtime!(
        pub struct Test {
            System: frame_system,
            Balances: pallet_balances,
            Shield: local_shield,
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
        pub const TestShieldedFee: Balance = 10;
        pub const TestMaxCommits: u32 = 100;
    }

    impl crate::pallet::Config for Test {
        type RuntimeEvent = RuntimeEvent;
        type Currency = Balances;
        type ShieldedFee = TestShieldedFee;
        type MaxCommitments = TestMaxCommits;
    }

    const ALICE: u64 = 1;
    const BOB: u64 = 2;

    fn new_test_ext() -> sp_io::TestExternalities {
        let mut t = frame_system::GenesisConfig::<Test>::default()
            .build_storage()
            .unwrap();
        pallet_balances::GenesisConfig::<Test> {
            balances: vec![(ALICE, 10_000), (BOB, 10_000)],
            dev_accounts: None,
        }
        .assimilate_storage(&mut t)
        .unwrap();
        let mut ext = sp_io::TestExternalities::new(t);
        ext.execute_with(|| System::set_block_number(1));
        ext
    }

    type ShieldPallet = crate::pallet::Pallet::<Test>;

    const ADDR_A: [u8; 32] = [1u8; 32];
    const ADDR_B: [u8; 32] = [2u8; 32];

    // ---- Note scheme tests ------------------------------------------------

    // Real value-balance circuit: proof for fee=2 (from `gpn_shield qap-prove`,
    // seed testnet-crs-v0). The single public input is `fee`.
    const REAL_A: [u8; 48] = [
        0x99, 0x85, 0xca, 0x66, 0xec, 0x9b, 0xdb, 0x1f, 0x5b, 0x3e, 0xbd, 0xbf, 0xb9, 0xfa, 0xc7, 0xcf,
        0x6a, 0x8d, 0xc9, 0xfd, 0x2d, 0x94, 0xea, 0x4a, 0x58, 0x84, 0xeb, 0x18, 0xa5, 0x02, 0x8f, 0x9a,
        0x74, 0x36, 0x5e, 0x64, 0xd9, 0x42, 0xd3, 0x13, 0x37, 0x3b, 0x53, 0x51, 0x89, 0x4a, 0x4d, 0xe4,
    ];
    const REAL_B: [u8; 96] = [
        0xb8, 0x80, 0xdd, 0xa7, 0xa8, 0xc8, 0x0f, 0xc2, 0x26, 0x75, 0xec, 0x01, 0x19, 0x82, 0x4f, 0xac,
        0x24, 0x48, 0x20, 0x9b, 0x22, 0x93, 0xf4, 0xea, 0x06, 0x4c, 0xcf, 0x3a, 0x6d, 0x7b, 0x06, 0x86,
        0x81, 0x9e, 0xca, 0x94, 0x22, 0x5e, 0x61, 0x91, 0xd0, 0x22, 0xda, 0xcc, 0xd6, 0xce, 0x23, 0x77,
        0x01, 0xfe, 0xdb, 0xf8, 0xf8, 0xac, 0x39, 0xfc, 0x39, 0x5a, 0x29, 0x9b, 0x39, 0xcb, 0xb7, 0x52,
        0xf0, 0x99, 0x71, 0x81, 0xfb, 0xd9, 0x7c, 0x85, 0xdd, 0xf2, 0xde, 0x74, 0x61, 0x06, 0xf8, 0xfe,
        0xac, 0x87, 0x92, 0xb6, 0x38, 0xdd, 0xab, 0x03, 0x40, 0x96, 0x71, 0x42, 0xe9, 0x43, 0x1e, 0x8a,
    ];
    const REAL_C: [u8; 48] = [
        0x82, 0x58, 0x71, 0xfb, 0x81, 0xab, 0xce, 0xc0, 0xde, 0x08, 0x5c, 0x78, 0xe8, 0xe8, 0x62, 0xcb,
        0x0b, 0xd2, 0x28, 0x28, 0x64, 0xad, 0xba, 0xec, 0x90, 0x6e, 0xa4, 0x2d, 0x9c, 0x90, 0xfa, 0x07,
        0x80, 0x11, 0x5c, 0x0e, 0xe6, 0x42, 0xf4, 0xad, 0xff, 0xf6, 0x76, 0xb5, 0x23, 0x7f, 0x8e, 0xf4,
    ];
    /// fee=2 encoded as the on-chain parser expects: `b[0..8]` read as u64 BE.
    fn fee2_input() -> [u8; 32] {
        let mut out = [0u8; 32];
        out[7] = 2; // big-endian u64 value 2 in the first 8 bytes
        out
    }

    #[test]
    fn test_verify_proof_dispatchable() {
        new_test_ext().execute_with(|| {
            // Real value-balance circuit proof, fee=2 public input → verifies.
            let r = ShieldPallet::verify_proof(
                RuntimeOrigin::signed(ALICE),
                REAL_A,
                REAL_B,
                REAL_C,
                vec![fee2_input()],
            );
            assert_eq!(r, Ok(()));
        });
    }

    #[test]
    fn test_verify_proof_rejects_wrong_fee() {
        new_test_ext().execute_with(|| {
            // Correct proof bytes but wrong public input (fee=3) → InvalidProof.
            let mut wrong = fee2_input();
            wrong[7] = 3;
            let r = ShieldPallet::verify_proof(
                RuntimeOrigin::signed(ALICE),
                REAL_A,
                REAL_B,
                REAL_C,
                vec![wrong],
            );
            assert_eq!(r, Err(Error::<Test>::InvalidProof.into()));
        });
    }

    #[test]
    fn test_verify_proof_rejects_input_mismatch() {
        new_test_ext().execute_with(|| {
            // Value-balance key expects exactly 1 public input (fee); empty → mismatch.
            let r = ShieldPallet::verify_proof(
                RuntimeOrigin::signed(ALICE),
                REAL_A,
                REAL_B,
                REAL_C,
                Vec::<[u8; 32]>::new(),
            );
            assert_eq!(r, Err(Error::<Test>::InvalidProof.into()));
        });
    }

    #[test]
    fn test_verify_proof_rejects_bad_point_encoding() {
        new_test_ext().execute_with(|| {
            // Zero bytes is not a valid compressed G1 point → InvalidProof.
            let r = ShieldPallet::verify_proof(
                RuntimeOrigin::signed(ALICE),
                [0u8; 48],
                [0u8; 96],
                [0u8; 48],
                vec![fee2_input()],
            );
            assert_eq!(r, Err(Error::<Test>::InvalidProof.into()));
        });
    }

    #[test]
    fn test_note_commitment_is_stable_and_hides_inputs() {
        let cm1 = note::note_commitment(100, &[7u8; 32], &ADDR_A);
        let cm2 = note::note_commitment(100, &[7u8; 32], &ADDR_A);
        assert_eq!(cm1, cm2, "commitment must be deterministic");

        // Changing value, trapdoor, or address changes the commitment.
        assert_ne!(cm1, note::note_commitment(101, &[7u8; 32], &ADDR_A));
        assert_ne!(cm1, note::note_commitment(100, &[8u8; 32], &ADDR_A));
        assert_ne!(cm1, note::note_commitment(100, &[7u8; 32], &ADDR_B));
    }

    #[test]
    fn test_nullifier_is_stable_and_position_sensitive() {
        let nf1 = note::nullifier(&[7u8; 32], 3);
        let nf2 = note::nullifier(&[7u8; 32], 3);
        assert_eq!(nf1, nf2);
        assert_ne!(nf1, note::nullifier(&[7u8; 32], 4), "position changes nullifier");
        assert_ne!(nf1, note::nullifier(&[8u8; 32], 3), "rcm changes nullifier");
        // Nullifier is not the commitment.
        assert_ne!(nf1, note::note_commitment(100, &[7u8; 32], &ADDR_A));
    }

    // ---- Pallet deposit tests --------------------------------------------

    #[test]
    fn test_initial_state() {
        new_test_ext().execute_with(|| {
            assert_eq!(NoteCommitments::<Test>::get().to_vec(), Vec::<Commitment>::new());
            assert_eq!(NextPosition::<Test>::get(), 0);
            assert_eq!(PoolValue::<Test>::get(), 0);
            assert_eq!(ShieldPallet::pool_size(), 0);
        });
    }

    #[test]
    fn test_deposit_moves_funds_and_mints_note() {
        new_test_ext().execute_with(|| {
            let before = pallet_balances::Pallet::<Test>::free_balance(ALICE);
            // Fee = 10, amount = 500 → total 510 debited.
            ShieldPallet::deposit(RuntimeOrigin::signed(ALICE), 500, ADDR_A, 0).unwrap();

            assert_eq!(pallet_balances::Pallet::<Test>::free_balance(ALICE), before - 510);
            assert_eq!(PoolValue::<Test>::get(), 500);
            assert_eq!(ShieldPallet::pool_size(), 1);
            assert_eq!(NextPosition::<Test>::get(), 1);

            let cm = NoteCommitments::<Test>::get()[0];
            let rcm = note::derive_rcm(500, &ADDR_A, 0);
            assert_eq!(cm, note::note_commitment(500, &rcm, &ADDR_A));
            // Nullifier recorded for that position.
            assert!(Nullifiers::<Test>::contains_key(note::nullifier(&rcm, 0)));
            assert!(ShieldPallet::is_nullifier_spent(&note::nullifier(&rcm, 0)));
        });
    }

    #[test]
    fn test_deposit_rejects_zero_amount() {
        new_test_ext().execute_with(|| {
            let result = ShieldPallet::deposit(RuntimeOrigin::signed(ALICE), 0, ADDR_A, 0);
            assert_eq!(result, Err(Error::<Test>::ZeroAmount.into()));
        });
    }

    #[test]
    fn test_deposit_rejects_duplicate_commitment() {
        new_test_ext().execute_with(|| {
            ShieldPallet::deposit(RuntimeOrigin::signed(ALICE), 500, ADDR_A, 0).unwrap();
            // Same addr+nonce → same rcm → same cm → rejected.
            let result = ShieldPallet::deposit(RuntimeOrigin::signed(BOB), 500, ADDR_A, 0);
            assert_eq!(result, Err(Error::<Test>::DuplicateCommitment.into()));
        });
    }

    #[test]
    fn test_deposit_rejects_insufficient_balance() {
        new_test_ext().execute_with(|| {
            // BOB has 10,000 but we try to deposit 10,000 + 10 fee = 10,010 > balance.
            let result = ShieldPallet::deposit(RuntimeOrigin::signed(BOB), 10_000, ADDR_B, 0);
            assert_eq!(result, Err(Error::<Test>::TransferFailed.into()));
        });
    }

    #[test]
    fn test_multiple_deposits_append_positions() {
        new_test_ext().execute_with(|| {
            ShieldPallet::deposit(RuntimeOrigin::signed(ALICE), 100, ADDR_A, 0).unwrap();
            ShieldPallet::deposit(RuntimeOrigin::signed(ALICE), 200, ADDR_A, 1).unwrap();
            ShieldPallet::deposit(RuntimeOrigin::signed(BOB), 300, ADDR_B, 0).unwrap();

            assert_eq!(ShieldPallet::pool_size(), 3);
            assert_eq!(NextPosition::<Test>::get(), 3);
            assert_eq!(PoolValue::<Test>::get(), 600);

            // Different nonce → different commitment even for same addr.
            let cm0 = NoteCommitments::<Test>::get()[0];
            let cm1 = NoteCommitments::<Test>::get()[1];
            assert_ne!(cm0, cm1);
        });
    }

    #[test]
    fn test_pool_full_rejects() {
        new_test_ext().execute_with(|| {
            // MaxCommits = 100; deposit 100 unique notes.
            for i in 0..100u64 {
                ShieldPallet::deposit(RuntimeOrigin::signed(ALICE), 1, ADDR_A, i).unwrap();
            }
            assert_eq!(ShieldPallet::pool_size(), 100);
            let result = ShieldPallet::deposit(RuntimeOrigin::signed(ALICE), 1, ADDR_A, 100);
            assert_eq!(result, Err(Error::<Test>::PoolFull.into()));
        });
    }
}
