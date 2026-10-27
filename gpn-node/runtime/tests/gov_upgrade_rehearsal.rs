//! Full **governance enactment** rehearsal against the real `Runtime` (spec 42).
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
//! It asserts the whole hardened governance contract:
//!   propose (member only, idempotent by call hash)
//!   -> approve (distinct members, threshold **3 of 5**)
//!   -> timelock (17 280 blocks, exact boundary)
//!   -> execute (anyone) dispatches with **Root**
//!   -> `System.set_code` writes `:code` and emits `CodeUpdated`
//!   -> a second proposal can replace it again (the rollback mechanism)
//! plus expiry, cancel, hash-addressed preimages and the v0 → v1 migration.

use codec::Encode;
use frame_support::{
    assert_noop, assert_ok,
    storage::unhashed,
    traits::{BuildGenesisConfig, GetStorageVersion, Hooks, StorageVersion},
};
use gpn_runtime::{
    governance_committee_seed, GpnGovernance, Runtime, RuntimeCall, RuntimeEvent, RuntimeOrigin,
    System,
};
use pallet_gpn_governance::{Error as GovError, ProposalCall};
use sp_core::{crypto::Ss58Codec, H256};
use sp_runtime::traits::Dispatchable;
use sp_runtime::traits::{BlakeTwo256, Hash};
use sp_runtime::BuildStorage;

type AccountId = <Runtime as frame_system::Config>::AccountId;

/// The five committee accounts, in `governance-keys.txt` order.
const COMMITTEE_SS58: [&str; 5] = [
    "5HTWBtWro5ARwrghSmbkcbhVKk8o7xxYirdR7J8T3PJRteWN", // committee-1
    "5Cd5NiY2qkGzVuQufh844DwKj2k4ExxwyzyrDU4pGUvEX3Dx", // committee-2
    "5HeUVTK6zd9ii8nXoXBbRU5Q2zdE1Moc9zd1E9GW5JqYej5h", // committee-3
    "5DjziBJUZnD1ZVzLh2mGEW3DNA4bThyGSVCY6e5jMzqnJ1TC", // committee-4
    "5DPBfcDbf9knHdu77aNsyZ2cvFPPn8vKRupLZTDewUi43FtM", // committee-5
];

const MALLORY: [u8; 32] = [9u8; 32];

fn acct(raw: [u8; 32]) -> AccountId {
    AccountId::new(raw)
}

fn committee() -> Vec<AccountId> {
    COMMITTEE_SS58
        .iter()
        .map(|s| AccountId::from_string(s).expect("valid SS58; qed"))
        .collect()
}

fn new_test_ext() -> sp_io::TestExternalities {
    let mut storage = frame_system::GenesisConfig::<Runtime>::default()
        .build_storage()
        .expect("system genesis");
    pallet_gpn_governance::GenesisConfig::<Runtime> {
        initial_members: governance_committee_seed(),
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

fn expiry() -> u32 {
    <Runtime as pallet_gpn_governance::Config>::ProposalExpiry::get()
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

/// The inline proposal id is `blake2_256(SCALE(RuntimeCall))` — the same value
/// `gpn_gov_upgrade` prints for committee members to approve.
fn call_hash(call: &RuntimeCall) -> H256 {
    <Runtime as frame_system::Config>::Hashing::hash(call.encode().as_slice())
}

fn set_code_call(code: Vec<u8>) -> RuntimeCall {
    // `set_code_without_checks` is used because `System.set_code`'s version
    // gate calls the *wasm executor* (`sp_io::misc::runtime_version`), which
    // does not exist in a native test host. Everything else — Root origin, the
    // pallet's checks, the storage write, the event — is identical.
    RuntimeCall::System(frame_system::Call::set_code_without_checks { code })
}

#[test]
fn governance_config_is_the_documented_one() {
    assert_eq!(
        approvals_required(),
        3,
        "committee is 3-of-5 after the hardening upgrade"
    );
    assert_eq!(
        timelock(),
        if cfg!(feature = "testnet-fast-timelock") {
            10
        } else {
            17_280
        },
        "3 days at 15 s blocks on mainnet (feature off); 10 blocks on a \
         testnet-fast-timelock build — changing the default changes the mainnet \
         upgrade timeline"
    );
    assert_eq!(
        expiry(),
        14 * 5_760,
        "14 days at 15 s blocks — proposals expire instead of lingering forever"
    );
    assert_eq!(
        <Runtime as pallet_gpn_governance::Config>::MaxMembers::get(),
        8
    );
    assert_eq!(
        <Runtime as pallet_gpn_governance::Config>::MaxOpenProposals::get(),
        64
    );
    new_test_ext().execute_with(|| {
        assert_eq!(pallet_gpn_governance::MemberCount::<Runtime>::get(), 5);
    });
}

/// The runtime's raw-byte seed must be exactly the five SS58 addresses in
/// `governance-keys.txt` (guards against a typo'd hex constant).
#[test]
fn committee_seed_matches_governance_keys() {
    let seeded = governance_committee_seed();
    assert_eq!(seeded.len(), 5);
    for (i, expected) in committee().iter().enumerate() {
        assert_eq!(
            &seeded[i],
            expected,
            "committee-{} differs from the SS58 in governance-keys.txt",
            i + 1
        );
        assert_eq!(seeded[i].to_ss58check(), COMMITTEE_SS58[i]);
    }
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

        let call = set_code_call(wasm.clone());
        let hash = call_hash(&call);
        assert_eq!(
            hash,
            BlakeTwo256::hash(call.encode().as_slice()),
            "proposal id is blake2_256 of the encoded call"
        );

        let c = committee();

        // ── propose ───────────────────────────────────────────────────────
        assert_noop!(
            GpnGovernance::propose(RuntimeOrigin::signed(acct(MALLORY)), Box::new(call.clone())),
            GovError::<Runtime>::NotMember
        );
        assert_ok!(GpnGovernance::propose(
            RuntimeOrigin::signed(c[0].clone()),
            Box::new(call.clone())
        ));
        assert_noop!(
            GpnGovernance::propose(RuntimeOrigin::signed(c[1].clone()), Box::new(call.clone())),
            GovError::<Runtime>::ProposalExists
        );

        let p = pallet_gpn_governance::Proposals::<Runtime>::get(hash).expect("proposal stored");
        assert_eq!(p.approvals.len(), 1, "the proposer self-approves");
        assert_eq!(p.created_at, 1);
        assert!(matches!(p.call, ProposalCall::Inline(_)));

        // ── threshold: 2 of 5 is not enough ──────────────────────────────
        assert_ok!(GpnGovernance::approve(
            RuntimeOrigin::signed(c[1].clone()),
            hash
        ));
        assert_noop!(
            GpnGovernance::execute(RuntimeOrigin::signed(acct(MALLORY)), hash),
            GovError::<Runtime>::NotEnoughApprovals
        );
        assert_noop!(
            GpnGovernance::approve(RuntimeOrigin::signed(acct(MALLORY)), hash),
            GovError::<Runtime>::NotMember
        );
        assert_ok!(GpnGovernance::approve(
            RuntimeOrigin::signed(c[2].clone()),
            hash
        ));
        assert_noop!(
            GpnGovernance::approve(RuntimeOrigin::signed(c[2].clone()), hash),
            GovError::<Runtime>::AlreadyApproved
        );
        assert_eq!(
            pallet_gpn_governance::Proposals::<Runtime>::get(hash)
                .unwrap()
                .approvals
                .len(),
            3
        );

        // ── timelock boundary, exactly ───────────────────────────────────
        let timelock = timelock();
        System::set_block_number(1 + timelock - 1);
        assert_noop!(
            GpnGovernance::execute(RuntimeOrigin::signed(acct(MALLORY)), hash),
            GovError::<Runtime>::TimelockNotElapsed
        );
        assert!(pallet_gpn_governance::Proposals::<Runtime>::get(hash).is_some());

        System::set_block_number(1 + timelock);
        // anyone may execute — not just committee members
        assert_ok!(GpnGovernance::execute(
            RuntimeOrigin::signed(acct(MALLORY)),
            hash
        ));

        // ── effects ──────────────────────────────────────────────────────
        assert_eq!(code_in_storage().as_deref(), Some(wasm.as_slice()));
        assert_eq!(
            code_updated_events(),
            vec![BlakeTwo256::hash(&wasm)],
            "System::CodeUpdated carries blake2_256(:code)"
        );
        assert!(pallet_gpn_governance::Proposals::<Runtime>::get(hash).is_none());
        assert_eq!(
            pallet_gpn_governance::ProposalIds::<Runtime>::get().len(),
            0,
            "the open-proposal index is kept in sync"
        );
    });
}

/// A second governance upgrade can replace the first — the rollback mechanism.
#[test]
fn a_second_upgrade_replaces_the_first() {
    new_test_ext().execute_with(|| {
        let c = committee();
        let first = runtime_wasm();
        let second = {
            // Any different byte string proves the storage write without
            // needing a second real WASM artifact.
            let mut v = runtime_wasm();
            v.extend_from_slice(b"\0\0\0\0");
            v
        };

        for (i, code) in [first.clone(), second.clone()].into_iter().enumerate() {
            let call = set_code_call(code.clone());
            let hash = call_hash(&call);
            assert_ok!(GpnGovernance::propose(
                RuntimeOrigin::signed(c[i].clone()),
                Box::new(call)
            ));
            assert_ok!(GpnGovernance::approve(
                RuntimeOrigin::signed(c[i + 1].clone()),
                hash
            ));
            assert_ok!(GpnGovernance::approve(
                RuntimeOrigin::signed(c[i + 2].clone()),
                hash
            ));
            System::set_block_number(1 + timelock() * (i as u32 + 1));
            assert_ok!(GpnGovernance::execute(
                RuntimeOrigin::signed(acct(MALLORY)),
                hash
            ));
            assert_eq!(code_in_storage().as_deref(), Some(code.as_slice()));
        }
        assert_eq!(code_updated_events().len(), 2);
    });
}

#[test]
fn root_only_calls_stay_unreachable_outside_governance() {
    new_test_ext().execute_with(|| {
        let call = set_code_call(vec![0u8; 8]);
        assert_noop!(
            call.dispatch(RuntimeOrigin::signed(acct(MALLORY))),
            sp_runtime::DispatchError::BadOrigin
        );
        // `set_members` is Root-gated, so the committee itself cannot replace
        // itself except through propose → threshold → timelock → execute.
        assert_noop!(
            GpnGovernance::set_members(RuntimeOrigin::signed(committee()[0].clone()), committee()),
            sp_runtime::DispatchError::BadOrigin
        );
    });
}

// ── expiry ──────────────────────────────────────────────────────────────────

#[test]
fn execute_at_exactly_expiry_succeeds_and_one_block_later_fails() {
    new_test_ext().execute_with(|| {
        let c = committee();

        // Two proposals, both created at block 1 (the expiry clock starts there).
        let call = set_code_call(runtime_wasm());
        let hash = call_hash(&call);
        assert_ok!(GpnGovernance::propose(
            RuntimeOrigin::signed(c[0].clone()),
            Box::new(call)
        ));
        assert_ok!(GpnGovernance::approve(
            RuntimeOrigin::signed(c[1].clone()),
            hash
        ));
        assert_ok!(GpnGovernance::approve(
            RuntimeOrigin::signed(c[2].clone()),
            hash
        ));

        let call2 = set_code_call({
            let mut v = runtime_wasm();
            v.push(1);
            v
        });
        let hash2 = call_hash(&call2);
        assert_ok!(GpnGovernance::propose(
            RuntimeOrigin::signed(c[3].clone()),
            Box::new(call2)
        ));
        assert_ok!(GpnGovernance::approve(
            RuntimeOrigin::signed(c[4].clone()),
            hash2
        ));
        assert_ok!(GpnGovernance::approve(
            RuntimeOrigin::signed(c[0].clone()),
            hash2
        ));

        // +14 days is still (just) admissible.
        System::set_block_number(1 + expiry());
        assert_ok!(GpnGovernance::execute(
            RuntimeOrigin::signed(acct(MALLORY)),
            hash
        ));

        // One block past expiry: refused.
        System::set_block_number(1 + expiry() + 1);
        assert_noop!(
            GpnGovernance::execute(RuntimeOrigin::signed(acct(MALLORY)), hash2),
            GovError::<Runtime>::ProposalExpired
        );

        // ... and `on_initialize` prunes it.
        GpnGovernance::on_initialize(1 + expiry() + 1);
        assert!(pallet_gpn_governance::Proposals::<Runtime>::get(hash2).is_none());
        assert!(System::events().iter().any(|r| matches!(
            r.event,
            RuntimeEvent::GpnGovernance(
                pallet_gpn_governance::Event::ProposalExpired { proposal }
            ) if proposal == hash2
        )));
    });
}

// ── cancel ──────────────────────────────────────────────────────────────────

#[test]
fn cancel_requires_the_threshold_and_emits() {
    new_test_ext().execute_with(|| {
        let c = committee();
        let call = set_code_call(runtime_wasm());
        let hash = call_hash(&call);
        assert_ok!(GpnGovernance::propose(
            RuntimeOrigin::signed(c[0].clone()),
            Box::new(call)
        ));
        assert_ok!(GpnGovernance::approve(
            RuntimeOrigin::signed(c[1].clone()),
            hash
        ));
        // 2 of 3 required approvals: no veto, no cancel.
        assert_noop!(
            GpnGovernance::cancel(RuntimeOrigin::signed(c[0].clone()), hash),
            GovError::<Runtime>::NotEnoughApprovals
        );
        assert_ok!(GpnGovernance::approve(
            RuntimeOrigin::signed(c[2].clone()),
            hash
        ));
        assert_ok!(GpnGovernance::cancel(
            RuntimeOrigin::signed(c[3].clone()),
            hash
        ));
        assert!(pallet_gpn_governance::Proposals::<Runtime>::get(hash).is_none());
        assert_eq!(
            pallet_gpn_governance::ProposalIds::<Runtime>::get().len(),
            0
        );
    });
}

// ── hash-addressed preimages ────────────────────────────────────────────────

/// The rehearsal the design doc asks for: note a real ~500 KB artifact as a
/// preimage, propose **by hash**, fast-forward the timelock, execute, and check
/// that `:code` is exactly the bytes whose hash was approved.
#[test]
fn preimage_proposal_installs_the_hashed_artifact() {
    new_test_ext().execute_with(|| {
        let c = committee();
        let wasm = runtime_wasm();
        let call = set_code_call(wasm.clone());
        let bytes = call.encode();
        assert!(
            bytes.len() > 400_000,
            "rehearse with a realistic artifact ({} bytes)",
            bytes.len()
        );

        let code_hash = BlakeTwo256::hash(&bytes);
        let len = bytes.len() as u32;
        let id = GpnGovernance::preimage_proposal_id(code_hash, len);
        assert_ne!(id, call_hash(&call), "preimage ids use a domain tag");

        // Not a member: rejected.
        assert_noop!(
            GpnGovernance::propose_preimage(
                RuntimeOrigin::signed(acct(MALLORY)),
                code_hash,
                bytes.clone()
            ),
            GovError::<Runtime>::NotMember
        );

        assert_ok!(GpnGovernance::propose_preimage(
            RuntimeOrigin::signed(c[0].clone()),
            code_hash,
            bytes.clone()
        ));

        // Verifiable by hash, before the timelock elapses.
        assert_eq!(GpnGovernance::peek(code_hash), Some(bytes.clone()));

        let p = pallet_gpn_governance::Proposals::<Runtime>::get(id).expect("proposal");
        assert!(matches!(
            p.call,
            ProposalCall::Preimage { hash, len: l } if hash == code_hash && l == len
        ));

        assert_ok!(GpnGovernance::approve(
            RuntimeOrigin::signed(c[1].clone()),
            id
        ));
        assert_ok!(GpnGovernance::approve(
            RuntimeOrigin::signed(c[2].clone()),
            id
        ));
        System::set_block_number(1 + timelock());
        assert_ok!(GpnGovernance::execute(
            RuntimeOrigin::signed(acct(MALLORY)),
            id
        ));

        assert_eq!(code_in_storage().as_deref(), Some(wasm.as_slice()));
        assert_eq!(code_updated_events(), vec![BlakeTwo256::hash(&wasm)]);
        assert_eq!(GpnGovernance::peek(code_hash), None, "preimage consumed");
        assert!(pallet_gpn_governance::Proposals::<Runtime>::get(id).is_none());
    });
}

/// A missing preimage is recoverable: the proposal survives, and re-noting the
/// same bytes (same hash ⇒ same id) makes it executable again.
#[test]
fn missing_preimage_is_recoverable() {
    new_test_ext().execute_with(|| {
        let c = committee();
        let call = set_code_call(runtime_wasm());
        let bytes = call.encode();
        let code_hash = BlakeTwo256::hash(&bytes);
        let id = GpnGovernance::preimage_proposal_id(code_hash, bytes.len() as u32);

        assert_ok!(GpnGovernance::propose_preimage(
            RuntimeOrigin::signed(c[0].clone()),
            code_hash,
            bytes.clone()
        ));
        assert_ok!(GpnGovernance::approve(
            RuntimeOrigin::signed(c[1].clone()),
            id
        ));
        assert_ok!(GpnGovernance::approve(
            RuntimeOrigin::signed(c[2].clone()),
            id
        ));
        System::set_block_number(1 + timelock());

        // Simulate the preimage being dropped.
        unhashed::kill(&pallet_gpn_governance::Preimages::<Runtime>::hashed_key_for(code_hash));

        assert_noop!(
            GpnGovernance::execute(RuntimeOrigin::signed(acct(MALLORY)), id),
            GovError::<Runtime>::PreimageMissing
        );
        assert!(
            pallet_gpn_governance::Proposals::<Runtime>::get(id).is_some(),
            "the proposal (and its approvals) survive a missing preimage"
        );

        assert_ok!(GpnGovernance::note_preimage(
            RuntimeOrigin::signed(c[3].clone()),
            code_hash,
            bytes
        ));
        assert_ok!(GpnGovernance::execute(
            RuntimeOrigin::signed(acct(MALLORY)),
            id
        ));
        assert!(code_in_storage().is_some());
    });
}

// ── migration ───────────────────────────────────────────────────────────────

/// Rehearse the v0 → v1 migration on the real runtime: a pre-hardening
/// proposal is preserved (with its approvals), the committee grows to five, and
/// the storage version is bumped.
#[test]
fn migration_v0_to_v1_on_the_real_runtime() {
    new_test_ext().execute_with(|| {
        StorageVersion::new(0).put::<GpnGovernance>();
        assert_eq!(
            <GpnGovernance as GetStorageVersion>::on_chain_storage_version(),
            StorageVersion::new(0)
        );

        // A legacy (inline, non-enum) proposal written by hand.
        let call = set_code_call(runtime_wasm());
        let hash = call_hash(&call);
        let old = pallet_gpn_governance::migrations::OldProposal::<Runtime> {
            call: call.clone(),
            approvals: Default::default(),
            created_at: 1,
        };
        let mut old = old;
        old.approvals
            .try_push(committee()[0].clone())
            .expect("1 of 8");
        unhashed::put::<pallet_gpn_governance::migrations::OldProposal<Runtime>>(
            &pallet_gpn_governance::Proposals::<Runtime>::hashed_key_for(hash),
            &old,
        );

        GpnGovernance::on_runtime_upgrade();

        let p = pallet_gpn_governance::Proposals::<Runtime>::get(hash)
            .expect("legacy proposal preserved by the migration");
        assert_eq!(p.approvals.len(), 1);
        assert_eq!(p.created_at, 1);
        assert!(matches!(p.call, ProposalCall::Inline(_)));
        assert_eq!(
            pallet_gpn_governance::ProposalIds::<Runtime>::get().to_vec(),
            vec![hash],
            "the migration rebuilds the open-proposal index"
        );
        assert_eq!(pallet_gpn_governance::MemberCount::<Runtime>::get(), 5);
        assert_eq!(
            <GpnGovernance as GetStorageVersion>::on_chain_storage_version(),
            StorageVersion::new(1)
        );

        // Idempotent.
        GpnGovernance::on_runtime_upgrade();
        assert_eq!(pallet_gpn_governance::MemberCount::<Runtime>::get(), 5);
    });
}
