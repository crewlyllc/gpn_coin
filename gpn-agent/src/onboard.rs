//! Proof-of-work entry (spec 47) — **client side**.
//!
//! Spec 47 makes onboarding free in coins and paid in computation:
//! `GPUAttestation::attest_entry(pow, …)` and
//! `ComputeMarketplace::register_provider_entry(pow, …)` are dispatched
//! `Pays::No` iff a valid [`EntryPow`] is supplied in the same call. This
//! module is the missing producer of that proof:
//!
//! 1. read the live chain state (registered / attested / free entry used, the
//!    per-block cap, and the **live** `GPUPoW::Difficulty` target);
//! 2. short-circuit if the account is already registered or has already spent
//!    its one free entry;
//! 3. mine a solution with the **existing sealer** ([`crate::mining::mine_entry`]
//!    → [`crate::mining::search_solution`], the same `blake2_256(encode(nonce,
//!    mix_hash, algorithm_version))` search the block-template loop uses);
//! 4. submit **one** extrinsic: `register_provider_entry(pow, …)`.
//!
//! Nothing here re-implements the hash or the difficulty comparison: the
//! client and the pallet share the sealer, so they cannot drift.
//!
//! The chain is reached through the [`EntryChain`] trait and the proof through
//! [`PowMiner`], so the whole decision tree is unit-tested with a fake chain
//! and an injected nonce — no GPU, no node.

use crate::chain::{signer, ChainClient, EntryChainState};
use crate::gpu::ethash_miner::MinedSeal;
use crate::mining;
use sp_core::U256;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use subxt::dynamic::Value;
use subxt::tx::Signer as _;

/// Algorithm version carried by an entry proof. Must equal the runtime's
/// `GPUPoW::AlgorithmVersion`; re-exported from the mining module so there is
/// a single constant.
pub const ALGORITHM_VERSION: u32 = mining::ALGORITHM_VERSION;

/// An Ethash-style proof of work, field-for-field identical to
/// `pallet_gpu_attestation::EntryPow` (and `pallet_gpu_pow::EthashSolution`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryPow {
    pub nonce: u64,
    pub mix_hash: [u8; 32],
    pub solution_hash: [u8; 32],
    pub algorithm_version: u32,
}

impl EntryPow {
    /// Wrap a seal produced by the shared sealer.
    pub fn from_mined(seal: &MinedSeal) -> Self {
        Self {
            nonce: seal.nonce,
            mix_hash: seal.mix,
            solution_hash: seal.result,
            algorithm_version: ALGORITHM_VERSION,
        }
    }

    /// True iff the solution hash is below the target (the pallet's
    /// `meets_difficulty`: higher target = easier).
    pub fn meets(&self, target: &U256) -> bool {
        U256::from_little_endian(&self.solution_hash) < *target
    }

    /// The `SubxtValue` shape of `pallet_gpu_attestation::EntryPow`.
    pub fn to_value(&self) -> Value {
        Value::named_composite([
            ("nonce", Value::u128(self.nonce as u128)),
            ("mix_hash", Value::from_bytes(self.mix_hash)),
            ("solution_hash", Value::from_bytes(self.solution_hash)),
            (
                "algorithm_version",
                Value::u128(self.algorithm_version as u128),
            ),
        ])
    }
}

/// `Some(proof)` as the dynamic `Option<EntryPow>` the pallet expects.
pub fn some_pow_value(pow: &EntryPow) -> Value {
    Value::unnamed_variant("Some", vec![pow.to_value()])
}

/// Compute the solution hash exactly as the pallet does — delegates to the
/// single existing implementation in [`crate::mining`].
pub fn entry_solution_hash(nonce: u64, mix: &[u8; 32], algorithm_version: u32) -> [u8; 32] {
    mining::solution_hash(nonce, mix, algorithm_version)
}

/// Deterministic entry-proof builder: given a nonce and the target, either the
/// (verified) proof or `None`. This is what tests inject to avoid mining.
pub fn build_entry_pow(nonce: u64, mix: [u8; 32], target: &U256) -> Option<EntryPow> {
    let solution_hash = entry_solution_hash(nonce, &mix, ALGORITHM_VERSION);
    let pow = EntryPow {
        nonce,
        mix_hash: mix,
        solution_hash,
        algorithm_version: ALGORITHM_VERSION,
    };
    pow.meets(target).then_some(pow)
}

// ─────────────────────────────────────────────────────────────────────────────
// Planning — pure, and the whole reason a registered node never mines
// ─────────────────────────────────────────────────────────────────────────────

/// The subset of [`EntryChainState`] the plan depends on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EntryStatus {
    pub registered: bool,
    pub attested: bool,
    pub free_entry_used: bool,
    pub free_limit: u32,
    pub free_used_this_block: u32,
    /// True when `free_used_this_block` belongs to the block being read.
    pub counter_is_current_block: bool,
}

impl EntryStatus {
    /// Project the chain-read state (ignores difficulty/challenge).
    pub fn from_chain(state: &EntryChainState) -> Self {
        Self {
            registered: state.registered,
            attested: state.attested,
            free_entry_used: state.free_entry_used,
            free_limit: state.free_limit,
            free_used_this_block: state.free_used_this_block,
            counter_is_current_block: state.free_count_block == state.block_number,
        }
    }
}

/// What the client should do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryPlan {
    /// Already registered — nothing to do, exit 0.
    AlreadyRegistered,
    /// Already attested but not registered: a free entry would be a *second*
    /// attestation, which the pallet forbids. Report, don't mine.
    AlreadyAttested,
    /// This account consumed its one permanent fee-free entry.
    FreeEntrySpent,
    /// The per-block cap is exhausted right now; retry after the next block.
    FreeBudgetThisBlock,
    /// `--entry` was not requested: register directly via `register_provider`
    /// (no proof of work, and `Pays::No` since spec 53 — it costs nothing).
    Direct,
    /// Mine a proof and submit `register_provider_entry`.
    Proceed,
}

/// Decide the next step. Pure — no I/O — so every branch is unit-tested.
///
/// `want_entry` is false when the operator did not ask for the free path; then
/// the answer is always [`EntryPlan::Direct`] except for the read-only
/// "already registered" short-circuit, and the plain `register_provider` call
/// is used — which costs nothing at spec 53 and up.
pub fn plan_entry(want_entry: bool, status: &EntryStatus) -> EntryPlan {
    if status.registered {
        return EntryPlan::AlreadyRegistered;
    }
    if !want_entry {
        return EntryPlan::Direct;
    }
    if status.attested {
        return EntryPlan::AlreadyAttested;
    }
    if status.free_entry_used {
        return EntryPlan::FreeEntrySpent;
    }
    if status.counter_is_current_block
        && status.free_limit > 0
        && status.free_used_this_block >= status.free_limit
    {
        return EntryPlan::FreeBudgetThisBlock;
    }
    EntryPlan::Proceed
}

// ─────────────────────────────────────────────────────────────────────────────
// Outcomes and failure reasons
// ─────────────────────────────────────────────────────────────────────────────

/// Stable reason codes for a failed/declined entry. Written to `last-exit.json`
/// by the supervisor, so they must stay stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureReason {
    ChainReadFailed,
    GpuIdentityMissing,
    BelowEligibilityFloor,
    PowMiningCancelled,
    FreeEntryAccountUsed,
    FreeEntryGpuUsed,
    FreeEntryLimitReached,
    GpuAlreadyOwned,
    SubmitRejected,
}

impl FailureReason {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ChainReadFailed => "entry-chain-read-failed",
            Self::GpuIdentityMissing => "entry-gpu-identity-missing",
            Self::BelowEligibilityFloor => "entry-below-eligibility-floor",
            Self::PowMiningCancelled => "entry-pow-cancelled",
            Self::FreeEntryAccountUsed => "entry-free-account-used",
            Self::FreeEntryGpuUsed => "entry-free-gpu-used",
            Self::FreeEntryLimitReached => "entry-free-per-block-cap",
            Self::GpuAlreadyOwned => "entry-gpu-already-owned",
            Self::SubmitRejected => "entry-submit-rejected",
        }
    }
}

/// Result of an onboarding attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryOutcome {
    /// Nothing to do: already a registered provider.
    AlreadyRegistered,
    /// Attested already; the free entry is spent / not applicable.
    AlreadyAttested,
    /// The one-call free entry succeeded.
    Registered,
    /// The combined call was missing from metadata; attest + legacy register
    /// both succeeded.
    RegisteredViaFallback,
    /// The operator asked for the direct path and it succeeded (no fee).
    RegisteredDirect,
    /// Declined or failed, with a stable reason code.
    Failed(FailureReason, String),
}

impl EntryOutcome {
    pub fn succeeded(&self) -> bool {
        matches!(
            self,
            Self::AlreadyRegistered
                | Self::Registered
                | Self::RegisteredViaFallback
                | Self::RegisteredDirect
                | Self::AlreadyAttested
        )
    }

    /// One-line operator-facing summary.
    pub fn summary(&self) -> String {
        match self {
            Self::AlreadyRegistered => "already registered".into(),
            Self::AlreadyAttested => {
                "already attested — this account cannot use another free entry; it registers \
                 through the unsigned re-registration path (register_provider_entry_unsigned) \
                 or the fee-free register_provider call, and neither costs anything"
                    .into()
            }
            Self::Registered => "registered via proof-of-work entry (no coins spent)".into(),
            Self::RegisteredViaFallback => {
                "attested and registered via the fallback path (no coins spent)".into()
            }
            Self::RegisteredDirect => "registered directly (no fee, no coins spent)".into(),
            Self::Failed(reason, detail) => format!("{}: {detail}", reason.code()),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Injected dependencies
// ─────────────────────────────────────────────────────────────────────────────

/// The registration parameters sent on chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryParams {
    pub challenge_response: [u8; 32],
    pub gpu_info_hash: [u8; 32],
    pub gpu_count: u8,
    pub compute_gflops: u64,
    pub requirements: ProviderRequirements,
}

/// Mirrors `pallet_compute_marketplace::GPURequirements` (u32/u16/u8/u32/u64).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderRequirements {
    pub min_vram_mb: u32,
    pub min_compute_capability: u16,
    pub min_gpu_count: u8,
    pub min_cuda_cores: u32,
    pub min_gflops: u64,
}

impl Default for ProviderRequirements {
    fn default() -> Self {
        // The values `gpn_register` has always used.
        Self {
            min_vram_mb: 8_192,
            min_compute_capability: 89,
            min_gpu_count: 1,
            min_cuda_cores: 3_072,
            min_gflops: 150,
        }
    }
}

impl ProviderRequirements {
    pub fn to_value(&self) -> Value {
        Value::named_composite([
            ("min_vram_mb", Value::u128(self.min_vram_mb as u128)),
            (
                "min_compute_capability",
                Value::u128(self.min_compute_capability as u128),
            ),
            ("min_gpu_count", Value::u128(self.min_gpu_count as u128)),
            ("min_cuda_cores", Value::u128(self.min_cuda_cores as u128)),
            ("min_gflops", Value::u128(self.min_gflops as u128)),
        ])
    }
}

/// Read/submit surface, faked in tests.
#[allow(async_fn_in_trait)]
pub trait EntryChain {
    async fn status(&self, account: &str) -> Result<EntryStatus, String>;
    async fn difficulty(&self) -> Result<U256, String>;
    async fn submit_register_provider_entry(
        &self,
        pow: &EntryPow,
        params: &EntryParams,
    ) -> Result<(), String>;
    /// Spec 50: the unsigned entry. Default delegates to the signed path so
    /// existing tests/mocks keep working; the live chain overrides it.
    async fn submit_register_provider_entry_unsigned(
        &self,
        pow: &EntryPow,
        params: &EntryParams,
    ) -> Result<(), String> {
        self.submit_register_provider_entry(pow, params).await
    }
    async fn submit_attest_entry(&self, pow: &EntryPow, params: &EntryParams)
        -> Result<(), String>;
    async fn submit_register_provider(&self, params: &EntryParams) -> Result<(), String>;
}

/// Proof producer, faked in tests (and CPU-driven in production).
pub trait PowMiner {
    fn mine(&self, target: U256, log: &mut dyn FnMut(&str)) -> Result<EntryPow, String>;
}

/// The production proof producer: the shared sealer, single-threaded, with
/// progress logging. Expected work is `2^256 / target` hashes — at the testnet
/// target (~2^228) that is ~2.3e8 hashes: about 1–3 minutes single-threaded,
/// well under a minute on a fast box.
pub struct SharedSealer {
    pub batch: usize,
    pub start_nonce: u64,
    pub cancel: Arc<AtomicBool>,
}

impl Default for SharedSealer {
    fn default() -> Self {
        Self {
            batch: 1 << 20,
            start_nonce: 0,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl PowMiner for SharedSealer {
    fn mine(&self, target: U256, log: &mut dyn FnMut(&str)) -> Result<EntryPow, String> {
        log(&format!(
            "mining entry proof against live difficulty (~2^{:.1} hashes expected)…",
            difficulty_bits(&target)
        ));
        let mut last_logged = 0u64;
        let seal = mining::mine_entry(target, self.start_nonce, self.batch, &self.cancel, |p| {
            // The shared sealer reports every ~2 s; forward at most one line
            // every ~10 s so a long search does not flood the supervisor log.
            if p.elapsed.as_secs() >= last_logged + 10 {
                last_logged = p.elapsed.as_secs();
                log(&format!(
                    "  entry proof: {} hashes searched, {:.1} MH/s, {}s elapsed",
                    p.hashes,
                    p.hashrate_mhs,
                    p.elapsed.as_secs()
                ));
            }
        });
        match seal {
            Some(s) => {
                let pow = EntryPow::from_mined(&s);
                if !pow.meets(&target) {
                    return Err("sealer returned a solution above the target".to_string());
                }
                log(&format!("  entry proof found: nonce={:#x}", pow.nonce));
                Ok(pow)
            }
            None => Err("entry proof search was cancelled".to_string()),
        }
    }
}

fn difficulty_bits(target: &U256) -> f64 {
    let bits = target.bits();
    if bits == 0 {
        return 0.0;
    }
    256.0 - bits as f64
}

// ─────────────────────────────────────────────────────────────────────────────
// The flow
// ─────────────────────────────────────────────────────────────────────────────

/// Run the entry flow. `log` receives plain-language progress lines.
pub async fn run_entry<C: EntryChain, M: PowMiner>(
    chain: &C,
    miner: &M,
    account: &str,
    params: &EntryParams,
    want_entry: bool,
    log: &mut dyn FnMut(&str),
) -> EntryOutcome {
    let status = match chain.status(account).await {
        Ok(s) => s,
        Err(e) => {
            return EntryOutcome::Failed(
                FailureReason::ChainReadFailed,
                format!("could not read chain state for {account}: {e}"),
            )
        }
    };

    match plan_entry(want_entry, &status) {
        EntryPlan::AlreadyRegistered => {
            log("already registered — nothing to do");
            return EntryOutcome::AlreadyRegistered;
        }
        EntryPlan::AlreadyAttested => {
            log("this account is already attested; the free entry is for first-time onboarding only");
            return EntryOutcome::AlreadyAttested;
        }
        EntryPlan::FreeEntrySpent => {
            return EntryOutcome::Failed(
                FailureReason::FreeEntryAccountUsed,
                "this account already used its one free proof-of-work entry — registration is \
                 still free: re-register through the unsigned re-registration path \
                 (`register_provider_entry_unsigned`) or the fee-free `register_provider` call; \
                 neither asks for a deposit or a fee"
                    .to_string(),
            )
        }
        EntryPlan::FreeBudgetThisBlock => {
            return EntryOutcome::Failed(
                FailureReason::FreeEntryLimitReached,
                "the per-block free-entry cap is exhausted right now — wait for the next block \
                 (a few seconds) and retry; the limit is not consumed by this attempt"
                    .to_string(),
            )
        }
        EntryPlan::Direct => {
            log("proof-of-work entry not requested — registering directly (no fee, no proof of work)");
            return match chain.submit_register_provider(params).await {
                Ok(()) => EntryOutcome::RegisteredDirect,
                Err(e) => EntryOutcome::Failed(FailureReason::SubmitRejected, e),
            };
        }
        EntryPlan::Proceed => {}
    }

    if params.gpu_info_hash == [0u8; 32] {
        return EntryOutcome::Failed(
            FailureReason::GpuIdentityMissing,
            "no GPU hardware fingerprint could be derived (nvidia-smi/rocm-smi did not report a \
             UUID and GPN_GENESIS_HASH/network registry is unavailable)"
                .to_string(),
        );
    }

    // Mine, then re-check the target: difficulty can move while we search, and
    // the pallet verifies against the difficulty *at submission*. Re-mine only
    // when the found proof no longer clears the newer target.
    let mut target = match chain.difficulty().await {
        Ok(t) => t,
        Err(e) => {
            return EntryOutcome::Failed(
                FailureReason::ChainReadFailed,
                format!("could not read the live entry difficulty: {e}"),
            )
        }
    };
    let mut pow = match miner.mine(target, log) {
        Ok(p) => p,
        Err(e) => {
            let reason = if e.contains("cancelled") {
                FailureReason::PowMiningCancelled
            } else {
                FailureReason::SubmitRejected
            };
            return EntryOutcome::Failed(reason, e);
        }
    };
    for _ in 0..2 {
        let latest = match chain.difficulty().await {
            Ok(t) => t,
            Err(_) => break,
        };
        if pow.meets(&latest) {
            target = latest;
            break;
        }
        log("difficulty moved while mining — re-mining against the newer target");
        pow = match miner.mine(latest, log) {
            Ok(p) => p,
            Err(e) => return EntryOutcome::Failed(FailureReason::PowMiningCancelled, e),
        };
        target = latest;
    }

    // Row 244: the target can also move between this client's last read and the
    // *pool's* precheck, which is the failure the 5th box hit. `Custom(1)` is
    // that race, not a bad proof: re-read the live difficulty, re-mine against
    // it, and resubmit — twice before giving up.
    submit_unsigned_with_remine(chain, miner, pow, target, params, log).await
}

/// Submit the unsigned entry; if the pool refuses the **proof itself** because
/// the difficulty target moved between our last read and its precheck, re-read
/// the live target, re-mine against it and resubmit — twice, then report.
///
/// Row 244. Two renderings of that refusal exist: from spec 54 the pallet sends
/// `InvalidTransaction::Custom(1)` (row 243), and on an older chain the same
/// refusal still arrives as `BadProof` / "Transaction has a bad signature". The
/// second is only a race when the proof no longer clears the live target — which
/// is checked before re-mining, so an already-used account is never re-mined.
pub async fn submit_unsigned_with_remine<C: EntryChain, M: PowMiner>(
    chain: &C,
    miner: &M,
    mut pow: EntryPow,
    mut target: U256,
    params: &EntryParams,
    log: &mut dyn FnMut(&str),
) -> EntryOutcome {
    let mut remines = 0u8;
    loop {
        log("submitting the unsigned entry (proof of work, no account, no fee)…");
        match chain
            .submit_register_provider_entry_unsigned(&pow, params)
            .await
        {
            Ok(()) => return EntryOutcome::Registered,
            Err(e) if is_unknown_call(&e) => {
                log("unsigned entry is not in the chain metadata — trying the signed entry call");
                match chain.submit_register_provider_entry(&pow, params).await {
                    Ok(()) => return EntryOutcome::Registered,
                    Err(e2) => log(&format!("signed entry also unavailable: {e2}")),
                }
                log("falling back to attest_entry + register");
                if let Err(att) = chain.submit_attest_entry(&pow, params).await {
                    let (reason, detail) = classify_submit_error(&att);
                    return EntryOutcome::Failed(reason, detail);
                }
                return match chain.submit_register_provider(params).await {
                    Ok(()) => EntryOutcome::RegisteredViaFallback,
                    Err(e2) => {
                        let (reason, detail) = classify_submit_error(&e2);
                        EntryOutcome::Failed(reason, detail)
                    }
                };
            }
            Err(e) if remines < 2 && is_proof_refusal(&e) => {
                log(&format!("the pool refused the proof: {e}"));
                let latest = match chain.difficulty().await {
                    Ok(t) => t,
                    Err(e2) => {
                        return EntryOutcome::Failed(
                            FailureReason::ChainReadFailed,
                            format!(
                                "the chain refused the entry proof and the live difficulty \
                                 could not be re-read: {e2}"
                            ),
                        )
                    }
                };
                if !is_proof_rejection(&e) && pow.meets(&latest) {
                    // Legacy wording, but the proof still clears the live target:
                    // a real refusal (spent free entry, used GPU, …), not a race.
                    let (reason, detail) = classify_submit_error(&e);
                    return EntryOutcome::Failed(reason, detail);
                }
                remines += 1;
                let old_target = target;
                let old_nonce = pow.nonce;
                pow = match miner.mine(latest, log) {
                    Ok(p) => p,
                    Err(e2) => return EntryOutcome::Failed(FailureReason::PowMiningCancelled, e2),
                };
                target = latest;
                log(&format!(
                    "re-mined against the newer target: old target ~2^{:.1} nonce {:#x} → new \
                     target ~2^{:.1} nonce {:#x} (attempt {remines}/2)",
                    difficulty_bits(&old_target),
                    old_nonce,
                    difficulty_bits(&latest),
                    pow.nonce
                ));
            }
            Err(e) => {
                let (reason, detail) = classify_submit_error(&e);
                return EntryOutcome::Failed(reason, detail);
            }
        }
    }
}

/// True when the pool refused the *proof itself* (row 243's `Custom(1)`): the
/// difficulty moved between our last read and the pool's precheck, so the cure
/// is to re-mine against the live target — not to report a failure.
pub fn is_proof_rejection(err: &str) -> bool {
    custom_reject_code(err) == Some(entry_reject_code::PROOF_INVALID)
}

/// Any refusal that could be the proof: row 243's `Custom(1)`, or the blanket
/// `BadProof` an older chain (spec ≤ 53) still returns for the same precheck.
pub fn is_proof_refusal(err: &str) -> bool {
    if is_proof_rejection(err) {
        return true;
    }
    let l = err.to_ascii_lowercase();
    l.contains("bad signature") || l.contains("badproof") || l.contains("bad proof")
}

/// Whether a submit error means the runtime metadata does not carry the call
/// (an older chain, or metadata that predates spec 47) — the only case in which
/// the fallback path is legitimate.
pub fn is_unknown_call(err: &str) -> bool {
    let l = err.to_ascii_lowercase();
    (l.contains("call") || l.contains("pallet") || l.contains("extrinsic"))
        && (l.contains("not found") || l.contains("unknown") || l.contains("does not exist"))
}

/// Stable wire codes the runtime sends as `InvalidTransaction::Custom(n)` when
/// the pool-boundary precheck refuses an unsigned entry (`EntryReject` in
/// `pallet-gpu-attestation`, row 243).
///
/// The pool boundary cannot report a dispatch error, so the code is the only
/// channel: `Custom(1)` reaches the client as the string "Custom error: 1".
/// These constants are the client's half of that contract — keep them in step
/// with the pallet's `EntryReject` (a test below pins each one to its sentence).
pub mod entry_reject_code {
    /// The proof does not clear the difficulty target in force now.
    pub const PROOF_INVALID: u8 = 1;
    pub const GPU_IDENTITY_MISSING: u8 = 2;
    pub const ACCOUNT_ALREADY_USED: u8 = 3;
    pub const GPU_ALREADY_USED: u8 = 4;
    pub const LIMIT_REACHED: u8 = 5;
}

/// Pull the number out of the pool's rendering of `InvalidTransaction::Custom`.
///
/// Substrate renders it as `Custom error: {n}` (data field) and, if the data is
/// missing, as `InvalidTransaction custom error` with no number at all — that
/// second form is reported as unknown rather than guessed at.
pub fn custom_reject_code(err: &str) -> Option<u8> {
    let l = err.to_ascii_lowercase();
    let at = l.find("custom error:")?;
    l[at + "custom error:".len()..]
        .trim_start()
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|n| n.parse::<u8>().ok())
}

/// Map a dispatch error string onto a stable reason code + operator guidance.
pub fn classify_submit_error(err: &str) -> (FailureReason, String) {
    let l = err.to_ascii_lowercase();
    // Row 243: the pool reports a refused entry proof as `Custom(n)`; turn the
    // number back into the reason the chain actually had, instead of leaving it
    // as "Transaction has a bad signature" (a signature this call does not have)
    // or as an opaque code.
    if let Some(code) = custom_reject_code(err) {
        let reason = match code {
            entry_reject_code::PROOF_INVALID => Some((
                FailureReason::SubmitRejected,
                "the entry proof was rejected: it does not meet the difficulty target in force \
                 now (the target retargets every 45 blocks, so a proof mined before the step is \
                 no longer valid) — re-mine against the current target and resubmit"
                    .to_string(),
            )),
            entry_reject_code::GPU_IDENTITY_MISSING => Some((
                FailureReason::GpuIdentityMissing,
                "the chain rejected the entry because no GPU hardware fingerprint was supplied"
                    .to_string(),
            )),
            entry_reject_code::ACCOUNT_ALREADY_USED => Some((
                FailureReason::FreeEntryAccountUsed,
                "this account already used its one free proof-of-work entry — re-register via \
                 `register_provider_entry_unsigned` (unsigned, no fee) or `register_provider` \
                 (fee-free since spec 53)"
                    .to_string(),
            )),
            entry_reject_code::GPU_ALREADY_USED => Some((
                FailureReason::FreeEntryGpuUsed,
                "this GPU fingerprint already used its one free entry — use a different physical \
                 GPU or an account that already owns it"
                    .to_string(),
            )),
            entry_reject_code::LIMIT_REACHED => Some((
                FailureReason::FreeEntryLimitReached,
                "the per-block cap on fee-free entries is exhausted — retry after the next block \
                 (the limit is not consumed by this attempt)"
                    .to_string(),
            )),
            _ => None,
        };
        if let Some((reason, text)) = reason {
            return (reason, format!("{text} ({err})"));
        }
    }
    if l.contains("freeentrygpuused") || l.contains("free entry gpu") {
        return (
            FailureReason::FreeEntryGpuUsed,
            format!(
                "this GPU's hardware fingerprint already consumed a free entry — the unsigned \
                 re-registration path covers it, or use a different physical GPU ({err})"
            ),
        );
    }
    if l.contains("freeentryaccountused") || l.contains("already consumed its one fee-free") {
        return (
            FailureReason::FreeEntryAccountUsed,
            format!(
                "this account already used its free entry — re-register via \
                 `register_provider_entry_unsigned` (unsigned re-registration, no fee) or \
                 `register_provider` (fee-free since spec 53) ({err})"
            ),
        );
    }
    if l.contains("freeentrylimitreached") || l.contains("per-block cap") {
        return (
            FailureReason::FreeEntryLimitReached,
            format!(
                "the per-block free-entry cap is exhausted — retry after the next block ({err})"
            ),
        );
    }
    if l.contains("gpualreadyowned") {
        return (
            FailureReason::GpuAlreadyOwned,
            format!("this GPU fingerprint is bound to another account — use that account or a different GPU ({err})"),
        );
    }
    if l.contains("beloweligibilityfloor") {
        return (
            FailureReason::BelowEligibilityFloor,
            format!("the measured GFLOPS is below the network's eligibility floor ({err})"),
        );
    }
    if l.contains("gpuidentitymissing") {
        return (
            FailureReason::GpuIdentityMissing,
            format!("the chain rejected the attestation because no GPU fingerprint was supplied ({err})"),
        );
    }
    (
        FailureReason::SubmitRejected,
        format!("the entry transaction was rejected: {err}"),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Live implementation over subxt
// ─────────────────────────────────────────────────────────────────────────────

/// Production [`EntryChain`]: reads through [`ChainClient`], submits signed
/// extrinsics through the shared `signer` plumbing.
pub struct LiveEntryChain {
    endpoint: String,
    signer_uri: String,
    client: ChainClient,
}

impl LiveEntryChain {
    pub async fn connect(
        endpoint: &str,
        signer_uri: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let client = ChainClient::new(endpoint).await?;
        Ok(Self {
            endpoint: endpoint.to_string(),
            signer_uri: signer_uri.to_string(),
            client,
        })
    }

    /// SS58 address of the signer.
    pub fn account(&self) -> Result<String, String> {
        let signer = signer::Sr25519Signer::from_uri(&self.signer_uri)
            .map_err(|e| format!("invalid signer: {e}"))?;
        Ok(signer.account_id().to_string())
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Full chain read (registered/attested/free-entry/difficulty/challenge).
    pub async fn state(&self, account: &str) -> Result<EntryChainState, String> {
        self.client.entry_chain_state(account).await
    }

    /// Genesis hash, for a chain-bound GPU fingerprint.
    pub async fn genesis_hash(&self) -> Option<[u8; 32]> {
        self.client.genesis_hash().await
    }

    async fn submit_unsigned<Call: subxt::tx::Payload>(&self, call: Call) -> Result<(), String> {
        let client = signer::connect(&self.endpoint)
            .await
            .map_err(|e| format!("connect: {e}"))?;
        signer::submit_unsigned(&client, &self.endpoint, &call)
            .await
            .map_err(|e| e.to_string())
    }

    async fn submit<Call: subxt::tx::Payload>(&self, call: Call) -> Result<(), String> {
        let client = signer::connect(&self.endpoint)
            .await
            .map_err(|e| format!("connect: {e}"))?;
        let signer = signer::Sr25519Signer::from_uri(&self.signer_uri)
            .map_err(|e| format!("signer: {e}"))?;
        let nonce = signer::current_nonce(&client, &self.endpoint, &signer.account_id()).await;
        signer::submit_signed(&client, &call, &signer, nonce)
            .await
            .map_err(|e| e.to_string())
    }
}

/// `ComputeMarketplace::register_provider_entry(pow, …)` as a dynamic payload.
pub fn register_provider_entry_call(
    pow: &EntryPow,
    params: &EntryParams,
) -> subxt::tx::DynamicPayload {
    subxt::dynamic::tx(
        "ComputeMarketplace",
        "register_provider_entry",
        vec![
            some_pow_value(pow),
            Value::from_bytes(params.challenge_response),
            Value::from_bytes(params.gpu_info_hash),
            Value::u128(params.gpu_count as u128),
            Value::u128(params.compute_gflops as u128),
            params.requirements.to_value(),
        ],
    )
}

/// `ComputeMarketplace::register_provider_entry_unsigned(...)` as a dynamic
/// payload (spec 50). Unsigned on purpose: no account, no fee, no deposit —
/// the proof of work is the authorisation.
pub fn register_provider_entry_unsigned_call(
    account: &[u8; 32],
    pow: &EntryPow,
    params: &EntryParams,
) -> subxt::tx::DynamicPayload {
    subxt::dynamic::tx(
        "ComputeMarketplace",
        "register_provider_entry_unsigned",
        vec![
            Value::from_bytes(account),
            pow.to_value(),
            Value::from_bytes(params.challenge_response),
            Value::from_bytes(params.gpu_info_hash),
            Value::u128(params.gpu_count as u128),
            Value::u128(params.compute_gflops as u128),
            params.requirements.to_value(),
        ],
    )
}

/// `GPUAttestation::attest_entry(pow, …)` as a dynamic payload.
pub fn attest_entry_call(pow: &EntryPow, params: &EntryParams) -> subxt::tx::DynamicPayload {
    subxt::dynamic::tx(
        "GPUAttestation",
        "attest_entry",
        vec![
            some_pow_value(pow),
            Value::from_bytes(params.challenge_response),
            Value::from_bytes(params.gpu_info_hash),
            Value::u128(params.gpu_count as u128),
            Value::u128(params.compute_gflops as u128),
        ],
    )
}

/// `ComputeMarketplace::register_provider(requirements)` — the direct path
/// (no proof of work, and fee-free from spec 53).
pub fn register_provider_call(params: &EntryParams) -> subxt::tx::DynamicPayload {
    subxt::dynamic::tx(
        "ComputeMarketplace",
        "register_provider",
        vec![params.requirements.to_value()],
    )
}

impl EntryChain for LiveEntryChain {
    async fn status(&self, account: &str) -> Result<EntryStatus, String> {
        let state = self.client.entry_chain_state(account).await?;
        Ok(EntryStatus::from_chain(&state))
    }

    async fn difficulty(&self) -> Result<U256, String> {
        let state = self.client.entry_chain_state(&self.account()?).await?;
        let target = U256(state.difficulty);
        if target.is_zero() {
            return Err("chain difficulty target is zero — is the node synced?".to_string());
        }
        Ok(target)
    }

    async fn submit_register_provider_entry(
        &self,
        pow: &EntryPow,
        params: &EntryParams,
    ) -> Result<(), String> {
        self.submit(register_provider_entry_call(pow, params)).await
    }

    async fn submit_register_provider_entry_unsigned(
        &self,
        pow: &EntryPow,
        params: &EntryParams,
    ) -> Result<(), String> {
        let account = self.account()?;
        let acct = sp_core::crypto::Ss58Codec::from_ss58check(&account)
            .map_err(|e| format!("account {account}: {e}"))?;
        let acct: sp_core::sr25519::Public = acct;
        self.submit_unsigned(register_provider_entry_unsigned_call(&acct.0, pow, params))
            .await
    }

    async fn submit_attest_entry(
        &self,
        pow: &EntryPow,
        params: &EntryParams,
    ) -> Result<(), String> {
        self.submit(attest_entry_call(pow, params)).await
    }

    async fn submit_register_provider(&self, params: &EntryParams) -> Result<(), String> {
        self.submit(register_provider_call(params)).await
    }
}

/// Everything the CLI/supervisor needs to build `EntryParams`, with the GPU
/// probes kept in one place (headless: `nvidia-smi`/`rocm-smi` + OpenCL).
pub struct LocalGpu {
    pub gpu_info_hash: [u8; 32],
    pub gpu_count: u8,
    pub compute_gflops: u64,
    pub weak_fingerprint: bool,
    pub gpu_name: String,
    /// True when `--gflops` supplied the number instead of the benchmark.
    pub gflops_was_override: bool,
    /// True when `--gpu-hash` supplied the fingerprint (no hardware probe).
    pub fingerprint_was_override: bool,
}

/// Measure the local GPU the same way `gpn_attest` does: hardware UUID →
/// fingerprint bound to the chain genesis, then the FP32 GEMM benchmark.
pub fn probe_local_gpu(gflops_override: Option<u64>) -> Result<LocalGpu, String> {
    probe_local_gpu_with(gflops_override, None)
}

/// Same, with optional operator overrides. `gpu_hash_override` skips the GPU
/// probe entirely (headless / GPU busy); `gflops_override` skips only the
/// benchmark.
pub fn probe_local_gpu_with(
    gflops_override: Option<u64>,
    gpu_hash_override: Option<[u8; 32]>,
) -> Result<LocalGpu, String> {
    let genesis = crate::gpu::canonical_genesis_bytes().ok_or_else(|| {
        "no chain genesis hash known (set GPN_GENESIS_HASH or GPN_NETWORK_REGISTRY)".to_string()
    })?;
    probe_local_gpu_for_genesis(gflops_override, gpu_hash_override, &genesis)
}

/// Same, with the genesis hash read from the running chain (the supervisor
/// uses this so a box without `network.json` still gets a chain-bound
/// fingerprint).
pub fn probe_local_gpu_for_genesis(
    gflops_override: Option<u64>,
    gpu_hash_override: Option<[u8; 32]>,
    genesis: &[u8; 32],
) -> Result<LocalGpu, String> {
    let compute_gflops = match gflops_override {
        Some(g) => g,
        None => crate::gpu::measure_gflops(0).map_err(|e| format!("GFLOPS benchmark: {e}"))?,
    };
    let gflops_was_override = gflops_override.is_some();

    if let Some(hash) = gpu_hash_override {
        return Ok(LocalGpu {
            gpu_info_hash: hash,
            gpu_count: 1,
            compute_gflops,
            weak_fingerprint: false,
            gpu_name: "(operator-supplied fingerprint)".into(),
            gflops_was_override,
            fingerprint_was_override: true,
        });
    }

    let monitor = crate::gpu::GPUMonitor::new().map_err(|e| format!("GPU monitor: {e}"))?;
    let info = monitor
        .get_gpu_info()
        .map_err(|e| format!("could not read GPU info: {e}"))?;
    let fp = crate::gpu::resolve_fingerprint(&info, genesis);
    Ok(LocalGpu {
        gpu_info_hash: fp.hash,
        gpu_count: info.gpu_count.max(1),
        compute_gflops,
        weak_fingerprint: fp.weak,
        gpu_name: info.model_name,
        gflops_was_override,
        fingerprint_was_override: false,
    })
}

/// Human-readable expected duration line for docs/logs.
pub fn expected_entry_proof_duration(target: &U256) -> String {
    let expected = 2f64.powi(256) / 2f64.powi(target.bits() as i32).max(1.0);
    // ~2 MH/s single-threaded blake2, ~10 MH/s on an 8-core box.
    let slow = expected / 2e6;
    let fast = expected / 1e7;
    format!(
        "~{:.0}–{:.0} s ({:.1}M–{:.1}M hashes expected)",
        slow.min(3600.0),
        fast.min(3600.0),
        expected / 1e6,
        expected / 1e6
    )
}

/// Convenience used by the supervisor: run the flow with the production chain
/// and sealer, returning the outcome and the live target (for logging).
pub async fn run_live_entry(
    endpoint: &str,
    signer_uri: &str,
    params: &EntryParams,
    want_entry: bool,
    log: &mut dyn FnMut(&str),
) -> EntryOutcome {
    let chain = match LiveEntryChain::connect(endpoint, signer_uri).await {
        Ok(c) => c,
        Err(e) => {
            return EntryOutcome::Failed(
                FailureReason::ChainReadFailed,
                format!("could not connect to {endpoint}: {e}"),
            )
        }
    };
    let account = match chain.account() {
        Ok(a) => a,
        Err(e) => return EntryOutcome::Failed(FailureReason::SubmitRejected, e),
    };
    let miner = SharedSealer::default();
    run_entry(&chain, &miner, &account, params, want_entry, log).await
}

/// Kept for symmetry with the rest of the agent: the deadline the caller may
/// wait for one submission (used by the supervisor when bounding first-run).
pub const SUBMIT_TIMEOUT: Duration = Duration::from_secs(600);

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake chain whose answers are set per test, plus recorded submissions.
    struct FakeChain {
        status: EntryStatus,
        difficulty: std::cell::Cell<U256>,
        /// How many entry submissions to refuse with `entry_failure`.
        entry_failures: std::cell::Cell<u32>,
        entry_failure: std::cell::RefCell<String>,
        /// Target the chain moves to when it refuses a proof (the retarget race).
        target_on_reject: std::cell::Cell<Option<U256>>,
        register_entry: Result<(), String>,
        attest_entry: Result<(), String>,
        register_plain: Result<(), String>,
        submitted_entry: std::cell::RefCell<Vec<EntryPow>>,
        submitted_attest: std::cell::RefCell<Vec<EntryPow>>,
        submitted_plain: std::cell::RefCell<u32>,
    }

    impl FakeChain {
        fn new(status: EntryStatus, difficulty: U256) -> Self {
            Self {
                status,
                difficulty: std::cell::Cell::new(difficulty),
                entry_failures: std::cell::Cell::new(0),
                entry_failure: Default::default(),
                target_on_reject: std::cell::Cell::new(None),
                register_entry: Ok(()),
                attest_entry: Ok(()),
                register_plain: Ok(()),
                submitted_entry: Default::default(),
                submitted_attest: Default::default(),
                submitted_plain: Default::default(),
            }
        }

        /// Refuse the next `times` entry submissions with `err`, moving the
        /// difficulty to `then_target` first (the retarget race).
        fn refuse(&self, err: &str, times: u32, then_target: Option<U256>) {
            self.entry_failures.set(times);
            self.entry_failure.borrow_mut().clear();
            self.entry_failure.borrow_mut().push_str(err);
            self.target_on_reject.set(then_target);
        }
    }

    impl EntryChain for FakeChain {
        async fn status(&self, _account: &str) -> Result<EntryStatus, String> {
            Ok(self.status)
        }
        async fn difficulty(&self) -> Result<U256, String> {
            Ok(self.difficulty.get())
        }
        async fn submit_register_provider_entry(
            &self,
            pow: &EntryPow,
            _params: &EntryParams,
        ) -> Result<(), String> {
            if self.entry_failures.get() > 0 {
                self.entry_failures.set(self.entry_failures.get() - 1);
                if let Some(t) = self.target_on_reject.get() {
                    self.difficulty.set(t);
                }
                return Err(self.entry_failure.borrow().clone());
            }
            self.register_entry.clone()?;
            self.submitted_entry.borrow_mut().push(*pow);
            Ok(())
        }
        async fn submit_attest_entry(
            &self,
            pow: &EntryPow,
            _params: &EntryParams,
        ) -> Result<(), String> {
            self.attest_entry.clone()?;
            self.submitted_attest.borrow_mut().push(*pow);
            Ok(())
        }
        async fn submit_register_provider(&self, _params: &EntryParams) -> Result<(), String> {
            self.register_plain.clone()?;
            *self.submitted_plain.borrow_mut() += 1;
            Ok(())
        }
    }

    /// Deterministic miner: finds the first nonce from `start` that clears the
    /// (deliberately easy) target. Never touches a GPU.
    struct InjectedMiner {
        start: u64,
        calls: std::cell::Cell<u32>,
    }

    impl PowMiner for InjectedMiner {
        fn mine(&self, target: U256, _log: &mut dyn FnMut(&str)) -> Result<EntryPow, String> {
            self.calls.set(self.calls.get() + 1);
            for nonce in self.start..self.start.saturating_add(1_000_000) {
                if let Some(pow) = build_entry_pow(nonce, [0u8; 32], &target) {
                    return Ok(pow);
                }
            }
            Err("no solution below target".into())
        }
    }

    fn easy_target() -> U256 {
        // Everything below 2^250 is a solution: ~1 in 64 nonces.
        U256::from(1u8) << 250
    }

    fn fresh_status() -> EntryStatus {
        EntryStatus {
            registered: false,
            attested: false,
            free_entry_used: false,
            free_limit: 2,
            free_used_this_block: 0,
            counter_is_current_block: true,
        }
    }

    fn params() -> EntryParams {
        EntryParams {
            challenge_response: [7u8; 32],
            gpu_info_hash: [9u8; 32],
            gpu_count: 1,
            compute_gflops: 9_000,
            requirements: ProviderRequirements::default(),
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn already_registered_short_circuits_before_mining() {
        let mut status = fresh_status();
        status.registered = true;
        let chain = FakeChain::new(status, easy_target());
        let miner = InjectedMiner {
            start: 0,
            calls: Default::default(),
        };
        let mut log = |_: &str| {};
        let outcome = block_on(run_entry(&chain, &miner, "acct", &params(), true, &mut log));
        assert_eq!(outcome, EntryOutcome::AlreadyRegistered);
        assert_eq!(miner.calls.get(), 0, "a registered node must not mine");
        assert!(chain.submitted_entry.borrow().is_empty());
    }

    #[test]
    fn already_attested_does_not_mine_or_submit() {
        let mut status = fresh_status();
        status.attested = true;
        let chain = FakeChain::new(status, easy_target());
        let miner = InjectedMiner {
            start: 0,
            calls: Default::default(),
        };
        let mut log = |_: &str| {};
        let outcome = block_on(run_entry(&chain, &miner, "acct", &params(), true, &mut log));
        assert_eq!(outcome, EntryOutcome::AlreadyAttested);
        assert_eq!(miner.calls.get(), 0);
    }

    #[test]
    fn no_pow_flag_registers_directly_without_mining() {
        let chain = FakeChain::new(fresh_status(), easy_target());
        let miner = InjectedMiner {
            start: 0,
            calls: Default::default(),
        };
        let mut log = |_: &str| {};
        let outcome = block_on(run_entry(
            &chain,
            &miner,
            "acct",
            &params(),
            false,
            &mut log,
        ));
        assert_eq!(outcome, EntryOutcome::RegisteredDirect);
        assert_eq!(miner.calls.get(), 0, "the direct path must not mine");
        assert_eq!(*chain.submitted_plain.borrow(), 1);
        assert!(chain.submitted_entry.borrow().is_empty());
    }

    #[test]
    fn entry_build_is_deterministic_for_an_injected_nonce_and_target() {
        let target = easy_target();
        // Find the first qualifying nonce, then rebuild it twice.
        let nonce = (0u64..1_000_000)
            .find(|n| build_entry_pow(*n, [0u8; 32], &target).is_some())
            .expect("an easy target must have a solution");
        let a = build_entry_pow(nonce, [0u8; 32], &target).unwrap();
        let b = build_entry_pow(nonce, [0u8; 32], &target).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            a.solution_hash,
            mining::solution_hash(nonce, &[0u8; 32], ALGORITHM_VERSION)
        );
        assert!(a.meets(&target));
        // A target below the hash rejects the same nonce deterministically.
        let tiny = U256::from(1u8) << 8;
        assert!(build_entry_pow(nonce, [0u8; 32], &tiny).is_none());
    }

    #[test]
    fn fresh_account_uses_the_one_call_entry() {
        let chain = FakeChain::new(fresh_status(), easy_target());
        let miner = InjectedMiner {
            start: 0,
            calls: Default::default(),
        };
        let mut log = |_: &str| {};
        let outcome = block_on(run_entry(&chain, &miner, "acct", &params(), true, &mut log));
        assert_eq!(outcome, EntryOutcome::Registered);
        assert_eq!(chain.submitted_entry.borrow().len(), 1);
        assert!(
            chain.submitted_attest.borrow().is_empty(),
            "one call, not two"
        );
    }

    #[test]
    fn a_proof_refused_by_the_pool_is_re_mined_against_the_new_target_and_accepted() {
        let chain = FakeChain::new(fresh_status(), easy_target());
        let harder = U256::from(1u8) << 249;
        chain.refuse("Custom error: 1", 1, Some(harder));
        let miner = InjectedMiner {
            start: 0,
            calls: Default::default(),
        };
        let mut lines: Vec<String> = Vec::new();
        let mut log = |s: &str| lines.push(s.to_string());
        let outcome = block_on(run_entry(&chain, &miner, "acct", &params(), true, &mut log));
        assert_eq!(outcome, EntryOutcome::Registered);
        assert_eq!(miner.calls.get(), 2, "one mine, then exactly one re-mine");
        let submitted = chain.submitted_entry.borrow();
        assert_eq!(submitted.len(), 1, "only the accepted proof is recorded");
        assert!(
            submitted[0].meets(&harder),
            "the resubmitted proof must clear the target that refused the first one"
        );
        let old_nonce = (0u64..1_000_000)
            .find(|n| build_entry_pow(*n, [0u8; 32], &easy_target()).is_some())
            .expect("an easy target must have a solution");
        let joined = lines.join("\n");
        assert!(
            joined.contains("re-mined against the newer target"),
            "the log must name the re-mine, got: {joined}"
        );
        assert!(
            joined.contains(&format!("nonce {old_nonce:#x}")),
            "the log must carry the refused nonce, got: {joined}"
        );
        assert!(
            joined.contains(&format!("nonce {:#x}", submitted[0].nonce)),
            "the log must carry the new nonce, got: {joined}"
        );
        assert!(
            joined.contains(&format!(
                "old target ~2^{:.1}",
                difficulty_bits(&easy_target())
            )) && joined.contains(&format!("new target ~2^{:.1}", difficulty_bits(&harder))),
            "the log must carry both targets, got: {joined}"
        );
    }

    #[test]
    fn a_refusal_that_is_not_a_proof_race_is_not_re_mined() {
        let chain = FakeChain::new(fresh_status(), easy_target());
        chain.refuse("Custom error: 3", 1, Some(U256::from(1u8) << 249));
        let miner = InjectedMiner {
            start: 0,
            calls: Default::default(),
        };
        let mut log = |_: &str| {};
        let outcome = block_on(run_entry(&chain, &miner, "acct", &params(), true, &mut log));
        assert!(
            matches!(
                outcome,
                EntryOutcome::Failed(FailureReason::FreeEntryAccountUsed, _)
            ),
            "a spent free entry is a real refusal, got {outcome:?}"
        );
        assert_eq!(miner.calls.get(), 1, "only a proof race is worth re-mining");
    }

    #[test]
    fn a_legacy_bad_proof_that_still_meets_the_live_target_is_not_re_mined() {
        let chain = FakeChain::new(fresh_status(), easy_target());
        chain.refuse("Transaction has a bad signature", 1, None);
        let miner = InjectedMiner {
            start: 0,
            calls: Default::default(),
        };
        let mut log = |_: &str| {};
        let outcome = block_on(run_entry(&chain, &miner, "acct", &params(), true, &mut log));
        assert!(
            matches!(
                outcome,
                EntryOutcome::Failed(FailureReason::SubmitRejected, _)
            ),
            "a refusal the proof does not explain is reported, got {outcome:?}"
        );
        assert_eq!(
            miner.calls.get(),
            1,
            "no re-mine when the target did not move"
        );
    }

    #[test]
    fn a_legacy_bad_proof_after_a_target_move_is_re_mined_and_accepted() {
        // A chain older than spec 54 renders the same precheck refusal as
        // `BadProof`; the live target is what tells a race from a real refusal.
        let chain = FakeChain::new(fresh_status(), easy_target());
        let harder = U256::from(1u8) << 249;
        chain.refuse(
            "Invalid Transaction: Transaction has a bad signature",
            1,
            Some(harder),
        );
        let miner = InjectedMiner {
            start: 0,
            calls: Default::default(),
        };
        let mut lines: Vec<String> = Vec::new();
        let mut log = |s: &str| lines.push(s.to_string());
        let outcome = block_on(run_entry(&chain, &miner, "acct", &params(), true, &mut log));
        assert_eq!(outcome, EntryOutcome::Registered);
        assert_eq!(miner.calls.get(), 2, "one mine, then one re-mine");
        assert!(
            chain.submitted_entry.borrow()[0].meets(&harder),
            "the accepted proof clears the newer target"
        );
        assert!(lines
            .join("\n")
            .contains("re-mined against the newer target"));
    }

    #[test]
    fn a_proof_race_that_never_clears_gives_up_after_two_re_mines() {
        let chain = FakeChain::new(fresh_status(), easy_target());
        chain.refuse("Custom error: 1", 5, Some(U256::from(1u8) << 249));
        let miner = InjectedMiner {
            start: 0,
            calls: Default::default(),
        };
        let mut log = |_: &str| {};
        let outcome = block_on(run_entry(&chain, &miner, "acct", &params(), true, &mut log));
        assert!(
            matches!(
                outcome,
                EntryOutcome::Failed(FailureReason::SubmitRejected, _)
            ),
            "three refusals must end in a reported failure, got {outcome:?}"
        );
        assert_eq!(
            miner.calls.get(),
            3,
            "initial mine + two re-mines, then stop"
        );
    }

    #[test]
    fn combined_call_missing_falls_back_to_attest_then_register() {
        let mut chain = FakeChain::new(fresh_status(), easy_target());
        chain.register_entry =
            Err("Call `register_provider_entry` not found in pallet ComputeMarketplace".into());
        let miner = InjectedMiner {
            start: 0,
            calls: Default::default(),
        };
        let mut log = |_: &str| {};
        let outcome = block_on(run_entry(&chain, &miner, "acct", &params(), true, &mut log));
        assert_eq!(outcome, EntryOutcome::RegisteredViaFallback);
        assert_eq!(chain.submitted_attest.borrow().len(), 1);
        assert_eq!(*chain.submitted_plain.borrow(), 1);
    }

    #[test]
    fn free_entry_spent_reports_a_specific_reason() {
        let mut status = fresh_status();
        status.free_entry_used = true;
        let chain = FakeChain::new(status, easy_target());
        let miner = InjectedMiner {
            start: 0,
            calls: Default::default(),
        };
        let mut log = |_: &str| {};
        let outcome = block_on(run_entry(&chain, &miner, "acct", &params(), true, &mut log));
        match outcome {
            EntryOutcome::Failed(reason, detail) => {
                assert_eq!(reason, FailureReason::FreeEntryAccountUsed);
                assert!(detail.contains("register_provider_entry_unsigned"));
                assert!(
                    !detail.to_ascii_lowercase().contains("paid"),
                    "a spent free entry must not offer a 'paid path': {detail}"
                );
                assert!(!detail.to_ascii_lowercase().contains("deposit gpn"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(miner.calls.get(), 0);
    }

    #[test]
    fn per_block_cap_is_distinguished_from_permanent_spend() {
        let mut status = fresh_status();
        status.free_used_this_block = 2;
        let chain = FakeChain::new(status, easy_target());
        let miner = InjectedMiner {
            start: 0,
            calls: Default::default(),
        };
        let mut log = |_: &str| {};
        let outcome = block_on(run_entry(&chain, &miner, "acct", &params(), true, &mut log));
        match outcome {
            EntryOutcome::Failed(reason, _) => {
                assert_eq!(reason, FailureReason::FreeEntryLimitReached)
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn chain_rejection_is_classified_per_reason() {
        let cases = [
            (
                "GPUAttestation.FreeEntryGpuUsed",
                FailureReason::FreeEntryGpuUsed,
            ),
            (
                "GPUAttestation.FreeEntryAccountUsed",
                FailureReason::FreeEntryAccountUsed,
            ),
            (
                "GPUAttestation.FreeEntryLimitReached",
                FailureReason::FreeEntryLimitReached,
            ),
            (
                "GPUAttestation.GpuAlreadyOwned",
                FailureReason::GpuAlreadyOwned,
            ),
            (
                "GPUAttestation.BelowEligibilityFloor",
                FailureReason::BelowEligibilityFloor,
            ),
            ("SomethingElse", FailureReason::SubmitRejected),
        ];
        for (err, want) in cases {
            let (got, detail) = classify_submit_error(err);
            assert_eq!(got, want, "{err}");
            assert!(!detail.is_empty());
        }
    }

    /// Row 243: the pool reports a refused *unsigned entry* as
    /// `InvalidTransaction::Custom(n)` — the node's rendering is
    /// `Custom error: {n}`. Each code must come back as the real reason and as
    /// the sentence an operator can act on, and no code may fall back to the
    /// "bad signature" wording that sent two investigations the wrong way.
    #[test]
    fn pool_boundary_reject_codes_are_translated_back_into_reasons() {
        let cases = [
            (1u8, FailureReason::SubmitRejected, "difficulty target"),
            (2, FailureReason::GpuIdentityMissing, "hardware fingerprint"),
            (
                3,
                FailureReason::FreeEntryAccountUsed,
                "free proof-of-work entry",
            ),
            (4, FailureReason::FreeEntryGpuUsed, "GPU fingerprint"),
            (5, FailureReason::FreeEntryLimitReached, "per-block cap"),
        ];
        for (code, want, phrase) in cases {
            // Exactly the string the node's RPC layer produces for Custom(n).
            let err = format!(
                "entry-submit-rejected: {{\"code\":1010,\"data\":\"Custom error: {code}\"}}"
            );
            assert_eq!(custom_reject_code(&err), Some(code), "{err}");
            let (got, detail) = classify_submit_error(&err);
            assert_eq!(got, want, "{err}");
            assert!(
                detail
                    .to_ascii_lowercase()
                    .contains(&phrase.to_ascii_lowercase()),
                "code {code} should explain '{phrase}', got: {detail}"
            );
            assert!(
                !detail.contains("bad signature"),
                "code {code} must not claim a signature fault: {detail}"
            );
        }

        // An unknown number is reported as unknown, not silently mapped onto a
        // different reason — a new failure mode must not masquerade as an old one.
        let (got, detail) = classify_submit_error("Custom error: 200");
        assert_eq!(got, FailureReason::SubmitRejected);
        assert!(detail.contains("rejected"), "{detail}");

        // No number at all (the pool's data-less rendering) is not guessed at.
        assert_eq!(custom_reject_code("InvalidTransaction custom error"), None);
        assert_eq!(custom_reject_code("Transaction has a bad signature"), None);
    }

    #[test]
    fn missing_gpu_identity_never_mines() {
        let chain = FakeChain::new(fresh_status(), easy_target());
        let miner = InjectedMiner {
            start: 0,
            calls: Default::default(),
        };
        let mut p = params();
        p.gpu_info_hash = [0u8; 32];
        let mut log = |_: &str| {};
        let outcome = block_on(run_entry(&chain, &miner, "acct", &p, true, &mut log));
        match outcome {
            EntryOutcome::Failed(reason, _) => {
                assert_eq!(reason, FailureReason::GpuIdentityMissing)
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(miner.calls.get(), 0);
    }

    #[test]
    fn chain_read_failure_is_not_mistaken_for_unregistered() {
        struct Broken;
        impl EntryChain for Broken {
            async fn status(&self, _a: &str) -> Result<EntryStatus, String> {
                Err("rpc down".into())
            }
            async fn difficulty(&self) -> Result<U256, String> {
                Err("rpc down".into())
            }
            async fn submit_register_provider_entry(
                &self,
                _p: &EntryPow,
                _q: &EntryParams,
            ) -> Result<(), String> {
                panic!("must not submit without a status read")
            }
            async fn submit_attest_entry(
                &self,
                _p: &EntryPow,
                _q: &EntryParams,
            ) -> Result<(), String> {
                panic!("must not submit without a status read")
            }
            async fn submit_register_provider(&self, _q: &EntryParams) -> Result<(), String> {
                panic!("must not submit without a status read")
            }
        }
        let miner = InjectedMiner {
            start: 0,
            calls: Default::default(),
        };
        let mut log = |_: &str| {};
        let outcome = block_on(run_entry(
            &Broken,
            &miner,
            "acct",
            &params(),
            true,
            &mut log,
        ));
        match outcome {
            EntryOutcome::Failed(reason, _) => assert_eq!(reason, FailureReason::ChainReadFailed),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(miner.calls.get(), 0);
    }

    #[test]
    fn plan_matrix_is_total() {
        let mut s = fresh_status();
        assert_eq!(plan_entry(true, &s), EntryPlan::Proceed);
        assert_eq!(plan_entry(false, &s), EntryPlan::Direct);
        s.registered = true;
        assert_eq!(plan_entry(false, &s), EntryPlan::AlreadyRegistered);
        s = fresh_status();
        s.attested = true;
        assert_eq!(plan_entry(true, &s), EntryPlan::AlreadyAttested);
        s = fresh_status();
        s.free_entry_used = true;
        assert_eq!(plan_entry(true, &s), EntryPlan::FreeEntrySpent);
        s = fresh_status();
        s.free_used_this_block = 2;
        assert_eq!(plan_entry(true, &s), EntryPlan::FreeBudgetThisBlock);
        // A stale counter from an older block is not a cap signal.
        s.counter_is_current_block = false;
        assert_eq!(plan_entry(true, &s), EntryPlan::Proceed);
        // A zero limit disables the free path at runtime → not a cap hit.
        s = fresh_status();
        s.free_limit = 0;
        assert_eq!(plan_entry(true, &s), EntryPlan::Proceed);
    }
}
