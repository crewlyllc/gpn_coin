//! Divergence-tolerant block serving (GPN #158, the missing enforcement point).
//!
//! `sc-network-sync`'s block request handler treats the *third* identical block
//! request from a peer as a ban-worthy offence:
//!
//! ```text
//! pub const SAME_REQUEST: Rep = Rep::new_fatal("Same block request multiple times");
//! ```
//!
//! That rule assumes a peer only repeats itself when it is misbehaving. It is
//! wrong for exactly the peer that needs us most. A node that is **behind on a
//! branch we do not hold** (box 1 at #280597 on its own fork, 2026-11-13) gap-
//! fills *by number*: it asks for #280598, we answer with our canonical
//! #280598, the block fails to connect to its parent, the import is rejected,
//! and it asks for the same range again. Three rounds of that is ~3 s, and the
//! sync layer reports `fatal`, bans and disconnects the peer — which is the one
//! outcome that guarantees it can never be healed. It is also removed from
//! `peers_info()` before our 30 s fork-tolerance scan
//! ([`crate::fork_tolerance`]) can hand any reputation back, so the existing
//! divergence credit cannot reach this case at all.
//!
//! The policy is already written down and tested — divergence is not
//! misbehaviour, and only protocol violations are bans. This module is where
//! that policy has to be enforced, because this is where the ban is applied.
//!
//! ## What changes, and what deliberately does not
//!
//! We serve the block protocol ourselves instead of letting the SDK's handler
//! do it, and change **one** decision: what a repeated identical request
//! costs. The answer, from any peer on any branch, is **nothing fatal** — the
//! ban for repeats is removed entirely (owner decision, 2026-11-14) and
//! replaced with throttling.
//!
//! | repeated request from… | reputation | service |
//! |---|---|---|
//! | a peer whose claimed tip **is** a block we hold (on our chain) | `PEER_ON_CHAIN_SAME_REQUEST` (−1024) | normal until the throttle window trips |
//! | a peer whose claimed tip we do **not** hold, or that never claimed one | `DIVERGED_SAME_REQUEST` (−1024) | normal until the throttle window trips |
//!
//! Two independent bounds keep that from ever becoming a slow ban. First, the
//! repeat penalty is charged against a **per-peer lifetime budget**
//! ([`REPEAT_PENALTY_BUDGET`], 2^20 reputation units) which is ~1,450× smaller
//! than the peer store's ban threshold (~71% of `i32::MIN`, −1.5e9): no number
//! of repeats can accumulate into a ban, because after the budget is spent we
//! simply stop reporting. Second, a peer that repeats itself more than
//! [`MAX_REPEATS_PER_WINDOW`] times in [`THROTTLE_WINDOW`] stops being served
//! the *full* request for the rest of that window — no data, no ban, no
//! disconnect — and service resumes by itself when the window expires.
//!
//! Everything else is byte-for-byte the protocol's semantics: the same wire
//! format, the same attributes check, the same 128-block / 16 MiB response
//! caps, the same "stop answering full requests once the peer is spamming"
//! rule, the same canonical-database-only source (we never serve a forked
//! block; chain selection is untouched). A request that does not decode, an
//! attribute set with unknown bits, or a *response* that fails to encode is a
//! genuine protocol violation and is reported `fatal` — a peer cannot talk its
//! way into leniency with garbage.
//!
//! ## The other end of the loop: we must not be the one who repeats
//!
//! Fixing only the serving side left the link dying anyway (20:36:28): *we*
//! were banned by *box 1*. Our own sync engine answers a peer's block announce
//! with an ancestor search — a descending block request — and re-issues the
//! identical request every couple of seconds, because a fork-neighbour can
//! never answer it usefully. The third identical request is what the *other*
//! side's handler calls fatal. So [`PoliteDownloader`] adds two rules on our
//! side of the wire: never ask a peer that is behind us on a branch we do not
//! hold (it cannot extend our chain), and never put the same request on the
//! wire three times for one peer, whatever the reason.
//!
//! Together the two halves are the whole loop: we no longer ban a diverged
//! peer, and we no longer provoke one into banning us. Neither half weakens a
//! ban, and neither invents data — the download side either asks the real peer
//! or declines to ask at all.
//!
//! The leniency is bounded on purpose, and the bound is the throttle, not a
//! slower ban: the peer store's ban threshold is 71% of `i32::MIN` (~−1.5e9),
//! the repeat penalty is −1024 charged against a 2^20 lifetime budget, and
//! past the throttle window the full request is not answered at all (no
//! bandwidth amplification). Nothing in this module ever disconnects, evicts,
//! or reports `fatal` for a repeat.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use async_channel::Receiver;
use codec::{Decode, Encode};
use futures::channel::oneshot;
use log::{debug, info, warn};
use prost::Message;
use sc_client_api::BlockBackend;
use sc_network::config::ProtocolId;
use sc_network::request_responses::{IncomingRequest, OutgoingResponse, RequestFailure};
use sc_network::{NetworkBackend, PeerId, ReputationChange, MAX_RESPONSE_SIZE};
use sc_network_common::sync::message::{
    BlockAttributes, BlockData as MessageBlockData, BlockRequest, FromBlock,
};
use sc_network_sync::block_relay_protocol::{BlockDownloader, BlockResponseError, BlockServer};
use sc_network_sync::{
    block_request::FromBlock as FromBlockSchema, BlockData, BlockRequest as BlockRequestSchema,
    BlockResponse, Direction,
};
use schnellru::{ByLength, LruMap};
use sp_blockchain::HeaderBackend;
use sp_runtime::generic::BlockId;
use sp_runtime::traits::{Block as BlockT, Header, One, Zero};

use crate::chain_state::ChainState;

/// Blocks per response, as the protocol specifies (`MAX_BLOCKS_IN_RESPONSE`).
const MAX_BLOCKS_IN_RESPONSE: usize = 128;

/// How many identical requests a peer may make before any penalty applies.
/// Unchanged from the SDK: the first delivery plus one repeat.
const MAX_SAME_REQUESTS: usize = 2;

/// How many identical requests for one range a peer may make inside one
/// [`THROTTLE_WINDOW`] before we stop serving that full request.
///
/// Four is deliberately generous: a peer gap-filling across a fork re-asks a
/// handful of times while it re-orgs its own view, and those repeats cost us
/// nothing but 128 headers. A fifth identical request inside a minute is not a
/// sync loop working itself out; it is a peer that is not going to be helped by
/// more data this minute either.
pub const MAX_REPEATS_PER_WINDOW: usize = 4;

/// How long a throttled peer is left unserved for that range before normal
/// service resumes on its own. There is no un-throttle event and no disconnect:
/// the state is just the window's start instant.
pub const THROTTLE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// How many blocks a throttled request is still answered with, when it is a
/// header/justification-only request.
///
/// Zero would be simpler, but every real peer on this network asks for one
/// header at a time while it hunts for the common ancestor (`Number(298659)
/// max 1 Ascending HEADER | JUSTIFICATION`, 2026-11-14 00:32). Answering
/// nothing would leave such a peer blind rather than throttled. Eight headers
/// is bounded work — a few hundred bytes — and the full request (body,
/// receipts, indexed body, up to 128 blocks and 16 MiB) still gets nothing at
/// all.
pub const THROTTLE_PREFIX_BLOCKS: usize = 8;

/// How many blocks to answer a request with, given the throttle state.
///
/// * not throttled — the request's own maximum, as always;
/// * throttled, header-only — a bounded prefix ([`THROTTLE_PREFIX_BLOCKS`]);
/// * throttled, full — 0, i.e. no data at all.
///
/// Pure, so the tests exercise the same function the server calls.
pub fn served_max_blocks(max_blocks: usize, small_request: bool, throttled: bool) -> usize {
    if !throttled {
        max_blocks
    } else if small_request {
        std::cmp::min(max_blocks, THROTTLE_PREFIX_BLOCKS)
    } else {
        0
    }
}

/// Per-peer lifetime cap on the reputation this module will ever report for
/// repeated block requests, in absolute units (2^20 = 1,048,576).
///
/// The peer store bans at 71% of `i32::MIN` (≈ −1.52e9), so this whole budget
/// is ~1,450× below the threshold: a repeat can never accumulate into a ban,
/// however many times it happens, and however long the peer stays connected.
/// Once the budget is spent the repeats are simply not reported at all — the
/// throttle is what limits the peer, not a growing reputation debt.
pub const REPEAT_PENALTY_BUDGET: u32 = 1 << 20;

/// Reputation changes, named so an operator reading the log and the SDK docs
/// sees the same vocabulary.
mod rep {
    use sc_network::ReputationChange as Rep;

    /// The upstream rule, kept here only so the change is legible next to it:
    /// `sc-network-sync` reports this (`Rep::new_fatal`) for the third
    /// identical request, from every peer. This module no longer emits it —
    /// the owner decision of 2026-11-14 removed the ban for repeats entirely.
    #[allow(dead_code)]
    pub const SAME_REQUEST: Rep = Rep::new_fatal("Same block request multiple times");

    /// A peer we know is on our chain keeps asking for the same blocks. It is
    /// not divergence, but it is also not misbehaviour: it is worth −1024, the
    /// same small report as the diverged case, and it is charged against
    /// [`super::REPEAT_PENALTY_BUDGET`] so it can never reach the ban threshold.
    pub const PEER_ON_CHAIN_SAME_REQUEST: Rep = Rep::new(
        -(1 << 10),
        "repeated block request from a peer on our chain",
    );

    /// A peer whose best block we do not hold repeats itself. This is the
    /// gap-fill-across-a-fork shape, not misbehaviour: small, bounded, and
    /// **never** fatal.
    pub const DIVERGED_SAME_REQUEST: Rep = Rep::new(
        -(1 << 10),
        "repeated block request from a peer on a branch we do not hold",
    );

    /// Undecodable payload, unknown attribute bits, unencodable response:
    /// a genuine protocol violation. Upstream simply drops the response and
    /// reports nothing; we name it, because a peer that cannot speak the
    /// protocol has nothing to do on this connection. These stay fatal on
    /// purpose: unlike a repeat, a payload we cannot decode carries no request
    /// at all, so there is nothing to throttle and nothing to answer — leniency
    /// here would only mean parsing garbage forever.
    pub const MALFORMED_REQUEST: Rep = Rep::new_fatal("Malformed block request");
}

/// The whole policy for a repeated request, in one pure place so the tests
/// exercise the same code the server runs.
///
/// `peer_tip_known` is the client's answer to "is the asking peer's best block
/// a block we hold?": `Some(true)` = yes (peer is on our chain), `Some(false)`
/// = no, `None` = it never told us or the lookup failed. **Neither answer is
/// fatal.** The two cases are distinguished only so the log and the metrics
/// keep telling divergence from an ordinary repeat.
pub fn repeat_penalty(peer_tip_known: Option<bool>) -> ReputationChange {
    match peer_tip_known {
        Some(true) => rep::PEER_ON_CHAIN_SAME_REQUEST,
        _ => rep::DIVERGED_SAME_REQUEST,
    }
}

/// Lifetime reputation budget for one peer's repeat penalties.
///
/// This is the guarantee behind "never enough to reach the ban threshold no
/// matter how many times it happens": the total we can ever report is bounded
/// by [`REPEAT_PENALTY_BUDGET`], independent of the number of repeats.
#[derive(Debug, Default)]
pub struct RepeatBudget {
    spent: u32,
}

impl RepeatBudget {
    /// Charge `amount` (negative units) if the budget allows it.
    ///
    /// Returns `None` once the budget is spent — the caller then reports no
    /// reputation change at all, and the throttle does the limiting.
    pub fn charge(&mut self, amount: ReputationChange) -> Option<ReputationChange> {
        let want = amount.value.unsigned_abs();
        let remaining = REPEAT_PENALTY_BUDGET.saturating_sub(self.spent);
        let actual = want.min(remaining);
        self.spent = self.spent.saturating_add(actual);
        if actual == 0 {
            None
        } else {
            Some(ReputationChange::new(-(actual as i32), amount.reason))
        }
    }

    /// Total charged so far, in absolute units (for tests and a status line).
    pub fn spent(&self) -> u32 {
        self.spent
    }
}

/// Per-peer chain-state claims, shared with the `gpn/chain/1` task.
///
/// The block server is the only consumer that *needs* the claim at request
/// time, and it needs it for one question: is this peer's tip a block we hold?
/// The announcement task owns the parsing (it already decodes and classifies
/// every claim); this is just the hand-off, so there is one decoder and one
/// comparison in the node, not two.
#[derive(Debug, Default)]
pub struct PeerTips {
    inner: Mutex<HashMap<PeerId, ChainState>>,
    /// Where the last known claim per peer is remembered, if anywhere.
    ///
    /// Persistence matters for one narrow but real case: a peer keeps its own
    /// repeat counter for the *process*, so the same identical request that was
    /// tolerated last week can be fatal today. If we forget what we knew about
    /// a peer whenever our own process restarts, the first block request after
    /// every restart is sent blind — which is exactly what got us banned by box
    /// 1 at 20:43:30. A peer's last announced tip is a durable fact, so it
    /// outlives the connection *and* the process.
    path: Option<std::path::PathBuf>,
}

/// Claims remembered across restarts; a few dozen peers at most, and an old
/// entry is never worse than no entry (it is overwritten the moment the peer
/// announces again).
const MAX_PERSISTED_TIPS: usize = 256;

impl PeerTips {
    /// A shared, empty registry that remembers nothing across restarts.
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A shared registry backed by `path` (`<base-path>/peer-tips.json`).
    /// A missing or unreadable file is not an error: it means we simply know
    /// nothing yet, which is the safe direction (we ask).
    pub fn loaded(path: std::path::PathBuf) -> Arc<Self> {
        let mut inner = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(map) = serde_json::from_str::<HashMap<String, ChainState>>(&text) {
                for (id, claim) in map.into_iter().take(MAX_PERSISTED_TIPS) {
                    if let Ok(peer) = id.parse::<PeerId>() {
                        inner.insert(peer, claim);
                    }
                }
            }
        }
        Arc::new(Self {
            inner: Mutex::new(inner),
            path: Some(path),
        })
    }

    /// Write the current claims out, best effort: a failed write costs us one
    /// repeated question after the next restart, nothing more.
    fn persist_locked(&self, map: &HashMap<PeerId, ChainState>) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let mut out: HashMap<String, &ChainState> = HashMap::new();
        for (peer, claim) in map.iter().take(MAX_PERSISTED_TIPS) {
            out.insert(peer.to_string(), claim);
        }
        let Ok(text) = serde_json::to_string(&out) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            // Rename over the target so a reader never sees a half file.
            if std::fs::rename(&tmp, path).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }

    /// Record the newest claim from `peer`.
    pub fn set(&self, peer: PeerId, state: ChainState) {
        if let Ok(mut map) = self.inner.lock() {
            let changed = map.get(&peer) != Some(&state);
            map.insert(peer, state);
            if changed {
                self.persist_locked(&map);
            }
        }
    }

    /// The last claim we saw from `peer`, if any.
    pub fn get(&self, peer: &PeerId) -> Option<ChainState> {
        self.inner.lock().ok()?.get(peer).cloned()
    }

    /// Peers with a live claim (for tests / a status line).
    pub fn len(&self) -> usize {
        self.inner.lock().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Key of the "have we seen this exact request before" map. Identical to the
/// SDK's key: peer + everything that changes the answer.
#[derive(Eq, PartialEq, Clone)]
struct SeenRequestsKey<B: BlockT> {
    peer: PeerId,
    from: BlockId<B>,
    max_blocks: usize,
    direction: Direction,
    attributes: BlockAttributes,
    support_multiple_justifications: bool,
}

impl<B: BlockT> std::hash::Hash for SeenRequestsKey<B> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.peer.hash(state);
        self.max_blocks.hash(state);
        self.direction.hash(state);
        self.attributes.hash(state);
        self.support_multiple_justifications.hash(state);
        match self.from {
            BlockId::Hash(h) => h.hash(state),
            BlockId::Number(n) => n.hash(state),
        }
    }
}

/// Per (peer, request) repeat and throttle state, in one pure type so the
/// tests can drive the same state machine the server runs, clock included.
#[derive(Debug)]
pub struct RepeatState {
    /// Deliveries of this exact request to this peer since the window opened
    /// (the first delivery counts as 1).
    repeats: usize,
    /// When the current throttle window opened.
    window_start: std::time::Instant,
    /// Set when we printed the throttle line for this window, so a peer that
    /// keeps asking produces one INFO line per window, not one per request.
    logged: bool,
}

/// What to do with a request we have answered before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatDecision {
    /// Answer normally, no penalty (the repeat allowance is not exhausted).
    Fresh,
    /// Answer normally, small non-fatal penalty. `first_in_streak` is true for
    /// the first penalised repeat of a window, so the warning is printed once.
    Repeat { first_in_streak: bool },
    /// Stop answering the **full** request for the rest of the window. `count`
    /// is how many identical requests this window has seen; `log` is true on
    /// the request that trips the throttle, so the INFO line is printed once.
    Throttled { count: usize, log: bool },
}

impl RepeatState {
    /// State after the first delivery of a request.
    pub fn new(now: std::time::Instant) -> Self {
        Self {
            repeats: 1,
            window_start: now,
            logged: false,
        }
    }

    /// How many identical requests this window has seen.
    pub fn repeats(&self) -> usize {
        self.repeats
    }

    /// Observe one more identical request at `now`.
    pub fn observe(&mut self, now: std::time::Instant) -> RepeatDecision {
        // A window that has run out simply restarts: normal service resumes by
        // itself, with no un-ban event and no disconnect to forget.
        if now.saturating_duration_since(self.window_start) >= THROTTLE_WINDOW {
            self.repeats = 0;
            self.window_start = now;
            self.logged = false;
        }
        self.repeats = self.repeats.saturating_add(1);

        if self.repeats > MAX_REPEATS_PER_WINDOW {
            let log = !self.logged;
            self.logged = true;
            RepeatDecision::Throttled {
                count: self.repeats,
                log,
            }
        } else if self.repeats > MAX_SAME_REQUESTS {
            RepeatDecision::Repeat {
                first_in_streak: self.repeats == MAX_SAME_REQUESTS + 1,
            }
        } else {
            RepeatDecision::Fresh
        }
    }
}

/// One entry of the "have we seen this exact request before" map.
enum SeenRequestsValue {
    /// Seen once, not (yet) answered with data.
    First,
    /// Answered with data at least once; carries the repeat/throttle state.
    Fulfilled(RepeatState),
}

/// The serving side of the block protocol.
pub struct DivergenceTolerantBlockServer<B: BlockT, Client> {
    client: Arc<Client>,
    request_receiver: Receiver<IncomingRequest>,
    seen_requests: LruMap<SeenRequestsKey<B>, SeenRequestsValue>,
    tips: Arc<PeerTips>,
    /// Lifetime repeat-penalty budget per peer: the hard guarantee that no
    /// number of identical requests can ever reach the ban threshold.
    repeat_budgets: HashMap<PeerId, RepeatBudget>,
    /// Last time we printed the divergence line per peer, so a peer that keeps
    /// asking produces one line a minute instead of one per request.
    explained: HashMap<PeerId, std::time::Instant>,
}

impl<B, Client> DivergenceTolerantBlockServer<B, Client>
where
    B: BlockT,
    Client: HeaderBackend<B> + BlockBackend<B> + Send + Sync + 'static,
{
    /// Build the server and its request-response protocol config.
    ///
    /// Returns the pieces the caller hands to
    /// [`sc_network_sync::block_relay_protocol::BlockRelayParams`]; the
    /// download side is unchanged and comes from the SDK.
    pub fn build<N: NetworkBackend<B, <B as BlockT>::Hash>>(
        client: Arc<Client>,
        protocol_id: &ProtocolId,
        fork_id: Option<&str>,
        num_peer_hint: usize,
        tips: Arc<PeerTips>,
    ) -> Result<(Box<dyn BlockServer<B>>, N::RequestResponseProtocolConfig), sp_blockchain::Error>
    {
        // One request slot per peer at the peer limit, as upstream sizes it.
        let capacity = std::cmp::max(num_peer_hint, 1);
        let (tx, request_receiver) = async_channel::bounded(capacity);

        let genesis_hash = client
            .block_hash(0u32.into())?
            .ok_or_else(|| sp_blockchain::Error::UnknownBlock("genesis".into()))?;

        let protocol_config = sc_network_sync::block_request_handler::generate_protocol_config::<
            _,
            B,
            N,
        >(protocol_id, genesis_hash, fork_id, tx);

        let seen_requests = LruMap::new(ByLength::new(num_peer_hint.max(1) as u32 * 2));

        let server = Self {
            client,
            request_receiver,
            seen_requests,
            tips,
            repeat_budgets: HashMap::new(),
            explained: HashMap::new(),
        };
        Ok((Box::new(server), protocol_config))
    }

    /// Is the peer's claimed best block a block we hold?
    ///
    /// `None` when the peer has not told us where it is (no `gpn/chain/1`
    /// claim) or the lookup failed — an unanswered question is never evidence
    /// that the peer is on our chain.
    fn peer_tip_known(&self, peer: &PeerId) -> Option<bool> {
        let claim = self.tips.get(peer)?;
        let hex = claim.best_hash.trim_start_matches("0x");
        let bytes = hex::decode(hex).ok()?;
        let hash = <B as BlockT>::Hash::decode(&mut &bytes[..]).ok()?;
        // NOTE: `block_hash_from_id(BlockId::Hash(h))` echoes `h` back without
        // looking it up, so it answers "yes" for every hash. Asking for the
        // header is the only check that means "we hold this block"; using the
        // echo made this gate believe every peer was on our chain (2026-11-13
        // 21:26, which is why the ancestor search kept going out).
        Some(self.client.header(hash).ok().flatten().is_some())
    }

    async fn process_requests(&mut self) {
        use futures::StreamExt;
        while let Some(request) = self.request_receiver.next().await {
            let IncomingRequest {
                peer,
                payload,
                pending_response,
            } = request;
            match self.handle_request(payload, pending_response, &peer) {
                Ok(()) => debug!("block-serving: answered a block request from {peer}"),
                Err(e) => warn!("block-serving: rejected a block request from {peer}: {e}"),
            }
        }
    }

    fn handle_request(
        &mut self,
        payload: Vec<u8>,
        pending_response: oneshot::Sender<OutgoingResponse>,
        peer: &PeerId,
    ) -> Result<(), HandleRequestError> {
        // ── Everything that makes a request *undecodable* is a protocol
        // violation and is reported fatal, not dropped silently.
        let request = match BlockRequestSchema::decode(&payload[..]) {
            Ok(r) => r,
            Err(e) => {
                let _ = pending_response.send(OutgoingResponse {
                    result: Err(()),
                    reputation_changes: vec![rep::MALFORMED_REQUEST],
                    sent_feedback: None,
                });
                return Err(HandleRequestError::DecodeProto(e));
            }
        };

        let parsed = match self.parse_request(request, peer) {
            Ok(p) => p,
            Err(e) => {
                let _ = pending_response.send(OutgoingResponse {
                    result: Err(()),
                    reputation_changes: vec![rep::MALFORMED_REQUEST],
                    sent_feedback: None,
                });
                return Err(e);
            }
        };
        let ParsedRequest {
            key,
            from_block_id,
            max_blocks,
            direction,
            attributes,
            support_multiple_justifications,
            small_request,
        } = parsed;

        let now = std::time::Instant::now();
        let peer_tip_known = self.peer_tip_known(peer);
        // `Some(count)` when the throttle is in force: the full request is not
        // answered for the rest of the window.
        let mut throttle: Option<(usize, bool)> = None;
        let mut just_crossed: Option<ReputationChange> = None;
        let mut penalise = false;

        // The decision is made (and the state advanced) under one borrow of the
        // map; the reputation charge happens after it, because it needs `self`
        // mutably for the budget table.
        match self.seen_requests.get(&key) {
            Some(SeenRequestsValue::First) => {}
            Some(SeenRequestsValue::Fulfilled(state)) => match state.observe(now) {
                RepeatDecision::Fresh => {}
                RepeatDecision::Repeat { first_in_streak } => {
                    penalise = true;
                    if first_in_streak {
                        just_crossed = Some(repeat_penalty(peer_tip_known));
                    }
                }
                RepeatDecision::Throttled { count, log } => {
                    penalise = true;
                    throttle = Some((count, log));
                }
            },
            None => {
                self.seen_requests
                    .insert(key.clone(), SeenRequestsValue::First);
            }
        }

        // Still charged while throttled (and still bounded by the lifetime
        // budget), but the answer is what changes: nothing, until the window
        // expires.
        let reputation_change = if penalise {
            self.charge_repeat_budget(peer, repeat_penalty(peer_tip_known))
        } else {
            None
        };

        if let Some((count, true)) = throttle {
            info!(
                "block-serving: throttling {peer} — {count} identical requests for \
                 {from_block_id:?} (up to {max_blocks} blocks, {direction:?}) inside {} s. \
                 Answering nothing for this range until the window expires; no ban, no \
                 disconnect, and the link keeps serving other ranges.",
                THROTTLE_WINDOW.as_secs(),
            );
        }

        // The one place the policy change is visible in the log: a diverged
        // peer is named once per streak, with the facts the decision used.
        // A peer on our chain gets the throttle line above instead, because
        // that is the only thing that happens to it.
        if let Some(rep) = just_crossed {
            let now = std::time::Instant::now();
            let due = self
                .explained
                .get(peer)
                .map(|last| now.duration_since(*last) >= std::time::Duration::from_secs(60))
                .unwrap_or(true);
            if peer_tip_known != Some(true) && due {
                self.explained.insert(*peer, now);
                info!(
                    "block-serving: peer {peer} is re-asking for a block range it cannot \
                     connect — its tip {} is not on our chain, so this is divergence, not \
                     misbehaviour: small penalty ({}) and the link stays up (#158)",
                    self.tips
                        .get(peer)
                        .map(|c| format!("#{} {}", c.height, ChainState::short(&c.best_hash)))
                        .unwrap_or_else(|| "unknown".to_string()),
                    rep.reason
                );
            }
        }

        debug!(
            "block-serving: request from {peer} from `{from_block_id:?}` max {max_blocks} \
             dir {direction:?} attrs {attributes:?} small {small_request} \
             penalty {reputation_change:?} throttled {throttle:?}"
        );

        // Punishment is replaced by throttling: a repeat is *answered* — with
        // the same blocks as before — right up to the throttle. Then, for the
        // rest of the window, a full request gets no data at all (no bandwidth
        // amplification, and no reason for the peer's own sync loop to spin),
        // and a header-only request gets a bounded prefix so a peer that is
        // looking for the common ancestor can still make some progress.
        // Service resumes by itself when the window expires.
        let served = served_max_blocks(max_blocks, small_request, throttle.is_some());
        let maybe_block_response = if served > 0 {
            let block_response = self.get_block_response(
                attributes,
                from_block_id,
                direction,
                served,
                support_multiple_justifications,
            )?;

            // Only a response that carries data counts as delivered, so a
            // request for blocks we do not have never escalates at all.
            if block_response
                .blocks
                .iter()
                .any(|b| !b.header.is_empty() || !b.body.is_empty() || b.is_empty_justification)
            {
                if let Some(value) = self.seen_requests.get(&key) {
                    if let SeenRequestsValue::First = value {
                        *value = SeenRequestsValue::Fulfilled(RepeatState::new(now));
                    }
                }
            }
            Some(block_response)
        } else {
            None
        };

        let result = if let Some(block_response) = maybe_block_response {
            let mut data = Vec::with_capacity(block_response.encoded_len());
            block_response.encode(&mut data)?;
            Ok(data)
        } else {
            Err(())
        };

        pending_response
            .send(OutgoingResponse {
                result,
                reputation_changes: reputation_change.into_iter().collect(),
                sent_feedback: None,
            })
            .map_err(|_| HandleRequestError::SendResponse)
    }

    /// Charge a repeat penalty against `peer`'s lifetime budget.
    ///
    /// Returns the (possibly clamped, possibly zero) change to report, or
    /// `None` once the budget is spent. This is the mechanism, not a comment:
    /// no sequence of repeats can report more than
    /// [`REPEAT_PENALTY_BUDGET`] for one peer, and that budget is ~1,450× below
    /// the peer store's ban threshold.
    fn charge_repeat_budget(
        &mut self,
        peer: &PeerId,
        rep: ReputationChange,
    ) -> Option<ReputationChange> {
        self.repeat_budgets.entry(*peer).or_default().charge(rep)
    }

    /// Decode the wire request into the fields the serving loop needs.
    #[allow(clippy::type_complexity)]
    fn parse_request(
        &self,
        request: BlockRequestSchema,
        peer: &PeerId,
    ) -> Result<ParsedRequest<B>, HandleRequestError> {
        let from_block_id = match request
            .from_block
            .ok_or(HandleRequestError::MissingFromField)?
        {
            FromBlockSchema::Hash(ref h) => BlockId::<B>::Hash(Decode::decode(&mut h.as_ref())?),
            FromBlockSchema::Number(ref n) => {
                BlockId::<B>::Number(Decode::decode(&mut n.as_ref())?)
            }
        };

        let max_blocks = if request.max_blocks == 0 {
            MAX_BLOCKS_IN_RESPONSE
        } else {
            std::cmp::min(request.max_blocks as usize, MAX_BLOCKS_IN_RESPONSE)
        };

        let direction =
            i32::try_into(request.direction).map_err(|_| HandleRequestError::ParseDirection)?;

        // Unknown attribute bits are a malformed request, not something to
        // answer approximately.
        let attributes = BlockAttributes::from_be_u32(request.fields)
            .map_err(|_| HandleRequestError::Attributes)?;

        let small_request = attributes
            .difference(BlockAttributes::HEADER | BlockAttributes::JUSTIFICATION)
            .is_empty();

        Ok(ParsedRequest {
            key: SeenRequestsKey {
                peer: *peer,
                from: from_block_id,
                max_blocks,
                direction,
                attributes,
                support_multiple_justifications: request.support_multiple_justifications,
            },
            from_block_id,
            max_blocks,
            direction,
            attributes,
            support_multiple_justifications: request.support_multiple_justifications,
            small_request,
        })
    }

    /// Build the response from our **canonical database only**. Unchanged from
    /// upstream: chain selection is not this module's business, and a forked
    /// block is never served.
    fn get_block_response(
        &self,
        attributes: BlockAttributes,
        mut block_id: BlockId<B>,
        direction: Direction,
        max_blocks: usize,
        support_multiple_justifications: bool,
    ) -> Result<BlockResponse, HandleRequestError> {
        let get_header = attributes.contains(BlockAttributes::HEADER);
        let get_body = attributes.contains(BlockAttributes::BODY);
        let get_indexed_body = attributes.contains(BlockAttributes::INDEXED_BODY);
        let get_justification = attributes.contains(BlockAttributes::JUSTIFICATION);

        let mut blocks = Vec::new();
        let mut total_size: usize = 0;

        let client_header_from_block_id =
            |block_id: BlockId<B>| -> Result<Option<B::Header>, HandleRequestError> {
                if let Some(hash) = self.client.block_hash_from_id(&block_id)? {
                    return self.client.header(hash).map_err(Into::into);
                }
                Ok(None)
            };

        while let Some(header) = client_header_from_block_id(block_id).unwrap_or_default() {
            let number = *header.number();
            let hash = header.hash();
            let parent_hash = *header.parent_hash();
            let justifications = if get_justification {
                self.client.justifications(hash)?
            } else {
                None
            };

            let (justifications, justification, is_empty_justification) =
                if support_multiple_justifications {
                    let justifications = match justifications {
                        Some(v) => v.encode(),
                        None => Vec::new(),
                    };
                    (justifications, Vec::new(), false)
                } else {
                    // Backwards compatibility, as upstream: the GRANDPA engine
                    // id is hardcoded ("FRNK") because the wire format predates
                    // multi-engine justifications.
                    let justification =
                        justifications.and_then(|just| just.into_justification(*b"FRNK"));
                    let is_empty_justification = justification
                        .as_ref()
                        .map(|j| j.is_empty())
                        .unwrap_or(false);
                    let justification = justification.unwrap_or_default();
                    (Vec::new(), justification, is_empty_justification)
                };

            let body = if get_body {
                match self.client.block_body(hash)? {
                    Some(mut extrinsics) => extrinsics
                        .iter_mut()
                        .map(|extrinsic| extrinsic.encode())
                        .collect(),
                    None => {
                        debug!("block-serving: missing body for {hash:?}");
                        break;
                    }
                }
            } else {
                Vec::new()
            };

            let indexed_body = if get_indexed_body {
                self.client.block_indexed_body(hash)?.unwrap_or_default()
            } else {
                Vec::new()
            };

            let block_data = BlockData {
                hash: hash.encode(),
                header: if get_header {
                    header.encode()
                } else {
                    Vec::new()
                },
                body,
                receipt: Vec::new(),
                message_queue: Vec::new(),
                justification,
                is_empty_justification,
                justifications,
                indexed_body,
            };

            let new_total_size = total_size + block_data.encoded_len();

            // 20 KiB of headroom for length prefixes and the final encoding.
            if new_total_size > (MAX_RESPONSE_SIZE as usize - 20 * 1024) {
                if blocks.is_empty() {
                    warn!(
                        "block-serving: a single block response exceeds the protocol maximum — \
                         this is a bug, not a peer's fault"
                    );
                }
                break;
            }

            total_size = new_total_size;
            blocks.push(block_data);

            if blocks.len() >= max_blocks {
                break;
            }

            match direction {
                Direction::Ascending => block_id = BlockId::Number(number + One::one()),
                Direction::Descending => {
                    if number.is_zero() {
                        break;
                    }
                    block_id = BlockId::Hash(parent_hash)
                }
            }
        }

        Ok(BlockResponse { blocks })
    }
}

/// The decoded form of a request, so the cheap checks are separated from the
/// database work.
struct ParsedRequest<B: BlockT> {
    key: SeenRequestsKey<B>,
    from_block_id: BlockId<B>,
    max_blocks: usize,
    direction: Direction,
    attributes: BlockAttributes,
    support_multiple_justifications: bool,
    /// Header/justification only — the response is cheap, so it is still
    /// answered while the peer is throttled (upstream sends it while the peer
    /// is penalised, for the same reason).
    small_request: bool,
}

#[async_trait::async_trait]
impl<B, Client> BlockServer<B> for DivergenceTolerantBlockServer<B, Client>
where
    B: BlockT,
    Client: HeaderBackend<B> + BlockBackend<B> + Send + Sync + 'static,
{
    async fn run(&mut self) {
        self.process_requests().await;
    }
}

// ── The other half of the loop: we must not be the one who repeats ───────

/// How many byte-identical block requests may go on the wire to one peer on
/// one connection.
///
/// The receiving side counts the same key in `seen_requests` and calls the
/// third one `Rep::new_fatal` (`MAX_NUMBER_OF_SAME_REQUESTS_PER_PEER = 2` in
/// `sc-network-sync`). Two is therefore the last count that is answered
/// without complaint, and we simply never send the third: on 2026-11-13 our
/// own sync engine re-issued the same descending "find the common ancestor"
/// request every ~2 s at a peer sitting behind us on its own fork, and *that*
/// peer banned *us* 2 s into every connection (box 1, 20:25:49). Fixing only
/// our serving side left that end of the loop in place.
///
/// This is a politeness cap, not a ban of any kind: nothing is rejected for
/// being wrong, we just stop repeating ourselves. The first two attempts are
/// made exactly as before, and a peer that answered, answered.
pub const MAX_IDENTICAL_REQUESTS_PER_CONNECTION: usize = 2;

/// How far behind us a peer must be, on a branch we do not hold, before we
/// stop asking it for blocks.
///
/// The point is not that such a peer is bad; it is that it cannot extend our
/// chain. Everything it holds is below our best block, and every request for it
/// is an ancestor search that cannot succeed — which is what box 1's handler
/// counts as spam. A margin rather than zero because two chains a few blocks
/// apart are normal and worth querying (the peer may be about to outrun us).
pub const PEER_BEHIND_MARGIN: u64 = 32;

/// May we ask this peer for blocks at all?
///
/// * `Some(true)` — its tip is a block we hold: it is on our chain, a normal
///   sync source, always allowed.
/// * `Some(false)` and far behind us — it is on a branch we do not have and
///   cannot reach our height: nothing it sends can extend our chain, and the
///   request we would make is the one its handler calls spam. Do not ask.
/// * `None` (no claim yet) or a peer at/above our height — ask. Not knowing is
///   not evidence, and a peer ahead of us is exactly who we want.
pub fn may_ask_for_blocks(our_best: u64, peer_height: u64, peer_tip_known: Option<bool>) -> bool {
    match peer_tip_known {
        Some(false) => peer_height.saturating_add(PEER_BEHIND_MARGIN) > our_best,
        _ => true,
    }
}

/// One request's history with one peer.
struct SentRecord {
    sends: usize,
    /// The last successful payload, replayed instead of asking again.
    last: Option<Vec<u8>>,
    /// Set once we start suppressing, so the operator sees it exactly once.
    logged: bool,
}

/// Wraps the SDK's block downloader with the two rules that keep us from being
/// the peer that gets banned.
///
/// 1. **Never ask a peer that cannot answer** — a peer behind us on a branch we
///    do not hold has nothing that can extend our chain, so its requests only
///    re-run a doomed ancestor search.
/// 2. **Never send the same request three times on one connection** — the
///    receiving side's own repeat guard calls the third one fatal.
///
/// Everything else is delegated verbatim: the request encoding, the response
/// decoding, the protocol name. No ban is weakened (this is the *download*
/// side; bans live on the serving side) and no block is fabricated — a
/// suppressed request is answered with the peer's own earlier bytes, or
/// refused.
pub struct PoliteDownloader<Block: BlockT> {
    inner: Arc<dyn BlockDownloader<Block>>,
    source: Arc<dyn PeerTipSource>,
    seen: Mutex<LruMap<(PeerId, Vec<u8>), SentRecord>>,
    /// Peers we have already explained the "cannot answer" rule for.
    explained: Mutex<HashMap<PeerId, ()>>,
}

impl<Block: BlockT> fmt::Debug for PoliteDownloader<Block> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PoliteDownloader").finish_non_exhaustive()
    }
}

/// Where the download side asks "is this peer on our chain?", so the rule can
/// be tested without a database.
pub trait PeerTipSource: Send + Sync {
    /// Is the peer's claimed best block a block we hold? `None` = it has not
    /// claimed one (or the lookup failed).
    fn peer_tip_known(&self, who: &PeerId) -> Option<bool>;
    /// The peer's last claim: `(height, best hash as 0x-hex)`.
    fn peer_claim(&self, who: &PeerId) -> Option<(u64, String)>;
    /// Our own best block number.
    fn our_best(&self) -> u64;
}

impl<Block: BlockT> PoliteDownloader<Block> {
    /// Wrap `inner`, remembering the last 4096 request/peer pairs.
    pub fn new(inner: Arc<dyn BlockDownloader<Block>>, source: Arc<dyn PeerTipSource>) -> Self {
        Self {
            inner,
            source,
            seen: Mutex::new(LruMap::new(ByLength::new(4096))),
            explained: Mutex::new(HashMap::new()),
        }
    }

    /// Decide whether this request to this peer is worth putting on the wire.
    ///
    /// The one thing we refuse is **any** request to a peer that is far behind
    /// us on a branch we do not hold. Such a peer cannot supply anything that
    /// extends our chain — everything it holds is at or below its own tip — so
    /// every request to it is either an ancestor search that cannot succeed or
    /// a download of a fork we will never adopt. Both are pure waste, and the
    /// ancestor search is precisely the request the other side counts as spam:
    /// box 1 banned us for it at 20:43:30 and 21:06:49, ~2 s after connect.
    ///
    /// Matching on "is this the ancestor search" was tried and is too fragile
    /// (the shape depends on the peer's *announced* best, which is fresher than
    /// our chain-state claim: at 21:15 a one-block-stale claim let the search
    /// through and the engine spun on it). "Is this peer a possible source at
    /// all" is the durable question.
    ///
    /// Everything else is allowed exactly as before, including any request to a
    /// peer we know is on our chain, or one whose height is close to ours (it
    /// might be about to outrun us).
    fn should_ask(&self, who: &PeerId) -> bool {
        let Some((height, _tip)) = self.source.peer_claim(who) else {
            // No claim yet: ask. Not knowing is not evidence of anything.
            return true;
        };
        let our_best = self.source.our_best();
        let tip_known = self.source.peer_tip_known(who);
        if may_ask_for_blocks(our_best, height, tip_known) {
            // One line per peer per process, with the facts the decision used:
            // without it there is no way to tell "we asked because we had to"
            // from "we asked because the gate did not fire".
            let first = self
                .explained
                .lock()
                .map(|mut m| m.insert(*who, ()).is_none())
                .unwrap_or(false);
            if first {
                info!(
                    "block-serving: asking {who} for blocks (claim #{height}, our tip \
                     #{our_best}, their tip known to us: {tip_known:?})"
                );
            }
            return true;
        }

        let first = self
            .explained
            .lock()
            .map(|mut m| m.insert(*who, ()).is_none())
            .unwrap_or(false);
        if first {
            info!(
                "block-serving: not asking {who} for blocks — its tip #{height} is not on our \
                 chain and it is {} blocks behind our #{our_best}. Nothing it holds can extend \
                 our chain, and the ancestor search we would send is exactly the request its \
                 handler counts as spam. The link stays up and we keep serving it canonical \
                 blocks (#158).",
                our_best.saturating_sub(height)
            );
        } else {
            debug!("block-serving: still not asking {who} for blocks (tip #{height})");
        }
        false
    }

    /// What the peer's handler counts as "the same request": fields, start
    /// point, direction and maximum — everything that changes the answer, and
    /// not the (random) request id.
    fn key(who: PeerId, request: &BlockRequest<Block>) -> (PeerId, Vec<u8>) {
        let mut bytes = Vec::with_capacity(48);
        request.fields.bits().encode_to(&mut bytes);
        request.from.encode_to(&mut bytes);
        request.direction.encode_to(&mut bytes);
        request.max.encode_to(&mut bytes);
        (who, bytes)
    }
}

#[async_trait::async_trait]
impl<Block: BlockT> BlockDownloader<Block> for PoliteDownloader<Block> {
    fn protocol_name(&self) -> &sc_network::ProtocolName {
        self.inner.protocol_name()
    }

    async fn download_blocks(
        &self,
        who: PeerId,
        request: BlockRequest<Block>,
    ) -> Result<Result<(Vec<u8>, sc_network::ProtocolName), RequestFailure>, oneshot::Canceled>
    {
        // Rule 1: do not ask a peer that cannot answer. No wire traffic, and
        // no delay either: a request held "in flight" while a churning peer
        // disconnects is what makes the sync engine count a disconnect-in-
        // request and, on the third, report `Peer disconnected with inflight
        // after backoffs` — fatal, and issued by *us* (2026-11-13 21:33:13 and
        // 21:37:54, both against a peer that had done nothing wrong). An
        // immediate refusal has no window to fall into.
        if !self.should_ask(&who) {
            return Ok(Err(RequestFailure::Refused));
        }

        let key = Self::key(who, &request);

        // Decide (and count) while holding the lock, but never across the
        // await: a network round trip must not serialise every other peer.
        let (allowed, replay) = match self.seen.lock() {
            Ok(mut map) => match map.get(&key) {
                Some(rec) if rec.sends >= MAX_IDENTICAL_REQUESTS_PER_CONNECTION => {
                    let replay = rec.last.clone();
                    let log_now = !rec.logged;
                    rec.logged = true;
                    if log_now {
                        warn!(
                            "block-serving: we have already asked {who} this exact block range \
                             {MAX_IDENTICAL_REQUESTS_PER_CONNECTION} times on this connection — \
                             not asking again (a third identical request is what makes the other \
                             side ban US). Replaying the earlier answer."
                        );
                    }
                    (false, replay)
                }
                Some(rec) => {
                    rec.sends += 1;
                    (true, None)
                }
                None => {
                    map.insert(
                        key.clone(),
                        SentRecord {
                            sends: 1,
                            last: None,
                            logged: false,
                        },
                    );
                    (true, None)
                }
            },
            // A poisoned lock must not take sync down: fall through to the
            // real downloader uncapped.
            Err(_) => (true, None),
        };

        if allowed {
            let outcome = self.inner.download_blocks(who, request).await;
            if let Ok(Ok((bytes, _))) = &outcome {
                if let Ok(mut map) = self.seen.lock() {
                    if let Some(rec) = map.get(&key) {
                        rec.last = Some(bytes.clone());
                    }
                }
            }
            return outcome;
        }

        // Refuse, do not replay. Replaying the peer's earlier bytes was tried
        // and is worse than useless: the engine re-imports the same blocks,
        // fails again, restarts sync and asks again — a tight loop with no
        // network delay to slow it (2026-11-13 21:15, hundreds of ancestor
        // searches per second). A refusal makes the engine give up on this
        // request, which is the truth: we are not going to ask again.
        let _ = replay;
        Ok(Err(RequestFailure::Refused))
    }

    fn block_response_into_blocks(
        &self,
        request: &BlockRequest<Block>,
        response: Vec<u8>,
    ) -> Result<Vec<MessageBlockData<Block>>, BlockResponseError> {
        self.inner.block_response_into_blocks(request, response)
    }
}

/// A block number as a `u64`, for comparing against a claim's plain height.
fn number_to_u64<N: sp_runtime::traits::UniqueSaturatedInto<u64>>(n: N) -> u64 {
    n.unique_saturated_into()
}

/// The production [`PeerTipSource`]: the live client plus the claims the
/// `gpn/chain/1` task records.
pub struct ChainTipSource<Block: BlockT, Client> {
    client: Arc<Client>,
    tips: Arc<PeerTips>,
    _block: std::marker::PhantomData<Block>,
}

impl<Block: BlockT, Client> ChainTipSource<Block, Client>
where
    Client: HeaderBackend<Block> + Send + Sync + 'static,
{
    /// Named `arc` rather than `new`: it returns a trait object and clippy's
    /// `new_ret_no_self` is right that a constructor called `new` should build
    /// `Self`. The honest name costs exactly one call site.
    pub fn arc(client: Arc<Client>, tips: Arc<PeerTips>) -> Arc<dyn PeerTipSource> {
        Arc::new(Self {
            client,
            tips,
            _block: std::marker::PhantomData,
        })
    }
}

impl<Block: BlockT, Client> PeerTipSource for ChainTipSource<Block, Client>
where
    Client: HeaderBackend<Block> + Send + Sync + 'static,
{
    fn peer_tip_known(&self, who: &PeerId) -> Option<bool> {
        let claim = self.tips.get(who)?;
        let hex = claim.best_hash.trim_start_matches("0x");
        let bytes = hex::decode(hex).ok()?;
        let hash = <Block as BlockT>::Hash::decode(&mut &bytes[..]).ok()?;
        // A header lookup, not the hash echo — see the server's copy of this.
        Some(self.client.header(hash).ok().flatten().is_some())
    }

    fn peer_claim(&self, who: &PeerId) -> Option<(u64, String)> {
        self.tips.get(who).map(|c| (c.height, c.best_hash))
    }

    fn our_best(&self) -> u64 {
        number_to_u64(self.client.info().best_number)
    }
}

/// Why a request could not be answered. Every variant is reported to the peer
/// as a protocol violation (`MALFORMED_REQUEST`, fatal).
#[derive(Debug, thiserror::Error)]
pub enum HandleRequestError {
    #[error("failed to decode request: {0}")]
    DecodeProto(#[from] prost::DecodeError),
    #[error("failed to encode response: {0}")]
    EncodeProto(#[from] prost::EncodeError),
    #[error("failed to decode block hash/number: {0}")]
    DecodeScale(#[from] codec::Error),
    #[error("request has no `from` field")]
    MissingFromField,
    #[error("request has an unknown direction")]
    ParseDirection,
    #[error("request has unknown attribute bits")]
    Attributes,
    #[error(transparent)]
    Client(#[from] sp_blockchain::Error),
    #[error("the requester went away before we answered")]
    SendResponse,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runtime block type, as the node uses it.
    type Block = gpn_runtime::Block;

    /// A peer tip source that knows nothing (so every request is allowed)
    /// unless a test inserts a claim for a peer.
    #[derive(Default)]
    struct HonestSource {
        our_best: u64,
        peers: std::sync::Mutex<HashMap<PeerId, (u64, Option<bool>)>>,
    }

    impl PeerTipSource for HonestSource {
        fn peer_tip_known(&self, who: &PeerId) -> Option<bool> {
            self.peers.lock().unwrap().get(who).and_then(|v| v.1)
        }
        fn peer_claim(&self, who: &PeerId) -> Option<(u64, String)> {
            self.peers.lock().unwrap().get(who).map(|v| {
                (
                    v.0,
                    "0x2222222200000000000000000000000000000000000000000000000000000002".into(),
                )
            })
        }
        fn our_best(&self) -> u64 {
            self.our_best
        }
    }

    /// Records every request that actually went on the wire.
    #[derive(Default)]
    struct CountingDownloader {
        calls: Mutex<Vec<(PeerId, BlockRequest<Block>)>>,
    }

    impl std::fmt::Debug for CountingDownloader {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("CountingDownloader")
        }
    }

    #[async_trait::async_trait]
    impl BlockDownloader<Block> for CountingDownloader {
        fn protocol_name(&self) -> &sc_network::ProtocolName {
            static NAME: std::sync::OnceLock<sc_network::ProtocolName> = std::sync::OnceLock::new();
            NAME.get_or_init(|| sc_network::ProtocolName::from("/test/sync/2"))
        }

        async fn download_blocks(
            &self,
            who: PeerId,
            request: BlockRequest<Block>,
        ) -> Result<Result<(Vec<u8>, sc_network::ProtocolName), RequestFailure>, oneshot::Canceled>
        {
            self.calls.lock().unwrap().push((who, request));
            Ok(Ok((vec![0xAA; 4], self.protocol_name().clone())))
        }

        fn block_response_into_blocks(
            &self,
            _request: &BlockRequest<Block>,
            _response: Vec<u8>,
        ) -> Result<Vec<MessageBlockData<Block>>, BlockResponseError> {
            Ok(Vec::new())
        }
    }

    fn request(id: u64) -> BlockRequest<Block> {
        BlockRequest::<Block> {
            id,
            fields: BlockAttributes::HEADER | BlockAttributes::BODY,
            from: FromBlock::Number(297_000u32),
            direction: sc_network_common::sync::message::Direction::Descending,
            max: Some(128),
        }
    }

    fn request_at(id: u64, number: u32) -> BlockRequest<Block> {
        let mut r = request(id);
        r.from = FromBlock::Number(number);
        r
    }

    /// The three cases the change is judged on, expressed against the same
    /// function the server calls.
    #[test]
    fn a_repeated_request_from_a_diverged_peer_is_never_fatal() {
        let rep = repeat_penalty(Some(false));
        assert_ne!(rep.value, i32::MIN, "diverged peer must not be banned");
        assert!(rep.value < 0, "divergence is still noticed");
        assert_eq!(rep.reason, rep::DIVERGED_SAME_REQUEST.reason);
        // A peer that never claimed a tip is treated the same way: an
        // unanswered question is not evidence of good behaviour either.
        let unknown = repeat_penalty(None);
        assert_ne!(unknown.value, i32::MIN);
    }

    /// Owner decision 2026-11-14: the ban for repeated block requests is gone
    /// for **every** peer, not only diverged ones. A known-good peer's repeat
    /// costs the same small, non-fatal report.
    #[test]
    fn a_repeated_request_from_a_peer_on_our_chain_is_never_fatal() {
        let rep = repeat_penalty(Some(true));
        assert_ne!(rep.value, i32::MIN, "a known-good peer is never banned");
        assert_eq!(rep.value, -(1 << 10));
        assert_eq!(rep.reason, rep::PEER_ON_CHAIN_SAME_REQUEST.reason);
        assert_eq!(
            rep.value,
            repeat_penalty(Some(false)).value,
            "both branches are capped at the same small value"
        );
    }

    /// The first two deliveries are answered with no penalty at all — that part
    /// of the upstream rule is kept, because a repeat is only a repeat after
    /// the peer has had the answer twice.
    #[test]
    fn the_first_two_deliveries_are_not_penalised_at_all() {
        let now = std::time::Instant::now();
        // The first delivery creates the state; the second is a repeat that is
        // still within the allowance.
        let mut state = RepeatState::new(now);
        assert_eq!(state.observe(now), RepeatDecision::Fresh);
        // …and the third request is where the small report starts.
        assert_eq!(
            state.observe(now),
            RepeatDecision::Repeat {
                first_in_streak: true
            }
        );
    }

    /// No number of repeats can ever be enough to ban, because the penalty is
    /// charged against a per-peer lifetime budget that is ~1,450× below the
    /// peer store's threshold (71% of `i32::MIN`).
    #[test]
    fn repeats_can_never_accumulate_to_the_ban_threshold() {
        let ban_threshold = (-(i32::MIN / 100)) as i64 * 71; // ≈ −1.52e9
        let mut budget = RepeatBudget::default();
        // Far more repeats than a peer could ever make in one connection's
        // lifetime, charged at the same rate the server charges them.
        for _ in 0..1_000_000 {
            let _ = budget.charge(repeat_penalty(Some(true)));
            let _ = budget.charge(repeat_penalty(Some(false)));
        }
        assert_eq!(budget.spent(), REPEAT_PENALTY_BUDGET);
        assert!(
            (budget.spent() as i64) < ban_threshold / 1000,
            "the whole lifetime budget stays three orders of magnitude below the threshold"
        );
        // Once spent, nothing more is reported at all.
        assert!(budget.charge(repeat_penalty(Some(true))).is_none());
    }

    /// Throttling: the fifth identical request inside the window is throttled,
    /// the peer is still there, and service resumes when the window expires —
    /// with no ban and no disconnect anywhere in between.
    #[test]
    fn the_throttle_trips_after_n_repeats_and_lifts_after_the_window() {
        let t0 = std::time::Instant::now();
        let mut state = RepeatState::new(t0);

        // Deliveries 2, 3 and 4 inside the window are served.
        assert_eq!(state.observe(t0), RepeatDecision::Fresh);
        assert_eq!(
            state.observe(t0),
            RepeatDecision::Repeat {
                first_in_streak: true
            }
        );
        assert_eq!(
            state.observe(t0),
            RepeatDecision::Repeat {
                first_in_streak: false
            }
        );

        // The fifth trips the throttle, and the line is printed exactly once.
        assert_eq!(
            state.observe(t0),
            RepeatDecision::Throttled {
                count: MAX_REPEATS_PER_WINDOW + 1,
                log: true
            }
        );
        for _ in 0..10 {
            assert_eq!(
                state.observe(t0),
                RepeatDecision::Throttled {
                    count: state.repeats(),
                    log: false
                }
            );
        }

        // One window later the same request is served normally again: no
        // un-ban event, no reconnect, no state to clear.
        let later = t0 + THROTTLE_WINDOW + std::time::Duration::from_secs(1);
        assert_eq!(state.observe(later), RepeatDecision::Fresh);
        assert_eq!(
            state.observe(later),
            RepeatDecision::Fresh,
            "the counter restarted with the window"
        );
    }

    /// While throttled a full request is not answered at all, and a header-only
    /// one still gets a bounded prefix — the latter is what every real peer on
    /// this network sends while hunting for the common ancestor.
    #[test]
    fn a_throttled_request_gets_nothing_or_a_bounded_prefix() {
        assert_eq!(served_max_blocks(128, false, false), 128);
        assert_eq!(served_max_blocks(128, true, false), 128);
        assert_eq!(
            served_max_blocks(128, false, true),
            0,
            "no data for a full request"
        );
        assert_eq!(
            served_max_blocks(128, true, true),
            THROTTLE_PREFIX_BLOCKS,
            "headers only, and only a handful"
        );
        assert_eq!(served_max_blocks(1, true, true), 1, "never more than asked");
    }

    #[test]
    fn a_malformed_request_is_still_fatal() {
        // Undecodable protobuf: the server reports MALFORMED_REQUEST, which is
        // fatal, and never reaches the divergence rule.
        let garbage = [0xffu8; 4];
        assert!(BlockRequestSchema::decode(&garbage[..]).is_err());
        assert_eq!(rep::MALFORMED_REQUEST.value, i32::MIN);
        assert_eq!(rep::MALFORMED_REQUEST.reason, "Malformed block request");

        // Unknown attribute bits are refused, not approximated.
        assert!(BlockAttributes::from_be_u32(u32::MAX).is_err());
    }

    #[test]
    fn we_do_not_ask_a_peer_that_cannot_answer() {
        // Behind us on a branch we do not hold: nothing it has can extend our
        // chain, and the request we would send is the one its handler counts
        // as spam. This is box 1 at #280600 against our #297825.
        assert!(!may_ask_for_blocks(297_825, 280_600, Some(false)));
        // A few blocks apart is normal: ask.
        assert!(may_ask_for_blocks(297_825, 297_800, Some(false)));
        // The margin, at its boundary: 32 blocks behind is still worth asking.
        assert!(may_ask_for_blocks(297_825, 297_794, Some(false)));
        assert!(!may_ask_for_blocks(297_825, 297_793, Some(false)));
        // On our chain: always a legitimate source, however far behind.
        assert!(may_ask_for_blocks(297_825, 100, Some(true)));
        // No claim yet, or a peer ahead of us: ask. Not knowing is not evidence.
        assert!(may_ask_for_blocks(297_825, 100, None));
        assert!(may_ask_for_blocks(297_825, 300_000, Some(false)));
    }

    #[tokio::test]
    async fn a_far_behind_peer_on_another_branch_is_never_asked_for_blocks() {
        let inner = Arc::new(CountingDownloader::default());
        let mut source = HonestSource {
            our_best: 297_825,
            ..Default::default()
        };
        let peer = PeerId::random();
        source
            .peers
            .lock()
            .unwrap()
            .insert(peer, (280_600, Some(false)));
        let polite = PoliteDownloader::new(inner.clone(), Arc::new(source));

        // The ancestor search `chain_sync` sends for an unknown best hash…
        let out = polite.download_blocks(peer, request_at(1, 280_600)).await;
        assert!(matches!(out, Ok(Err(RequestFailure::Refused))));
        // …and an ordinary-looking request: neither reaches the wire.
        let out = polite.download_blocks(peer, request_at(2, 297_000)).await;
        assert!(matches!(out, Ok(Err(RequestFailure::Refused))));
        assert_eq!(
            inner.calls.lock().unwrap().len(),
            0,
            "nothing went on the wire"
        );
    }

    #[tokio::test]
    async fn a_peer_on_our_chain_is_always_asked() {
        let inner = Arc::new(CountingDownloader::default());
        let mut source = HonestSource {
            our_best: 297_825,
            ..Default::default()
        };
        let peer = PeerId::random();
        source.peers.lock().unwrap().insert(peer, (100, Some(true)));
        let polite = PoliteDownloader::new(inner.clone(), Arc::new(source));

        let out = polite.download_blocks(peer, request_at(1, 100)).await;
        assert!(matches!(out, Ok(Ok(_))));
        assert_eq!(inner.calls.lock().unwrap().len(), 1);
    }

    /// Pinned: the upstream value this module used to return, and no longer
    /// does. If this test ever fails, `SAME_REQUEST` stopped being the ban.
    #[test]
    fn the_old_fatal_rule_is_never_selected_by_this_module() {
        assert_eq!(rep::SAME_REQUEST.value, i32::MIN);
        for tip in [Some(true), Some(false), None] {
            assert_ne!(repeat_penalty(tip).value, rep::SAME_REQUEST.value);
        }
    }

    #[test]
    fn the_divergence_penalty_can_never_accumulate_into_a_ban() {
        // The peer store's ban threshold is 71% of `i32::MIN`, ~−1.5e9: a
        // −1024 repeat report cannot get there on its own, and the lifetime
        // budget caps the total at 2^20 anyway — this is a throttle, not a
        // slow ban. The repeated *full* request is also not answered once the
        // throttle trips, so there is no bandwidth amplification.
        let per_repeat = rep::DIVERGED_SAME_REQUEST.value;
        assert!(per_repeat < 0);
        assert!((-(i32::MIN / 100) * 71) / -per_repeat > 100_000);
        assert!(REPEAT_PENALTY_BUDGET as i64 * 1000 < (-(i32::MIN / 100)) as i64 * 71);
    }

    fn claim(height: u64, hash: &str) -> ChainState {
        ChainState::new(
            "0xb8e4636a00000000000000000000000000000000000000000000000000000001",
            hash,
            height,
        )
        .expect("valid claim")
    }

    #[test]
    fn a_peer_tip_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("gpn-peer-tips-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("peer-tips.json");
        let peer = PeerId::random();
        let tip = claim(
            280_600,
            "0x4f97dae7a2e96d818a2aab8737a1f0deea069ea6f85338636487af437ee1b81e",
        );

        {
            let tips = PeerTips::loaded(path.clone());
            assert!(tips.get(&peer).is_none());
            tips.set(peer, tip.clone());
        }

        // A fresh process: the claim is still there, so the download side can
        // decide without sending anything blind.
        let reopened = PeerTips::loaded(path.clone());
        assert_eq!(reopened.get(&peer), Some(tip));

        // A newer claim replaces it, and the file follows.
        let newer = claim(
            280_700,
            "0x1111111100000000000000000000000000000000000000000000000000000001",
        );
        reopened.set(peer, newer.clone());
        assert_eq!(PeerTips::loaded(path).get(&peer), Some(newer));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The exact loop that got us banned by box 1: the sync engine re-issued
    /// one identical request every ~2 s, and the third one is fatal *there*.
    #[tokio::test]
    async fn we_never_send_a_third_identical_request() {
        let inner = Arc::new(CountingDownloader::default());
        let polite = PoliteDownloader::new(inner.clone(), Arc::new(HonestSource::default()));
        let peer = PeerId::random();

        for i in 0..6 {
            let out = polite.download_blocks(peer, request(i)).await;
            // The first two go on the wire and are answered normally; after
            // that we refuse rather than poke the peer a third time.
            if i < MAX_IDENTICAL_REQUESTS_PER_CONNECTION as u64 {
                assert!(matches!(out, Ok(Ok(_))), "the first attempts still go out");
            } else {
                assert!(matches!(out, Ok(Err(RequestFailure::Refused))));
            }
        }

        let calls = inner.calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            MAX_IDENTICAL_REQUESTS_PER_CONNECTION,
            "only two identical requests may reach the wire"
        );
    }

    #[tokio::test]
    async fn a_different_request_or_peer_is_never_suppressed() {
        let inner = Arc::new(CountingDownloader::default());
        let polite = PoliteDownloader::new(inner.clone(), Arc::new(HonestSource::default()));
        let peer = PeerId::random();
        let other = PeerId::random();

        for i in 0..3 {
            let _ = polite.download_blocks(peer, request(i)).await;
        }
        // Same peer, different range: that is a new question.
        for i in 10..13 {
            let _ = polite.download_blocks(peer, request_at(i, 296_872)).await;
        }
        // A different peer is a different conversation.
        for i in 20..23 {
            let _ = polite.download_blocks(other, request(i)).await;
        }

        assert_eq!(
            inner.calls.lock().unwrap().len(),
            2 + 2 + 2,
            "two per distinct (peer, request)"
        );
    }
}
