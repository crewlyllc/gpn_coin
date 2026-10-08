//! Clock / NTP skew detection at node startup (roadmap item 8).
//!
//! A skewed wall clock can **wedge** a node: block timestamps are validated
//! against the local clock and difficulty is derived from block timing, so a
//! host whose clock is off by tens of seconds can have otherwise-valid blocks
//! rejected — and this is **not self-healing** (it needs NTP). The Windows
//! node was observed ~57 s off. This module performs a single, best-effort NTP
//! exchange at startup and logs a loud warning when the local clock disagrees
//! with a trusted time source by more than a configurable threshold.
//!
//! It is deliberately dependency-free (std `UdpSocket`), never fatal (a failed
//! lookup only logs at debug/info), and disabled with `GPN_NTP_CHECK=0`.
//!
//! Config:
//!   * `GPN_NTP_SERVER`      — NTP host (default `pool.ntp.org`).
//!   * `GPN_NTP_MAX_SKEW_MS` — warn threshold in ms (default 5000; a value
//!                             >30_000 escalates the log to ERROR).
//!   * `GPN_NTP_CHECK`       — set to `0`/`false` to skip the check entirely.

use std::net::UdpSocket;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// NTP timestamps count seconds since 1900; UNIX counts from 1970.
const NTP_UNIX_EPOCH_DELTA: i64 = 2_208_988_800;
/// A minimal NTP packet is 48 bytes.
const NTP_PACKET_LEN: usize = 48;
/// Local-clock skew at/below this is normal (ms).
const DEFAULT_MAX_SKEW_MS: i64 = 5_000;

/// Convert an NTP (seconds, fraction) pair to UNIX milliseconds.
fn ntp_to_unix_ms(secs: u32, frac: u32) -> i64 {
    (secs as i64 - NTP_UNIX_EPOCH_DELTA) * 1000 + ((frac as i64) * 1000) / (1i64 << 32)
}

/// Local UNIX time in milliseconds, or `None` if the system clock is before
/// the epoch (a nonsense clock we can't reason about).
fn local_unix_ms() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as i64)
}

/// Local-clock offset: `((T1 - T2) + (T4 - T3)) / 2`, where T1 = client send,
/// T2 = server receive, T3 = server transmit, T4 = client receive (all UNIX
/// ms). A positive value means the **local** clock is ahead of the server.
/// (This is the negation of the RFC 5905 θ, chosen so the sign reads as
/// "local minus server".)
fn ntp_offset_ms(t1: i64, t2: i64, t3: i64, t4: i64) -> i64 {
    (((t1 - t2) as i128 + (t4 - t3) as i128) / 2) as i64
}

/// Parse a 48-byte NTP server response into `(originate, receive, transmit)`
/// UNIX-ms timestamps. Returns `None` for short packets or a server that is
/// not synchronised (leap-indicator 3).
fn parse_ntp_response(buf: &[u8]) -> Option<(i64, i64, i64)> {
    if buf.len() < NTP_PACKET_LEN {
        return None;
    }
    // Top two bits of byte 0 = leap indicator; 0b11 = unsynchronised.
    if buf[0] >> 6 == 3 {
        return None;
    }
    let rd = |o: usize| u32::from_be_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);
    Some((
        ntp_to_unix_ms(rd(24), rd(28)), // originate (echoes our T1)
        ntp_to_unix_ms(rd(32), rd(36)), // receive  (T2)
        ntp_to_unix_ms(rd(40), rd(44)), // transmit (T3)
    ))
}

/// Build a minimal NTPv3 client-mode request (48 bytes).
fn ntp_request() -> [u8; NTP_PACKET_LEN] {
    let mut p = [0u8; NTP_PACKET_LEN];
    // LI = 0, version = 3, mode = 3 (client) → first byte 0b00_011_011.
    p[0] = 0x1b;
    p
}

/// Query `server` once over UDP and return the measured local-clock offset in
/// ms (positive = local ahead). Errors are descriptive strings, never fatal.
fn measure_offset(server: &str, timeout: Duration) -> Result<i64, String> {
    let addr = format!("{server}:123");
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("bind: {e}"))?;
    socket
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("set_read_timeout: {e}"))?;

    let t1 = local_unix_ms().ok_or("local clock before UNIX epoch")?;
    socket
        .send_to(&ntp_request(), &addr)
        .map_err(|e| format!("send_to {addr}: {e}"))?;

    let mut buf = [0u8; NTP_PACKET_LEN];
    let (n, _src) = socket
        .recv_from(&mut buf)
        .map_err(|e| format!("recv_from: {e}"))?;
    let t4 = local_unix_ms().ok_or("local clock before UNIX epoch")?;

    let (_orig, t2, t3) = parse_ntp_response(&buf[..n]).ok_or("malformed NTP response")?;
    Ok(ntp_offset_ms(t1, t2, t3, t4))
}

/// Parse `GPN_NTP_MAX_SKEW_MS` (defaults to 5 s on absence/garbage).
fn max_skew_ms() -> i64 {
    std::env::var("GPN_NTP_MAX_SKEW_MS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_MAX_SKEW_MS)
}

/// Run the startup clock check and log the outcome. Intended to be called once
/// from `new_full` on a background task so a slow NTP lookup never delays
/// startup.
pub fn run_startup_check() {
    // Opt-out for air-gapped/CI hosts.
    if let Ok(v) = std::env::var("GPN_NTP_CHECK") {
        if v == "0" || v.eq_ignore_ascii_case("false") {
            log::debug!("clock check disabled (GPN_NTP_CHECK={v})");
            return;
        }
    }

    let server = std::env::var("GPN_NTP_SERVER")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "pool.ntp.org".to_string());
    let threshold = max_skew_ms();

    match measure_offset(&server, Duration::from_secs(3)) {
        Ok(offset) => {
            let ahead = if offset >= 0 { "ahead of" } else { "behind" };
            if offset.abs() > 30_000 {
                log::error!(
                    "⛔ CLOCK SKEW: local clock is {} ms {} {server} (limit {} ms). A large \
                     clock offset can wedge PoW block/timestamp validation — fix NTP/`timedatectl` \
                     before trusting this node's view of the chain.",
                    offset.abs(),
                    ahead,
                    threshold,
                );
            } else if offset.abs() > threshold {
                log::warn!(
                    "⚠️ Clock skew: local clock is {} ms {} {server} (limit {} ms). Consider \
                     syncing NTP (e.g. `sudo timedatectl set-ntp true`); large skew can wedge \
                     block/timestamp validation.",
                    offset.abs(),
                    ahead,
                    threshold,
                );
            } else {
                log::info!(
                    "Clock check OK: local clock within {} ms of {server} (offset {} ms).",
                    threshold,
                    offset
                );
            }
        }
        Err(e) => {
            // Never fatal — just note that we couldn't verify the clock.
            log::info!(
                "Clock check skipped: could not reach NTP server {server} ({e}). Set \
                 GPN_NTP_CHECK=0 to silence, or GPN_NTP_SERVER=<host> to point at a local peer."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntp_epoch_conversion() {
        // NTP epoch 1900 → UNIX 0.
        assert_eq!(ntp_to_unix_ms(NTP_UNIX_EPOCH_DELTA as u32, 0), 0);
        // A known instant: 2021-01-01T00:00:00Z = 1609459200 UNIX s.
        let unix = 1_609_459_200u32;
        let ntp_secs = (unix as i64 + NTP_UNIX_EPOCH_DELTA) as u32;
        assert_eq!(ntp_to_unix_ms(ntp_secs, 0), 1_609_459_200_000);
        // Half a second of fraction ≈ 500 ms.
        assert_eq!(ntp_to_unix_ms(ntp_secs, 1 << 31), 1_609_459_200_500);
    }

    #[test]
    fn offset_is_symmetric_and_signs_correctly() {
        // Symmetric round trip with a locally-ahead clock: T1/T4 shifted +2 s
        // relative to a server that is exactly "now" → offset +2000.
        let (t1, t2, t3, t4) = (2_000, 0, 0, 2_000);
        assert_eq!(ntp_offset_ms(t1, t2, t3, t4), 2_000);
        // Locally behind by 3 s.
        let (t1, t2, t3, t4) = (0, 3_000, 3_000, 0);
        assert_eq!(ntp_offset_ms(t1, t2, t3, t4), -3_000);
        // Perfectly synced.
        assert_eq!(ntp_offset_ms(10, 10, 10, 10), 0);
    }

    #[test]
    fn parses_valid_response() {
        let mut buf = [0u8; NTP_PACKET_LEN];
        buf[0] = 0x1c; // LI=0, VN=3, mode=4 (server)
        let secs = (NTP_UNIX_EPOCH_DELTA as u32) + 1_000_000;
        buf[24..28].copy_from_slice(&secs.to_be_bytes()); // originate
        buf[32..36].copy_from_slice(&secs.to_be_bytes()); // receive
        buf[40..44].copy_from_slice(&secs.to_be_bytes()); // transmit
        let (o, r, t) = parse_ntp_response(&buf).expect("valid packet");
        assert_eq!(o, 1_000_000_000);
        assert_eq!(r, 1_000_000_000);
        assert_eq!(t, 1_000_000_000);
    }

    #[test]
    fn rejects_short_and_unsynced() {
        assert!(parse_ntp_response(&[0u8; 10]).is_none(), "short packet");
        let mut buf = [0u8; NTP_PACKET_LEN];
        buf[0] = 0b11 << 6; // LI = 3 → unsynchronised
        assert!(parse_ntp_response(&buf).is_none(), "unsynced server");
    }
}
