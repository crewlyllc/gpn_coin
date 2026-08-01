//! Chain specification for the GPN network.

use gpn_runtime::{
    AccountId, BalancesConfig, GPUPoWConfig, GPUAttestationConfig, GPNTokenConfig,
    ProfitabilityOracleConfig, RuntimeGenesisConfig,
};
use sc_chain_spec::Properties;
use sc_service::{ChainType, GenericChainSpec};
use sp_core::{sr25519, Pair, Public};
use sp_runtime::Perbill;

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
            vec![account_id_from_seed("Alice")],
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
            vec![account_id_from_seed("Alice"), account_id_from_seed("Bob")],
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
            balances: endowed_accounts.into_iter().map(|k| (k, initial_balance)).collect(),
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
        compute_marketplace: Default::default(),
        profitability_oracle: ProfitabilityOracleConfig {
            initial_operators,
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
