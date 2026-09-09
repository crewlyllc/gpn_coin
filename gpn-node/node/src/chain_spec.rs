//! Chain specification for the GPN network.

use gpn_runtime::{
    AccountId, BalancesConfig, GPUPoWConfig, GPUAttestationConfig, GPNTokenConfig,
    ProfitabilityOracleConfig, RuntimeGenesisConfig,
};
use sc_chain_spec::Properties;
use sc_service::{ChainType, GenericChainSpec};
use sp_core::{sr25519, Pair, Public};
use sp_runtime::Perbill;

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
                let mut accts: Vec<AccountId> = endowed_accounts.clone();
                if !accts.is_empty() {
                    accts.push(FEE_SINK_ACCOUNT.clone());
                }
                accts.into_iter().map(|k| (k, initial_balance)).collect()
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
