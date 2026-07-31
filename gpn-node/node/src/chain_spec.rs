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
        ))
        .build())
}

/// Build the genesis config as JSON (consumed by the runtime's GenesisBuilder API).
fn genesis_config_json(
    initial_operators: Vec<AccountId>,
    endowed_accounts: Vec<AccountId>,
) -> serde_json::Value {
    let initial_difficulty = {
        let mut diff = [0u64; 4];
        diff[3] = 0x4000; // Initial easy difficulty for dev
        diff
    };

    let initial_balance: u128 = 1_000_000_000_000_000_000_000_000; // 1,000,000 GPN

    let config = RuntimeGenesisConfig {
        system: Default::default(),
        balances: BalancesConfig {
            balances: endowed_accounts.into_iter().map(|k| (k, initial_balance)).collect(),
            dev_accounts: None,
        },
        gpu_po_w: GPUPoWConfig {
            initial_difficulty,
            initial_block_reward: 50_000_000_000_000_000_000u128, // 50 GPN
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
    };

    serde_json::to_value(config).expect("Genesis config is valid JSON")
}
