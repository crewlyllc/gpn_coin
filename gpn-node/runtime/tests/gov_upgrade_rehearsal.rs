//! Full **governance enactment** rehearsal against the real `Runtime`.
//!
//! The live-chain rehearsal (`gpn_gov_upgrade` against a dev node) can only go
//! as far as the timelock: `GovernanceTimelockPeriod` is 3 days (17 280 blocks)
//! of *real* blocks and is a runtime **constant**, so no CLI trick shortens it
//! (faking `System.Number` trips frame_system's
//! "Block number must be strictly increasing" assertion and halts the chain).
//!
//! This test therefore completes the rehearsal in-process, on the real
//! `Runtime` (the same pallet set, the same `frame_system::set_code` path, the
//! same WASM), by fast-forwarding the block number with
//! `System::set_block_number` — which is exactly what test externalities are
//! for.
//!
//! It asserts the whole governance contract:
//!   propose (member only, idempotent by call hash)
//!   -> approve (distinct members, threshold 2)
//!   -> timelock (17 280 blocks, exact boundary)
//!   -> execute (anyone) dispatches with **Root**
//!   -> `System.set_code` writes `:code` and emits `CodeUpdated`
//!   -> a second proposal can replace it again (the rollback mechanism).

use codec::Encode;
use frame_support::{assert_noop, assert_ok, storage::unhashed, traits::BuildGenesisConfig};
use gpn_runtime::{GpnGovernance, Runtime, RuntimeCall, RuntimeEvent, RuntimeOrigin, System};
use pallet_gpn_governance::Error as GovError;
use sp_core::H256;
use sp_runtime::traits::Dispatchable;
use sp_runtime::traits::{BlakeTwo256, Hash};
use sp_runtime::BuildStorage;

type AccountId = <Runtime as frame_system::Config>::AccountId;

const ALICE: [u8; 32] = [1u8; 32];
const BOB: [u8; 32] = [2u8; 32];
const CHARLIE: [u8; 32] = [3u8; 32];
const MALLORY: [u8; 32] = [9u8; 32];

fn acct(raw: [u8; 32]) -> AccountId {
    AccountId::new(raw)
}

fn new_test_ext() -> sp_io::TestExternalities {
    let mut storage = frame_system::GenesisConfig::<Runtime>::default()
        .build_storage()
        .expect("system genesis");
    pallet_gpn_governance::GenesisConfig::<Runtime> {
        initial_members: vec![acct(ALICE), acct(BOB)],
        _config: Default::default(),
    }
    .assimilate_storage(&mut storage)
    .expect("governance genesis");
    let mut ext: sp_io::TestExternalities = storage.into();
    ext.execute_with(|| System::set_block_number(1));
    ext
}

fn timelock() -> u32 {
    <Runtime as pallet_gpn_governance::Config>::TimelockPeriod::get()
}

fn approvals_required() -> u32 {
    <Runtime as pallet_gpn_governance::Config>::ApprovalsRequired::get()
}

/// A structurally valid runtime WASM: the runtime's own bundled artifact.
fn runtime_wasm() -> Vec<u8> {
    gpn_runtime::WASM_BINARY
        .expect("the runtime crate embeds the canonical WASM")
        .to_vec()
}

fn code_in_storage() -> Option<Vec<u8>> {
    unhashed::get_raw(b":code")
}

fn code_updated_events() -> Vec<H256> {
    System::events()
        .into_iter()
        .filter_map(|r| match r.event {
            RuntimeEvent::System(frame_system::Event::CodeUpdated { hash }) => Some(hash),
            _ => None,
        })
        .collect()
}

/// The proposal id is `blake2_256(SCALE(RuntimeCall))` — the same value
/// `gpn_gov_upgrade` prints for committee members to approve.
fn call_hash(call: &RuntimeCall) -> H256 {
    <Runtime as frame_system::Config>::Hashing::hash(call.encode().as_slice())
}

#[test]
fn governance_config_is_the_documented_one() {
    assert_eq!(
        approvals_required(),
        2,
        "mainnet committee is 2-of-N; the runtime constant must stay 2"
    );
    assert_eq!(
        timelock(),
        17_280,
        "3 days at 15 s blocks — changing this changes the mainnet upgrade timeline"
    );
    assert_eq!(
        <Runtime as pallet_gpn_governance::Config>::MaxMembers::get(),
        8
    );
    new_test_ext().execute_with(|| {
        assert_eq!(pallet_gpn_governance::MemberCount::<Runtime>::get(), 2);
    });
}

#[test]
fn full_cycle_propose_approve_timelock_execute_installs_the_code() {
    new_test_ext().execute_with(|| {
        let wasm = runtime_wasm();
        let original = code_in_storage();
        assert_ne!(
            original.as_deref(),
            Some(wasm.as_slice()),
            "the test needs the proposed code to differ from what is already installed"
        );

        // `set_code_without_checks` is used because `System.set_code`'s version
        // gate calls the *wasm executor* (`sp_io::misc::runtime_version`), which
        // does not exist in a native test host. Everything else — Root origin,
        // the pallet's checks, the storage write, the event — is identical.
        let call =
            RuntimeCall::System(frame_system::Call::set_code_without_checks { code: wasm.clone() });
        let hash = call_hash(&call);
        assert_eq!(
            hash,
            BlakeTwo256::hash(call.encode().as_slice()),
            "proposal id is blake2_256 of the encoded call"
        );

        // ── propose ───────────────────────────────────────────────────────
        assert_noop!(
            GpnGovernance::propose(RuntimeOrigin::signed(acct(MALLORY)), Box::new(call.clone())),
            GovError::<Runtime>::NotMember
        );
        assert_ok!(GpnGovernance::propose(
            RuntimeOrigin::signed(acct(ALICE)),
            Box::new(call.clone())
        ));
        // identical bytes cannot be proposed twice (the map is keyed by call hash)
        assert_noop!(
            GpnGovernance::propose(RuntimeOrigin::signed(acct(BOB)), Box::new(call.clone())),
            GovError::<Runtime>::ProposalExists
        );

        let p = pallet_gpn_governance::Proposals::<Runtime>::get(hash).expect("proposal stored");
        assert_eq!(p.approvals.len(), 1, "the proposer self-approves");
        assert_eq!(p.created_at, 1);

        // ── one approval is not enough ───────────────────────────────────
        assert_noop!(
            GpnGovernance::execute(RuntimeOrigin::signed(acct(CHARLIE)), hash),
            GovError::<Runtime>::NotEnoughApprovals
        );
        assert_noop!(
            GpnGovernance::approve(RuntimeOrigin::signed(acct(MALLORY)), hash),
            GovError::<Runtime>::NotMember
        );
        assert_ok!(GpnGovernance::approve(
            RuntimeOrigin::signed(acct(BOB)),
            hash
        ));
        assert_noop!(
            GpnGovernance::approve(RuntimeOrigin::signed(acct(BOB)), hash),
            GovError::<Runtime>::AlreadyApproved
        );

        // ── timelock boundary, exactly ───────────────────────────────────
        let timelock = timelock();
        System::set_block_number(1 + timelock - 1);
        assert_noop!(
            GpnGovernance::execute(RuntimeOrigin::signed(acct(CHARLIE)), hash),
            GovError::<Runtime>::TimelockNotElapsed
        );
        // ... and a failed execute must NOT consume the proposal
        assert!(pallet_gpn_governance::Proposals::<Runtime>::get(hash).is_some());

        System::set_block_number(1 + timelock);
        // anyone may execute — not just committee members
        assert_ok!(GpnGovernance::execute(
            RuntimeOrigin::signed(acct(CHARLIE)),
            hash
        ));

        // ── effects: code installed, event emitted, proposal consumed ────
        assert_eq!(
            code_in_storage().as_deref(),
            Some(wasm.as_slice()),
            ":code must be exactly the proposed bytes"
        );
        assert_eq!(
            code_updated_events(),
            vec![BlakeTwo256::hash(&wasm)],
            "System::CodeUpdated carries blake2_256(:code)"
        );
        assert!(pallet_gpn_governance::Proposals::<Runtime>::get(hash).is_none());
        assert!(pallet_gpn_governance::Proposals::<Runtime>::get(hash).is_none());
    });
}

/// A second governance upgrade can replace the first — which is exactly the
/// mechanism a rollback uses (see docs/upgrades-governance.md §6).
#[test]
fn a_second_upgrade_replaces_the_first() {
    new_test_ext().execute_with(|| {
        let first = runtime_wasm();
        let second = {
            // Any different byte string proves the storage write, without
            // needing a second real WASM artifact.
            let mut v = runtime_wasm();
            v.extend_from_slice(b"\0\0\0\0");
            v
        };

        for (i, code) in [first.clone(), second.clone()].into_iter().enumerate() {
            let call = RuntimeCall::System(frame_system::Call::set_code_without_checks {
                code: code.clone(),
            });
            let hash = call_hash(&call);
            let proposer = if i == 0 { ALICE } else { BOB };
            assert_ok!(GpnGovernance::propose(
                RuntimeOrigin::signed(acct(proposer)),
                Box::new(call)
            ));
            let other = if i == 0 { BOB } else { ALICE };
            assert_ok!(GpnGovernance::approve(
                RuntimeOrigin::signed(acct(other)),
                hash
            ));
            System::set_block_number(1 + timelock() * (i as u32 + 1));
            assert_ok!(GpnGovernance::execute(
                RuntimeOrigin::signed(acct(CHARLIE)),
                hash
            ));
            assert_eq!(code_in_storage().as_deref(), Some(code.as_slice()));
        }
        assert_eq!(code_updated_events().len(), 2);
    });
}

/// Root-only calls stay unreachable without governance: a plain signed account
/// cannot call `System.set_code_without_checks` directly.
#[test]
fn set_code_is_root_only_outside_governance() {
    new_test_ext().execute_with(|| {
        let call =
            RuntimeCall::System(frame_system::Call::set_code_without_checks { code: vec![0u8; 8] });
        assert_noop!(
            call.dispatch(RuntimeOrigin::signed(acct(ALICE))),
            sp_runtime::DispatchError::BadOrigin
        );
        // ... and the governance path reaches it (proved by the cycle test).
    });
}
