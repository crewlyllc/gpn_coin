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

use sc_network::{PeerId, ReputationChange};
use sc_network_sync::service::network::NetworkServiceHandle;
use sc_network_sync::strategy::{StrategyKey, SyncingAction, SyncingStrategy};
use sc_network_sync::types::BadPeer;
use sp_runtime::traits::Block as BlockT;
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
    /// is spent.
    pub charge: Option<ReputationChange>,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    count: u64,
    last: Instant,
    spent: u32,
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
        }
    }

    /// Same as [`Self::new`] with a shorter backoff, for tests.
    #[cfg(test)]
    pub fn with_backoff(backoff: Duration) -> Self {
        Self {
            peers: HashMap::new(),
            backoff,
        }
    }

    /// Number of peers currently tracked (bounded by [`MAX_TRACKED_PEERS`]).
    #[cfg(test)]
    pub fn tracked(&self) -> usize {
        self.peers.len()
    }

    /// Record one abort for `peer` and decide what to report.
    pub fn observe(&mut self, peer: PeerId, now: Instant) -> Observed {
        let (count, spent) = match self.peers.get(&peer) {
            // The SDK would have forgotten this entry after `backoff × count`.
            Some(e) if now.saturating_duration_since(e.last) < self.backoff * e.count as u32 => {
                (e.count.saturating_add(1), e.spent)
            }
            Some(e) => (1, e.spent),
            None => (1, 0),
        };

        let remaining = INFLIGHT_PENALTY_BUDGET.saturating_sub(spent);
        let actual = INFLIGHT_PENALTY.unsigned_abs().min(remaining);
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
            },
        );

        Observed { count, charge }
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
}

impl<B: BlockT> TolerantSyncStrategy<B> {
    pub fn new(inner: Box<dyn SyncingStrategy<B>>) -> Self {
        Self {
            inner,
            inflight: InflightTolerance::new(),
        }
    }

    /// Apply the one policy this wrapper owns to the inner strategy's actions.
    fn filter(
        &mut self,
        actions: Vec<SyncingAction<B>>,
        net: &NetworkServiceHandle,
        now: Instant,
    ) -> Vec<SyncingAction<B>> {
        let mut kept = Vec::with_capacity(actions.len());
        for action in actions {
            match &action {
                SyncingAction::DropPeer(BadPeer(peer, rep)) => {
                    if self.screen(*peer, rep, net, now) {
                        kept.push(action);
                    }
                }
                _ => kept.push(action),
            }
        }
        kept
    }

    /// The single decision, split out and generic-free so the tests exercise
    /// the exact code the engine runs.
    ///
    /// Returns `true` when the peer must still be dropped (every other fatal
    /// report), `false` when the action is neutralised.
    fn screen(
        &mut self,
        peer: PeerId,
        rep: &ReputationChange,
        net: &NetworkServiceHandle,
        now: Instant,
    ) -> bool {
        screen(&mut self.inflight, peer, rep, net, now)
    }
}

/// The policy itself, on plain types so it is directly testable.
///
/// See [`TolerantSyncStrategy::screen`].
pub fn screen(
    inflight: &mut InflightTolerance,
    peer: PeerId,
    rep: &ReputationChange,
    net: &NetworkServiceHandle,
    now: Instant,
) -> bool {
    if !is_inflight_abandon(rep) {
        // Everything else stays fatal, and is named in the log so a
        // reason-string drift above is visible rather than silent.
        log::info!("sync: dropping peer {peer}: {rep:?}");
        return true;
    }

    let observed = inflight.observe(peer, now);
    if let Some(charge) = observed.charge {
        // Report, but do NOT disconnect: the peer keeps its connection, its
        // requests and its place in the peer set.
        net.report_peer(peer, charge);
    }
    log::info!(
        "sync: peer {peer} abandoned a request mid-flight {}/{} — would have been \
         banned by the SDK, kept connected (penalty {}, budget spent)",
        observed.count,
        MAX_REPORTED_ABORTS,
        observed.charge.map(|c| c.value).unwrap_or(0),
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
                screen(&mut tol, peer, &rep, &net, Instant::now()),
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
