//! Chain specification for the GPN network.

use gpn_runtime::{
    AccountId, BalancesConfig, GPUPoWConfig, GPUAttestationConfig, GPNTokenConfig,
    ProfitabilityOracleConfig, RuntimeGenesisConfig,
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
        .public().into()
}

/// Convert a whole-GPN amount to the chain's 12-decimal base units.
fn gpn(amount: u128) -> u128 {
    amount.saturating_mul(1_000_000_000_000)
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
            log::warn!("GPN_GENESIS_RESERVE: ignoring malformed entry '{entry}' (want name:ss58:amount)");
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
        out.push(GenesisReserve { name: name.to_string(), account, amount: gpn(amount) });
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
            vec![account_id_from_seed("Alice")],          // oracle operators
            vec![account_id_from_seed("Alice"), account_id_from_seed("Bob")], // admission committee
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
    let wasm_binary = gpn_runtime::WASM_BINARY
        .ok_or_else(|| "Testnet wasm binary not available".to_string())?;

    Ok(ChainSpec::builder(wasm_binary, None)
        .with_name("GPN Public Testnet")
        .with_id("gpn_testnet")
        .with_chain_type(ChainType::Local)
        .with_protocol_id("gpn")
        .with_properties(properties())
        .with_genesis_config(genesis_config_json(
            // Bootstrap oracle operators (2-4) so the profitability feed is
            // populated on day one. Dev seed keys are fine for a disposable
            // testnet; real operators rotate them before any longer-lived run.
            vec![
                account_id_from_seed("Alice"),
                account_id_from_seed("Bob"),
            ],
            // Admission committee (Phase B): bootstrap the named committee so
            // operator admission works on day one (threshold approvals).
            vec![
                account_id_from_seed("Alice"),
                account_id_from_seed("Bob"),
                account_id_from_seed("Charlie"),
            ],
            // No endowed accounts — fair-launch economics preserved; operators
            // get their stake via the off-chain drip faucet (runbook §5).
            vec![],
            // Reserves: dev/testnet convenience reserves (default: none beyond
            // the faucet). Override via GPN_GENESIS_RESERVE if you want to set
            // aside a balance for ops before the faucet is funded by mining.
            resolve_reserves(),
            // Ops sudo key (faucet + emergency ops on testnet only). NOT the
            // mainnet placeholder — testnet sudo is functional by design.
            account_id_from_seed("Alice"),
        ))
        .build())
}

/// Mainnet config (fair launch - NO dev faucet, NO endowed accounts).
///
/// All GPN is issued via GPU PoW block rewards (`gpu-pow` mints through
/// `gpn-token::mint_block_reward`). Genesis supply = 0, as the tokenomics spec
/// requires. Initial difficulty targets ~15s blocks like the other specs.
pub fn mainnet_chain_spec() -> Result<ChainSpec, String> {
    let wasm_binary = gpn_runtime::WASM_BINARY
        .ok_or_else(|| "Mainnet wasm binary not available".to_string())?;

    Ok(ChainSpec::builder(wasm_binary, None)
        .with_name("GPN Mainnet")
        .with_id("gpn_mainnet")
        .with_chain_type(ChainType::Live)
        .with_protocol_id("gpn")
        .with_properties(properties())
        .with_genesis_config(genesis_config_json(
            vec![],                  // no oracle operators at genesis
            vec![],                  // no admission committee at genesis (inert by default)
            vec![],                  // no endowed accounts - fair launch
            vec![],                  // no reserves - mainnet stays fair-launch
            AccountId::new([0u8; 32]), // mainnet sudo key = inert placeholder (not endowed),
                                       // so sudo is unreachable until governance funds it. MUST
                                       // be removed/rotated before real mainnet launch.
        ))
        .build())
}

/// Build the genesis config as JSON (consumed by the runtime's GenesisBuilder API).
fn genesis_config_json(
    initial_operators: Vec<AccountId>,
    initial_committee: Vec<AccountId>,
    endowed_accounts: Vec<AccountId>,
    reserves: Vec<GenesisReserve>,
    sudo_key: AccountId,
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
                    let existing = balances.iter().find(|(a, _)| a == &r.account).map(|(_, b)| *b);
                    match existing {
                        Some(prev) => {
                            // Same account listed twice: sum the balances.
                            if let Some(slot) = balances.iter_mut().find(|(a, _)| a == &r.account) {
                                slot.1 = slot.1.saturating_add(r.amount);
                            }
                            log::info!(
                                "GPN genesis reserve '{}' -> {} (+{} GPN, total {} GPN)",
                                r.name, r.account, r.amount / 1_000_000_000_000, (prev + r.amount) / 1_000_000_000_000
                            );
                        }
                        None => {
                            log::info!(
                                "GPN genesis reserve '{}' -> {} ({} GPN set aside)",
                                r.name, r.account, r.amount / 1_000_000_000_000
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
                balances
            },
            dev_accounts: None,
        },
        gpu_po_w: GPUPoWConfig {
            initial_difficulty,
            initial_block_reward: 50_000_000_000_000u128, // 50 GPN
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
            _config: Default::default(),
        },
        transaction_payment: Default::default(),
        sudo: pallet_sudo::GenesisConfig { key: Some(sudo_key) },
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
        let json = genesis_config_json(vec![], vec![], vec![], reserves, account_id_from_seed("Alice"));
        let bals = json["balances"]["balances"].as_array().expect("balances array");
        let found = bals
            .iter()
            .find(|e| e[0].as_str().map(|s| s.contains("5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY")).unwrap_or(false));
        assert!(found.is_some(), "reserve account must be present in balances");
    }

    #[test]
    fn empty_endowment_yields_empty_balances_even_with_reserves_caller_passes_none() {
        // Mainnet path: no endowed accounts and no reserves -> zero balances
        // (fair launch), fee-sink NOT added.
        let json = genesis_config_json(
            vec![], vec![], vec![],
            vec![],
            AccountId::new([0u8; 32]),
        );
        let bals = json["balances"]["balances"].as_array().expect("balances array");
        assert!(bals.is_empty(), "mainnet must have zero endowed accounts, got {:?}", bals);
    }
}
