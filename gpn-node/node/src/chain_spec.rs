//! Chain specification for the GPN network.

use gpn_runtime::{
    AccountId, BalancesConfig, CouncilConfig, GPNTokenConfig, GPNTreasuryConfig,
    GpnGovernanceConfig, GPUAttestationConfig, GPUPoWConfig, ProfitabilityOracleConfig,
    RuntimeGenesisConfig,
};
use sc_chain_spec::Properties;
use sc_service::{ChainType, GenericChainSpec};
use sp_core::{sr25519, Pair, Public};
use sp_runtime::Perbill;
use std::str::FromStr;

/// The `gpn/tokn` pallet account (`PalletId` -> `into_account_truncating` =
/// `"modl" ++ pallet_id ++ 0x00..`). Endowed at genesis (dev/local only) so the
/// fee-sink always exists: pallet_balances cannot create an account with a
/// sub-ED balance, so a marketplace job whose fee is below the existential
/// deposit would otherwise fail on a fresh chain until some larger job creates
/// the sink.
pub const FEE_SINK_ACCOUNT: AccountId = AccountId::new([
    0x6d, 0x6f, 0x64, 0x6c, // "modl"
    0x67, 0x70, 0x6e, 0x2f, 0x74, 0x6f, 0x6b, 0x6e, // "gpn/tokn"
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
]);

/// Specialized `ChainSpec`. Specialization of the general Substrate ChainSpec type.
pub type ChainSpec = GenericChainSpec;

/// Generate a crypto pair from seed.
pub fn account_id_from_seed(seed: &str) -> AccountId {
    sr25519::Pair::from_string(&format!("//{}", seed), None)
        .expect("static values are valid; qed")
        .public()
        .into()
}

/// The 5 bootstrap oracle operators for the public testnet (Jeremy's
/// `oracle-roster`, 2026-09-16). Public SS58 addresses only — the secret
/// phrases/seeds stay cold in the roster file and are NOT embedded here.
/// `AccountId::from_str` parses SS58 (same pattern as `GPN_GENESIS_RESERVE`).
fn testnet_oracle_operators() -> Vec<AccountId> {
    [
        "5DUaYAf5wSDSZrQVvMygLRCMmkF9BBqrrrSsVdw3QHboM5Y6",
        "5Dk6Yv3B5PhjhafS7YauXWbNTcmHVbeyFuNzczKB78RtwNtV",
        "5F1eQwdyGW3Csaz9FNSQncK62EyqfF76RM8j2hg8V3MwKwD9",
        "5DV1GPahj1kTbjQY2rVwSzYVKdL7d4AWzjL1sEqHiqenSDUa",
        "5Cvguz6wfB7EiLDghdTAZzE7TT3vGUCYegS7seyMDPJGASbm",
    ]
    .iter()
    .map(|s| AccountId::from_str(s).expect("static SS58 is valid; qed"))
    .collect()
}

/// The 3 admission-committee members for the public testnet (Jeremy controls
/// all three; `AdmissionThreshold = 2` → propose self-approves + 1 more approval
/// is needed to admit an operator). Public SS58 addresses only.
fn testnet_committee() -> Vec<AccountId> {
    [
        "5HTWBtWro5ARwrghSmbkcbhVKk8o7xxYirdR7J8T3PJRteWN",
        "5Cd5NiY2qkGzVuQufh844DwKj2k4ExxwyzyrDU4pGUvEX3Dx",
        "5HeUVTK6zd9ii8nXoXBbRU5Q2zdE1Moc9zd1E9GW5JqYej5h",
    ]
    .iter()
    .map(|s| AccountId::from_str(s).expect("static SS58 is valid; qed"))
    .collect()
}

/// The testnet ops `sudo` key (D5): a fresh OFFLINE key held by Jeremy. Only the
/// public address is embedded; the secret phrase stays cold (never on a server).
fn testnet_sudo_key() -> AccountId {
    AccountId::from_str("5HHSuwZgsYpa5Mfdfdgzr7LJcY4sU21t2iyu8sq1yV2PjuMn").expect("valid; qed")
}

/// The testnet faucet key (D5 hot key): a warm account that receives block
/// rewards so it self-funds the drip (runbook §5). Its secret lives on the
/// faucet server and is passed via `gpn_faucet --signer-uri`; only the public
/// address is embedded here.
fn testnet_faucet_key() -> AccountId {
    AccountId::from_str("5GjVjMhy2MfsEMFrT1BWB5NMXEXpsdqW2Dv2J8fHFnCRCwrs").expect("valid; qed")
}

/// Convert a whole-GPN amount to the chain's 12-decimal base units.
fn gpn(amount: u128) -> u128 {
    amount.saturating_mul(1_000_000_000_000)
}

// ── Treasury allocation parameters (LOCKED — Jeremy, 2026-09-10) ──────────
/// 20% of the 1B GPN cap = 200,000,000 GPN, funded to the treasury at genesis.
const TREASURY_ALLOC_GPN: u128 = 200_000_000;
/// Phase 1: 100M GPN over 48 months, every 6 months → 8 tranches × 12.5M.
const TREASURY_PHASE1_TRANCHES: u64 = 8;
const TREASURY_PHASE1_PER_TRANCHE_GPN: u128 = 12_500_000;
/// Phase 2: 100M GPN over 120 months, every 6 months → 20 tranches × 5M.
const TREASURY_PHASE2_TRANCHES: u64 = 20;
const TREASURY_PHASE2_PER_TRANCHE_GPN: u128 = 5_000_000;
/// Governance: 25-address roster, 13 approvals to spend.
const TREASURY_ROSTER_SIZE: u32 = 25;
const TREASURY_THRESHOLD: u32 = 13;
/// ~6 months in blocks at 15s/block: 6*30*24*60*60/15 = 1,036,800.
const SIX_MONTHS_BLOCKS: u64 = 1_036_800;

/// Build the default treasury tranche schedule (discrete 6-month unlocks).
/// The treasury account is a PalletId account (`gpn/trsy`); amounts are set in
/// base units. `start_block` is the first unlock offset (relative to genesis).
fn treasury_schedule() -> Vec<(u32, u128)> {
    let mut s = Vec::new();
    for i in 1..=TREASURY_PHASE1_TRANCHES {
        s.push((
            (i * SIX_MONTHS_BLOCKS) as u32,
            gpn(TREASURY_PHASE1_PER_TRANCHE_GPN),
        ));
    }
    for i in 1..=TREASURY_PHASE2_TRANCHES {
        let blk = (TREASURY_PHASE1_TRANCHES + i) * SIX_MONTHS_BLOCKS;
        s.push((blk as u32, gpn(TREASURY_PHASE2_PER_TRANCHE_GPN)));
    }
    s
}

/// The treasury account (must match `pallet_gpn_treasury::Pallet::account_id`).
///
/// `PalletId(*b"gpn/trsy")` truncates to `"modl" ++ "gpn/trsy" ++ 0x00..`: a
/// 4-byte `"modl"` prefix, the 8-byte pallet id, then zero padding to 32 bytes.
/// Built manually here because the node crate does not depend on `frame_support`.
pub fn treasury_account() -> AccountId {
    let mut bytes = [0u8; 32];
    bytes[0..4].copy_from_slice(b"modl");
    bytes[4..12].copy_from_slice(b"gpn/trsy");
    AccountId::new(bytes)
}

/// The production mainnet governance roster (25 ss58 addresses). These are the
/// REAL roster addresses generated by the project owner (2026-09-11). Mainnet
/// uses these by default; dev/local/testnet use placeholders.
const MAINNET_TREASURY_ROSTER: [&str; 25] = [
    "5Ey5GqHNJ7RTjBKZ6NKZtRyg4K1QkxR8wZs8XDdyQqNQrcvG",
    "5DM2X71PRvr9fyt7oqTzTWbBH5L2KgW5vNpbcy4sRgDSM8ZN",
    "5E2UN3GqfGYcPzd1y5Px3vWpHYPnxAXXSh9w5wYj99oUue9U",
    "5GbSmd4kTXwmYjuCwJA7d8HWmApbj646WDmkeSHbLHAovpEw",
    "5FEcbkgjLLqk7vdcQ63kz2XYZmgcT39BNbrWSTiuYpRS63Rj",
    "5HErbL2cB69uy2Vp1F3xTRusaSKhJ757M8X8YE8GieMJknnz",
    "5FpuYJm4EfsyH5oCqjULumn7cwYzbeLsufv4kfuq36CNwQr9",
    "5GEtaSy9CRyZr5Q9YxxiHcEwBxh3XrdqfpBs1YXZUBsButkC",
    "5DA8HfeXnANKLzaRC53itVHYfVaYbfe55T1wcz4sdofWqVkN",
    "5Hh4KsoFa7AG5vXmfDYkKM9WbejSVqWkibpBE8HKsMNxxeph",
    "5CoDLqDNTwULhfxdV6sKXkEq5cYFizw3ZmDAYDJmQ6znmG9m",
    "5FcKwbAGtHpHufgFe7u81FS92eCXwgfUkWzSfb8vig3gpmMa",
    "5Ey8spPv6YLvMBb29SQsnxttJUj2k5mecAUhMjxhmFq3Vw8U",
    "5FF8B7PBRFFeW8KYfYWwYh5nHhsLjQzDaLVrDrwjLJyxnvrD",
    "5D7im4sj9gR4HigjPVqG7HC1Wmvx28jehQYtjbAkx7mUhHxa",
    "5G11K2sNTDM4TAgVTccFWKYxeQniE3FPVyUkuAp6zuz6SRmm",
    "5HKukQgjbCEF3MWaXvBzHqC72xoephazXsdnWkQdnm9u1DK2",
    "5GQvN7wWMMsdVBHtXhwUDChBCRMAbhHshhy7YX1XnLEqTnuW",
    "5DvaMRVYLwLVXGibj8qDHbw77ksTbwSZ47W2NeZRsFnQtfGE",
    "5CVEYtSMNTJ7biDXSJ8vG4GC4t2ShMFz7gPMagy4NQPc2mWW",
    "5D1s35iLquurNiqqKDi9X2HdqbGcmYLwawxzU2FEXGmPJeCR",
    "5HKJnEkcbQTGvCm9Ju7t3NNhidvMpaX5MNH41kFZiV5qPNEt",
    "5F7Wo1LHHg64GExyfP2G67S7TwRz3d5oxHdmd34HfnUpj6yM",
    "5G1bNfdZeUokjuVJNbru6D7EeDCUuhaLtuwYpHU31k5Uro25",
    "5GeR5tgXj2b9187xorXeqnrw8Nv4rN1EACrstvyhZvqkow77",
];

/// The mainnet governance roster: `GPN_TREASURY_ROSTER` env override, else the
/// hardcoded production roster above.
fn mainnet_treasury_roster() -> Vec<AccountId> {
    if let Ok(list) = std::env::var("GPN_TREASURY_ROSTER") {
        let mut out = Vec::new();
        for part in list.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            match AccountId::from_str(part) {
                Ok(a) => out.push(a),
                Err(e) => log::warn!("GPN_TREASURY_ROSTER: bad ss58 '{part}': {e}"),
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    MAINNET_TREASURY_ROSTER
        .iter()
        .filter_map(|s| match AccountId::from_str(s) {
            Ok(a) => Some(a),
            Err(e) => {
                log::error!("built-in mainnet roster entry '{s}' invalid: {e}");
                None
            }
        })
        .collect()
}

/// Default governance roster (25 addresses) for dev/local/testnet. Configured
/// via `GPN_TREASURY_ROSTER` env (comma-separated ss58 addresses), else the dev
/// accounts 1..25 are used as placeholders (NOT for production).
fn treasury_roster() -> Vec<AccountId> {
    if let Ok(list) = std::env::var("GPN_TREASURY_ROSTER") {
        let mut out = Vec::new();
        for part in list.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            match AccountId::from_str(part) {
                Ok(a) => out.push(a),
                Err(e) => log::warn!("GPN_TREASURY_ROSTER: bad ss58 '{part}': {e}"),
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    // Placeholder roster derived from dev seeds (dev/local only — replace on
    // any real chain by setting GPN_TREASURY_ROSTER or editing chain_spec).
    (1..=TREASURY_ROSTER_SIZE)
        .map(|i| account_id_from_seed(&format!("treasury{i}")))
        .collect()
}

/// A named genesis reserve: an account funded at genesis so the operator can
/// "set aside" a balance in a separate account (e.g. a testnet treasury, a
/// grant pool, or a cold reserve) without touching another account's funds.
///
/// Configured per-chain at genesis — funding is genesis-only (there is no
/// runtime premine), which keeps mainnet fair-launch while letting dev/local/
/// testnet allocate reserves for testing and operations.
#[derive(Clone, Debug)]
pub struct GenesisReserve {
    pub name: String,
    pub account: AccountId,
    pub amount: u128,
}

/// Default reserve list for dev/local/testnet chains.
///
/// Override at runtime with the `GPN_GENESIS_RESERVE` env var:
///   `GPN_GENESIS_RESERVE="treasury:5FHneW46...:1000000,cold:5Grwva...:500000"`
/// Format: `name:ss58address:whole_gpn[,name:ss58address:whole_gpn...]`.
/// Amounts are in **whole GPN** (converted to 12-decimal base units). An empty
/// or unset env var falls back to [`default_reserves`] (or none, if `defaults`
/// is false).
fn parse_reserve_env() -> Option<Vec<GenesisReserve>> {
    let raw = std::env::var("GPN_GENESIS_RESERVE").ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let parts: Vec<&str> = entry.splitn(3, ':').collect();
        if parts.len() != 3 {
            log::warn!(
                "GPN_GENESIS_RESERVE: ignoring malformed entry '{entry}' (want name:ss58:amount)"
            );
            continue;
        }
        let (name, addr, amt) = (parts[0].trim(), parts[1].trim(), parts[2].trim());
        let account = match AccountId::from_str(addr) {
            Ok(a) => a,
            Err(e) => {
                log::warn!("GPN_GENESIS_RESERVE: bad ss58 '{addr}': {e}");
                continue;
            }
        };
        let amount: u128 = match amt.parse() {
            Ok(v) => v,
            Err(e) => {
                log::warn!("GPN_GENESIS_RESERVE: bad amount '{amt}': {e}");
                continue;
            }
        };
        out.push(GenesisReserve {
            name: name.to_string(),
            account,
            amount: gpn(amount),
        });
    }
    Some(out)
}

/// Default reserves used when `GPN_GENESIS_RESERVE` is unset.
///
/// Empty by default — a fresh chain behaves exactly as before (no surprise
/// balances). To set GPN aside, provide a list via the env var, e.g.:
///   GPN_GENESIS_RESERVE="treasury:5FHneW...:1000000" gpn-node --chain local
/// Mainnet ignores all reserves (fair launch).
fn default_reserves() -> Vec<GenesisReserve> {
    Vec::new()
}

/// Resolve the reserve list for a non-mainnet chain.
fn resolve_reserves() -> Vec<GenesisReserve> {
    parse_reserve_env().unwrap_or_else(default_reserves)
}

fn properties() -> Properties {
    let mut props = Properties::new();
    props.insert("tokenDecimals".into(), 12.into());
    props.insert("tokenSymbol".into(), "GPN".into());
    props
}

/// Development config (single node, pre-funded accounts)
pub fn development_chain_spec() -> Result<ChainSpec, String> {
    let wasm_binary = gpn_runtime::WASM_BINARY
        .ok_or_else(|| "Development wasm binary not available".to_string())?;

    Ok(ChainSpec::builder(wasm_binary, None)
        .with_name("GPN Development")
        .with_id("gpn_dev")
        .with_chain_type(ChainType::Development)
        .with_protocol_id("gpn")
        .with_properties(properties())
        .with_genesis_config(genesis_config_json(
            vec![account_id_from_seed("Alice")], // oracle operators
            vec![account_id_from_seed("Alice"), account_id_from_seed("Bob")], // admission committee
            vec![account_id_from_seed("Alice"), account_id_from_seed("Bob")], // governance council
            vec![
                account_id_from_seed("Alice"),
                account_id_from_seed("Bob"),
                account_id_from_seed("Charlie"),
                account_id_from_seed("Dave"),
                account_id_from_seed("Eve"),
                account_id_from_seed("Ferdie"),
            ],
            resolve_reserves(), // "set aside" balances (override via GPN_GENESIS_RESERVE)
            account_id_from_seed("Alice"), // dev sudo key
            0,
            treasury_roster(), // dev placeholder roster
            // Block-reward auto-wiring (item 4): dev node credits Alice so a
            // mined block pays out without any manual extrinsic.
            Some(account_id_from_seed("Alice")),
        ))
        .build())
}

/// Local testnet config (2-4 validators)
pub fn local_chain_spec() -> Result<ChainSpec, String> {
    let wasm_binary = gpn_runtime::WASM_BINARY
        .ok_or_else(|| "Local testnet wasm binary not available".to_string())?;

    Ok(ChainSpec::builder(wasm_binary, None)
        .with_name("GPN Local Testnet")
        .with_id("gpn_test")
        .with_chain_type(ChainType::Local)
        .with_protocol_id("gpn")
        .with_properties(properties())
        .with_genesis_config(genesis_config_json(
            vec![account_id_from_seed("Alice"), account_id_from_seed("Bob")], // operators
            vec![
                account_id_from_seed("Alice"),
                account_id_from_seed("Bob"),
                account_id_from_seed("Charlie"),
            ], // admission committee
            vec![account_id_from_seed("Alice"), account_id_from_seed("Bob"), account_id_from_seed("Charlie")], // governance council
            vec![
                account_id_from_seed("Alice"),
                account_id_from_seed("Bob"),
                account_id_from_seed("Charlie"),
                account_id_from_seed("Dave"),
                account_id_from_seed("Eve"),
                account_id_from_seed("Ferdie"),
            ],
            resolve_reserves(), // "set aside" balances (override via GPN_GENESIS_RESERVE)
            account_id_from_seed("Alice"), // local testnet sudo key
            0,
            treasury_roster(), // dev placeholder roster
            // Block-reward auto-wiring (item 4): local testnet credits Alice.
            Some(account_id_from_seed("Alice")),
        ))
        .build())
}

/// Public testnet config — a config-only fork of mainnet (same WASM runtime, so
/// testnet behavior == mainnet behavior), run by a small set of known bootnodes.
///
/// Differences from mainnet (see testnet-public-runbook.md §4):
/// - ChainType::Local (testnet is disposable)
/// - Oracle operators: bootstrap ops seeded at genesis (feed populated day 1)
/// - Sudo key: ops-controlled key (faucet + emergency ops)
/// - Endowed accounts: NONE (like mainnet) — a faucet service mints/transfers the
///   drip needed for the 1,000 GPN attestation stake (runbook §5, option 3).
///
/// Everything else (initial difficulty, block reward, inflation, 1B GPN supply
/// cap, fair-launch economics) is identical to mainnet, so testnet-verified
/// behavior transfers directly to mainnet.
pub fn testnet_chain_spec() -> Result<ChainSpec, String> {
    let wasm_binary =
        gpn_runtime::WASM_BINARY.ok_or_else(|| "Testnet wasm binary not available".to_string())?;

    Ok(ChainSpec::builder(wasm_binary, None)
        .with_name("GPN Public Testnet")
        .with_id("gpn_testnet")
        .with_chain_type(ChainType::Local)
        .with_protocol_id("gpn")
        .with_properties(properties())
        .with_genesis_config(genesis_config_json(
            // Bootstrap oracle operators (Jeremy's oracle-roster, 2026-09-16):
            // all 5 authorized at genesis; run `gpn_operate_oracle` per operator
            // as machines come online. Secrets stay cold in the roster file.
            testnet_oracle_operators(),
            // Admission committee (Phase B): bootstrap the named committee so
            // operator admission works on day one (threshold approvals).
            testnet_committee(),
            // Governance Council (Option D): committee-3 = 2-of-3 multisig.
            testnet_committee(),
            // No endowed accounts — fair-launch economics preserved; operators
            // get their stake via the off-chain drip faucet (runbook §5).
            vec![],
            // Reserves: dev/testnet convenience reserves (default: none beyond
            // the faucet). Override via GPN_GENESIS_RESERVE if you want to set
            // aside a balance for ops before the faucet is funded by mining.
            resolve_reserves(),
            // Ops sudo key (faucet + emergency ops on testnet only). NOT the
            // mainnet placeholder — testnet sudo is functional by design.
            testnet_sudo_key(),
            // No treasury allocation on testnet (keeps it a clean fair-launch
            // fork of mainnet's economics for testing). Enable via a custom spec
            // if you want to rehearse the vested treasury on the testnet.
            0,
            treasury_roster(), // dev placeholder roster
            // Block-reward auto-wiring (item 4): the faucet key receives block
            // rewards so it self-funds the drip (runbook §5) — no manual mining.
            Some(testnet_faucet_key()),
        ))
        .build())
}

/// Mainnet config (fair launch - NO dev faucet, NO endowed accounts).
///
/// All GPN is issued via GPU PoW block rewards (`gpu-pow` mints through
/// `gpn-token::mint_block_reward`). Genesis supply = 0, as the tokenomics spec
/// requires. Initial difficulty targets ~15s blocks like the other specs.
pub fn mainnet_chain_spec() -> Result<ChainSpec, String> {
    let wasm_binary =
        gpn_runtime::WASM_BINARY.ok_or_else(|| "Mainnet wasm binary not available".to_string())?;

    Ok(ChainSpec::builder(wasm_binary, None)
        .with_name("GPN Mainnet")
        .with_id("gpn_mainnet")
        .with_chain_type(ChainType::Live)
        .with_protocol_id("gpn")
        .with_properties(properties())
        .with_genesis_config(genesis_config_json(
            vec![],                    // no oracle operators at genesis
            vec![],                    // no admission committee at genesis (inert by default)
            testnet_committee(),       // governance council (committee-3 = 2-of-3 multisig)
            vec![],                    // no endowed accounts - fair launch
            vec![],                    // no reserves - mainnet stays fair-launch
            AccountId::new([0u8; 32]), // mainnet sudo key = inert placeholder (not endowed),
            // so sudo is unreachable until governance funds it. MUST
            // be removed/rotated before real mainnet launch.
            // Vested treasury allocation (D-T1/D-T6): 20% of the 1B cap = 200M
            // GPN, funded to the `gpn/trsy` pallet account and locked by the
            // tranche vesting schedule. Counts against the 1B supply cap.
            gpn(TREASURY_ALLOC_GPN),
            mainnet_treasury_roster(), // production 25-address roster
            // Block-reward auto-wiring (item 4): mainnet leaves this `None` at
            // genesis (no hardcoded dev key = no backdoor). The mining account
            // is set post-launch via root/governance `set_block_beneficiary`;
            // see docs/mainnet-launch-runbook.md.
            None,
        ))
        .build())
}

/// Build the genesis config as JSON (consumed by the runtime's GenesisBuilder API).
fn genesis_config_json(
    initial_operators: Vec<AccountId>,
    initial_committee: Vec<AccountId>,
    council_members: Vec<AccountId>,
    endowed_accounts: Vec<AccountId>,
    reserves: Vec<GenesisReserve>,
    sudo_key: AccountId,
    treasury_alloc: u128,
    treasury_roster: Vec<AccountId>,
    block_reward_beneficiary: Option<AccountId>,
) -> serde_json::Value {
    let initial_difficulty = {
        // Target 2^245 (limb3 = 2^53) -> ~10s/block with the light-cache CPU
        // miner. Eases the pallet toward TargetBlockTime (15s) from here.
        // Fast enough to observe self-tuning quickly, hard enough that mining
        // is not instant (instant mining -> timestamp-rejection chaos).
        let mut diff = [0u64; 4];
        diff[3] = 0x0020_0000_0000_0000; // 2^53 limb -> 2^245 target
        diff
    };

    // DEV-ONLY faucet (test convenience). Far below the 1B GPN MaxSupply cap and
    // well above the 1,000 GPN attestation stake, so a dev chain can stake + run
    // marketplace jobs without ever colliding with the supply cap.
    // Mainnet genesis MUST use an empty endowment (fair launch) - see
    // mainnet_chain_spec().
    let initial_balance: u128 = 100_000_000_000_000_000; // 100,000 GPN (dev faucet)

    let config = RuntimeGenesisConfig {
        system: Default::default(),
        balances: BalancesConfig {
            // Dev/local only (endowed_accounts non-empty); mainnet (empty) stays
            // fair-launch. Endow the fee-sink too so sub-ED fees route cleanly.
            balances: {
                let mut balances: Vec<(AccountId, u128)> = endowed_accounts
                    .clone()
                    .into_iter()
                    .map(|k| (k, initial_balance))
                    .collect();
                // Named reserves ("set aside" accounts) — genesis-only funding.
                for r in &reserves {
                    let existing = balances
                        .iter()
                        .find(|(a, _)| a == &r.account)
                        .map(|(_, b)| *b);
                    match existing {
                        Some(prev) => {
                            // Same account listed twice: sum the balances.
                            if let Some(slot) = balances.iter_mut().find(|(a, _)| a == &r.account) {
                                slot.1 = slot.1.saturating_add(r.amount);
                            }
                            log::info!(
                                "GPN genesis reserve '{}' -> {} (+{} GPN, total {} GPN)",
                                r.name,
                                r.account,
                                r.amount / 1_000_000_000_000,
                                (prev + r.amount) / 1_000_000_000_000
                            );
                        }
                        None => {
                            log::info!(
                                "GPN genesis reserve '{}' -> {} ({} GPN set aside)",
                                r.name,
                                r.account,
                                r.amount / 1_000_000_000_000
                            );
                            balances.push((r.account.clone(), r.amount));
                        }
                    }
                }
                if !balances.is_empty() {
                    // Ensure the fee-sink exists (avoid sub-ED fee failures).
                    if !balances.iter().any(|(a, _)| a == &FEE_SINK_ACCOUNT) {
                        balances.push((FEE_SINK_ACCOUNT.clone(), initial_balance));
                    }
                }
                // Treasury allocation: fund the pallet account with the genesis
                // reserve (0 = no allocation, e.g. fair-launch mainnet unless
                // enabled). This balance is *locked by the vesting schedule*
                // (pallet-gpn-treasury), so it can't move until tranches unlock.
                if treasury_alloc > 0 {
                    let t = treasury_account();
                    balances.push((t, treasury_alloc));
                }
                balances
            },
            dev_accounts: None,
        },
        gpu_po_w: GPUPoWConfig {
            initial_difficulty,
            initial_block_reward: 50_000_000_000_000u128, // 50 GPN
            block_reward_beneficiary: block_reward_beneficiary.clone(),
            _config: Default::default(),
        },
        gpu_attestation: GPUAttestationConfig {
            initial_challenge: [0u8; 32],
            _config: Default::default(),
        },
        liveness: Default::default(),
        compute_marketplace: Default::default(),
        profitability_oracle: ProfitabilityOracleConfig {
            initial_operators,
            initial_committee,
            _config: Default::default(),
        },
        gpn_token: GPNTokenConfig {
            initial_inflation: Perbill::from_percent(10),
            // Canonical supply counter is seeded with the genesis treasury
            // allocation so it counts against the 1B MaxSupply cap (item 1).
            initial_minted: treasury_alloc,
            _config: Default::default(),
        },
        gpn_treasury: GPNTreasuryConfig {
            roster: treasury_roster,
            threshold: TREASURY_THRESHOLD,
            schedule: if treasury_alloc > 0 {
                treasury_schedule()
            } else {
                Vec::new()
            },
            _config: Default::default(),
        },
        transaction_payment: Default::default(),
        council: CouncilConfig {
            members: council_members.clone(),
            phantom: Default::default(),
        },
        gpn_governance: GpnGovernanceConfig {
            initial_members: council_members,
            _config: Default::default(),
        },
        sudo: pallet_sudo::GenesisConfig {
            key: Some(sudo_key),
        },
    };

    serde_json::to_value(config).expect("Genesis config is valid JSON")
}

#[cfg(test)]
mod reserve_tests {
    use super::*;

    #[test]
    fn gpn_conversion_is_12_decimals() {
        assert_eq!(gpn(1), 1_000_000_000_000);
        assert_eq!(gpn(100_000), 100_000_000_000_000_000);
    }

    #[test]
    fn dev_spec_includes_a_named_reserve() {
        // Build the genesis JSON directly with an explicit reserve and confirm
        // the account is funded at exactly the reserve amount.
        let acct = account_id_from_seed("Alice");
        let reserves = vec![GenesisReserve {
            name: "treasury".to_string(),
            account: acct.clone(),
            amount: gpn(1_000_000),
        }];
        let json = genesis_config_json(
            vec![],
            vec![],
            vec![],
            vec![],
            reserves,
            account_id_from_seed("Alice"),
            0,
            treasury_roster(),
            None,
        );
        let bals = json["balances"]["balances"]
            .as_array()
            .expect("balances array");
        let found = bals.iter().find(|e| {
            e[0].as_str()
                .map(|s| s.contains("5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY"))
                .unwrap_or(false)
        });
        assert!(
            found.is_some(),
            "reserve account must be present in balances"
        );
    }

    #[test]
    fn empty_endowment_yields_empty_balances_even_with_reserves_caller_passes_none() {
        // Mainnet path: no endowed accounts and no reserves -> zero balances
        // (fair launch), fee-sink NOT added.
        let json = genesis_config_json(
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            AccountId::new([0u8; 32]),
            0,
            treasury_roster(),
            None,
        );
        let bals = json["balances"]["balances"]
            .as_array()
            .expect("balances array");
        assert!(
            bals.is_empty(),
            "mainnet must have zero endowed accounts, got {:?}",
            bals
        );
    }
}
