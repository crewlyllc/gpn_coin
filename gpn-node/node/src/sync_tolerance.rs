//! The last SDK ban that drops an innocent peer (release 0.3.48, item F).
//!
//! `sc-network-sync` keeps a counter of peers that disconnected while a request
//! was in flight, and bans one on the third such disconnect:
//!
//! ```text
//! substrate/client/network/sync/src/strategy/disconnected_peers.rs
//!   :50  pub const REPUTATION_REPORT: Rep = Rep::new_fatal("Peer disconnected with inflight after backoffs");
//!   :43  const MAX_NUM_DISCONNECTS: u64 = 3;
//!   :152 fn on_disconnect_during_request(...) -> Option<BadPeer>  // Some(..) == "ban it"
//! ```
//!
//! and `strategy/chain_sync.rs:469` turns that into
//! `SyncingAction::DropPeer(..)`, which `engine.rs:641` executes as
//! `disconnect_peer()` + `report_peer(rep)` — reputation `i32::MIN`, i.e. the
//! peer is banned outright.
//!
//! The rule assumes a peer only abandons a request mid-flight because it is
//! malicious or cannot keep up. Most of the time it is neither: our journal for
//! 2026-11-14 has 66 of these in one day (5–7 in a twenty-minute window), all
//! against two peers that were otherwise healthy, and each one dropped a peer
//! for the SDK's own retry behaviour — three separate aborts spread over
//! minutes, each of which the SDK itself would have forgotten after
//! `60 s × count`.
//!
//! ## What changes
//!
//! We do not fork the SDK. `SyncingEngine::new` takes
//! `Box<dyn SyncingStrategy<B>>`, and `sc_service::build_polkadot_syncing_strategy`
//! is public, so we build the stock strategy and wrap it: every method
//! delegates unchanged, and `actions()` replaces exactly one action —
//! `DropPeer` whose reputation reason is [`INFLIGHT_ABANDON_REASON`] — with a
//! small, budgeted reputation charge and an INFO line. The peer is **not**
//! disconnected: no `DropPeer` is ever emitted for this condition.
//!
//! The same wrapper passes every other `DropPeer` through untouched, so
//! undecodable payloads, bad signatures, forged justifications and genesis
//! mismatches stay fatal — that is the whole point of keeping one narrow
//! exception instead of a general one.
//!
//! ## What deliberately does not change
//!
//! * The SDK's repeat-request accounting, chain-selection and justification
//!   logic — we only read its `actions()` output.
//! * The genesis-mismatch ban, and every other `Rep::new_fatal` in the tree.
//! * The peer store's ban threshold. The penalty here is capped by a per-peer
//!   lifetime budget ([`INFLIGHT_PENALTY_BUDGET`], 2^16 units) which is
//!   ~23,000× below `BANNED_THRESHOLD` (71% of `i32::MIN`), so no amount of
//!   repetition can add up to a ban.
//!
//! ## The terminal state the exception still needs (task #189)
//!
//! Removing the SDK's ban also removed its *only* terminal action for this
//! condition, and the SDK's counter is not a usable exit condition: an entry is
//! forgotten after `backoff × count` (60 s × 1), so a peer whose retry cadence
//! is *longer* than that (measured in production: 70 s) resets to `1` on every
//! observation and can never reach 3. A peer stuck at one block was therefore
//! served a fresh substream every 70 s indefinitely, and the penalty budget
//! (64 aborts) would eventually run out and hide it from the log as well.
//!
//! So the exit condition here is **progress**, not a count: we remember the
//! highest block number the peer has told us about ([`InflightTolerance::note_peer`],
//! fed by `add_peer` and block announces). If the peer abandons
//! [`STALL_ABORTS`] requests and its best number has not advanced for
//! [`STALL_HORIZON`], it is treated as wedged: we disconnect it (once per
//! observation, logged once) instead of feeding it more work. A peer that
//! announces a higher block clears the state and starts again from zero — that
//! is the difference between "slow" and "stuck".

use sc_network::{PeerId, ProtocolName, ReputationChange};
use sc_network_sync::service::network::NetworkServiceHandle;
use sc_network_sync::strategy::{StrategyKey, SyncingAction, SyncingStrategy};
use sc_network_sync::types::BadPeer;
use sp_runtime::traits::Block as BlockT;
use sp_runtime::traits::{Header, UniqueSaturatedInto};
use std::any::Any;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The SDK's reason string, verbatim.
///
/// `substrate/client/network/sync/src/strategy/disconnected_peers.rs:50` —
/// `Rep::new_fatal("Peer disconnected with inflight after backoffs")`. The
/// module is private, so the constant cannot be imported; a unit test
/// ([`tests::reason_string_matches_pinned_sdk`]) reads the pinned checkout and
/// fails if this literal ever stops matching the SDK.
pub const INFLIGHT_ABANDON_REASON: &str = "Peer disconnected with inflight after backoffs";

/// Reputation charged per neutralised event.
///
/// Same magnitude as the block server's repeat penalty (`block_serving`):
/// small enough that it is noise next to the peer store's `-256` per
/// disconnect, and capped by [`INFLIGHT_PENALTY_BUDGET`].
pub const INFLIGHT_PENALTY: i32 = -1024;

/// Per-peer lifetime cap on what this module will ever report.
///
/// `BANNED_THRESHOLD` is `71 * (i32::MIN / 100)` ≈ −1.52e9, so the whole
/// budget is ~23,000× below a ban. Once it is spent we report nothing at all
/// and only log — a peer that abandons this many requests is still throttled
/// by the SDK's own backoff (`60 s × count`), which we leave in place.
pub const INFLIGHT_PENALTY_BUDGET: u32 = 1 << 16;

/// At most this many peers are tracked (the SDK uses 512 for the same table).
pub const MAX_TRACKED_PEERS: usize = 512;

/// Aborts, without progress, before a peer is treated as wedged rather than
/// flaky. Ten is roughly the 47 observed in 75 min on 2026-11-15 divided by
/// five, so a genuinely flaky-but-syncing peer (which resets on every announce)
/// is nowhere near it, while a stuck one reaches it in ~12 min.
pub const STALL_ABORTS: u64 = 10;

/// How long a peer may make no progress at all before the abort count above
/// means "wedged". Long enough that a slow-but-moving peer (announce every few
/// minutes) is never mistaken for a stuck one; short enough that a caller
/// waiting on sync does not lose more than a few minutes to a dead peer.
pub const STALL_HORIZON: Duration = Duration::from_secs(12 * 60);

/// The SDK's ban threshold for this condition
/// (`disconnected_peers.rs:43`, `const MAX_NUM_DISCONNECTS: u64 = 3`). Used
/// only to make the log line read "3/3" — the count no longer bans anything.
pub const MAX_REPORTED_ABORTS: u64 = 3;

/// The SDK's backoff unit: an entry is forgotten after `backoff × count`.
pub const DISCONNECTED_PEER_BACKOFF: Duration = Duration::from_secs(60);

/// Is this reputation change the SDK's "disconnected with inflight after
/// backoffs" fatal report?
///
/// Matches on the reason string, not on `i32::MIN`: every other fatal report
/// in the tree (bad message, malformed block request, invalid justification,
/// genesis mismatch) must keep its behaviour.
pub fn is_inflight_abandon(rep: &ReputationChange) -> bool {
    rep.reason == INFLIGHT_ABANDON_REASON
}

/// One observation of a peer that abandoned a request mid-flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observed {
    /// How many such aborts this peer has accumulated in the current window.
    /// The SDK would have banned at 3.
    pub count: u64,
    /// What we report instead of `i32::MIN`. `None` once the lifetime budget
    /// is spent, or when the peer is judged wedged (the disconnect is then the
    /// whole response).
    pub charge: Option<ReputationChange>,
    /// Aborts accumulated since the peer last made progress.
    pub aborts: u64,
    /// This peer is wedged: no progress for [`STALL_HORIZON`] across
    /// [`STALL_ABORTS`] aborts. The caller must stop serving it.
    pub stalled: bool,
    /// True only for the observation that first reached the verdict, so the
    /// caller can log it once instead of once per retry cycle.
    pub first_stall: bool,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    count: u64,
    last: Instant,
    spent: u32,
    /// Aborts since the peer last advanced; unlike `count` this is *not* reset
    /// by the SDK's `backoff × count` expiry, which is the defect #189 names.
    aborts: u64,
    /// Highest block number this peer has reported (0 = never heard from).
    best: u64,
    /// When `best` last advanced (or when the peer was first seen).
    progress_at: Instant,
    /// Verdict already reached; cleared by progress.
    stalled: bool,
}

/// Bounded, self-expiring per-peer accounting for abandoned requests.
///
/// Expiry mirrors the SDK's own rule (`elapsed >= backoff × count` → forget the
/// entry), so our state cannot grow without bound and a peer that disconnects
/// cleanly for a while starts again from zero — the same reset the SDK
/// performs in `DisconnectedPeers::is_peer_available`.
#[derive(Debug)]
pub struct InflightTolerance {
    peers: HashMap<PeerId, Entry>,
    backoff: Duration,
    stall_aborts: u64,
    stall_horizon: Duration,
}

impl Default for InflightTolerance {
    fn default() -> Self {
        Self::new()
    }
}

impl InflightTolerance {
    pub fn new() -> Self {
        Self {
            peers: HashMap::new(),
            backoff: DISCONNECTED_PEER_BACKOFF,
            stall_aborts: STALL_ABORTS,
            stall_horizon: STALL_HORIZON,
        }
    }

    /// Same as [`Self::new`] with a shorter backoff, for tests.
    #[cfg(test)]
    pub fn with_backoff(backoff: Duration) -> Self {
        Self {
            peers: HashMap::new(),
            backoff,
            stall_aborts: STALL_ABORTS,
            stall_horizon: STALL_HORIZON,
        }
    }

    /// Record that the peer answered something. A completed response breaks an
    /// abort streak, so a peer that serves us and occasionally abandons a
    /// request is never mistaken for a wedged one.
    pub fn note_activity(&mut self, peer: PeerId, now: Instant) {
        if let Some(e) = self.peers.get_mut(&peer) {
            e.aborts = 0;
            e.stalled = false;
            e.progress_at = now;
        }
    }

    /// Record the peer's reported best block number. Called from `add_peer`
    /// (the baseline) and from block announces (the progress signal). An
    /// increase clears the abort streak and any stall verdict.
    pub fn note_peer(&mut self, peer: PeerId, best: u64, now: Instant) {
        if self.peers.len() >= MAX_TRACKED_PEERS && !self.peers.contains_key(&peer) {
            self.evict_oldest(now);
        }
        match self.peers.get_mut(&peer) {
            Some(e) if best > e.best => {
                e.best = best;
                e.progress_at = now;
                e.aborts = 0;
                e.stalled = false;
            }
            Some(_) => {}
            None => {
                self.peers.insert(
                    peer,
                    Entry {
                        // Not an abort: `observe` starts the SDK's count at 1.
                        count: 0,
                        last: now,
                        spent: 0,
                        aborts: 0,
                        best,
                        progress_at: now,
                        stalled: false,
                    },
                );
            }
        }
    }

    /// Number of peers currently tracked (bounded by [`MAX_TRACKED_PEERS`]).
    #[cfg(test)]
    pub fn tracked(&self) -> usize {
        self.peers.len()
    }

    /// Record one abort for `peer` and decide what to report.
    pub fn observe(&mut self, peer: PeerId, now: Instant) -> Observed {
        let (count, spent, aborts, best, progress_at, stalled) = match self.peers.get(&peer) {
            // The SDK would have forgotten this entry after `backoff × count`.
            Some(e) if now.saturating_duration_since(e.last) < self.backoff * e.count as u32 => (
                e.count.saturating_add(1),
                e.spent,
                e.aborts,
                e.best,
                e.progress_at,
                e.stalled,
            ),
            Some(e) => (1, e.spent, e.aborts, e.best, e.progress_at, e.stalled),
            None => (1, 0, 0, 0, now, false),
        };

        let aborts = aborts.saturating_add(1);
        let first_stall = !stalled
            && aborts >= self.stall_aborts
            && now.saturating_duration_since(progress_at) >= self.stall_horizon;
        let stalled = stalled || first_stall;

        let remaining = INFLIGHT_PENALTY_BUDGET.saturating_sub(spent);
        let actual = if stalled {
            0
        } else {
            INFLIGHT_PENALTY.unsigned_abs().min(remaining)
        };
        let charge = if actual == 0 {
            None
        } else {
            Some(ReputationChange::new(
                -(actual as i32),
                "Peer abandoned a request mid-flight",
            ))
        };

        if self.peers.len() >= MAX_TRACKED_PEERS && !self.peers.contains_key(&peer) {
            self.evict_oldest(now);
        }
        self.peers.insert(
            peer,
            Entry {
                count,
                last: now,
                spent: spent.saturating_add(actual),
                aborts,
                best,
                progress_at,
                stalled,
            },
        );

        Observed {
            count,
            charge,
            aborts,
            stalled,
            first_stall,
        }
    }

    /// Forget everything the SDK's backoff would have forgotten by `now`.
    fn evict_oldest(&mut self, now: Instant) {
        // Prefer an entry that has already expired; otherwise drop the least
        // recently seen one. Either way the table stays at its bound.
        let mut victim: Option<(PeerId, Duration)> = None;
        for (peer, e) in &self.peers {
            let age = now.saturating_duration_since(e.last);
            if age >= self.backoff * e.count as u32 {
                victim = Some((*peer, age));
                break;
            }
            if victim.map(|(_, a)| age > a).unwrap_or(true) {
                victim = Some((*peer, age));
            }
        }
        if let Some((peer, _)) = victim {
            self.peers.remove(&peer);
        }
    }
}

/// A [`SyncingStrategy`] that removes exactly one fatal report.
pub struct TolerantSyncStrategy<B: BlockT> {
    inner: Box<dyn SyncingStrategy<B>>,
    inflight: InflightTolerance,
    /// The block-announces notification protocol, needed by
    /// `NetworkServiceHandle::disconnect_peer` — the only terminal action this
    /// wrapper can take on its own without banning the peer.
    announce_protocol: ProtocolName,
}

impl<B: BlockT> TolerantSyncStrategy<B> {
    pub fn new(inner: Box<dyn SyncingStrategy<B>>, announce_protocol: ProtocolName) -> Self {
        Self {
            inner,
            inflight: InflightTolerance::new(),
            announce_protocol,
        }
    }

    /// Apply the one policy this wrapper owns to the inner strategy's actions.
    fn filter(
        &mut self,
        actions: Vec<SyncingAction<B>>,
        net: &NetworkServiceHandle,
        now: Instant,
    ) -> Vec<SyncingAction<B>> {
        let protocol = self.announce_protocol.clone();
        let mut kept = Vec::with_capacity(actions.len());
        for action in actions {
            match &action {
                SyncingAction::DropPeer(BadPeer(peer, rep)) => {
                    if screen(&mut self.inflight, *peer, rep, net, now, &protocol) {
                        kept.push(action);
                    }
                }
                _ => kept.push(action),
            }
        }
        kept
    }
}

/// The policy itself, on plain types so it is directly testable.
///
/// The single decision lives here, generic-free, so the tests exercise the
/// exact code the engine runs.
pub fn screen(
    inflight: &mut InflightTolerance,
    peer: PeerId,
    rep: &ReputationChange,
    net: &NetworkServiceHandle,
    now: Instant,
    protocol: &ProtocolName,
) -> bool {
    if !is_inflight_abandon(rep) {
        // Everything else stays fatal, and is named in the log so a
        // reason-string drift above is visible rather than silent.
        log::info!("sync: dropping peer {peer}: {rep:?}");
        return true;
    }

    let observed = inflight.observe(peer, now);
    if observed.stalled {
        // No progress while costing us a request every retry cycle. Item F's
        // whole point was "do not ban"; the honest terminal action is "stop
        // serving this peer", not "keep serving it forever". Disconnect it
        // (no reputation change at all) and say so once per stall, then once
        // more if it reconnects and stalls again.
        if observed.first_stall {
            log::warn!(
                "sync: peer {peer} abandoned {} requests over {:?} without announcing a \
                 single new block — wedged, not flaky: disconnecting it instead of serving it \
                 again (no ban; it may return and will be judged from zero if it makes \
                 progress) (#189)",
                observed.aborts,
                STALL_HORIZON,
            );
        }
        net.disconnect_peer(peer, protocol.clone());
        return false;
    }

    if let Some(charge) = observed.charge {
        // Report, but do NOT disconnect: the peer keeps its connection, its
        // requests and its place in the peer set.
        net.report_peer(peer, charge);
    }
    log::info!(
        "sync: peer {peer} abandoned a request mid-flight {}/{} — would have been \
         banned by the SDK, kept connected (penalty {}, budget spent; {} without progress)",
        observed.count,
        MAX_REPORTED_ABORTS,
        observed.charge.map(|c| c.value).unwrap_or(0),
        observed.aborts,
    );
    false
}

impl<B: BlockT> SyncingStrategy<B> for TolerantSyncStrategy<B> {
    fn add_peer(
        &mut self,
        peer_id: PeerId,
        best_hash: B::Hash,
        best_number: sp_runtime::traits::NumberFor<B>,
    ) {
        self.inflight
            .note_peer(peer_id, best_number.unique_saturated_into(), Instant::now());
        self.inner.add_peer(peer_id, best_hash, best_number)
    }

    fn remove_peer(&mut self, peer_id: &PeerId) {
        self.inner.remove_peer(peer_id)
    }

    fn on_validated_block_announce(
        &mut self,
        is_best: bool,
        peer_id: PeerId,
        announce: &sc_network_common::sync::message::BlockAnnounce<B::Header>,
    ) -> Option<(B::Hash, sp_runtime::traits::NumberFor<B>)> {
        // The only progress signal available to this wrapper: a peer that
        // announces a higher block is alive, whatever its retry behaviour.
        self.inflight.note_peer(
            peer_id,
            (*announce.header.number()).unique_saturated_into(),
            Instant::now(),
        );
        self.inner
            .on_validated_block_announce(is_best, peer_id, announce)
    }

    fn set_sync_fork_request(
        &mut self,
        peers: Vec<PeerId>,
        hash: &B::Hash,
        number: sp_runtime::traits::NumberFor<B>,
    ) {
        self.inner.set_sync_fork_request(peers, hash, number)
    }

    fn request_justification(&mut self, hash: &B::Hash, number: sp_runtime::traits::NumberFor<B>) {
        self.inner.request_justification(hash, number)
    }

    fn clear_justification_requests(&mut self) {
        self.inner.clear_justification_requests()
    }

    fn on_justification_import(
        &mut self,
        hash: B::Hash,
        number: sp_runtime::traits::NumberFor<B>,
        success: bool,
    ) {
        self.inner.on_justification_import(hash, number, success)
    }

    fn on_generic_response(
        &mut self,
        peer_id: &PeerId,
        key: StrategyKey,
        protocol_name: sc_network::ProtocolName,
        response: Box<dyn Any + Send>,
    ) {
        // A response means the peer answered: progress, whatever its announces
        // say. Without this, a peer serving blocks while re-announcing the same
        // best number could look silent (#189).
        self.inflight.note_activity(*peer_id, Instant::now());
        self.inner
            .on_generic_response(peer_id, key, protocol_name, response)
    }

    fn on_blocks_processed(
        &mut self,
        imported: usize,
        count: usize,
        results: Vec<(
            Result<
                sc_consensus::BlockImportStatus<sp_runtime::traits::NumberFor<B>>,
                sc_consensus::BlockImportError,
            >,
            B::Hash,
        )>,
    ) {
        self.inner.on_blocks_processed(imported, count, results)
    }

    fn on_block_finalized(&mut self, hash: &B::Hash, number: sp_runtime::traits::NumberFor<B>) {
        self.inner.on_block_finalized(hash, number)
    }

    fn update_chain_info(
        &mut self,
        best_hash: &B::Hash,
        best_number: sp_runtime::traits::NumberFor<B>,
    ) {
        self.inner.update_chain_info(best_hash, best_number)
    }

    fn is_major_syncing(&self) -> bool {
        self.inner.is_major_syncing()
    }

    fn num_peers(&self) -> usize {
        self.inner.num_peers()
    }

    fn status(&self) -> sc_network_sync::SyncStatus<B> {
        self.inner.status()
    }

    fn num_downloaded_blocks(&self) -> usize {
        self.inner.num_downloaded_blocks()
    }

    fn num_sync_requests(&self) -> usize {
        self.inner.num_sync_requests()
    }

    fn actions(
        &mut self,
        network_service: &NetworkServiceHandle,
    ) -> Result<Vec<SyncingAction<B>>, sp_blockchain::Error> {
        let actions = self.inner.actions(network_service)?;
        Ok(self.filter(actions, network_service, Instant::now()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sc_network::ReputationChange as Rep;
    use sc_network_sync::service::network::ToServiceCommand;

    fn sdk_inflight() -> Rep {
        Rep::new_fatal(INFLIGHT_ABANDON_REASON)
    }

    /// The counter the wrapper owns, bare, because `screen()` is generic-free.
    fn tolerance() -> InflightTolerance {
        InflightTolerance::new()
    }

    fn proto() -> ProtocolName {
        "/gpn-test/block-announces/1".into()
    }

    fn channel() -> (
        NetworkServiceHandle,
        sc_utils::mpsc::TracingUnboundedReceiver<ToServiceCommand>,
    ) {
        let (tx, rx) = sc_utils::mpsc::tracing_unbounded("test-network-service", 100);
        (NetworkServiceHandle::new(tx), rx)
    }

    /// BEFORE: the SDK's third abort became `DropPeer` → `disconnect_peer` +
    /// `report_peer(i32::MIN)`. AFTER: three aborts produce no disconnect and
    /// only small reports, and `screen` says "do not drop".
    #[test]
    fn three_aborts_no_longer_drop_the_peer() {
        let mut tol = tolerance();
        let (net, mut rx) = channel();
        let peer = PeerId::random();
        let now = Instant::now();

        for i in 0..3u64 {
            let keep = screen(
                &mut tol,
                peer,
                &sdk_inflight(),
                &net,
                now + Duration::from_secs(i),
                &proto(),
            );
            assert!(!keep, "abort {} must not drop the peer", i + 1);
        }

        drop(net); // close the channel so the drain terminates
        let mut reports = 0;
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                ToServiceCommand::ReportPeer(p, rep) => {
                    assert_eq!(p, peer);
                    assert_ne!(rep.value, i32::MIN, "never fatal");
                    assert!(rep.value > sc_network::peer_store::BANNED_THRESHOLD);
                    reports += 1;
                }
                ToServiceCommand::DisconnectPeer(p, _) => {
                    panic!("peer {p} was disconnected for abandoning a request")
                }
                other => panic!("unexpected command {other:?}"),
            }
        }
        assert_eq!(reports, 3, "one small report per abort, no disconnect");
    }

    /// AFTER: a genuine protocol violation is still dropped, and nothing is
    /// reported in its place.
    #[test]
    fn a_malformed_payload_is_still_fatal() {
        let mut tol = tolerance();
        let (net, mut rx) = channel();
        let peer = PeerId::random();

        for rep in [
            Rep::new_fatal("Malformed block request"),
            Rep::new_fatal("Bad message"),
            Rep::new_fatal("Invalid justification"),
            Rep::new_fatal("Genesis mismatch"),
        ] {
            assert!(
                screen(&mut tol, peer, &rep, &net, Instant::now(), &proto()),
                "{rep:?} must still drop the peer"
            );
        }
        drop(net);
        assert!(rx.try_recv().is_err(), "no reputation change of our own");
    }

    #[test]
    fn the_sdk_report_is_recognised() {
        assert!(is_inflight_abandon(&sdk_inflight()));
    }

    #[test]
    fn genuine_protocol_violations_are_not_recognised() {
        // Every other fatal report in the SDK and in our own block server must
        // keep its behaviour.
        for rep in [
            Rep::new_fatal("Bad message"),
            Rep::new_fatal("Malformed block request"),
            Rep::new_fatal("Same block request multiple times"),
            Rep::new_fatal("Invalid justification"),
            Rep::new_fatal("Genesis mismatch"),
            Rep::new(-1024, "Peer disconnected with inflight after backoffs"), // not fatal, not ours
        ] {
            if rep.reason == "Peer disconnected with inflight after backoffs" {
                continue;
            }
            assert!(!is_inflight_abandon(&rep), "{rep:?} must stay as it was");
        }
    }

    #[test]
    fn three_aborts_never_ban_and_never_reach_the_threshold() {
        let mut tol = InflightTolerance::new();
        let peer = PeerId::random();
        let now = Instant::now();

        let mut total = 0i64;
        for i in 1..=3u64 {
            let o = tol.observe(peer, now + Duration::from_secs(i));
            assert_eq!(o.count, i, "counter must increment per abort");
            let charge = o.charge.expect("budget covers three aborts");
            assert!(charge.value > sc_network::peer_store::BANNED_THRESHOLD);
            assert_ne!(charge.value, i32::MIN, "never fatal");
            total += charge.value as i64;
        }
        assert!(total > sc_network::peer_store::BANNED_THRESHOLD as i64);
        // The state the peer is in after 3 aborts is exactly what the SDK
        // would have banned for; ours still tracks it and keeps going.
        let o = tol.observe(peer, now + Duration::from_secs(4));
        assert_eq!(o.count, 4);
    }

    #[test]
    fn lifetime_budget_caps_the_charge_and_then_reports_nothing() {
        let mut tol = InflightTolerance::new();
        let peer = PeerId::random();
        let mut now = Instant::now();
        let mut spent = 0u32;
        for _ in 0..200 {
            now += Duration::from_secs(100_000); // long past any backoff
            if let Some(c) = tol.observe(peer, now).charge {
                spent += c.value.unsigned_abs();
                assert_ne!(c.value, i32::MIN);
            }
        }
        assert!(spent <= INFLIGHT_PENALTY_BUDGET);
        // At least 1,000× below the peer store's ban threshold.
        let threshold_abs = (sc_network::peer_store::BANNED_THRESHOLD as i64).unsigned_abs();
        assert!((INFLIGHT_PENALTY_BUDGET as u64) * 1000 < threshold_abs);
    }

    #[test]
    fn the_counter_resets_after_the_sdk_backoff_and_state_stays_bounded() {
        let mut tol = InflightTolerance::with_backoff(Duration::from_secs(1));
        let peer = PeerId::random();
        let t0 = Instant::now();

        assert_eq!(tol.observe(peer, t0).count, 1);
        assert_eq!(tol.observe(peer, t0 + Duration::from_millis(10)).count, 2);
        // A clean stretch longer than `backoff × count` (the SDK forgets the
        // peer there too) starts the count again from 1.
        let later = t0 + Duration::from_secs(3);
        assert_eq!(tol.observe(peer, later).count, 1);

        // 4× the bound of peers still leaves a bounded table.
        for _ in 0..(MAX_TRACKED_PEERS * 4) {
            tol.observe(PeerId::random(), later);
        }
        assert!(
            tol.tracked() <= MAX_TRACKED_PEERS,
            "tracked {} > {MAX_TRACKED_PEERS}",
            tol.tracked()
        );
    }

    /// #189: the SDK's own exit condition cannot fire at the cadence observed in
    /// production. Measured on 2026-11-15: one peer abandoned a request 47 times
    /// in 75 min — median gap 70 s — and `sync_tolerance` logged `1/3` every
    /// time, because an entry expires after `backoff × count` = 60 s × 1.
    /// This is the negative control for the count rule: at 70 s the count never
    /// grows, so "it would be banned eventually" was never true.
    #[test]
    fn a_seventy_second_cadence_never_reaches_the_sdk_ban_count() {
        let mut tol = InflightTolerance::new();
        let peer = PeerId::random();
        let mut now = Instant::now();

        for i in 0..47u64 {
            let o = tol.observe(peer, now);
            assert_eq!(
                o.count,
                1,
                "abort {}: a 70 s cadence must keep resetting the SDK's count — \
                 if this ever reaches 3 the premise of #189 changed",
                i + 1
            );
            now += Duration::from_secs(70);
        }

        // ... and with the count neutralised, the progress rule is what fires.
        let o = tol.observe(peer, now);
        assert!(
            o.stalled,
            "a peer with no progress must eventually be judged wedged"
        );
    }

    /// #189, the behaviour itself: at the measured cadence a peer that never
    /// announces a new block is disconnected instead of served, and never
    /// banned. Every abort is answered exactly once.
    #[test]
    fn a_wedged_peer_is_disconnected_not_banned() {
        let mut tol = InflightTolerance::new();
        let (net, mut rx) = channel();
        let peer = PeerId::random();
        let t0 = Instant::now();

        // 47 aborts, 70 s apart, no announcement: the live shape from #189.
        for i in 0..47u64 {
            let keep = screen(
                &mut tol,
                peer,
                &sdk_inflight(),
                &net,
                t0 + Duration::from_secs(70 * i),
                &proto(),
            );
            assert!(!keep, "abort {} must not be handed to the SDK's ban", i + 1);
        }

        drop(net);
        let mut reports = 0;
        let mut disconnects = 0;
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                ToServiceCommand::ReportPeer(_, rep) => {
                    assert_ne!(rep.value, i32::MIN, "never fatal");
                    reports += 1;
                }
                ToServiceCommand::DisconnectPeer(p, _) => {
                    assert_eq!(p, peer);
                    disconnects += 1;
                }
                other => panic!("unexpected command {other:?}"),
            }
        }
        assert!(disconnects > 0, "a wedged peer must stop being served");
        assert_eq!(
            reports + disconnects,
            47,
            "every abort is answered exactly once (report before the verdict, \
             disconnect after) — reports {reports}, disconnects {disconnects}"
        );
    }

    /// #189: progress is the exit from the verdict. One higher announce clears
    /// it, so a slow-but-moving peer is never cut off.
    #[test]
    fn progress_clears_the_stall_and_a_moving_peer_is_never_cut_off() {
        let mut tol = InflightTolerance::new();
        let (net, mut rx) = channel();
        let peer = PeerId::random();
        let t0 = Instant::now();

        tol.note_peer(peer, 306_604, t0);

        // Nine aborts inside the horizon: noisy, not wedged.
        for i in 0..9u64 {
            let o = tol.observe(peer, t0 + Duration::from_secs(70 * i));
            assert!(!o.stalled, "abort {} is below the verdict", i + 1);
        }

        // It announces a new block: the streak is cleared...
        tol.note_peer(peer, 306_700, t0 + Duration::from_secs(700));
        let o = tol.observe(peer, t0 + Duration::from_secs(770));
        assert!(!o.stalled, "a peer that makes progress is never wedged");
        assert_eq!(o.aborts, 1, "progress resets the abort streak");

        // ... and eight more aborts after that are still only noise.
        for i in 1..9u64 {
            let o = tol.observe(peer, t0 + Duration::from_secs(770 + 70 * i));
            assert!(
                !o.stalled,
                "abort {} after progress is below the verdict",
                i + 1
            );
        }

        drop(net);
        while let Ok(cmd) = rx.try_recv() {
            assert!(
                !matches!(cmd, ToServiceCommand::DisconnectPeer(..)),
                "a moving peer must never be disconnected"
            );
        }
    }

    /// #189, the narrowed window: the verdict needs BOTH the abort count and a
    /// silent horizon, so a burst of aborts from a peer that just joined is not
    /// mistaken for a wedge.
    #[test]
    fn aborts_alone_inside_the_horizon_do_not_wedge_a_peer() {
        let mut tol = InflightTolerance::new();
        let peer = PeerId::random();
        let t0 = Instant::now();
        for i in 0..50u64 {
            let o = tol.observe(peer, t0 + Duration::from_millis(100 * i));
            assert!(
                !o.stalled,
                "abort {} inside the horizon must not be a verdict",
                i + 1
            );
        }
    }

    /// #189, the safety valve: a peer that answers requests is making progress
    /// even when it re-announces the same best number, so it must not be judged
    /// wedged on the announce stream alone.
    #[test]
    fn a_peer_that_answers_is_never_wedged() {
        let mut tol = InflightTolerance::new();
        let peer = PeerId::random();
        let mut now = Instant::now();

        tol.note_peer(peer, 306_604, now);
        for _ in 0..12u64 {
            now += Duration::from_secs(70);
            // It serves a response between aborts, as a working peer does.
            tol.note_activity(peer, now);
            let o = tol.observe(peer, now);
            assert!(!o.stalled, "an answering peer is not wedged");
            assert!(
                o.charge.is_some(),
                "still answers every abort with a report"
            );
        }
    }

    /// The reason literal above is the whole match. If the SDK is bumped and
    /// renames it, this test fails instead of the filter silently doing nothing.
    #[test]
    fn reason_string_matches_pinned_sdk() {
        let home = std::env::var("HOME").unwrap_or_default();
        let checkouts = std::path::Path::new(&home).join(".cargo/git/checkouts");
        let mut found = false;
        if let Ok(entries) = std::fs::read_dir(&checkouts) {
            for e in entries.flatten() {
                if !e.file_name().to_string_lossy().starts_with("polkadot-sdk") {
                    continue;
                }
                for rev in std::fs::read_dir(e.path()).into_iter().flatten().flatten() {
                    let f = rev
                        .path()
                        .join("substrate/client/network/sync/src/strategy/disconnected_peers.rs");
                    if let Ok(src) = std::fs::read_to_string(&f) {
                        found = true;
                        assert!(
                            src.contains(&format!("Rep::new_fatal(\"{INFLIGHT_ABANDON_REASON}\")")),
                            "the pinned SDK no longer reports {:?} as fatal — \
                             the wrapper's match must be updated",
                            INFLIGHT_ABANDON_REASON
                        );
                    }
                }
            }
        }
        if !found {
            eprintln!("pinned polkadot-sdk checkout not present; skipping SDK source check");
        }
    }
}
