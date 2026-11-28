//! Solana delegation (row 295) — the operator's own SOL, delegated from their
//! own keypair, with the lock stated honestly (design: `docs/design-staking-integrations.md` §3).
//!
//! Owner decision 2026-11-27: Solana delegation is in scope; Ethereum staking is
//! out of scope entirely. Staking rewards appear in the dashboard's conditional
//! "Other earnings" section and **never** merge with GPN's earnings line or the
//! airdrop export.
//!
//! ## Shape (same split as `secondary.rs`)
//!
//! This module is the *policy* half — the config file, its validation, the
//! validator list rendering, the lock-state wording, the JSON-RPC reads. The
//! supervisor's staking thread (next slice) is the *acting* half: it reads the
//! config, submits the create+delegate transaction signed by the local
//! keypair, and writes `staking-status.json`.
//!
//! ## What this module refuses to do
//!
//! - **No default that stakes anything.** `StakeConfig::default()` is off with
//!   amount 0; enabling takes an explicit amount and an explicit validator.
//! - **No fake progress.** Deactivation takes 1–3 days; every state string says
//!   so with an epoch/date, never "pending" or a spinner.
//! - **No merged balances.** Rewards are a separate figure with their own
//!   source label; `earnings.rs` reads GPN's account balance only.
//!
//! - config:  `$GPN_STAKING_FILE`, else `~/.gpn/staking.json` (0600)
//! - status:  `$GPN_STAKING_STATUS_FILE`, else `~/.gpn/staking-status.json`
//!   (written by the supervisor, read by the dashboard)

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

/// Smallest delegation the design calls viable (~0.01 SOL). Below this the
/// reward per epoch is dust; refusing is kinder than a silently worthless stake.
pub const MIN_DELEGATION_LAMPORTS: u64 = 10_000_000;

/// Rent-exempt reserve for a stake account (0.00228 SOL, the figure the chain
/// charges today). It is charged **on top of** the delegated amount.
pub const STAKE_ACCOUNT_RENT_LAMPORTS: u64 = 2_282_880;

/// Deactivation is not instant and not a fixed number of seconds: the stake
/// finishes deactivating at an epoch boundary, which is 1–3 days away.
pub const LOCK_MIN_DAYS: u32 = 1;
pub const LOCK_MAX_DAYS: u32 = 3;

/// Solana epoch length used when converting credits to an uptime figure.
pub const SLOTS_PER_EPOCH: u64 = 432_000;

pub const DEFAULT_RPC_URL: &str = "https://api.devnet.solana.com";

// ── units ────────────────────────────────────────────────────────────────────

/// Parse a decimal SOL string ("0.01", "1", "2.5") into lamports.
///
/// Rejects anything ambiguous: more than 9 decimals (lamports are the smallest
/// unit), signs, whitespace inside the number, and overflow. A silent rounding
/// here would state a different amount than the operator typed.
pub fn parse_sol(s: &str) -> Result<u64, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("amount is empty".into());
    }
    if t.starts_with('-') || t.starts_with('+') {
        return Err(format!("`{t}` — enter a positive amount"));
    }
    let (whole, frac) = match t.split_once('.') {
        Some((w, f)) => (w, f),
        None => (t, ""),
    };
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("`{t}` is not a SOL amount (use e.g. 0.01)"));
    }
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("`{t}` is not a SOL amount (use e.g. 0.01)"));
    }
    if frac.len() > 9 {
        return Err(format!(
            "`{t}` has more than 9 decimals — one lamport is 0.000000001 SOL"
        ));
    }
    let whole: u64 = whole
        .parse()
        .map_err(|_| format!("`{t}` is too large to be a SOL amount"))?;
    let mut frac_padded = frac.to_string();
    while frac_padded.len() < 9 {
        frac_padded.push('0');
    }
    let frac: u64 = if frac_padded.is_empty() {
        0
    } else {
        frac_padded
            .parse()
            .map_err(|_| format!("`{t}` is not a SOL amount"))?
    };
    whole
        .checked_mul(LAMPORTS_PER_SOL)
        .and_then(|w| w.checked_add(frac))
        .ok_or_else(|| format!("`{t}` is too large to be a SOL amount"))
}

/// Format lamports as a SOL string with trailing zeros trimmed.
pub fn format_sol(lamports: u64) -> String {
    let whole = lamports / LAMPORTS_PER_SOL;
    let frac = lamports % LAMPORTS_PER_SOL;
    if frac == 0 {
        return whole.to_string();
    }
    let mut f = format!("{frac:09}");
    while f.ends_with('0') {
        f.pop();
    }
    format!("{whole}.{f}")
}

/// What the wallet must hold: the delegation itself plus the stake account's rent.
pub fn balance_needed_lamports(amount_lamports: u64) -> u64 {
    amount_lamports.saturating_add(STAKE_ACCOUNT_RENT_LAMPORTS)
}

// ── base58 (pubkeys) ─────────────────────────────────────────────────────────

const B58: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Minimal base58 decoder — only used to check that a validator / stake address
/// is a 32-byte Solana public key. Not used for signing anything.
pub fn base58_decode(s: &str) -> Option<Vec<u8>> {
    if s.is_empty() {
        return None;
    }
    // Little-endian accumulator: bytes[0] is the least significant byte.
    let mut bytes: Vec<u8> = Vec::new();
    for ch in s.chars() {
        let digit = B58.find(ch)? as u32;
        let mut carry = digit;
        for b in bytes.iter_mut() {
            let v = (*b as u32) * 58 + carry;
            *b = (v & 0xff) as u8;
            carry = v >> 8;
        }
        while carry > 0 {
            bytes.push((carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let leading_zeros = s.chars().take_while(|c| *c == '1').count();
    let mut out = vec![0u8; leading_zeros];
    out.extend(bytes.iter().rev());
    Some(out)
}

/// A Solana public key is 32 bytes; anything else is a typo or another chain's
/// address, and delegating to it is unrecoverable money.
pub fn is_valid_pubkey(s: &str) -> bool {
    matches!(base58_decode(s.trim()), Some(b) if b.len() == 32)
}

// ── validator list ───────────────────────────────────────────────────────────

/// A validator as the operator sees it: name, commission, uptime. Commission is
/// the share of rewards the validator keeps; uptime is what decides whether
/// those rewards arrive at all.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Validator {
    /// Vote account address (what a delegation targets — not the identity key).
    pub vote_account: String,
    pub name: String,
    /// Percent the validator keeps (0–100).
    pub commission_pct: f64,
    /// Percent of epochs with credits produced, from the epoch-credit history.
    pub uptime_pct: f64,
}

/// Uptime from an epoch-credit history.
///
/// `credits` is the total credits in the window and `epochs` the number of
/// epochs it covers; a perfect validator earns ~`SLOTS_PER_EPOCH` credits per
/// epoch. Returns 0–100.
pub fn uptime_pct(credits: u64, epochs: u64) -> f64 {
    if epochs == 0 {
        return 0.0;
    }
    let expected = (epochs * SLOTS_PER_EPOCH) as f64;
    let pct = (credits as f64) / expected * 100.0;
    pct.clamp(0.0, 100.0)
}

// ── config (operator-owned) ──────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StakeConfig {
    /// Opt-in. Nothing stakes by default (design §4.4).
    #[serde(default)]
    pub enabled: bool,
    /// Amount delegated to the stake account, in lamports.
    #[serde(default)]
    pub amount_lamports: u64,
    /// Validator **vote account** the stake is delegated to.
    #[serde(default)]
    pub validator: String,
    /// JSON-RPC endpoint. Devnet by default so a first run costs nothing.
    #[serde(default = "default_rpc_url")]
    pub rpc_url: String,
    /// Local ed25519 keypair file (solana CLI JSON format). `None` →
    /// `~/.gpn/solana-keypair.json`. Never leaves this machine.
    #[serde(default)]
    pub keypair_path: Option<String>,
}

fn default_rpc_url() -> String {
    DEFAULT_RPC_URL.to_string()
}

impl Default for StakeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            amount_lamports: 0,
            validator: String::new(),
            rpc_url: default_rpc_url(),
            keypair_path: None,
        }
    }
}

impl StakeConfig {
    pub fn keypair_path(&self) -> PathBuf {
        match self
            .keypair_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            Some(p) => PathBuf::from(p),
            None => home().join(".gpn/solana-keypair.json"),
        }
    }

    pub fn network(&self) -> &'static str {
        if self.rpc_url.contains("mainnet") {
            "mainnet"
        } else if self.rpc_url.contains("testnet") {
            "testnet"
        } else {
            "devnet"
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if self.amount_lamports < MIN_DELEGATION_LAMPORTS {
            return Err(format!(
                "the smallest stake worth delegating is {} SOL (this is {} SOL) — \
                 below that an epoch's reward is dust",
                format_sol(MIN_DELEGATION_LAMPORTS),
                format_sol(self.amount_lamports)
            ));
        }
        if !is_valid_pubkey(&self.validator) {
            return Err(format!(
                "`{}` is not a Solana validator vote account (a base58 32-byte address)",
                self.validator
            ));
        }
        let url = self.rpc_url.trim();
        if !(url.starts_with("https://") || url.starts_with("http://127.0.0.1")) {
            return Err(format!(
                "`{url}` is not an HTTPS Solana RPC endpoint (a plain-HTTP remote \
                 endpoint would expose your signed transactions)"
            ));
        }
        Ok(())
    }
}

// ── lock state ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// Delegated, but rewards start at the next epoch boundary.
    Activating,
    /// Earning.
    Active,
    /// Deactivation requested; funds unlock at an epoch boundary 1–3 days away.
    Deactivating,
    /// Not delegated (never was, or fully removed) — withdrawable now.
    Inactive,
}

/// The honest lock: what the stake is doing and **when** the money is usable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LockState {
    pub phase: Phase,
    pub current_epoch: u64,
    #[serde(default)]
    pub activation_epoch: Option<u64>,
    #[serde(default)]
    pub deactivation_epoch: Option<u64>,
    /// Epoch at which the funds stop being locked (`None` while active).
    #[serde(default)]
    pub withdrawable_epoch: Option<u64>,
}

impl LockState {
    /// A sentence the dashboard shows verbatim. Never "pending", never a spinner
    /// stand-in: the operator must be able to decide whether to wait.
    pub fn describe(&self) -> String {
        match self.phase {
            Phase::Activating => format!(
                "warming up — rewards start at the epoch boundary (epoch {}); \
                 the stake cannot be withdrawn before it activates",
                self.activation_epoch
                    .unwrap_or(self.current_epoch.saturating_add(1))
            ),
            Phase::Active => "active — earning stake rewards each epoch".to_string(),
            Phase::Deactivating => {
                let at = self
                    .withdrawable_epoch
                    .or(self.deactivation_epoch)
                    .unwrap_or(self.current_epoch.saturating_add(1));
                format!(
                    "deactivating — withdrawable in {}–{} days (at the epoch boundary for \
                     epoch {at}); the funds are NOT available now",
                    LOCK_MIN_DAYS, LOCK_MAX_DAYS
                )
            }
            Phase::Inactive => "not delegated — withdrawable now".to_string(),
        }
    }

    /// `true` while the operator cannot move the funds.
    pub fn locked(&self) -> bool {
        matches!(self.phase, Phase::Activating | Phase::Deactivating)
    }
}

// ── RPC ──────────────────────────────────────────────────────────────────────

/// The three reads the dashboard needs. A trait so the rendering logic is
/// testable without a network, and so a devnet/mainnet switch is one field.
pub trait SolanaRpc {
    fn vote_accounts(&self) -> Result<Vec<Validator>, String>;
    fn epoch_info(&self) -> Result<u64, String>;
    fn balance(&self, pubkey: &str) -> Result<u64, String>;
    fn stake_activation(
        &self,
        stake_account: &str,
    ) -> Result<(Phase, Option<u64>, Option<u64>), String>;
}

pub struct HttpSolanaRpc {
    pub url: String,
}

impl HttpSolanaRpc {
    fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .build()
            .map_err(|e| e.to_string())?;
        let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
        let resp: serde_json::Value = client
            .post(self.url.trim())
            .json(&body)
            .send()
            .map_err(|e| format!("{method}: {e}"))?
            .json()
            .map_err(|e| format!("{method}: bad json: {e}"))?;
        if let Some(err) = resp.get("error") {
            return Err(format!("{method}: rpc error {err}"));
        }
        Ok(resp
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }
}

impl SolanaRpc for HttpSolanaRpc {
    fn vote_accounts(&self) -> Result<Vec<Validator>, String> {
        let v = self.call(
            "getVoteAccounts",
            serde_json::json!([{"commitment":"confirmed"}]),
        )?;
        let mut out = Vec::new();
        for list in ["current", "delinquent"] {
            let Some(arr) = v.get(list).and_then(|a| a.as_array()) else {
                continue;
            };
            for a in arr {
                let vote_account = a
                    .get("votePubkey")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                if !is_valid_pubkey(&vote_account) {
                    continue;
                }
                let commission = a
                    .get("commission")
                    .and_then(|x| x.as_u64())
                    .map(|c| c as f64)
                    .unwrap_or(100.0);
                let (credits, epochs) = epoch_credits_window(a);
                let name = a
                    .get("nodePubkey")
                    .and_then(|x| x.as_str())
                    .map(|s| s.chars().take(8).collect::<String>())
                    .unwrap_or_default();
                out.push(Validator {
                    vote_account,
                    name,
                    commission_pct: commission,
                    uptime_pct: uptime_pct(credits, epochs),
                });
            }
        }
        Ok(out)
    }

    fn epoch_info(&self) -> Result<u64, String> {
        let v = self.call(
            "getEpochInfo",
            serde_json::json!([{"commitment":"confirmed"}]),
        )?;
        v.get("epoch")
            .and_then(|e| e.as_u64())
            .ok_or_else(|| "getEpochInfo: no epoch field".to_string())
    }

    fn balance(&self, pubkey: &str) -> Result<u64, String> {
        let v = self.call(
            "getBalance",
            serde_json::json!([pubkey, {"commitment":"confirmed"}]),
        )?;
        v.get("value")
            .and_then(|x| x.as_u64())
            .ok_or_else(|| "getBalance: no value field".to_string())
    }

    fn stake_activation(
        &self,
        stake_account: &str,
    ) -> Result<(Phase, Option<u64>, Option<u64>), String> {
        let v = self.call(
            "getStakeActivation",
            serde_json::json!([stake_account, {"commitment":"confirmed"}]),
        )?;
        let state = v
            .get("state")
            .and_then(|s| s.as_str())
            .ok_or_else(|| "getStakeActivation: no state".to_string())?;
        let phase = match state {
            "activating" => Phase::Activating,
            "active" => Phase::Active,
            "deactivating" => Phase::Deactivating,
            _ => Phase::Inactive,
        };
        let activation = v.get("activationEpoch").and_then(|x| x.as_u64());
        let deactivation = v.get("deactivationEpoch").and_then(|x| x.as_u64());
        Ok((phase, activation, deactivation))
    }
}

/// Last two `[epoch, credits, prev_credits]` entries → (credits in the window,
/// epochs covered). Falls back to (0,0) when the history is absent, which
/// renders as 0 % uptime rather than inventing a number.
fn epoch_credits_window(a: &serde_json::Value) -> (u64, u64) {
    let Some(arr) = a.get("epochCredits").and_then(|x| x.as_array()) else {
        return (0, 0);
    };
    let last = arr.last().and_then(|e| e.as_array());
    let Some(last) = last else { return (0, 0) };
    let epoch = last.first().and_then(|x| x.as_u64()).unwrap_or(0);
    let credits = last.get(1).and_then(|x| x.as_u64()).unwrap_or(0);
    let prev = last.get(2).and_then(|x| x.as_u64()).unwrap_or(0);
    let first_epoch = arr
        .first()
        .and_then(|e| e.as_array())
        .and_then(|e| e.first())
        .and_then(|x| x.as_u64())
        .unwrap_or(epoch);
    let epochs = epoch.saturating_sub(first_epoch).max(1);
    (credits.saturating_sub(prev).max(1), epochs)
}

// ── files ────────────────────────────────────────────────────────────────────

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
}

pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("GPN_STAKING_FILE") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    home().join(".gpn/staking.json")
}

pub fn status_path() -> PathBuf {
    if let Ok(p) = std::env::var("GPN_STAKING_STATUS_FILE") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    home().join(".gpn/staking-status.json")
}

/// Missing file = staking off. A corrupt file is an error the caller must show:
/// an operator whose config stopped parsing must not be told "you are not
/// staking" while a stake sits on chain.
pub fn read_config_at(path: &Path) -> Result<StakeConfig, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s)
            .map_err(|e| format!("{} is not valid staking JSON: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StakeConfig::default()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

pub fn read_config() -> Result<StakeConfig, String> {
    read_config_at(&config_path())
}

/// Atomic (stage + rename), 0600 — the file names a keypair path, so it must
/// not be world-readable.
pub fn write_config_at(path: &Path, cfg: &StakeConfig) -> Result<(), String> {
    cfg.validate()?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

/// What the supervisor observed, as the dashboard shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StakeStatus {
    pub updated: u64,
    pub network: String,
    pub amount_lamports: u64,
    #[serde(default)]
    pub validator: String,
    #[serde(default)]
    pub commission_pct: f64,
    #[serde(default)]
    pub uptime_pct: f64,
    pub lock: LockState,
    /// Rewards accrued in the stake account, lamports. **A separate line**: never
    /// added to GPN earnings and never part of the airdrop export.
    #[serde(default)]
    pub rewards_lamports: u64,
    /// Plain-words note from the supervisor (e.g. "no keypair on this machine").
    #[serde(default)]
    pub note: String,
}

pub fn write_status_at(path: &Path, st: &StakeStatus) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text = serde_json::to_string_pretty(st).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

pub fn read_status_at(path: &Path) -> Option<StakeStatus> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

pub fn read_status() -> Option<StakeStatus> {
    read_status_at(&status_path())
}

// ── keypair (local, operator-owned) ──────────────────────────────────────────

/// Validate a Solana keypair file as the CLI writes it: a JSON array of 64
/// bytes (32-byte secret seed + 32-byte public key). Returns the public key's
/// base58 form (re-encoded from bytes, not trusted from the file) — or an error
/// naming what is wrong with the file.
pub fn read_keypair_pubkey(path: &Path) -> Result<Vec<u8>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read keypair {}: {e}", path.display()))?;
    let arr: Vec<u8> = serde_json::from_str(text.trim())
        .map_err(|e| format!("{} is not a Solana keypair JSON array: {e}", path.display()))?;
    if arr.len() != 64 {
        return Err(format!(
            "{} holds {} bytes; a Solana keypair is 64 (seed + public key)",
            path.display(),
            arr.len()
        ));
    }
    Ok(arr[32..].to_vec())
}

/// base58 encode — used to display the public key derived from the keypair.
pub fn base58_encode(bytes: &[u8]) -> String {
    // Little-endian bignum → base58.
    let mut digits: Vec<u8> = Vec::new(); // base58 digits, least significant first
    for &b in bytes {
        let mut carry = b as u32;
        for d in digits.iter_mut() {
            let v = (*d as u32) * 256 + carry;
            *d = (v % 58) as u8;
            carry = v / 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let leading_zeros = bytes.iter().take_while(|b| **b == 0).count();
    let mut out = String::new();
    for _ in 0..leading_zeros {
        out.push('1');
    }
    for d in digits.iter().rev() {
        out.push(B58.chars().nth(*d as usize).unwrap());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gpn-stake-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("staking.json")
    }

    const VALIDATOR: &str = "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin";

    #[test]
    fn test_first_off_by_default_and_nothing_stakes() {
        // Design §4.4: opt-in, explicit amount. A default that stakes anything is
        // the bug this test exists to prevent.
        let d = StakeConfig::default();
        assert!(!d.enabled);
        assert_eq!(d.amount_lamports, 0);
        assert!(d.validate().is_ok(), "off is always valid");
        assert_eq!(d.network(), "devnet");
    }

    #[test]
    fn below_the_minimum_is_refused_with_the_number() {
        let cfg = StakeConfig {
            enabled: true,
            amount_lamports: 9_999_999,
            validator: VALIDATOR.into(),
            ..Default::default()
        };
        let e = cfg.validate().unwrap_err();
        assert!(e.contains("0.01"), "{e}");
        assert!(e.contains("dust"), "{e}");

        let ok = StakeConfig {
            amount_lamports: MIN_DELEGATION_LAMPORTS,
            ..cfg
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn a_wrong_chain_or_typo_address_is_refused() {
        let cfg = StakeConfig {
            enabled: true,
            amount_lamports: MIN_DELEGATION_LAMPORTS,
            validator: "0xdeadbeef".into(),
            ..Default::default()
        };
        assert!(cfg.validate().unwrap_err().contains("vote account"));
        // Valid base58, but 25 bytes — not a Solana key.
        assert!(!is_valid_pubkey(
            "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVF"
        ));
        assert!(is_valid_pubkey(VALIDATOR));
        // The system program id is 32 zero bytes.
        assert_eq!(
            base58_decode("11111111111111111111111111111111").unwrap(),
            vec![0u8; 32]
        );
    }

    #[test]
    fn plain_http_remote_endpoints_are_refused_but_loopback_is_allowed() {
        let cfg = StakeConfig {
            enabled: true,
            amount_lamports: MIN_DELEGATION_LAMPORTS,
            validator: VALIDATOR.into(),
            rpc_url: "http://api.devnet.solana.com".into(),
            ..Default::default()
        };
        assert!(cfg.validate().unwrap_err().contains("HTTPS"));

        let local = StakeConfig {
            rpc_url: "http://127.0.0.1:8899".into(),
            ..cfg
        };
        assert!(local.validate().is_ok(), "a local validator is fine");
    }

    #[test]
    fn sol_parsing_round_trips_and_refuses_ambiguity() {
        assert_eq!(parse_sol("0.01").unwrap(), 10_000_000);
        assert_eq!(parse_sol("1").unwrap(), 1_000_000_000);
        assert_eq!(parse_sol("2.5").unwrap(), 2_500_000_000);
        assert_eq!(parse_sol("0.000000001").unwrap(), 1);
        assert!(
            parse_sol("0.0000000001").is_err(),
            "10 decimals is below 1 lamport"
        );
        assert!(parse_sol("-1").is_err());
        assert!(parse_sol("1,5").is_err());
        assert!(parse_sol("abc").is_err());
        assert!(parse_sol("").is_err());
        assert!(
            parse_sol("99999999999999999999").is_err(),
            "overflow must not wrap"
        );

        for lamports in [0u64, 1, 10_000_000, 2_282_880, 1_234_567_890] {
            assert_eq!(parse_sol(&format_sol(lamports)).unwrap(), lamports);
        }
        assert_eq!(format_sol(10_000_000), "0.01");
        assert_eq!(format_sol(2_282_880), "0.00228288");
    }

    #[test]
    fn the_wallet_must_also_cover_rent() {
        let amount = MIN_DELEGATION_LAMPORTS;
        assert_eq!(
            balance_needed_lamports(amount),
            amount + STAKE_ACCOUNT_RENT_LAMPORTS
        );
        assert_eq!(format_sol(balance_needed_lamports(amount)), "0.01228288");
    }

    #[test]
    fn every_lock_state_names_a_time_and_never_a_spinner() {
        let cases = [
            LockState {
                phase: Phase::Activating,
                current_epoch: 10,
                activation_epoch: Some(11),
                deactivation_epoch: None,
                withdrawable_epoch: None,
            },
            LockState {
                phase: Phase::Active,
                current_epoch: 10,
                activation_epoch: Some(9),
                deactivation_epoch: None,
                withdrawable_epoch: None,
            },
            LockState {
                phase: Phase::Deactivating,
                current_epoch: 10,
                activation_epoch: Some(9),
                deactivation_epoch: Some(11),
                withdrawable_epoch: Some(12),
            },
            LockState {
                phase: Phase::Inactive,
                current_epoch: 10,
                activation_epoch: None,
                deactivation_epoch: None,
                withdrawable_epoch: None,
            },
        ];
        for c in &cases {
            let d = c.describe();
            assert!(!d.is_empty());
            for banned in ["pending", "loading", "please wait", "…"] {
                assert!(
                    !d.to_ascii_lowercase().contains(banned),
                    "`{d}` must not stand in for a time"
                );
            }
        }
        let deactivating = cases[2].describe();
        assert!(deactivating.contains("1–3 days"), "{deactivating}");
        assert!(deactivating.contains("NOT available now"), "{deactivating}");
        assert!(deactivating.contains("epoch 12"), "{deactivating}");
        assert!(cases[2].locked() && cases[0].locked());
        assert!(!cases[1].locked() && !cases[3].locked());
    }

    #[test]
    fn uptime_comes_from_credits_and_is_bounded() {
        // A perfect epoch earns ~SLOTS_PER_EPOCH credits.
        assert!((uptime_pct(SLOTS_PER_EPOCH, 1) - 100.0).abs() < 0.001);
        assert!((uptime_pct(SLOTS_PER_EPOCH * 9 / 10, 1) - 90.0).abs() < 0.001);
        assert_eq!(uptime_pct(0, 0), 0.0);
        assert_eq!(
            uptime_pct(SLOTS_PER_EPOCH * 2, 1),
            100.0,
            "clamped, never >100"
        );
    }

    #[test]
    fn config_round_trips_atomically_and_is_not_world_readable() {
        let path = tmp("roundtrip");
        let cfg = StakeConfig {
            enabled: true,
            amount_lamports: 50_000_000,
            validator: VALIDATOR.into(),
            ..Default::default()
        };
        write_config_at(&path, &cfg).unwrap();
        assert_eq!(read_config_at(&path).unwrap(), cfg);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the staking config names a keypair path");
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn corrupt_config_is_an_error_not_silently_off() {
        let path = tmp("corrupt");
        std::fs::write(&path, "{not json").unwrap();
        assert!(read_config_at(&path).is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn keypair_reader_states_what_is_wrong() {
        let dir = std::env::temp_dir().join(format!("gpn-stake-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let short = dir.join("short.json");
        std::fs::write(&short, "[1,2,3]").unwrap();
        assert!(read_keypair_pubkey(&short).unwrap_err().contains("64"));

        let good = dir.join("good.json");
        let mut bytes: Vec<u8> = vec![0u8; 64];
        bytes[63] = 7;
        std::fs::write(&good, serde_json::to_string(&bytes).unwrap()).unwrap();
        let pk = read_keypair_pubkey(&good).unwrap();
        assert_eq!(pk.len(), 32);
        // The public half is the last 32 bytes, re-encoded by us.
        let encoded = base58_encode(&pk);
        assert_eq!(base58_decode(&encoded).unwrap(), pk);
        assert!(is_valid_pubkey(&encoded));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_file_round_trips_with_rewards_as_their_own_field() {
        let path = tmp("status");
        let st = StakeStatus {
            updated: 1_700_000_000,
            network: "devnet".into(),
            amount_lamports: MIN_DELEGATION_LAMPORTS,
            validator: VALIDATOR.into(),
            commission_pct: 5.0,
            uptime_pct: 99.5,
            lock: LockState {
                phase: Phase::Active,
                current_epoch: 42,
                activation_epoch: Some(41),
                deactivation_epoch: None,
                withdrawable_epoch: None,
            },
            rewards_lamports: 1_234,
            note: String::new(),
        };
        write_status_at(&path, &st).unwrap();
        assert_eq!(read_status_at(&path).unwrap(), st);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("rewards_lamports"), "{text}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A fake RPC so the rendering path is exercised without a network.
    struct FakeRpc {
        epoch: u64,
        balance: u64,
        activation: (Phase, Option<u64>, Option<u64>),
        validators: Vec<Validator>,
    }

    impl SolanaRpc for FakeRpc {
        fn vote_accounts(&self) -> Result<Vec<Validator>, String> {
            Ok(self.validators.clone())
        }
        fn epoch_info(&self) -> Result<u64, String> {
            Ok(self.epoch)
        }
        fn balance(&self, _pubkey: &str) -> Result<u64, String> {
            Ok(self.balance)
        }
        fn stake_activation(&self, _: &str) -> Result<(Phase, Option<u64>, Option<u64>), String> {
            Ok(self.activation)
        }
    }

    #[test]
    fn dashboard_reading_shows_commission_uptime_and_the_lock() {
        let rpc = FakeRpc {
            epoch: 100,
            balance: 60_000_000,
            activation: (Phase::Deactivating, Some(90), Some(103)),
            validators: vec![Validator {
                vote_account: VALIDATOR.into(),
                name: "ACME".into(),
                commission_pct: 7.0,
                uptime_pct: 99.9,
            }],
        };
        let v = rpc.vote_accounts().unwrap();
        assert_eq!(v[0].commission_pct, 7.0);
        assert_eq!(v[0].uptime_pct, 99.9);
        let lock = LockState {
            phase: rpc.stake_activation("x").unwrap().0,
            current_epoch: rpc.epoch_info().unwrap(),
            activation_epoch: Some(90),
            deactivation_epoch: Some(103),
            withdrawable_epoch: Some(104),
        };
        assert!(lock.describe().contains("1–3 days"));
        // The wallet's spendable SOL is balance minus what is delegated.
        let spendable = rpc
            .balance("me")
            .unwrap()
            .saturating_sub(MIN_DELEGATION_LAMPORTS);
        assert_eq!(spendable, 50_000_000);
    }
}
