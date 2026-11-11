//! Announce chain state to every peer (GPN #159, 2026-11-10 owner design).
//!
//! `identify` tells a peer *what software* we run; the sync handshake tells it
//! where our best block is — but neither of those is compared against a signed
//! reference, so a node on a fork only finds out at the next daily checkpoint
//! (`gpn_snapshot checkpoint`, #156/#157). By then it has served and sealed the
//! wrong chain for hours. On 2026-11-08/09 two pairs of boxes did exactly that,
//! each pair calling the other's chain canonical.
//!
//! This module is the wire format and the comparison for `gpn/chain/1`, a
//! notification protocol that carries four facts on the connection itself:
//!
//! ```text
//! gpncs/1 0x<b8e4636a…>:0x<best hash…>@<height>
//! ```
//!
//! * **genesis** — a different genesis is a *different network*, not a fork.
//!   It is checked first, at every height, so a rogue chain is named the moment
//!   the substream opens.
//! * **best hash @ height** — same height + different hash is the observed fork
//!   shape (see [`crate::fork_tolerance`]): worth a loud line, never a ban.
//!
//! Announcement policy lives in [`announce_due`]: send once when a peer's
//! substream opens, then only when our best height actually advanced. A node
//! that idles must not emit a heartbeat every tick, and a node must never
//! publish a stale height as if it were current (#141/#144 three-state rule:
//! the state we send is the state we just read, otherwise we send nothing).
//!
//! Everything here is pure and unit-tested; `service.rs` owns the libp2p half.

use std::collections::HashMap;

/// Protocol id. Substrate's notification protocols are multistream-select
/// names, so they carry the leading slash (the design says `gpn/chain/1`).
pub const CHAIN_STATE_PROTOCOL: &str = "/gpn/chain/1";
/// Wire format prefix + version. A future v2 must stay decodable by this
/// decoder (it is not, deliberately: an unknown version yields nothing).
pub const WIRE_PREFIX: &str = "gpncs/1 ";
/// Hard cap on a payload we will even look at. A real message is ~150 bytes;
/// the bound exists so a hostile peer cannot make us parse a megabyte.
pub const MAX_BYTES: usize = 1024;
/// Announce at most this often when the chain keeps advancing: fast enough for
/// "immediately" to mean seconds, cheap enough to ignore (one tiny message).
pub const TICK_SECS: u64 = 30;
/// A hex hash token: `0x` + 64 hex characters.
const HASH_LEN: usize = 66;

/// Our (or a peer's) claim about the chain it is on.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChainState {
    /// Genesis hash, normalised `0x` + 64 lowercase hex.
    pub genesis: String,
    /// Best block hash at `height`, same normalisation.
    pub best_hash: String,
    /// Best block number.
    pub height: u64,
}

/// What the two claims say about the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Same genesis, same height, same hash.
    SameChain,
    /// Same genesis, they are behind — they will sync from us (or we serve them).
    PeerBehind,
    /// Same genesis, they are ahead — we are the one that should sync.
    PeerAhead,
    /// Same genesis, same height, different hash: the observed fork shape.
    /// Divergence is not misbehaviour (#158) — stay connected and serve them.
    ForkAtSameHeight,
    /// Different genesis: a different network. No amount of syncing fixes it.
    WrongChain,
}

impl Verdict {
    /// Should this verdict be logged loudly? Same-chain chatter is `debug`.
    pub fn is_notable(self) -> bool {
        matches!(
            self,
            Verdict::WrongChain | Verdict::ForkAtSameHeight | Verdict::PeerAhead
        )
    }

    /// Stable slug used in log lines and tests.
    pub fn slug(self) -> &'static str {
        match self {
            Verdict::SameChain => "same-chain",
            Verdict::PeerBehind => "peer-behind",
            Verdict::PeerAhead => "peer-ahead",
            Verdict::ForkAtSameHeight => "fork-at-same-height",
            Verdict::WrongChain => "wrong-chain",
        }
    }
}

fn is_hash(s: &str) -> bool {
    s.len() == HASH_LEN
        && (s.starts_with("0x") || s.starts_with("0X"))
        && s[2..].chars().all(|c| c.is_ascii_hexdigit())
}

fn normalise_hash(s: &str) -> Option<String> {
    if !is_hash(s) {
        return None;
    }
    Some(format!("0x{}", s[2..].to_ascii_lowercase()))
}

impl ChainState {
    /// Build from a runtime `H256`-style debug string and a block number.
    /// Returns `None` for anything that is not a 32-byte hash, so an
    /// unreadable local tip is never published as a fact.
    pub fn new(genesis: &str, best_hash: &str, height: u64) -> Option<Self> {
        Some(Self {
            genesis: normalise_hash(genesis)?,
            best_hash: normalise_hash(best_hash)?,
            height,
        })
    }

    /// Short `0x1234abcd` form for log lines.
    pub fn short(hash: &str) -> String {
        let s = hash.trim_start_matches("0x");
        format!("0x{}", s.chars().take(8).collect::<String>())
    }
}

/// Encode exactly one line: `gpncs/1 <genesis>:<best_hash>@<height>`.
/// Returns `None` if the state is malformed (never emit a lie).
pub fn encode(state: &ChainState) -> Option<Vec<u8>> {
    if !is_hash(&state.genesis) || !is_hash(&state.best_hash) {
        return None;
    }
    let body = format!(
        "{WIRE_PREFIX}{}:{}@{}",
        normalise_hash(&state.genesis)?,
        normalise_hash(&state.best_hash)?,
        state.height
    );
    if body.len() > MAX_BYTES {
        return None;
    }
    Some(body.into_bytes())
}

/// Tolerant decoder: anything malformed, oversized or from a future version
/// yields `None`. A peer must never be able to make us act on garbage.
pub fn decode(bytes: &[u8]) -> Option<ChainState> {
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?.trim();
    let rest = text.strip_prefix(WIRE_PREFIX)?;
    let (genesis, tail) = rest.split_once(':')?;
    let (best_hash, height) = tail.rsplit_once('@')?;
    let height: u64 = height.parse().ok()?;
    ChainState::new(genesis, best_hash, height)
}

/// Compare a peer's claim with ours. Genesis wins over height: a chain with a
/// different genesis is a different network at *any* height.
pub fn compare(local: &ChainState, remote: &ChainState) -> Verdict {
    if local.genesis != remote.genesis {
        return Verdict::WrongChain;
    }
    if remote.height < local.height {
        return Verdict::PeerBehind;
    }
    if remote.height > local.height {
        return Verdict::PeerAhead;
    }
    if local.best_hash == remote.best_hash {
        Verdict::SameChain
    } else {
        Verdict::ForkAtSameHeight
    }
}

/// Send now, or wait? `last` is the height we last announced to this peer
/// (`None` = never). A regressed or unchanged height is not news; only a
/// strictly higher tip is, so an idle node is silent and a syncing node is
/// current within [`TICK_SECS`].
pub fn announce_due(last: Option<u64>, current: u64) -> bool {
    match last {
        None => true,
        Some(prev) => current > prev,
    }
}

/// Edge-triggered per-peer reporting: returns `Some(verdict)` only when this
/// peer's claim or our verdict changed since the last time we looked at it, so
/// a peer parked on a fork logs once instead of every tick (same shape as the
/// `RejectLog`/`notice_once` throttles).
#[derive(Debug, Default)]
pub struct StateLog {
    seen: HashMap<String, (Verdict, u64, String)>,
}

impl StateLog {
    /// Record a report. Returns the verdict to log when it is new or changed.
    pub fn record(&mut self, peer: &str, verdict: Verdict, remote: &ChainState) -> Option<Verdict> {
        let entry = (verdict, remote.height, remote.best_hash.clone());
        if self.seen.get(peer) == Some(&entry) {
            return None;
        }
        self.seen.insert(peer.to_string(), entry);
        Some(verdict)
    }

    /// Forget a peer that disconnected, so a fresh connection logs its state
    /// again rather than inheriting silence from the previous session.
    pub fn forget(&mut self, peer: &str) {
        self.seen.remove(peer);
    }

    /// Peers currently tracked (for tests / a status line).
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

/// One human-readable line for a peer whose claim deserves attention.
pub fn describe(peer: &str, local: &ChainState, remote: &ChainState, verdict: Verdict) -> String {
    match verdict {
        Verdict::WrongChain => format!(
            "chain-state: peer {peer} is on a DIFFERENT NETWORK — its genesis {} \
             (best #{} {}) vs ours {} — do not sync from it and do not serve it as \
             canonical; a genesis mismatch is not a fork",
            ChainState::short(&remote.genesis),
            remote.height,
            ChainState::short(&remote.best_hash),
            ChainState::short(&local.genesis)
        ),
        Verdict::ForkAtSameHeight => format!(
            "chain-state: peer {peer} reports a different block at the same height \
             #{} (theirs {}, ours {}) — divergence is not misbehaviour; keeping the \
             link and serving canonical blocks (#158)",
            local.height,
            ChainState::short(&remote.best_hash),
            ChainState::short(&local.best_hash)
        ),
        Verdict::PeerAhead => format!(
            "chain-state: peer {peer} is ahead at #{} (ours #{}, same genesis) — \
             syncing from it",
            remote.height, local.height
        ),
        Verdict::PeerBehind => format!(
            "chain-state: peer {peer} is behind at #{} (ours #{}); it will sync from us",
            remote.height, local.height
        ),
        Verdict::SameChain => format!(
            "chain-state: peer {peer} agrees on #{} {}",
            local.height,
            ChainState::short(&local.best_hash)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GENESIS: &str = "0xb8e4636a00000000000000000000000000000000000000000000000000000001";
    const OTHER: &str = "0xdeadbeef00000000000000000000000000000000000000000000000000000002";
    const H1: &str = "0x1111111100000000000000000000000000000000000000000000000000000001";
    const H2: &str = "0x2222222200000000000000000000000000000000000000000000000000000002";

    fn state(genesis: &str, hash: &str, height: u64) -> ChainState {
        ChainState::new(genesis, hash, height).expect("valid hash")
    }

    #[test]
    fn a_claim_round_trips_exactly() {
        let s = state(GENESIS, H1, 280_000);
        let bytes = encode(&s).expect("encodes");
        assert!(bytes.len() <= MAX_BYTES);
        assert_eq!(decode(&bytes), Some(s));
        // The wire format is the one the item named: genesis:hash@height.
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text, format!("gpncs/1 {GENESIS}:{H1}@280000"));
    }

    #[test]
    fn hashes_are_normalised_and_non_hashes_are_refused() {
        let upper = state(GENESIS, &H1.to_ascii_uppercase(), 1);
        assert_eq!(upper.best_hash, H1);
        assert!(ChainState::new(GENESIS, "0xnothex", 1).is_none());
        assert!(ChainState::new(GENESIS, &H1[..20], 1).is_none());
        // A malformed local tip is never encoded as a fact.
        assert!(encode(&ChainState {
            genesis: GENESIS.into(),
            best_hash: "short".into(),
            height: 5,
        })
        .is_none());
    }

    #[test]
    fn decoder_is_total_on_hostile_input() {
        assert!(decode(b"").is_none());
        assert!(decode(b"gpncs/2 0x00:0x00@1").is_none());
        assert!(decode(b"not a claim").is_none());
        assert!(decode(&vec![b'x'; MAX_BYTES + 1]).is_none());
        // Missing pieces / bad height / non-UTF8.
        assert!(decode(format!("gpncs/1 {GENESIS}@1").as_bytes()).is_none());
        assert!(decode(format!("gpncs/1 {GENESIS}:{H1}@").as_bytes()).is_none());
        assert!(decode(format!("gpncs/1 {GENESIS}:{H1}@-1").as_bytes()).is_none());
        assert!(
            decode(format!("gpncs/1 {GENESIS}:{H1}@99999999999999999999999").as_bytes()).is_none()
        );
        assert!(decode(&[0xff, 0xfe]).is_none());
        // A structurally valid line still needs real hashes.
        assert!(decode(b"gpncs/1 0x00:0x00@1").is_none());
    }

    #[test]
    fn a_genesis_mismatch_is_named_at_any_height() {
        let ours = state(GENESIS, H1, 100);
        // Behind, level and ahead: all a different network.
        for (h, height) in [(H2, 50u64), (H2, 100), (H2, 900)] {
            assert_eq!(
                compare(&ours, &state(OTHER, h, height)),
                Verdict::WrongChain
            );
        }
    }

    #[test]
    fn the_comparison_matrix_matches_the_design() {
        let ours = state(GENESIS, H1, 100);
        assert_eq!(compare(&ours, &state(GENESIS, H1, 100)), Verdict::SameChain);
        assert_eq!(compare(&ours, &state(GENESIS, H2, 99)), Verdict::PeerBehind);
        assert_eq!(compare(&ours, &state(GENESIS, H2, 101)), Verdict::PeerAhead);
        // The observed fork shape: same height, different block.
        assert_eq!(
            compare(&ours, &state(GENESIS, H2, 100)),
            Verdict::ForkAtSameHeight
        );
        // Same hash at the same height cannot be a fork.
        assert_eq!(compare(&ours, &state(GENESIS, H1, 100)), Verdict::SameChain);
        // Notability: divergence and a wrong network are loud; "behind" is not.
        assert!(Verdict::ForkAtSameHeight.is_notable());
        assert!(Verdict::WrongChain.is_notable());
        assert!(!Verdict::PeerBehind.is_notable());
        assert!(!Verdict::SameChain.is_notable());
    }

    #[test]
    fn we_announce_on_open_and_only_when_the_tip_advances() {
        assert!(announce_due(None, 5));
        assert!(!announce_due(Some(5), 5));
        assert!(!announce_due(Some(5), 4));
        assert!(announce_due(Some(5), 6));
    }

    #[test]
    fn a_parked_fork_logs_once_not_every_tick() {
        let mut log = StateLog::default();
        let ours = state(GENESIS, H1, 100);
        let theirs = state(GENESIS, H2, 100);
        let v = compare(&ours, &theirs);
        assert_eq!(log.record("12D3KooA", v, &theirs), Some(v));
        assert_eq!(log.record("12D3KooA", v, &theirs), None);
        // A new tip is news again (they moved or reorged).
        let moved = state(GENESIS, H2, 101);
        assert_eq!(
            log.record("12D3KooA", compare(&ours, &moved), &moved),
            Some(Verdict::PeerAhead)
        );
        // A reconnect logs its state again instead of inheriting silence.
        log.forget("12D3KooA");
        assert_eq!(log.record("12D3KooA", v, &theirs), Some(v));
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn the_line_names_what_an_operator_must_do() {
        let ours = state(GENESIS, H1, 100);
        let wrong = describe("p", &ours, &state(OTHER, H2, 100), Verdict::WrongChain);
        assert!(wrong.contains("DIFFERENT NETWORK"));
        assert!(wrong.contains("do not sync from it"));
        let fork = describe(
            "p",
            &ours,
            &state(GENESIS, H2, 100),
            Verdict::ForkAtSameHeight,
        );
        assert!(fork.contains("different block at the same height"));
        assert!(fork.contains("#158"));
        assert!(
            describe("p", &ours, &state(GENESIS, H1, 100), Verdict::SameChain).contains("#100")
        );
    }

    #[test]
    fn protocol_constants_are_pinned() {
        assert_eq!(CHAIN_STATE_PROTOCOL, "/gpn/chain/1");
        assert_eq!(WIRE_PREFIX, "gpncs/1 ");
        assert_eq!(TICK_SECS, 30);
    }
}
