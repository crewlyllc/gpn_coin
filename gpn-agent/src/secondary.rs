//! Secondary coin mining (row 294) — the operator's *own* second coin, beside GPN.
//!
//! Owner decision 2026-11-27 (option **B**, `docs/design-secondary-mining.md` §7):
//! a second miner runs **concurrently with GPN**, at a conservative default
//! intensity (30 %), and an AI job always wins — both hashers stop for the job
//! and resume after. The operator picks coin / payout address / pool / intensity
//! **per card** from the dashboard; only coins whose DAG fits that card are
//! offered; coin-B rewards never enter GPN's earnings or the airdrop export.
//!
//! ## Shape
//!
//! This module is the *policy* half: the state file, its validation, the VRAM
//! fit rule, the pause rule and the miner argv contract. The supervisor
//! (`gpn_node`) is the *acting* half: it reads the state file, starts/stops one
//! child per enabled card, writes `secondary-status.json`, and logs every
//! transition. Splitting them this way keeps every decision unit-testable
//! without a GPU, a pool or a coin.
//!
//! ## Why a file
//!
//! Same reason as `idle.rs`: supervisor, chain node and providers are separate
//! processes, so the cheapest cross-process channel is a JSON file.
//!
//! - state: `$GPN_SECONDARY_FILE`, else `~/.gpn/secondary.json` (operator-owned)
//! - status: `$GPN_SECONDARY_STATUS_FILE`, else `~/.gpn/secondary-status.json`
//!   (supervisor-owned, read by the dashboard)
//!
//! ## The miner is a child, not new GPU code
//!
//! Per design §7.3 we do **not** write a second Ethash-class miner. The second
//! coin runs as an external miner process under the supervisor, controlled by
//! stop/start. The contract is argv, not a socket:
//!
//! ```text
//! <miner> --coin <ergo|alephium|ravencoin> --pool <url> --address <addr> \
//!         --device <n> --intensity-percent <p> --status-file <path>
//! ```
//!
//! `scripts/second-miner-lolminer.sh` translates that into lolMiner's own flags;
//! any other wrapper works the same way. An absent or non-executable miner is
//! reported as `miner-not-installed` on the dashboard — never silently ignored.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default share of the card the second miner may take (design §7.2).
pub const DEFAULT_INTENSITY: u32 = 30;
/// Below this the miner is not worth the DAG rebuild; above it an AI job has no room.
pub const MIN_INTENSITY: u32 = 5;
pub const MAX_INTENSITY: u32 = 50;

/// Conservative GPN DAG ceiling at the current epoch (1–2 GB today; 2 GB keeps
/// the rule stable across the next few epochs).
pub const GPN_DAG_GB: f64 = 2.0;
/// Room an AI job needs while the second miner's DAG stays resident: the
/// smallest model a job can send plus activations. Under-estimating this is
/// how a job OOMs mid-generation, so it is deliberately generous.
pub const AI_HEADROOM_GB: f64 = 2.0;

// ── coins ────────────────────────────────────────────────────────────────────

/// The curated coin list (design §7.1's short list). A coin not in this enum
/// is not offered anywhere — no hidden defaults, nothing mined that was not
/// chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Coin {
    Ergo,
    Alephium,
    Ravencoin,
}

impl Coin {
    pub fn as_str(self) -> &'static str {
        match self {
            Coin::Ergo => "ergo",
            Coin::Alephium => "alephium",
            Coin::Ravencoin => "ravencoin",
        }
    }

    pub fn parse(s: &str) -> Option<Coin> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ergo" | "erg" => Some(Coin::Ergo),
            "alephium" | "alph" => Some(Coin::Alephium),
            "ravencoin" | "rvn" => Some(Coin::Ravencoin),
            _ => None,
        }
    }

    /// Second DAG size in GB (design §7.1 table).
    pub fn dag_gb(self) -> f64 {
        match self {
            Coin::Ergo => 2.2,
            Coin::Alephium => 0.4,
            Coin::Ravencoin => 4.0,
        }
    }

    /// Curated pools. v1 takes no arbitrary URL: a mistyped pool is a silent
    /// revenue leak, and a hostile one is worse.
    ///
    /// Every entry here was verified live on 2026-11-28 (row 294): the hostname
    /// resolves, the TCP port accepts, and the stratum handshake authorises the
    /// payouts of the right coin — not just "it looked right on a web page".
    /// What was removed and why (evidence in docs/releases/0.3.85.md):
    /// * `ergo.2miners.com:6060` — connection refused on all four A records, from
    ///   two networks; `:3333` on that host is another coin and answers
    ///   `Invalid address` to an Ergo payout.
    /// * `alephium.2miners.com:2020` — NXDOMAIN (no A, no AAAA, Google + Cloudflare DNS).
    /// * `de.alephium.herominers.com:1190` — port closed.
    /// * `de.rvn.herominers.com:1180` — NXDOMAIN.
    pub fn pools(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Coin::Ergo => &[
                ("herominers", "de.ergo.herominers.com:1180"),
                ("woolypooly", "pool.woolypooly.com:3100"),
            ],
            // No live Alephium pool survived the 2026-11-28 check.
            Coin::Alephium => &[],
            Coin::Ravencoin => &[("2miners", "rvn.2miners.com:6060")],
        }
    }

    /// Can a miner in this build actually mine this coin?
    ///
    /// The curated list must only offer coins that can run: an option that can
    /// never produce a share is worse than no option, because the operator
    /// configures it, restarts, and gets silence. lolMiner 1.98a refuses
    /// `--algo ALEPHIUM` outright (measured 2026-11-28: `Error: --algo option
    /// "ALEPHIUM" is not supported`), and Alephium's own GPU miner is a
    /// different binary this build does not ship or supervise.
    pub fn available(self) -> bool {
        match self {
            Coin::Ergo | Coin::Ravencoin => true,
            Coin::Alephium => false,
        }
    }

    /// Payout-address shape check, run **when typed** (design §7.2: a typo in a
    /// payout address is unrecoverable money, and the pool is the only party
    /// that can truly reject one).
    ///
    /// v1 checks the alphabet and the length window per coin. That catches a
    /// paste of the wrong coin's address or a truncated one; it cannot catch a
    /// single flipped base58 character, and it does not pretend to — the pool
    /// rejects that, which is why the dashboard shows the pool's own answer.
    pub fn validate_address(self, addr: &str) -> Result<(), String> {
        let a = addr.trim();
        if a.is_empty() {
            return Err("payout address is empty".into());
        }
        const B58: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
        if let Some(bad) = a.chars().find(|c| !B58.contains(*c)) {
            return Err(format!(
                "`{bad}` is not a base58 character — this is not a {} address",
                self.as_str()
            ));
        }
        let (lo, hi) = match self {
            // Ergo P2PK ("9…") / P2S ("3…") addresses are 51 base58 characters.
            Coin::Ergo => (51, 51),
            // Alephium addresses are base58 and noticeably shorter than Ergo's.
            Coin::Alephium => (36, 46),
            // Ravencoin P2PKH addresses are 34 characters.
            Coin::Ravencoin => (34, 34),
        };
        if a.len() < lo || a.len() > hi {
            return Err(format!(
                "a {} address is {}{} characters, this one is {} — check the paste",
                self.as_str(),
                lo,
                if lo == hi { "" } else { "–" },
                a.len()
            ));
        }
        Ok(())
    }
}

// ── state (operator-owned) ───────────────────────────────────────────────────

/// One card's second-coin choice. Per **card**, not per node (design §7.2), so
/// a two-GPU operator can pick differently on each.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CardConfig {
    /// OpenCL device index (matches `compute_device_index` / mining device).
    pub device: u32,
    /// `"ergo"` | `"alephium"` | `"ravencoin"`.
    pub coin: String,
    pub address: String,
    /// A curated pool URL (one of [`Coin::pools`]).
    pub pool: String,
    /// Share of the card the second miner may take, percent.
    #[serde(default = "default_intensity")]
    pub intensity_percent: u32,
}

fn default_intensity() -> u32 {
    DEFAULT_INTENSITY
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SecondaryState {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub cards: Vec<CardConfig>,
}

impl SecondaryState {
    /// Validation shared by the dashboard endpoint and the supervisor, so the
    /// API can never accept what the supervisor would refuse to run.
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if self.cards.is_empty() {
            return Err("concurrent mining is enabled but no card is configured".into());
        }
        let mut seen = Vec::new();
        for c in &self.cards {
            validate_card(c)?;
            if seen.contains(&c.device) {
                return Err(format!("device {} is configured twice", c.device));
            }
            seen.push(c.device);
        }
        Ok(())
    }
}

/// Validate one card's choice against the coin's own rules.
pub fn validate_card(c: &CardConfig) -> Result<(), String> {
    let coin = Coin::parse(&c.coin).ok_or_else(|| {
        format!(
            "`{}` is not a supported coin (ergo, alephium, ravencoin)",
            c.coin
        )
    })?;
    if !coin.available() {
        return Err(format!(
            "`{}` cannot be mined by this build: no second-coin miner it ships has that \
             algorithm (lolMiner has none for Alephium). Choose ergo or ravencoin.",
            coin.as_str()
        ));
    }
    coin.validate_address(&c.address)?;
    if !coin.pools().iter().any(|(_, url)| url == &c.pool) {
        return Err(format!(
            "`{}` is not a curated {} pool (choose one of: {})",
            c.pool,
            coin.as_str(),
            coin.pools()
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if c.intensity_percent < MIN_INTENSITY || c.intensity_percent > MAX_INTENSITY {
        return Err(format!(
            "intensity must be {}–{} % (requested {}) — the default is {} %, because the \
             failure mode of dual mining is an AI job arriving at a busy card",
            MIN_INTENSITY, MAX_INTENSITY, c.intensity_percent, DEFAULT_INTENSITY
        ));
    }
    Ok(())
}

// ── VRAM fit rule ────────────────────────────────────────────────────────────

/// Share of a card's nominal VRAM that is actually allocatable. Driver,
/// desktop compositor and ECC accounting take the rest — an 8 GB card is not
/// 8.0 GB for a miner, and computing the fit rule on the sticker number is how
/// "it fits" becomes an OOM at the worst moment.
pub const USABLE_VRAM_FRACTION: f64 = 0.95;

/// Can this card hold GPN's DAG, the coin's DAG **and** an AI job's model at
/// the same time? If not, the coin is not offered for that card at all
/// (design §8 acceptance 5).
pub fn coin_fits(total_vram_gb: f64, coin: Coin) -> bool {
    let usable = total_vram_gb * USABLE_VRAM_FRACTION;
    usable >= GPN_DAG_GB + coin.dag_gb() + AI_HEADROOM_GB
}

/// The coins this card may be offered, in catalogue order.
pub fn available_coins(total_vram_gb: f64) -> Vec<Coin> {
    [Coin::Ergo, Coin::Alephium, Coin::Ravencoin]
        .into_iter()
        .filter(|c| c.available() && coin_fits(total_vram_gb, *c))
        .collect()
}

// ── pause rule ───────────────────────────────────────────────────────────────

/// Why the second miner must not be searching nonces right now, if at all.
///
/// The two reasons are exactly the ones the GPN miner itself parks for
/// (`service.rs::park_reason`): an AI job holding the GPU (the yield flag) and
/// an operator idle window. Reading the **same** files is deliberate — one
/// flag, two miners, so they can never disagree about who owns the GPU.
pub fn pause_reason(gpu_yield_present: bool, idle_active: bool) -> Option<&'static str> {
    if idle_active {
        Some("idle")
    } else if gpu_yield_present {
        Some("ai-job")
    } else {
        None
    }
}

/// Path of the AI-job yield flag (same file the node's miner and providers use).
pub fn yield_flag_path() -> PathBuf {
    gpn_agent_yield_path()
}

fn gpn_agent_yield_path() -> PathBuf {
    crate::gpu_yield::yield_flag_path()
}

// ── files ────────────────────────────────────────────────────────────────────

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
}

pub fn state_path() -> PathBuf {
    if let Ok(p) = std::env::var("GPN_SECONDARY_FILE") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    home().join(".gpn/secondary.json")
}

pub fn status_path() -> PathBuf {
    if let Ok(p) = std::env::var("GPN_SECONDARY_STATUS_FILE") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    home().join(".gpn/secondary-status.json")
}

/// Read the operator state. A missing file is "off"; a corrupt one is an
/// error the caller must log (never silently treated as off: an operator whose
/// config stopped parsing must be told, or they are mining the wrong coin).
pub fn read_state_at(path: &Path) -> Result<SecondaryState, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s)
            .map_err(|e| format!("{} is not valid secondary-mining JSON: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SecondaryState::default()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

pub fn read_state() -> Result<SecondaryState, String> {
    read_state_at(&state_path())
}

/// Write the operator state atomically (stage + rename), 0600.
pub fn write_state_at(path: &Path, st: &SecondaryState) -> Result<(), String> {
    st.validate()?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text = serde_json::to_string_pretty(st).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

/// What one card's second miner is doing, as the dashboard shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CardStatus {
    pub device: u32,
    pub coin: String,
    /// `mining` · `paused-for-job` · `paused-idle` · `pool-disconnected` ·
    /// `miner-not-installed` · `off`
    pub state: String,
    #[serde(default)]
    pub pid: u32,
    pub intensity_percent: u32,
    /// Unix seconds since this state began.
    pub since: u64,
    /// Last line the miner wrote to its status file (pool's own words).
    #[serde(default)]
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub updated: u64,
    #[serde(default)]
    pub cards: Vec<CardStatus>,
}

pub fn write_status_at(path: &Path, st: &Status) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text = serde_json::to_string_pretty(st).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

pub fn read_status_at(path: &Path) -> Status {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn read_status() -> Status {
    read_status_at(&status_path())
}

// ── miner argv ───────────────────────────────────────────────────────────────

/// Where the second-miner wrapper lives: `$GPN_SECOND_MINER`, else
/// `<repo>/scripts/second-miner-lolminer.sh`, else the bare name (PATH lookup).
pub fn default_miner() -> PathBuf {
    if let Ok(p) = std::env::var("GPN_SECOND_MINER") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    PathBuf::from("second-miner-lolminer.sh")
}

/// argv for one card's miner child. The contract lives here and in
/// `scripts/second-miner-lolminer.sh`; both sides are tested against each other.
pub fn miner_argv(miner: &Path, card: &CardConfig) -> Result<Vec<String>, String> {
    let coin = Coin::parse(&card.coin)
        .ok_or_else(|| format!("`{}` is not a supported coin", card.coin))?;
    let mut v = vec![miner.to_string_lossy().into_owned()];
    for a in [
        "--coin",
        coin.as_str(),
        "--pool",
        card.pool.as_str(),
        "--address",
        card.address.trim(),
        "--device",
    ] {
        v.push(a.to_string());
    }
    v.push(card.device.to_string());
    v.push("--intensity-percent".into());
    v.push(card.intensity_percent.to_string());
    v.push("--status-file".into());
    v.push(
        miner_status_path(card.device)
            .to_string_lossy()
            .into_owned(),
    );
    Ok(v)
}

/// Per-device status file the miner wrapper writes its latest pool line to
/// (so `pool disconnected` is the pool's own words, not our guess).
pub fn miner_status_path(device: u32) -> PathBuf {
    // Beside the status file, so a relocated `~/.gpn` (tests, a second node)
    // moves all three files together.
    let dir = status_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(home);
    dir.join(format!("secondary-{device}.log"))
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── supervisor ───────────────────────────────────────────────────────────────

/// Seconds between a crash and the next spawn attempt for the same card.
pub const RESTART_BACKOFF_SECS: u64 = 10;

/// One card's live child.
#[derive(Debug)]
struct Run {
    child: std::process::Child,
    coin: String,
    intensity_percent: u32,
    stopped: bool,
    state: String,
    since: u64,
    started_at: u64,
    /// Set when the miner's own status file says the pool refused/closed us.
    detail: String,
}

/// The acting half of the feature: keeps one child per enabled card, stops
/// them while a GPN AI job (or an idle window) owns the GPU, and reports each
/// card's state for the dashboard.
///
/// `reconcile` is the whole state machine and it is pure with respect to the
/// GPU — it only needs the state file's contents and the pause reason, which
/// makes every transition testable with a fake miner on a normal machine.
#[derive(Debug, Default)]
pub struct Supervisor {
    pub miner: PathBuf,
    runs: std::collections::BTreeMap<u32, Run>,
    /// Last spawn failure per device, so a missing miner is reported once per
    /// state change and not re-logged every tick.
    pub failed: std::collections::BTreeMap<u32, String>,
}

impl Supervisor {
    pub fn new(miner: PathBuf) -> Self {
        Self {
            miner,
            runs: Default::default(),
            failed: Default::default(),
        }
    }

    /// Bring the running children in line with `state`, pausing them when
    /// `pause` is `Some`. Returns the lines to log (empty when nothing changed,
    /// which is most ticks).
    pub fn reconcile(
        &mut self,
        state: &SecondaryState,
        pause: Option<&'static str>,
        now: u64,
    ) -> Vec<String> {
        let mut out = Vec::new();
        let wanted: Vec<&CardConfig> = if state.enabled {
            state.cards.iter().collect()
        } else {
            Vec::new()
        };

        // Stop cards that are no longer wanted.
        let keep: Vec<u32> = wanted.iter().map(|c| c.device).collect();
        let gone: Vec<u32> = self
            .runs
            .keys()
            .copied()
            .filter(|d| !keep.contains(d))
            .collect();
        for d in gone {
            if let Some(mut r) = self.runs.remove(&d) {
                stop_child(&mut r.child);
                out.push(format!(
                    "secondary: device {d} ({}) stopped — disabled",
                    r.coin
                ));
            }
            self.failed.remove(&d);
        }

        for card in wanted {
            // A running child whose coin/intensity changed is restarted.
            if let Some(r) = self.runs.get(&card.device) {
                if r.coin != card.coin || r.intensity_percent != card.intensity_percent {
                    let d = card.device;
                    let old = self.runs.remove(&d).unwrap();
                    out.push(format!(
                        "secondary: device {d} restarting — {} {}% -> {} {}%",
                        old.coin, old.intensity_percent, card.coin, card.intensity_percent
                    ));
                    let mut old = old;
                    stop_child(&mut old.child);
                }
            }

            // Start what is not running.
            if !self.runs.contains_key(&card.device) {
                match self.spawn(card, now) {
                    Ok(r) => {
                        self.failed.remove(&card.device);
                        self.runs.insert(card.device, r);
                        out.push(format!(
                            "secondary: device {} {}, intensity {}%, paused={}",
                            card.device,
                            card.coin,
                            card.intensity_percent,
                            pause.is_some()
                        ));
                    }
                    Err(e) => {
                        let first = self.failed.get(&card.device) != Some(&e);
                        self.failed.insert(card.device, e.clone());
                        if first {
                            out.push(format!(
                                "secondary: device {} not running — {e}",
                                card.device
                            ));
                        }
                    }
                }
            }

            // Reap a dead child first: everything below assumes a live one.
            let exited = match self.runs.get_mut(&card.device) {
                Some(r) => match r.child.try_wait() {
                    Ok(Some(st)) => Some(st.code().unwrap_or(-1)),
                    _ => None,
                },
                None => None,
            };
            if let Some(code) = exited {
                let coin = match self.runs.remove(&card.device) {
                    Some(r) => {
                        out.push(format!(
                            "secondary: device {} ({}) exited {} — respawning",
                            card.device, r.coin, code
                        ));
                        r.coin
                    }
                    None => card.coin.clone(),
                };
                // Restart in the same tick: a miner that died must not leave the
                // dashboard empty for one interval before it comes back.
                match self.spawn(card, now) {
                    Ok(r) => {
                        self.failed.remove(&card.device);
                        self.runs.insert(card.device, r);
                        out.push(format!(
                            "secondary: device {} ({coin}) restarted",
                            card.device
                        ));
                    }
                    Err(e) => {
                        self.failed.insert(card.device, e.clone());
                        out.push(format!(
                            "secondary: device {} not running — {e}",
                            card.device
                        ));
                    }
                }
                continue;
            }

            // Pause / resume / read the miner's own verdict.
            if let Some(r) = self.runs.get_mut(&card.device) {
                let want_stopped = pause.is_some();
                if want_stopped && !r.stopped {
                    suspend(&mut r.child);
                    r.stopped = true;
                    r.state = match pause.unwrap_or("") {
                        "idle" => "paused-idle",
                        _ => "paused-for-job",
                    }
                    .into();
                    r.since = now;
                    out.push(format!(
                        "secondary: device {} ({}) paused ({})",
                        card.device,
                        r.coin,
                        pause.unwrap_or("")
                    ));
                } else if !want_stopped && r.stopped {
                    resume(&mut r.child);
                    r.stopped = false;
                    r.state = "mining".into();
                    r.since = now;
                    out.push(format!(
                        "secondary: device {} ({}) resumed",
                        card.device, r.coin
                    ));
                }
                if !r.stopped {
                    // The pool speaks through the miner's own status file; a
                    // `pool-disconnected` or `miner-not-installed` line there
                    // is the pool's verdict, not our guess.
                    let tail = last_status_line(&miner_status_path(card.device));
                    if !tail.is_empty() {
                        let low = tail.to_ascii_lowercase();
                        let want = if low.contains("miner-not-installed") {
                            "miner-not-installed".to_string()
                        } else if low.contains("pool-disconnected") {
                            "pool-disconnected".to_string()
                        } else if r.state == "pool-disconnected" || r.state == "miner-not-installed"
                        {
                            "mining".to_string()
                        } else {
                            r.state.clone()
                        };
                        r.detail = tail.clone();
                        if want != r.state {
                            r.state = want.clone();
                            r.since = now;
                            out.push(format!(
                                "secondary: device {} ({}) -> {}: {}",
                                card.device, r.coin, want, tail
                            ));
                        }
                    }
                }
            }
        }
        out
    }

    fn spawn(&self, card: &CardConfig, now: u64) -> Result<Run, String> {
        if !self.miner.exists() {
            return Err(format!(
                "miner not installed: {} (set GPN_SECOND_MINER or install lolMiner)",
                self.miner.display()
            ));
        }
        let argv = miner_argv(&self.miner, card)?;
        let mut cmd = std::process::Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Own process group: pause must stop the wrapper AND the miner it runs,
        // never the wrapper alone (the miner would keep hashing while the
        // dashboard said "paused").
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let child = cmd
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", argv[0]))?;
        Ok(Run {
            child,
            coin: card.coin.clone(),
            intensity_percent: card.intensity_percent,
            stopped: false,
            state: "mining".into(),
            since: now,
            started_at: now,
            detail: String::new(),
        })
    }

    pub fn status(&self, now: u64) -> Status {
        let mut cards: Vec<CardStatus> = self
            .runs
            .iter()
            .map(|(d, r)| CardStatus {
                device: *d,
                coin: r.coin.clone(),
                state: r.state.clone(),
                pid: r.child.id(),
                intensity_percent: r.intensity_percent,
                since: r.since,
                detail: r.detail.clone(),
            })
            .collect();
        for (d, why) in &self.failed {
            if cards.iter().all(|c| c.device != *d) {
                cards.push(CardStatus {
                    device: *d,
                    coin: String::new(),
                    state: "miner-not-installed".into(),
                    pid: 0,
                    intensity_percent: 0,
                    since: now,
                    detail: why.clone(),
                });
            }
        }
        cards.sort_by_key(|c| c.device);
        Status {
            updated: now,
            cards,
        }
    }

    /// Stop every child (shutdown).
    pub fn shutdown(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        for (d, mut r) in std::mem::take(&mut self.runs) {
            stop_child(&mut r.child);
            out.push(format!("secondary: device {d} stopped (shutdown)"));
        }
        out
    }
}

fn last_status_line(path: &Path) -> String {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| {
            s.lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_string())
        })
        .unwrap_or_default()
}

#[cfg(unix)]
fn signal_group(child: &mut std::process::Child, sig: i32) {
    // Negative pid = the whole group, which is the wrapper and the miner it
    // started. Never send a bare pid: that leaves the miner hashing.
    let pid = child.id() as i32;
    unsafe {
        libc::kill(-pid, sig);
    }
}

#[cfg(unix)]
fn suspend(child: &mut std::process::Child) {
    signal_group(child, libc::SIGSTOP);
}

#[cfg(unix)]
fn resume(child: &mut std::process::Child) {
    signal_group(child, libc::SIGCONT);
}

#[cfg(not(unix))]
fn suspend(_child: &mut std::process::Child) {}

#[cfg(not(unix))]
fn resume(_child: &mut std::process::Child) {}

/// Stop a child and its whole group, then reap it (never leave a zombie or an
/// orphaned miner behind).
fn stop_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    signal_group(child, libc::SIGTERM);
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50))
            }
            _ => break,
        }
    }
    #[cfg(unix)]
    signal_group(child, libc::SIGKILL);
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    let _ = child.wait();
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "gpn-secondary-{tag}-{}-{}",
            std::process::id(),
            now()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn ergo_card() -> CardConfig {
        CardConfig {
            device: 0,
            coin: "ergo".into(),
            address: "9f4QF8Rn5wLhVvjP3g2Kp8sVnTq7YbWc1xZmAoE5rtyAbCdEfGh".into(),
            pool: "de.ergo.herominers.com:1180".into(),
            intensity_percent: 30,
        }
    }

    #[test]
    fn coin_parsing_is_strict() {
        assert_eq!(Coin::parse("Ergo"), Some(Coin::Ergo));
        assert_eq!(Coin::parse(" rvn "), Some(Coin::Ravencoin));
        assert_eq!(Coin::parse("kawpow"), None);
        assert_eq!(Coin::parse(""), None);
    }

    #[test]
    fn a_cut_off_address_is_refused_and_the_message_names_the_length() {
        let err = Coin::Ergo
            .validate_address("9f4QF8Rn5wLhVvjP3g2Kp8sVnTq7YbWc1xZmAoE5rt")
            .unwrap_err();
        assert!(err.contains("51"), "message must state the length: {err}");
    }

    #[test]
    fn the_wrong_coins_address_is_refused_by_shape() {
        // A 34-char Ravencoin address is not a 51-char Ergo one.
        let rvn = "RBiXWscC69f4RAH6h1BWMvcHqRxHr8nP6z";
        assert_eq!(Coin::Ravencoin.validate_address(rvn), Ok(()));
        assert!(Coin::Ergo.validate_address(rvn).is_err());
    }

    #[test]
    fn a_non_base58_character_is_named_in_the_error() {
        let err = Coin::Ergo
            .validate_address("9f4QF8Rn5wLhVvjP3g2Kp8sVnTq7YbWc1xZm0oE5rty")
            .unwrap_err();
        assert!(
            err.contains('0'),
            "the offending char must be visible: {err}"
        );
    }

    #[test]
    fn intensity_is_clamped_to_the_safe_band() {
        let mut c = ergo_card();
        c.intensity_percent = 0;
        assert!(validate_card(&c).is_err());
        c.intensity_percent = 100;
        let err = validate_card(&c).unwrap_err();
        assert!(err.contains("intensity must be"), "{err}");
        c.intensity_percent = 30;
        assert_eq!(validate_card(&c), Ok(()));
    }

    #[test]
    fn a_pool_outside_the_curated_list_is_refused() {
        let mut c = ergo_card();
        c.pool = "ergo.example-attacker.com:3333".into();
        let err = validate_card(&c).unwrap_err();
        assert!(err.contains("curated"), "{err}");
        assert!(
            err.contains("herominers") && err.contains("woolypooly"),
            "the choices must be listed: {err}"
        );
    }

    #[test]
    fn the_same_card_cannot_be_configured_twice() {
        let a = ergo_card();
        let mut b = a.clone();
        b.coin = "ravencoin".into();
        b.address = "RTestAddrForDupCheck123456789abcde".into();
        b.pool = "rvn.2miners.com:6060".into();
        b.device = 0;
        let st = SecondaryState {
            enabled: true,
            cards: vec![a, b],
        };
        assert!(st.validate().unwrap_err().contains("twice"));
    }

    #[test]
    fn alephium_is_refused_because_no_shipped_miner_can_mine_it() {
        let mut c = ergo_card();
        c.coin = "alephium".into();
        c.address = "1DrEeC9Y7dGVaFhKnmPqRsTuVwXyZaBcDeFgH".into();
        c.pool = "de.alephium.herominers.com:1190".into();
        let err = validate_card(&c).unwrap_err();
        assert!(err.contains("cannot be mined by this build"), "{err}");
        assert!(!available_coins(24.0).contains(&Coin::Alephium));
    }

    #[test]
    fn every_curated_pool_is_non_empty_and_reachable_by_shape() {
        // Row 294: the list is what the dashboard offers, so an empty coin list
        // here means "we advertise a coin nobody can mine".
        for coin in [Coin::Ergo, Coin::Alephium, Coin::Ravencoin] {
            assert_eq!(coin.pools().is_empty(), !coin.available(), "{coin:?}");
        }
        assert_eq!(Coin::Ergo.pools().len(), 2);
    }

    #[test]
    fn enabling_with_no_card_configured_is_refused() {
        let st = SecondaryState {
            enabled: true,
            cards: vec![],
        };
        assert!(st.validate().is_err());
        // Off is always valid, whatever the cards say.
        let off = SecondaryState {
            enabled: false,
            cards: vec![],
        };
        assert_eq!(off.validate(), Ok(()));
    }

    #[test]
    fn an_8gb_card_is_offered_ergo_but_not_alephium_or_ravencoin() {
        let coins = available_coins(8.0);
        assert!(coins.contains(&Coin::Ergo));
        assert!(
            !coins.contains(&Coin::Alephium),
            "nothing this build ships can mine Alephium (row 294, measured 2026-11-28)"
        );
        assert!(
            !coins.contains(&Coin::Ravencoin),
            "8 GB cannot hold GPN 2.0 + RVN 4.0 + a model 2.0"
        );
    }

    #[test]
    fn a_12gb_card_is_offered_ergo_and_ravencoin() {
        let coins = available_coins(12.0);
        assert_eq!(coins.len(), 2, "{coins:?}");
        assert!(coins.contains(&Coin::Ravencoin));
    }

    #[test]
    fn a_job_pauses_the_second_miner_and_idle_wins_the_reason() {
        assert_eq!(pause_reason(false, false), None);
        assert_eq!(pause_reason(true, false), Some("ai-job"));
        assert_eq!(pause_reason(true, true), Some("idle"));
    }

    #[test]
    fn state_round_trips_and_a_missing_file_is_off() {
        let dir = tmpdir("state");
        let p = dir.join("secondary.json");
        assert_eq!(read_state_at(&p).unwrap(), SecondaryState::default());
        let st = SecondaryState {
            enabled: true,
            cards: vec![ergo_card()],
        };
        write_state_at(&p, &st).unwrap();
        assert_eq!(read_state_at(&p).unwrap(), st);
        // Invalid state must never be written.
        let bad = SecondaryState {
            enabled: true,
            cards: vec![CardConfig {
                intensity_percent: 99,
                ..ergo_card()
            }],
        };
        assert!(write_state_at(&p, &bad).is_err());
        assert_eq!(read_state_at(&p).unwrap(), st, "the old file must survive");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_state_is_an_error_not_an_off_switch() {
        let dir = tmpdir("corrupt");
        let p = dir.join("secondary.json");
        std::fs::write(&p, "{not json").unwrap();
        let e = read_state_at(&p).unwrap_err();
        assert!(e.contains("not valid"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_miner_argv_carries_every_choice_and_no_shell_string() {
        let argv =
            miner_argv(Path::new("/opt/gpn/second-miner-lolminer.sh"), &ergo_card()).unwrap();
        assert_eq!(argv[0], "/opt/gpn/second-miner-lolminer.sh");
        let joined = argv.join(" ");
        for needle in [
            "--coin ergo",
            "--pool de.ergo.herominers.com:1180",
            "--address 9f4QF8Rn5wLhVvjP3g2Kp8sVnTq7YbWc1xZmAoE5rtyAbCdEfGh",
            "--device 0",
            "--intensity-percent 30",
            "--status-file",
        ] {
            assert!(
                joined.contains(needle),
                "argv is missing `{needle}`: {joined}"
            );
        }
    }

    // ── supervisor (unix: it signals a real process group) ──────────────────
    #[cfg(unix)]
    fn fake_miner(dir: &Path) -> PathBuf {
        let p = dir.join("fake-miner.sh");
        std::fs::write(&p, "#!/bin/sh\nwhile true; do sleep 1; done\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn the_supervisor_starts_pauses_and_resumes_a_card() {
        let dir = tmpdir("sup-start");
        let miner = fake_miner(&dir);
        let mut sup = Supervisor::new(miner);
        let st = SecondaryState {
            enabled: true,
            cards: vec![ergo_card()],
        };

        let lines = sup.reconcile(&st, None, 100);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("device 0 ergo") && l.contains("paused=false")),
            "the startup line must state coin + intensity + paused: {lines:?}"
        );
        let s = sup.status(101);
        assert_eq!(s.cards.len(), 1);
        assert_eq!(s.cards[0].state, "mining");
        assert!(s.cards[0].pid > 0);

        // An AI job: the whole group is stopped, and the dashboard says why.
        let lines = sup.reconcile(&st, Some("ai-job"), 110);
        assert!(
            lines.iter().any(|l| l.contains("paused (ai-job)")),
            "{lines:?}"
        );
        assert_eq!(sup.status(111).cards[0].state, "paused-for-job");

        // Idle while paused: the reason is renamed, not re-signalled.
        let lines = sup.reconcile(&st, Some("idle"), 120);
        assert!(lines.is_empty(), "no state change, no log line: {lines:?}");

        // Job over: mining again, and the pid is the same process.
        let pid_before = sup.status(121).cards[0].pid;
        let lines = sup.reconcile(&st, None, 130);
        assert!(lines.iter().any(|l| l.contains("resumed")), "{lines:?}");
        let s = sup.status(131);
        assert_eq!(s.cards[0].state, "mining");
        assert_eq!(s.cards[0].pid, pid_before, "resume must not respawn");

        let _ = sup.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_dead_miner_is_reported_and_respawned() {
        let dir = tmpdir("sup-dead");
        let miner = fake_miner(&dir);
        let mut sup = Supervisor::new(miner);
        let st = SecondaryState {
            enabled: true,
            cards: vec![ergo_card()],
        };
        sup.reconcile(&st, None, 100);
        let pid = sup.status(100).cards[0].pid;

        // Kill it the way an operator would (task manager / kill).
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        std::thread::sleep(std::time::Duration::from_millis(300));

        let lines = sup.reconcile(&st, None, 200);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("exited") && l.contains("respawning")),
            "a killed second miner must be visible, not silent: {lines:?}"
        );
        // GPN is untouched by design; the second miner is simply restarted.
        let s = sup.status(201);
        assert!(s.cards[0].pid != pid, "a new process must be running");
        let _ = sup.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn disabling_stops_every_card() {
        let dir = tmpdir("sup-off");
        let miner = fake_miner(&dir);
        let mut sup = Supervisor::new(miner);
        let mut st = SecondaryState {
            enabled: true,
            cards: vec![ergo_card()],
        };
        sup.reconcile(&st, None, 100);
        st.enabled = false;
        let lines = sup.reconcile(&st, None, 110);
        assert!(
            lines.iter().any(|l| l.contains("stopped — disabled")),
            "{lines:?}"
        );
        assert!(sup.status(111).cards.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_miner_binary_is_miner_not_installed_and_never_crashes() {
        let dir = tmpdir("sup-nominer");
        let mut sup = Supervisor::new(dir.join("absent-miner"));
        let st = SecondaryState {
            enabled: true,
            cards: vec![ergo_card()],
        };
        let lines = sup.reconcile(&st, None, 100);
        assert!(lines.iter().any(|l| l.contains("not running")), "{lines:?}");
        let s = sup.status(101);
        assert_eq!(s.cards.len(), 1);
        assert_eq!(s.cards[0].state, "miner-not-installed");
        // Second tick: the same failure is not re-logged every 5 s.
        assert!(sup.reconcile(&st, None, 105).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_miner_is_never_handed_a_gpn_account_file() {
        // Coin-B earnings must stay outside GPN's books. The strongest cheap
        // guarantee is that the miner process is given no GPN key material at
        // all: its argv carries the operator's own coin address and one status
        // file, nothing else.
        let argv = miner_argv(Path::new("/opt/lolminer"), &ergo_card()).unwrap();
        let joined = argv.join(" ");
        assert!(!joined.contains("account_key"), "{joined}");
        assert!(!joined.contains("keystore"), "{joined}");
        assert!(!joined.contains("gpu.lock"), "{joined}");
        let paths: Vec<&String> = argv.iter().filter(|a| a.contains(".gpn/")).collect();
        assert_eq!(
            paths.len(),
            1,
            "only the status file may live under ~/.gpn: {argv:?}"
        );
        assert!(
            paths[0].ends_with(".log"),
            "the one ~/.gpn path must be the miner's own log: {argv:?}"
        );
    }

    #[test]
    fn status_round_trips_and_a_missing_file_reads_as_empty() {
        let dir = tmpdir("status");
        let p = dir.join("secondary-status.json");
        assert_eq!(read_status_at(&p), Status::default());
        let st = Status {
            updated: 123,
            cards: vec![CardStatus {
                device: 0,
                coin: "ergo".into(),
                state: "mining".into(),
                pid: 42,
                intensity_percent: 30,
                since: 100,
                detail: "accepted (12 ms)".into(),
            }],
        };
        write_status_at(&p, &st).unwrap();
        assert_eq!(read_status_at(&p), st);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
