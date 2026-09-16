//! # GPN Governance pallet (Option D)
//!
//! A minimal "technical committee" upgrade path for mainnet, so a non-single-key
//! body can change the chain (runtime upgrades via `set_code`, parameter changes)
//! without a live sudo key.
//!
//! A fixed set of members (the committee-3) can `propose` an arbitrary runtime
//! call and `approve` it. Once `ApprovalsRequired` (2) distinct members have
//! approved **and** `TimelockPeriod` (3 days) has elapsed since proposal, anyone
//! may `execute` the proposal — which dispatches the call with **Root** origin.
//!
//! This gives exactly the intended semantics: a 2-of-3 majority can upgrade the
//! chain, but only after a public timelock, and no single key can act alone.

#![cfg_attr(not(feature = "std"), no_std)]

pub use pallet::*;

#[frame_support::pallet]
pub mod pallet {
    use frame_support::pallet_prelude::*;
    use frame_system::pallet_prelude::*;
    use sp_runtime::traits::{Dispatchable, Hash, Saturating};
    use sp_std::prelude::*;

    #[pallet::pallet]
    #[pallet::without_storage_info]
    pub struct Pallet<T>(_);

    #[pallet::config]
    pub trait Config: frame_system::Config {
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;
        /// Distinct approvals required to execute (2 for a 2-of-3 committee).
        #[pallet::constant]
        type ApprovalsRequired: Get<u32>;
        /// Minimum blocks between proposal and execution (the timelock).
        #[pallet::constant]
        type TimelockPeriod: Get<BlockNumberFor<Self>>;
        /// Maximum number of members (bounds the approvals vector).
        #[pallet::constant]
        type MaxMembers: Get<u32>;
        type WeightInfo: WeightInfo;
    }

    /// The governance members (seeded at genesis).
    #[pallet::storage]
    pub type Members<T: Config> = StorageMap<_, Blake2_128Concat, T::AccountId, (), OptionQuery>;

    /// Number of members (for weight/validation bookkeeping).
    #[pallet::storage]
    pub type MemberCount<T: Config> = StorageValue<_, u32, ValueQuery>;

    /// A pending governance proposal awaiting threshold + timelock.
    #[derive(Clone, Encode, Decode, Eq, PartialEq, Debug, TypeInfo)]
    #[scale_info(skip_type_params(T))]
    pub struct Proposal<T: Config> {
        /// The call to execute as Root.
        pub call: T::RuntimeCall,
        /// Distinct member accounts that have approved.
        pub approvals: BoundedVec<T::AccountId, T::MaxMembers>,
        /// Block number the proposal was created.
        pub created_at: BlockNumberFor<T>,
    }

    /// Open proposals, keyed by the hash of the call.
    #[pallet::storage]
    pub type Proposals<T: Config> =
        StorageMap<_, Blake2_256, T::Hash, Proposal<T>, OptionQuery>;

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

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// A member proposes an arbitrary call (e.g. `set_code`) for Root execution.
        #[pallet::call_index(0)]
        #[pallet::weight(T::WeightInfo::propose())]
        pub fn propose(origin: OriginFor<T>, call: Box<T::RuntimeCall>) -> DispatchResult {
            let who = ensure_signed(origin)?;
            ensure!(Members::<T>::contains_key(&who), Error::<T>::NotMember);

            let proposal_hash = T::Hashing::hash_of(call.as_ref());
            ensure!(
                !Proposals::<T>::contains_key(proposal_hash),
                Error::<T>::ProposalExists
            );

            let mut approvals: BoundedVec<T::AccountId, T::MaxMembers> = BoundedVec::new();
            // The proposer self-approves.
            approvals
                .try_push(who.clone())
                .map_err(|_| Error::<T>::NotMember)?;

            let proposal = Proposal {
                call: *call,
                approvals,
                created_at: frame_system::Pallet::<T>::block_number(),
            };
            Proposals::<T>::insert(proposal_hash, proposal);

            Self::deposit_event(Event::Proposed {
                proposer: who,
                proposal: proposal_hash,
            });
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
                .map_err(|_| Error::<T>::NotMember)?;
            Proposals::<T>::insert(proposal_hash, proposal);

            Self::deposit_event(Event::Approved {
                member: who,
                proposal: proposal_hash,
            });
            Ok(())
        }

        /// Execute an approved proposal as Root, once threshold + timelock are met.
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
            ensure!(elapsed >= T::TimelockPeriod::get(), Error::<T>::TimelockNotElapsed);

            // Remove the proposal before dispatch (best-effort; a reentrant
            // failure leaves it gone, but Root calls here are trusted upgrades).
            Proposals::<T>::remove(proposal_hash);

            let call = proposal.call;
            let res = call.dispatch(frame_system::RawOrigin::<T::AccountId>::Root.into());
            res.map_err(|_| Error::<T>::ExecutionFailed)?;

            Self::deposit_event(Event::Executed {
                proposal: proposal_hash,
            });
            Ok(())
        }
    }

    /// Weight functions (unbenchmarked for now — `()` returns zero).
    pub trait WeightInfo {
        fn propose() -> Weight;
        fn approve() -> Weight;
        fn execute() -> Weight;
    }

    impl WeightInfo for () {
        fn propose() -> Weight {
            Weight::zero()
        }
        fn approve() -> Weight {
            Weight::zero()
        }
        fn execute() -> Weight {
            Weight::zero()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate as pallet_gpn_governance;
        use frame_support::{assert_noop, assert_ok, construct_runtime, derive_impl, parameter_types,
            traits::{ConstU16, ConstU64}};
        use sp_runtime::{
            traits::{BlakeTwo256, IdentityLookup},
            BuildStorage,
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
            pub const ApprovalsRequired: u32 = 2;
            pub const TimelockPeriod: u64 = 3;
            pub const MaxMembers: u32 = 8;
        }

        impl pallet_gpn_governance::Config for Test {
            type RuntimeEvent = RuntimeEvent;
            type ApprovalsRequired = ApprovalsRequired;
            type TimelockPeriod = TimelockPeriod;
            type MaxMembers = MaxMembers;
            type WeightInfo = ();
        }

        fn new_test_ext() -> sp_io::TestExternalities {
            let mut t = frame_system::GenesisConfig::<Test>::default().build_storage().unwrap();
            pallet_gpn_governance::GenesisConfig::<Test> {
                initial_members: vec![1, 2, 3],
                _config: Default::default(),
            }
            .assimilate_storage(&mut t)
            .unwrap();
            t.into()
        }

        #[test]
        fn non_member_cannot_propose() {
            new_test_ext().execute_with(|| {
                let call = RuntimeCall::System(frame_system::Call::remark { remark: vec![] });
                assert_noop!(
                    GpnGovernance::propose(RuntimeOrigin::signed(99), Box::new(call)),
                    Error::<Test>::NotMember
                );
            });
        }

        #[test]
        fn requires_two_approvals_and_timelock_before_root_execution() {
            new_test_ext().execute_with(|| {
                // A root-gated call: `kill_storage` (root-only) with no keys trivially
                // succeeds iff dispatched as Root, so Ok proves the Root origin.
                let call = RuntimeCall::System(frame_system::Call::kill_storage { keys: vec![] });
                let hash = BlakeTwo256::hash_of(&call);

                // Member 1 proposes (self-approves) -> 1 approval.
                assert_ok!(GpnGovernance::propose(
                    RuntimeOrigin::signed(1),
                    Box::new(call)
                ));

                // 1 approval is not enough.
                assert_noop!(
                    GpnGovernance::execute(RuntimeOrigin::signed(9), hash),
                    Error::<Test>::NotEnoughApprovals
                );

                // Member 2 approves -> 2 approvals (threshold met).
                assert_ok!(GpnGovernance::approve(RuntimeOrigin::signed(2), hash));

                // Timelock (3 blocks) not yet elapsed at block 1.
                assert_noop!(
                    GpnGovernance::execute(RuntimeOrigin::signed(9), hash),
                    Error::<Test>::TimelockNotElapsed
                );

                // Advance past the timelock.
                System::set_block_number(4);

                // Now it executes as Root and the proposal is removed.
                assert_ok!(GpnGovernance::execute(RuntimeOrigin::signed(9), hash));
                assert!(!Proposals::<Test>::contains_key(hash));
            });
        }
    }
}
