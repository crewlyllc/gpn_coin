//! # GPN Governance pallet (Option D, hardened)
//!
//! A minimal "technical committee" upgrade path for mainnet, so a non-single-key
//! body can change the chain (runtime upgrades via `set_code`, parameter changes)
//! without a live sudo key.
//!
//! A fixed set of members (the committee) can `propose` an arbitrary runtime
//! call and `approve` it. Once `ApprovalsRequired` (3 of 5) distinct members have
//! approved **and** `TimelockPeriod` (3 days) has elapsed since proposal, anyone
//! may `execute` the proposal — which dispatches the call with **Root** origin.
//!
//! Hardening added on top of the original 2-of-3 design:
//!
//! * **Membership is storage, not only genesis.** `Members` is still seeded at
//!   genesis, and `set_members` (Root-only — i.e. reachable only through this
//!   pallet's own threshold + timelock) can replace it. `MaxMembers` bounds it.
//! * **Proposal expiry** (`ProposalExpiry`, 14 days). `execute` refuses an
//!   expired proposal (`ProposalExpired`) and `on_initialize` prunes expired
//!   entries so storage does not leak.
//! * **Cancel / veto** (`cancel`) requires the *same* threshold as execution,
//!   so there is no single-member veto and no single-member block.
//! * **Hash-addressed preimages** (`propose_preimage`): the committee approves
//!   `blake2_256(bytes)`; the bytes live in `Preimages` and can be inspected
//!   with [`Pallet::peek`] before the timelock elapses. `execute` resolves the
//!   hash, verifies the length, SCALE-decodes, and only then dispatches.
//! * **Hand-set weights** (`weights::GpnWeight`) instead of `WeightInfo = ()`.
//!
//! The security invariant is unchanged: no single key can act alone, every
//! action is public for `TimelockPeriod` blocks before it can take effect, and
//! the enacted code is verifiable by hash before the timelock elapses.

#![cfg_attr(not(feature = "std"), no_std)]

pub use pallet::*;

pub mod weights;
pub use weights::WeightInfo;

#[frame_support::pallet]
pub mod pallet {
    use super::weights::WeightInfo;
    use frame_support::pallet_prelude::*;
    use frame_system::pallet_prelude::*;
    use sp_runtime::traits::{Dispatchable, Hash, Saturating};
    use sp_runtime::DispatchError;
    use sp_std::prelude::*;

    /// On-chain storage layout version.
    /// * `0` — pre-hardening: `Proposal { call: RuntimeCall, .. }`, members only
    ///   from genesis, no preimages, no expiry.
    /// * `1` — this version (`ProposalCall` enum, `Preimages`, `ProposalIds`).
    pub const STORAGE_VERSION: StorageVersion = StorageVersion::new(1);

    /// Domain separator for the id of a hash-addressed proposal:
    /// `id = blake2_256(b"gpn/gov/preimage/v1" ++ code_hash ++ len.to_le_bytes())`.
    pub const PREIMAGE_PROPOSAL_TAG: &[u8] = b"gpn/gov/preimage/v1";

    #[pallet::pallet]
    #[pallet::storage_version(STORAGE_VERSION)]
    #[pallet::without_storage_info]
    pub struct Pallet<T>(_);

    #[pallet::config]
    pub trait Config: frame_system::Config {
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;
        /// Distinct approvals required to execute *and* to cancel (3 for the
        /// 5-member committee).
        #[pallet::constant]
        type ApprovalsRequired: Get<u32>;
        /// Minimum blocks between proposal and execution (the timelock).
        #[pallet::constant]
        type TimelockPeriod: Get<BlockNumberFor<Self>>;
        /// Maximum age of a proposal, measured from `created_at`. A proposal
        /// older than this can no longer be executed and is pruned.
        #[pallet::constant]
        type ProposalExpiry: Get<BlockNumberFor<Self>>;
        /// Maximum number of members (bounds the approvals vector).
        #[pallet::constant]
        type MaxMembers: Get<u32>;
        /// Maximum SCALE-encoded size of a call or preimage (bounds the
        /// `propose` extrinsic and the `Preimages` map).
        #[pallet::constant]
        type MaxProposalCallLen: Get<u32>;
        /// Maximum number of *simultaneously open* proposals. Bounds the
        /// `ProposalIds` index, which is what `on_initialize` walks to prune
        /// expired entries (a `Blake2_256`-keyed map cannot be iterated, so the
        /// index is the only ordered view of the open set).
        #[pallet::constant]
        type MaxOpenProposals: Get<u32>;
        /// The members seeded by the `v0 → v1` migration. Existing members are
        /// kept (union), so this can only ever grow the committee.
        type MigrationMembers: Get<Vec<Self::AccountId>>;
        type WeightInfo: WeightInfo;
    }

    /// The governance members.
    #[pallet::storage]
    pub type Members<T: Config> = StorageMap<_, Blake2_128Concat, T::AccountId, (), OptionQuery>;

    /// Number of members (for weight/validation bookkeeping).
    #[pallet::storage]
    pub type MemberCount<T: Config> = StorageValue<_, u32, ValueQuery>;

    /// Hash-addressed call bytes. Key is `blake2_256(bytes)` — the value the
    /// committee approves. Publicly readable; see [`Pallet::peek`].
    #[pallet::storage]
    pub type Preimages<T: Config> =
        StorageMap<_, Blake2_256, T::Hash, BoundedVec<u8, T::MaxProposalCallLen>, OptionQuery>;

    /// The open proposal ids, in creation order.
    ///
    /// `Proposals` is keyed by `Blake2_256` (not reversible), so it cannot be
    /// iterated. This index is the ordered view `on_initialize` uses to find
    /// expired entries; it is bounded by `MaxOpenProposals`, which makes the
    /// per-block prune cost bounded and starvation-free.
    #[pallet::storage]
    pub type ProposalIds<T: Config> =
        StorageValue<_, BoundedVec<T::Hash, T::MaxOpenProposals>, ValueQuery>;

    /// Where the executable call comes from.
    #[derive(Clone, Encode, Decode, Eq, PartialEq, Debug, TypeInfo)]
    #[scale_info(skip_type_params(T))]
    pub enum ProposalCall<T: Config> {
        /// The call is stored inline (legacy path; id = `blake2_256(call)`).
        Inline(T::RuntimeCall),
        /// Only the hash + length are stored; the bytes live in `Preimages`.
        Preimage { hash: T::Hash, len: u32 },
    }

    /// A pending governance proposal awaiting threshold + timelock.
    #[derive(Clone, Encode, Decode, Eq, PartialEq, Debug, TypeInfo)]
    #[scale_info(skip_type_params(T))]
    pub struct Proposal<T: Config> {
        /// The call to execute as Root, inline or by preimage hash.
        pub call: ProposalCall<T>,
        /// Distinct member accounts that have approved.
        pub approvals: BoundedVec<T::AccountId, T::MaxMembers>,
        /// Block number the proposal was created.
        pub created_at: BlockNumberFor<T>,
    }

    /// Open proposals, keyed by the proposal id.
    #[pallet::storage]
    pub type Proposals<T: Config> = StorageMap<_, Blake2_256, T::Hash, Proposal<T>, OptionQuery>;

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        Proposed {
            proposer: T::AccountId,
            proposal: T::Hash,
        },
        Approved {
            member: T::AccountId,
            proposal: T::Hash,
        },
        Executed {
            proposal: T::Hash,
        },
        /// A pending proposal was removed by the committee (threshold met).
        ProposalCancelled {
            proposal: T::Hash,
            canceller: T::AccountId,
        },
        /// `on_initialize` pruned a proposal past `ProposalExpiry`.
        ProposalExpired {
            proposal: T::Hash,
        },
        /// The member set was replaced (by `set_members`, or by the migration).
        MembersChanged {
            members: Vec<T::AccountId>,
        },
        /// Call bytes were stored under `hash` (hash-addressed proposal).
        PreimageNoted {
            hash: T::Hash,
            len: u32,
            noter: T::AccountId,
        },
    }

    #[pallet::error]
    pub enum Error<T> {
        NotMember,
        ProposalExists,
        ProposalNotFound,
        AlreadyApproved,
        NotEnoughApprovals,
        TimelockNotElapsed,
        ExecutionFailed,
        /// `now - created_at > ProposalExpiry`.
        ProposalExpired,
        /// `execute` (or a `Preimage` proposal) referenced bytes that are not
        /// in `Preimages`. The proposal is **not** consumed — re-note the same
        /// bytes (same hash) and retry.
        PreimageMissing,
        /// Stored preimage length differs from the length the proposal carries.
        PreimageLenMismatch,
        /// `blake2_256(bytes) != hash` in `propose_preimage`.
        PreimageMismatch,
        /// Preimage bytes are not a valid, fully-consumed SCALE `RuntimeCall`.
        InvalidCallEncoding,
        /// Call/preimage larger than `MaxProposalCallLen`.
        CallTooLarge,
        /// `set_members` with an empty set.
        EmptyMembers,
        /// `set_members` with more than `MaxMembers` entries.
        TooManyMembers,
        /// `set_members` with a repeated account.
        DuplicateMember,
        /// `MaxOpenProposals` reached — execute, cancel or let one expire first.
        TooManyProposals,
    }

    #[pallet::genesis_config]
    pub struct GenesisConfig<T: Config> {
        pub initial_members: Vec<T::AccountId>,
        #[serde(skip)]
        pub _config: PhantomData<T>,
    }

    impl<T: Config> Default for GenesisConfig<T> {
        fn default() -> Self {
            Self {
                initial_members: Vec::new(),
                _config: PhantomData,
            }
        }
    }

    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            for m in &self.initial_members {
                Members::<T>::insert(m, ());
            }
            MemberCount::<T>::put(self.initial_members.len() as u32);
        }
    }

    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
        fn on_runtime_upgrade() -> Weight {
            crate::migrations::migrate_v0_to_v1::<T>()
        }

        fn on_initialize(now: BlockNumberFor<T>) -> Weight {
            Self::prune_expired(now)
        }
    }

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// A member proposes an arbitrary call (e.g. `set_code`) for Root execution.
        #[pallet::call_index(0)]
        #[pallet::weight(T::WeightInfo::propose(call.encoded_size() as u32))]
        pub fn propose(origin: OriginFor<T>, call: Box<T::RuntimeCall>) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(Members::<T>::contains_key(&who), Error::<T>::NotMember);

            let call = *call;
            let call_len = call.encoded_size() as u32;
            ensure!(
                call_len <= T::MaxProposalCallLen::get(),
                Error::<T>::CallTooLarge
            );

            let proposal_hash = T::Hashing::hash_of(&call);
            Self::insert_proposal(proposal_hash, ProposalCall::Inline(call), who)?;
            Ok(())
        }

        /// A member approves an existing proposal.
        #[pallet::call_index(1)]
        #[pallet::weight(T::WeightInfo::approve())]
        pub fn approve(origin: OriginFor<T>, proposal_hash: T::Hash) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(Members::<T>::contains_key(&who), Error::<T>::NotMember);

            let mut proposal =
                Proposals::<T>::get(proposal_hash).ok_or(Error::<T>::ProposalNotFound)?;
            ensure!(
                !proposal.approvals.contains(&who),
                Error::<T>::AlreadyApproved
            );
            proposal
                .approvals
                .try_push(who.clone())
                .map_err(|_| Error::<T>::TooManyMembers)?;
            Proposals::<T>::insert(proposal_hash, proposal);

            Self::deposit_event(Event::Approved {
                member: who,
                proposal: proposal_hash,
            });
            Ok(())
        }

        /// Execute an approved proposal as Root, once threshold + timelock are
        /// met and the proposal has not expired.
        ///
        /// Unlike the pre-hardening version, a **failed dispatch no longer
        /// consumes the proposal** (all checks and the removal happen after the
        /// dispatch result is known), so a bad `set_code` can be fixed and
        /// retried without re-collecting approvals.
        #[pallet::call_index(2)]
        #[pallet::weight(T::WeightInfo::execute())]
        pub fn execute(origin: OriginFor<T>, proposal_hash: T::Hash) -> DispatchResult {
            ensure_signed(origin)?; // anyone may execute once conditions are met

            let proposal =
                Proposals::<T>::get(proposal_hash).ok_or(Error::<T>::ProposalNotFound)?;

            let approvals = proposal.approvals.len() as u32;
            ensure!(
                approvals >= T::ApprovalsRequired::get(),
                Error::<T>::NotEnoughApprovals
            );

            let now = frame_system::Pallet::<T>::block_number();
            let elapsed = now.saturating_sub(proposal.created_at);
            ensure!(
                elapsed >= T::TimelockPeriod::get(),
                Error::<T>::TimelockNotElapsed
            );
            ensure!(
                elapsed <= T::ProposalExpiry::get(),
                Error::<T>::ProposalExpired
            );

            // Resolve the call *before* mutating anything, so a missing or
            // malformed preimage leaves the proposal intact for a retry.
            let (call, preimage_hash) = match proposal.call {
                ProposalCall::Inline(call) => (call, None),
                ProposalCall::Preimage { hash, len } => {
                    let bytes = Preimages::<T>::get(hash).ok_or(Error::<T>::PreimageMissing)?;
                    ensure!(bytes.len() as u32 == len, Error::<T>::PreimageLenMismatch);
                    let mut cursor: &[u8] = &bytes;
                    let call = T::RuntimeCall::decode(&mut cursor)
                        .map_err(|_| Error::<T>::InvalidCallEncoding)?;
                    ensure!(cursor.is_empty(), Error::<T>::InvalidCallEncoding);
                    (call, Some(hash))
                }
            };

            let res = call.dispatch(frame_system::RawOrigin::<T::AccountId>::Root.into());
            res.map_err(|_| Error::<T>::ExecutionFailed)?;

            // Only now consume the proposal (and the preimage it used).
            Self::remove_proposal(proposal_hash);
            if let Some(h) = preimage_hash {
                Preimages::<T>::remove(h);
            }

            Self::deposit_event(Event::Executed {
                proposal: proposal_hash,
            });
            Ok(())
        }

        /// Remove a pending proposal. Requires the **same** approval threshold
        /// as `execute`, so no single member can veto (or unblock) alone.
        #[pallet::call_index(3)]
        #[pallet::weight(T::WeightInfo::cancel())]
        pub fn cancel(origin: OriginFor<T>, proposal_hash: T::Hash) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(Members::<T>::contains_key(&who), Error::<T>::NotMember);

            let proposal =
                Proposals::<T>::get(proposal_hash).ok_or(Error::<T>::ProposalNotFound)?;
            ensure!(
                proposal.approvals.len() as u32 >= T::ApprovalsRequired::get(),
                Error::<T>::NotEnoughApprovals
            );

            Self::remove_proposal(proposal_hash);
            // The preimage (if any) is left in place: it is content-addressed
            // and may back another proposal. It can be dropped by a later
            // runtime upgrade if it ever becomes state bloat.
            Self::deposit_event(Event::ProposalCancelled {
                proposal: proposal_hash,
                canceller: who,
            });
            Ok(())
        }

        /// Replace the committee. **Root only** — which on mainnet means only
        /// through `propose` + `ApprovalsRequired` + timelock + `execute`.
        ///
        /// NOTE: existing proposals keep the approvals they already collected;
        /// `execute`/`cancel` count approvals without re-checking that each
        /// approver is still a member. Cancel anything you no longer trust
        /// before rotating the set.
        #[pallet::call_index(4)]
        #[pallet::weight(T::WeightInfo::set_members(members.len() as u32))]
        pub fn set_members(origin: OriginFor<T>, members: Vec<T::AccountId>) -> DispatchResult {
            ensure_root(origin)?;
            ensure!(!members.is_empty(), Error::<T>::EmptyMembers);
            ensure!(
                members.len() as u32 <= T::MaxMembers::get(),
                Error::<T>::TooManyMembers
            );

            let bounded: BoundedVec<T::AccountId, T::MaxMembers> =
                members.try_into().map_err(|_| Error::<T>::TooManyMembers)?;
            for i in 0..bounded.len() {
                for j in (i + 1)..bounded.len() {
                    ensure!(bounded[i] != bounded[j], Error::<T>::DuplicateMember);
                }
            }

            Self::replace_members(&bounded);
            Ok(())
        }

        /// Store call bytes and open a proposal that refers to them by hash.
        ///
        /// The proposer must be a member and must supply bytes whose
        /// `blake2_256` is `hash`; the bytes must SCALE-decode to a
        /// `RuntimeCall` with no trailing data. The proposal id is
        /// [`Pallet::preimage_proposal_id`].
        #[pallet::call_index(5)]
        #[pallet::weight(T::WeightInfo::propose_preimage(bytes.len() as u32))]
        pub fn propose_preimage(
            origin: OriginFor<T>,
            hash: T::Hash,
            bytes: Vec<u8>,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(Members::<T>::contains_key(&who), Error::<T>::NotMember);
            let len = Self::note_preimage_bytes(&who, hash, bytes)?;

            let proposal_hash = Self::preimage_proposal_id(hash, len);
            Self::insert_proposal(proposal_hash, ProposalCall::Preimage { hash, len }, who)?;
            Ok(())
        }

        /// Store call bytes under their hash **without** opening a proposal.
        ///
        /// This is the recovery path for `PreimageMissing`: re-note the same
        /// bytes (same hash ⇒ same proposal id) and `execute` the existing,
        /// already-approved proposal. Member-gated so the `Preimages` map
        /// cannot be spammed by arbitrary accounts.
        #[pallet::call_index(6)]
        #[pallet::weight(T::WeightInfo::propose_preimage(bytes.len() as u32))]
        pub fn note_preimage(
            origin: OriginFor<T>,
            hash: T::Hash,
            bytes: Vec<u8>,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(Members::<T>::contains_key(&who), Error::<T>::NotMember);
            Self::note_preimage_bytes(&who, hash, bytes)?;
            Ok(())
        }
    }

    impl<T: Config> Pallet<T> {
        /// Validate + store `bytes` under `hash`. Returns the length.
        fn note_preimage_bytes(
            who: &T::AccountId,
            hash: T::Hash,
            bytes: Vec<u8>,
        ) -> Result<u32, DispatchError> {
            ensure!(
                bytes.len() as u32 <= T::MaxProposalCallLen::get(),
                Error::<T>::CallTooLarge
            );
            ensure!(
                T::Hashing::hash(&bytes) == hash,
                Error::<T>::PreimageMismatch
            );

            // Must be an exercisable call, or the committee would be approving
            // bytes that can only fail at execute.
            let mut cursor: &[u8] = &bytes;
            T::RuntimeCall::decode(&mut cursor).map_err(|_| Error::<T>::InvalidCallEncoding)?;
            ensure!(cursor.is_empty(), Error::<T>::InvalidCallEncoding);

            let len = bytes.len() as u32;
            let bounded: BoundedVec<u8, T::MaxProposalCallLen> =
                bytes.try_into().map_err(|_| Error::<T>::CallTooLarge)?;

            if !Preimages::<T>::contains_key(hash) {
                Self::deposit_event(Event::PreimageNoted {
                    hash,
                    len,
                    noter: who.clone(),
                });
            }
            // Idempotent: the same hash can only describe the same bytes.
            Preimages::<T>::insert(hash, bounded);
            Ok(len)
        }

        /// Public, off-chain-callable view of a stored preimage. The same bytes
        /// are readable straight from chain state (`GpnGovernance.Preimages`),
        /// so this is a convenience for other pallets/tests, not a new API.
        pub fn peek(hash: T::Hash) -> Option<Vec<u8>> {
            Preimages::<T>::get(hash).map(|b| b.into_inner())
        }

        /// `id = blake2_256(b"gpn/gov/preimage/v1" ++ hash ++ len_le)`.
        ///
        /// Reproduced by `gpn_gov_upgrade preimage-id`, which lets the CLI and
        /// the chain agree on the id without a runtime API.
        pub fn preimage_proposal_id(hash: T::Hash, len: u32) -> T::Hash {
            let mut buf = Vec::with_capacity(PREIMAGE_PROPOSAL_TAG.len() + 36);
            buf.extend_from_slice(PREIMAGE_PROPOSAL_TAG);
            buf.extend_from_slice(&hash.encode());
            buf.extend_from_slice(&len.to_le_bytes());
            T::Hashing::hash(&buf)
        }

        fn insert_proposal(
            proposal_hash: T::Hash,
            call: ProposalCall<T>,
            proposer: T::AccountId,
        ) -> DispatchResult {
            ensure!(
                !Proposals::<T>::contains_key(proposal_hash),
                Error::<T>::ProposalExists
            );

            let mut ids = ProposalIds::<T>::get();
            ids.try_push(proposal_hash)
                .map_err(|_| Error::<T>::TooManyProposals)?;

            let mut approvals: BoundedVec<T::AccountId, T::MaxMembers> = BoundedVec::new();
            approvals
                .try_push(proposer.clone())
                .map_err(|_| Error::<T>::TooManyMembers)?;

            Proposals::<T>::insert(
                proposal_hash,
                Proposal {
                    call,
                    approvals,
                    created_at: frame_system::Pallet::<T>::block_number(),
                },
            );
            ProposalIds::<T>::put(ids);

            Self::deposit_event(Event::Proposed {
                proposer,
                proposal: proposal_hash,
            });
            Ok(())
        }

        fn replace_members(members: &BoundedVec<T::AccountId, T::MaxMembers>) {
            let _ = Members::<T>::clear(u32::MAX, None);
            for m in members.iter() {
                Members::<T>::insert(m, ());
            }
            MemberCount::<T>::put(members.len() as u32);
            Self::deposit_event(Event::MembersChanged {
                members: members.to_vec(),
            });
        }

        /// Remove `id` from both the proposal map and the open-proposal index.
        fn remove_proposal(id: T::Hash) {
            Proposals::<T>::remove(id);
            let mut ids = ProposalIds::<T>::get();
            if let Some(pos) = ids.iter().position(|x| *x == id) {
                ids.remove(pos);
                ProposalIds::<T>::put(ids);
            }
        }

        /// Drop proposals older than `ProposalExpiry`.
        ///
        /// The open set is bounded by `MaxOpenProposals`, so this is a bounded
        /// full scan (no cursor, no starvation) and the cost is proportional to
        /// the *actual* number of open proposals — normally zero or one.
        fn prune_expired(now: BlockNumberFor<T>) -> Weight {
            let expiry = T::ProposalExpiry::get();
            let ids = ProposalIds::<T>::get();
            if ids.is_empty() {
                return T::WeightInfo::prune_expired(0);
            }

            let mut scanned: u32 = 0;
            let mut kept: BoundedVec<T::Hash, T::MaxOpenProposals> = BoundedVec::new();
            for id in ids.iter().copied() {
                scanned = scanned.saturating_add(1);
                match Proposals::<T>::get(id) {
                    Some(p) if now.saturating_sub(p.created_at) > expiry => {
                        Proposals::<T>::remove(id);
                        Self::deposit_event(Event::ProposalExpired { proposal: id });
                    }
                    // Keep live proposals, and drop index entries whose
                    // proposal is already gone (defensive, cannot normally
                    // happen).
                    Some(_) => {
                        let _ = kept.try_push(id);
                    }
                    None => {}
                }
            }
            ProposalIds::<T>::put(kept);

            T::WeightInfo::prune_expired(scanned)
        }
    }
}

/// Storage migrations.
pub mod migrations {
    use crate::pallet::{
        Config, Event, MemberCount, Members, Pallet, Proposal, ProposalCall, ProposalIds,
    };
    use frame_support::pallet_prelude::*;
    use frame_support::storage::unhashed;
    use frame_support::traits::{GetStorageVersion, PalletInfoAccess, StorageVersion};
    use frame_system::pallet_prelude::BlockNumberFor;
    use sp_runtime::traits::Hash;
    use sp_std::prelude::*;

    /// Pre-hardening `Proposal` layout (storage version 0).
    #[derive(Clone, Encode, Decode, Eq, PartialEq, Debug, TypeInfo)]
    #[scale_info(skip_type_params(T))]
    pub struct OldProposal<T: Config> {
        pub call: T::RuntimeCall,
        pub approvals: BoundedVec<T::AccountId, T::MaxMembers>,
        pub created_at: BlockNumberFor<T>,
    }

    /// `v0 → v1`:
    ///
    /// 1. re-encode every `Proposals` value from `OldProposal` to
    ///    `Proposal { call: ProposalCall::Inline(..), .. }` — the storage *key*
    ///    (the proposal id) is untouched, so pending proposals keep their id and
    ///    their approvals;
    /// 2. rebuild the `ProposalIds` index (the new ordered view used to prune
    ///    expired proposals) from those entries;
    /// 3. seed `Members` with `Config::MigrationMembers` (union with whoever is
    ///    already there — this can only grow the committee, never silently drop
    ///    a sitting member);
    /// 4. recompute `MemberCount` and bump the storage version.
    ///
    /// Idempotent: a no-op once the on-chain storage version is already 1.
    ///
    /// The walk is prefix-based (`sp_io::storage::next_key` over
    /// `twox128("GpnGovernance") ++ twox128("Proposals")`) rather than
    /// `StorageMap::iter()`, which does not exist for a `Blake2_256`-keyed map.
    pub fn migrate_v0_to_v1<T: Config>() -> Weight {
        let on_chain = <Pallet<T> as GetStorageVersion>::on_chain_storage_version();
        if on_chain >= 1 {
            log::info!(
                target: "gpn-governance",
                "storage already at v{:?}; nothing to migrate", on_chain
            );
            return T::DbWeight::get().reads(1);
        }

        let prefix = frame_support::storage::storage_prefix(
            <Pallet<T> as PalletInfoAccess>::name().as_bytes(),
            b"Proposals",
        );

        let mut ids: BoundedVec<T::Hash, T::MaxOpenProposals> = BoundedVec::new();
        let mut translated: u64 = 0;
        let mut key = prefix.to_vec();
        while let Some(next) = sp_io::storage::next_key(&key) {
            if !next.starts_with(&prefix) {
                break;
            }
            key = next;

            let old = match unhashed::get::<OldProposal<T>>(&key) {
                Some(v) => v,
                None => {
                    log::error!(
                        target: "gpn-governance",
                        "v0→v1: value at {:?} is not a valid OldProposal; left untouched", key
                    );
                    continue;
                }
            };

            // The id was (and still is) `blake2_256(SCALE(call))`.
            let id = T::Hashing::hash_of(&old.call);
            unhashed::put::<Proposal<T>>(
                &key,
                &Proposal {
                    call: ProposalCall::Inline(old.call),
                    approvals: old.approvals,
                    created_at: old.created_at,
                },
            );
            // `try_push` failure would mean more than `MaxOpenProposals` legacy
            // proposals; keep the values (they remain executable) and log it.
            if ids.try_push(id).is_err() {
                log::warn!(
                    target: "gpn-governance",
                    "v0→v1: more than MaxOpenProposals proposals; index truncated"
                );
            }
            translated = translated.saturating_add(1);
        }
        ProposalIds::<T>::put(ids);
        log::info!(
            target: "gpn-governance",
            "v0→v1: re-encoded {} proposal(s)", translated
        );

        let seed = T::MigrationMembers::get();
        let mut added: u64 = 0;
        if !seed.is_empty() {
            let existing: Vec<T::AccountId> = Members::<T>::iter_keys().collect();
            for m in &existing {
                if !seed.contains(m) {
                    log::warn!(
                        target: "gpn-governance",
                        "v0→v1: sitting member {:?} is not in the seed set; keeping it (union)",
                        m
                    );
                }
            }
            for m in seed.iter() {
                if !Members::<T>::contains_key(m) {
                    Members::<T>::insert(m, ());
                    added = added.saturating_add(1);
                }
            }
            let all: Vec<T::AccountId> = Members::<T>::iter_keys().collect();
            MemberCount::<T>::put(all.len() as u32);
            Pallet::<T>::deposit_event(Event::<T>::MembersChanged { members: all });
            log::info!(
                target: "gpn-governance",
                "v0→v1: seeded {} new member(s)", added
            );
        }

        StorageVersion::new(1).put::<Pallet<T>>();
        T::DbWeight::get().reads_writes(4 + translated, 4 + translated + added)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate as pallet_gpn_governance;
    use crate::migrations::OldProposal;
    use codec::Encode;
    use frame_support::{
        assert_noop, assert_ok, construct_runtime, derive_impl, parameter_types,
        storage::unhashed,
        traits::{ConstU16, ConstU64, GetStorageVersion, Hooks, StorageVersion},
        BoundedVec,
    };
    use sp_runtime::{
        traits::{BlakeTwo256, Hash, IdentityLookup},
        BuildStorage, DispatchError,
    };

    type Block = frame_system::mocking::MockBlock<Test>;

    construct_runtime!(
        pub enum Test {
            System: frame_system,
            GpnGovernance: pallet_gpn_governance,
        }
    );

    #[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
    impl frame_system::Config for Test {
        type Block = Block;
        type AccountId = u64;
        type BlockHashCount = ConstU64<250>;
        type Lookup = IdentityLookup<u64>;
        type SS58Prefix = ConstU16<42>;
    }

    parameter_types! {
        pub const ApprovalsRequired: u32 = 3;
        pub const TimelockPeriod: u64 = 3;
        pub const ProposalExpiry: u64 = 10;
        pub const MaxMembers: u32 = 8;
        pub const MaxProposalCallLen: u32 = 4 * 1024 * 1024;
        pub const MaxOpenProposals: u32 = 64;
        pub MigrationMembers: Vec<u64> = vec![1, 2, 3, 4, 5];
    }

    impl pallet_gpn_governance::Config for Test {
        type RuntimeEvent = RuntimeEvent;
        type ApprovalsRequired = ApprovalsRequired;
        type TimelockPeriod = TimelockPeriod;
        type ProposalExpiry = ProposalExpiry;
        type MaxMembers = MaxMembers;
        type MaxProposalCallLen = MaxProposalCallLen;
        type MaxOpenProposals = MaxOpenProposals;
        type MigrationMembers = MigrationMembers;
        type WeightInfo = ();
    }

    fn new_test_ext() -> sp_io::TestExternalities {
        let mut t = frame_system::GenesisConfig::<Test>::default()
            .build_storage()
            .unwrap();
        pallet_gpn_governance::GenesisConfig::<Test> {
            initial_members: vec![1, 2, 3, 4, 5],
            _config: Default::default(),
        }
        .assimilate_storage(&mut t)
        .unwrap();
        let mut ext: sp_io::TestExternalities = t.into();
        ext.execute_with(|| System::set_block_number(1));
        ext
    }

    /// Genesis in the pallet tests does not run `on_genesis`, so the storage
    /// version starts at the pre-hardening 0. The migration test needs to say
    /// so explicitly; production chains get 0 by never having had a version.
    fn set_storage_version(v: u16) {
        StorageVersion::new(v).put::<GpnGovernance>();
    }

    fn remark_call() -> RuntimeCall {
        RuntimeCall::System(frame_system::Call::kill_storage { keys: vec![] })
    }

    fn preimage(bytes: &[u8]) -> (RuntimeCall, H256, Vec<u8>) {
        let call = remark_call();
        let encoded = call.encode();
        assert_eq!(&encoded[..], bytes);
        (call, BlakeTwo256::hash(bytes), encoded)
    }

    type H256 = <Test as frame_system::Config>::Hash;

    #[test]
    fn non_member_cannot_propose() {
        new_test_ext().execute_with(|| {
            assert_noop!(
                GpnGovernance::propose(RuntimeOrigin::signed(99), Box::new(remark_call())),
                Error::<Test>::NotMember
            );
        });
    }

    #[test]
    fn requires_three_approvals_and_timelock_before_root_execution() {
        new_test_ext().execute_with(|| {
            let call = remark_call();
            let hash = BlakeTwo256::hash_of(&call);

            // Member 1 proposes (self-approves) -> 1 approval.
            assert_ok!(GpnGovernance::propose(
                RuntimeOrigin::signed(1),
                Box::new(call.clone())
            ));
            assert_eq!(Proposals::<Test>::get(hash).unwrap().approvals.len(), 1);

            // 1 approval is not enough.
            assert_noop!(
                GpnGovernance::execute(RuntimeOrigin::signed(9), hash),
                Error::<Test>::NotEnoughApprovals
            );

            assert_ok!(GpnGovernance::approve(RuntimeOrigin::signed(2), hash));
            // 2 approvals is still not enough under the 3-of-5 committee.
            assert_noop!(
                GpnGovernance::execute(RuntimeOrigin::signed(9), hash),
                Error::<Test>::NotEnoughApprovals
            );
            assert_ok!(GpnGovernance::approve(RuntimeOrigin::signed(3), hash));

            // Timelock (3 blocks) not yet elapsed at block 1.
            assert_noop!(
                GpnGovernance::execute(RuntimeOrigin::signed(9), hash),
                Error::<Test>::TimelockNotElapsed
            );
            System::set_block_number(3);
            assert_noop!(
                GpnGovernance::execute(RuntimeOrigin::signed(9), hash),
                Error::<Test>::TimelockNotElapsed
            );

            System::set_block_number(4);
            assert_ok!(GpnGovernance::execute(RuntimeOrigin::signed(9), hash));
            assert!(!Proposals::<Test>::contains_key(hash));
        });
    }

    #[test]
    fn approval_rules() {
        new_test_ext().execute_with(|| {
            let hash = BlakeTwo256::hash_of(&remark_call());
            assert_ok!(GpnGovernance::propose(
                RuntimeOrigin::signed(1),
                Box::new(remark_call())
            ));
            // duplicate proposal of identical bytes
            assert_noop!(
                GpnGovernance::propose(RuntimeOrigin::signed(2), Box::new(remark_call())),
                Error::<Test>::ProposalExists
            );
            assert_noop!(
                GpnGovernance::approve(RuntimeOrigin::signed(99), hash),
                Error::<Test>::NotMember
            );
            assert_ok!(GpnGovernance::approve(RuntimeOrigin::signed(2), hash));
            assert_noop!(
                GpnGovernance::approve(RuntimeOrigin::signed(2), hash),
                Error::<Test>::AlreadyApproved
            );
            assert_noop!(
                GpnGovernance::approve(RuntimeOrigin::signed(1), hash),
                Error::<Test>::AlreadyApproved
            );
        });
    }

    // ── expiry ──────────────────────────────────────────────────────────────

    #[test]
    fn execute_is_allowed_at_exactly_the_expiry_boundary() {
        new_test_ext().execute_with(|| {
            let hash = BlakeTwo256::hash_of(&remark_call());
            assert_ok!(GpnGovernance::propose(
                RuntimeOrigin::signed(1),
                Box::new(remark_call())
            ));
            assert_ok!(GpnGovernance::approve(RuntimeOrigin::signed(2), hash));
            assert_ok!(GpnGovernance::approve(RuntimeOrigin::signed(3), hash));

            // created_at = 1, expiry = 10 → the last admissible block is 11.
            System::set_block_number(11);
            assert_ok!(GpnGovernance::execute(RuntimeOrigin::signed(9), hash));
        });
    }

    #[test]
    fn execute_rejects_past_expiry() {
        new_test_ext().execute_with(|| {
            let hash = BlakeTwo256::hash_of(&remark_call());
            assert_ok!(GpnGovernance::propose(
                RuntimeOrigin::signed(1),
                Box::new(remark_call())
            ));
            assert_ok!(GpnGovernance::approve(RuntimeOrigin::signed(2), hash));
            assert_ok!(GpnGovernance::approve(RuntimeOrigin::signed(3), hash));

            System::set_block_number(12); // elapsed 11 > expiry 10
            assert_noop!(
                GpnGovernance::execute(RuntimeOrigin::signed(9), hash),
                Error::<Test>::ProposalExpired
            );
            // ... and the failed execute does not consume it (pruning is the
            // job of on_initialize).
            assert!(Proposals::<Test>::contains_key(hash));
        });
    }

    #[test]
    fn on_initialize_prunes_every_expired_proposal_in_one_block() {
        new_test_ext().execute_with(|| {
            // Three distinct proposals, all created at block 1.
            for i in 0..3u8 {
                let call = RuntimeCall::System(frame_system::Call::remark { remark: vec![i] });
                assert_ok!(GpnGovernance::propose(
                    RuntimeOrigin::signed(1),
                    Box::new(call)
                ));
            }
            assert_eq!(ProposalIds::<Test>::get().len(), 3);

            // Block 11: exactly at the boundary — nothing may be pruned.
            System::set_block_number(11);
            GpnGovernance::on_initialize(11);
            assert_eq!(ProposalIds::<Test>::get().len(), 3, "boundary is inclusive");

            // Block 12: all three are past expiry and pruned in one pass
            // (the open set is bounded by MaxOpenProposals, so no cursor and
            // no starvation).
            System::set_block_number(12);
            GpnGovernance::on_initialize(12);
            assert_eq!(ProposalIds::<Test>::get().len(), 0);
            for i in 0..3u8 {
                let call = RuntimeCall::System(frame_system::Call::remark { remark: vec![i] });
                assert!(!Proposals::<Test>::contains_key(BlakeTwo256::hash_of(
                    &call
                )));
            }
            assert!(System::events().iter().any(|r| matches!(
                r.event,
                RuntimeEvent::GpnGovernance(Event::ProposalExpired { .. })
            )));
        });
    }

    #[test]
    fn on_initialize_keeps_unexpired_proposals() {
        new_test_ext().execute_with(|| {
            let hash = BlakeTwo256::hash_of(&remark_call());
            assert_ok!(GpnGovernance::propose(
                RuntimeOrigin::signed(1),
                Box::new(remark_call())
            ));
            System::set_block_number(5);
            GpnGovernance::on_initialize(5);
            assert!(Proposals::<Test>::contains_key(hash));
        });
    }

    // ── cancel ──────────────────────────────────────────────────────────────

    #[test]
    fn cancel_requires_the_full_threshold() {
        new_test_ext().execute_with(|| {
            let hash = BlakeTwo256::hash_of(&remark_call());
            assert_ok!(GpnGovernance::propose(
                RuntimeOrigin::signed(1),
                Box::new(remark_call())
            ));
            assert_ok!(GpnGovernance::approve(RuntimeOrigin::signed(2), hash));

            // 2 of 3 required approvals is not enough to cancel either.
            assert_noop!(
                GpnGovernance::cancel(RuntimeOrigin::signed(1), hash),
                Error::<Test>::NotEnoughApprovals
            );
            assert!(Proposals::<Test>::contains_key(hash));

            assert_ok!(GpnGovernance::approve(RuntimeOrigin::signed(3), hash));
            assert_ok!(GpnGovernance::cancel(RuntimeOrigin::signed(3), hash));
            assert!(!Proposals::<Test>::contains_key(hash));

            assert!(System::events().iter().any(|r| matches!(
                r.event,
                RuntimeEvent::GpnGovernance(Event::ProposalCancelled { proposal, .. })
                    if proposal == hash
            )));
        });
    }

    #[test]
    fn cancel_by_non_member_and_unknown_proposal() {
        new_test_ext().execute_with(|| {
            let hash = BlakeTwo256::hash_of(&remark_call());
            assert_ok!(GpnGovernance::propose(
                RuntimeOrigin::signed(1),
                Box::new(remark_call())
            ));
            assert_ok!(GpnGovernance::approve(RuntimeOrigin::signed(2), hash));
            assert_ok!(GpnGovernance::approve(RuntimeOrigin::signed(3), hash));

            assert_noop!(
                GpnGovernance::cancel(RuntimeOrigin::signed(99), hash),
                Error::<Test>::NotMember
            );
            assert_noop!(
                GpnGovernance::cancel(RuntimeOrigin::signed(1), BlakeTwo256::hash(b"nope")),
                Error::<Test>::ProposalNotFound
            );
        });
    }

    // ── membership ──────────────────────────────────────────────────────────

    #[test]
    fn set_members_is_root_only() {
        new_test_ext().execute_with(|| {
            assert_noop!(
                GpnGovernance::set_members(RuntimeOrigin::signed(1), vec![7, 8, 9]),
                DispatchError::BadOrigin
            );
            assert_ok!(GpnGovernance::set_members(
                RuntimeOrigin::root(),
                vec![7, 8, 9]
            ));
            assert_eq!(MemberCount::<Test>::get(), 3);
        });
    }

    #[test]
    fn set_members_bounds() {
        new_test_ext().execute_with(|| {
            assert_noop!(
                GpnGovernance::set_members(RuntimeOrigin::root(), vec![]),
                Error::<Test>::EmptyMembers
            );
            // MaxMembers = 8
            assert_noop!(
                GpnGovernance::set_members(RuntimeOrigin::root(), (1..=9u64).collect::<Vec<_>>()),
                Error::<Test>::TooManyMembers
            );
            assert_noop!(
                GpnGovernance::set_members(RuntimeOrigin::root(), vec![1, 2, 1]),
                Error::<Test>::DuplicateMember
            );
            // ... and nothing was changed by the rejections
            assert_eq!(MemberCount::<Test>::get(), 5);
        });
    }

    #[test]
    fn set_members_replaces_the_set_and_emits() {
        new_test_ext().execute_with(|| {
            assert_ok!(GpnGovernance::set_members(
                RuntimeOrigin::root(),
                vec![10, 11, 12, 13, 14]
            ));
            assert_eq!(MemberCount::<Test>::get(), 5);
            for m in [10, 11, 12, 13, 14] {
                assert!(Members::<Test>::contains_key(m));
            }
            for m in [1, 2, 3, 4, 5] {
                assert!(!Members::<Test>::contains_key(m));
            }
            assert!(System::events().iter().any(|r| matches!(
                r.event,
                RuntimeEvent::GpnGovernance(Event::MembersChanged { .. })
            )));
            // Old members lose their voting rights immediately.
            assert_noop!(
                GpnGovernance::propose(RuntimeOrigin::signed(1), Box::new(remark_call())),
                Error::<Test>::NotMember
            );
        });
    }

    // ── preimage ────────────────────────────────────────────────────────────

    #[test]
    fn preimage_propose_peek_approve_execute() {
        new_test_ext().execute_with(|| {
            let (_, hash, bytes) = preimage(&remark_call().encode());
            let id = GpnGovernance::preimage_proposal_id(hash, bytes.len() as u32);

            assert_ok!(GpnGovernance::propose_preimage(
                RuntimeOrigin::signed(1),
                hash,
                bytes.clone()
            ));

            // Public, pre-timelock inspection of exactly what will be enacted.
            assert_eq!(GpnGovernance::peek(hash), Some(bytes.clone()));
            let p = Proposals::<Test>::get(id).expect("proposal stored");
            assert!(matches!(
                p.call,
                ProposalCall::Preimage { hash: h, len } if h == hash && len as usize == bytes.len()
            ));
            assert_eq!(p.approvals.len(), 1, "proposer self-approves");

            // The id is content-addressed and distinct from the inline id.
            assert_ne!(id, BlakeTwo256::hash(&bytes));

            assert_ok!(GpnGovernance::approve(RuntimeOrigin::signed(2), id));
            assert_ok!(GpnGovernance::approve(RuntimeOrigin::signed(3), id));
            System::set_block_number(1 + 3);
            assert_ok!(GpnGovernance::execute(RuntimeOrigin::signed(9), id));

            assert!(!Proposals::<Test>::contains_key(id));
            assert_eq!(GpnGovernance::peek(hash), None, "preimage consumed");
        });
    }

    #[test]
    fn preimage_hash_mismatch_and_bad_encoding_rejected() {
        new_test_ext().execute_with(|| {
            let (_, hash, bytes) = preimage(&remark_call().encode());

            assert_noop!(
                GpnGovernance::propose_preimage(
                    RuntimeOrigin::signed(1),
                    BlakeTwo256::hash(b"other"),
                    bytes.clone()
                ),
                Error::<Test>::PreimageMismatch
            );

            // Valid call ++ trailing garbage: hash is right, encoding is not.
            let mut bad = bytes.clone();
            bad.extend_from_slice(&[0xff, 0xff]);
            assert_noop!(
                GpnGovernance::propose_preimage(
                    RuntimeOrigin::signed(1),
                    BlakeTwo256::hash(&bad),
                    bad
                ),
                Error::<Test>::InvalidCallEncoding
            );

            // Non-member cannot note a preimage.
            assert_noop!(
                GpnGovernance::propose_preimage(RuntimeOrigin::signed(99), hash, bytes),
                Error::<Test>::NotMember
            );
        });
    }

    #[test]
    fn missing_preimage_leaves_the_proposal_intact() {
        new_test_ext().execute_with(|| {
            let (_, hash, bytes) = preimage(&remark_call().encode());
            let id = GpnGovernance::preimage_proposal_id(hash, bytes.len() as u32);
            assert_ok!(GpnGovernance::propose_preimage(
                RuntimeOrigin::signed(1),
                hash,
                bytes.clone()
            ));
            assert_ok!(GpnGovernance::approve(RuntimeOrigin::signed(2), id));
            assert_ok!(GpnGovernance::approve(RuntimeOrigin::signed(3), id));
            System::set_block_number(1 + 3);

            // Simulate the preimage disappearing (e.g. a future `unnote`).
            Preimages::<Test>::remove(hash);

            assert_noop!(
                GpnGovernance::execute(RuntimeOrigin::signed(9), id),
                Error::<Test>::PreimageMissing
            );
            // Recoverable: the proposal (and its approvals) survive.
            assert!(Proposals::<Test>::contains_key(id));

            // Re-note the same bytes (same hash ⇒ same proposal id) and
            // execute the existing, already-approved proposal.
            assert_ok!(GpnGovernance::note_preimage(
                RuntimeOrigin::signed(1),
                hash,
                bytes
            ));
            assert_ok!(GpnGovernance::execute(RuntimeOrigin::signed(9), id));
        });
    }

    // ── migration ───────────────────────────────────────────────────────────

    #[test]
    fn migration_v0_to_v1_preserves_proposals_and_seeds_members() {
        let mut ext = new_test_ext();
        ext.execute_with(|| {
            set_storage_version(0);
            // An existing chain has only the committee-3 born at genesis.
            assert_ok!(GpnGovernance::set_members(
                RuntimeOrigin::root(),
                vec![1, 2, 3]
            ));
            assert_eq!(MemberCount::<Test>::get(), 3);

            // A pending proposal in the *old* (version 0) layout.
            let call = remark_call();
            let hash = BlakeTwo256::hash_of(&call);
            let old = OldProposal::<Test> {
                call: call.clone(),
                approvals: BoundedVec::try_from(vec![1, 2]).unwrap(),
                created_at: 1,
            };
            unhashed::put::<OldProposal<Test>>(&Proposals::<Test>::hashed_key_for(hash), &old);
            assert!(
                Proposals::<Test>::contains_key(hash),
                "legacy proposal written"
            );
            assert_eq!(
                ProposalIds::<Test>::get().len(),
                0,
                "no index before migration"
            );

            // Run the upgrade.
            GpnGovernance::on_runtime_upgrade();

            // 1. the proposal survived, re-encoded, with its approvals intact.
            let p = Proposals::<Test>::get(hash).expect("proposal preserved");
            assert_eq!(
                p.call,
                ProposalCall::Inline(call),
                "old inline call re-encoded into the new enum"
            );
            assert_eq!(p.approvals.to_vec(), vec![1, 2]);
            assert_eq!(p.created_at, 1);

            // 2. the committee grew to the seeded five, exactly once.
            assert_eq!(MemberCount::<Test>::get(), 5);
            for m in 1..=5u64 {
                assert!(Members::<Test>::contains_key(m));
            }
            // 3. storage version bumped.
            assert_eq!(
                <GpnGovernance as GetStorageVersion>::on_chain_storage_version(),
                StorageVersion::new(1)
            );
        });
    }

    #[test]
    fn migration_is_idempotent() {
        new_test_ext().execute_with(|| {
            set_storage_version(0);
            GpnGovernance::on_runtime_upgrade();
            assert_eq!(MemberCount::<Test>::get(), 5);

            // Remove two members, then run the migration again: it must be a
            // no-op (no re-seeding, no count change).
            assert_ok!(GpnGovernance::set_members(
                RuntimeOrigin::root(),
                vec![1, 2, 3]
            ));
            GpnGovernance::on_runtime_upgrade();
            assert_eq!(MemberCount::<Test>::get(), 3);
            assert_eq!(
                <GpnGovernance as GetStorageVersion>::on_chain_storage_version(),
                StorageVersion::new(1)
            );
        });
    }

    #[test]
    fn migration_never_drops_a_sitting_member() {
        new_test_ext().execute_with(|| {
            set_storage_version(0);
            // 42 is a sitting member who is not in the seed set.
            assert_ok!(GpnGovernance::set_members(
                RuntimeOrigin::root(),
                vec![1, 2, 42]
            ));
            GpnGovernance::on_runtime_upgrade();
            // union: 1, 2, 42 + 3, 4, 5
            assert_eq!(MemberCount::<Test>::get(), 6);
            assert!(Members::<Test>::contains_key(42));
        });
    }

    #[test]
    fn preimage_id_is_deterministic_and_length_sensitive() {
        new_test_ext().execute_with(|| {
            let h = BlakeTwo256::hash(b"bytes");
            let a = GpnGovernance::preimage_proposal_id(h, 10);
            let b = GpnGovernance::preimage_proposal_id(h, 10);
            let c = GpnGovernance::preimage_proposal_id(h, 11);
            assert_eq!(a, b);
            assert_ne!(a, c);
        });
    }
}
