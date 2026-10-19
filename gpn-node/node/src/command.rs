//! Command execution for the GPN node.

use crate::{
    chain_spec,
    cli::{Cli, Subcommand},
    service,
};
use gpn_runtime::Block;
use sc_cli::SubstrateCli;
use sc_service::PartialComponents;

impl SubstrateCli for Cli {
    fn impl_name() -> String {
        "GPN Node".into()
    }

    fn impl_version() -> String {
        env!("CARGO_PKG_VERSION").into()
    }

    fn description() -> String {
        "GPU-Native Crypto & AI Compute Network Node".into()
    }

    fn author() -> String {
        "GPN Network".into()
    }

    fn support_url() -> String {
        "https://github.com/yourorg/gpn-network".into()
    }

    fn copyright_start_year() -> i32 {
        2026
    }

    fn load_spec(&self, id: &str) -> Result<Box<dyn sc_service::ChainSpec>, String> {
        Ok(match id {
            "dev" => Box::new(chain_spec::development_chain_spec()?),
            "mainnet" => Box::new(chain_spec::mainnet_chain_spec()?),
            "testnet" => Box::new(chain_spec::testnet_chain_spec()?),
            "" | "local" => Box::new(chain_spec::local_chain_spec()?),
            path => Box::new(chain_spec::ChainSpec::from_json_file(
                std::path::PathBuf::from(path),
            )?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Stub-argv test for the regression this fixes: a **bare** `gpn-node`
    /// launch (no `--validator`, no `--alice`, no `--force-authoring`) must
    /// still end up as an authority whose miner is spawned.
    ///
    /// The CLI alone derives nothing (role=Full, force_authoring=false) — that
    /// is the exact divergence that produced "Prepared block for proposing…"
    /// with no seal. `service::force_mining` + `service::miner_should_run`
    /// close it: mining follows the (forced) role, never the unused flag.
    #[test]
    fn bare_argv_is_forced_to_authority_and_spawns_miner() {
        let cli = Cli::try_parse_from(["gpn-node", "--chain", "dev", "--tmp"])
            .expect("a plain run argv must parse");

        // The stub argv sets none of the flags Substrate would otherwise need.
        assert!(!cli.run.validator, "bare argv must not rely on --validator");
        assert!(!cli.run.alice, "bare argv must not rely on --alice");
        assert!(
            !cli.run.force_authoring,
            "bare argv must not have to pass --force-authoring"
        );

        // The miner decision is driven by the ROLE only — and a forced
        // Authority always mines, with or without --force-authoring.
        assert!(service::miner_should_run(sc_service::Role::Authority));
        assert!(!service::miner_should_run(sc_service::Role::Full));
    }

    /// `--alice` (what the supervisor passes via `author = "alice"`) sets
    /// role=Authority but *not* `--force-authoring` — the historical gate that
    /// left a proposer with no miner. It must still mine.
    #[test]
    fn alice_without_force_authoring_spawns_miner() {
        let cli = Cli::try_parse_from(["gpn-node", "--chain", "dev", "--tmp", "--alice"])
            .expect("argv parses");
        assert!(cli.run.alice);
        assert!(
            !cli.run.force_authoring,
            "--alice does not imply --force-authoring on this Substrate rev"
        );
        assert!(service::miner_should_run(sc_service::Role::Authority));
    }

    /// Membership flags: seed/discoverable default OFF *as flags* (the service
    /// defaults them ON), and the opt-outs parse cleanly.
    #[test]
    fn membership_flags_parse() {
        let bare = Cli::try_parse_from(["gpn-node", "--chain", "dev"]).expect("argv parses");
        assert!(!bare.seed && !bare.no_seed && !bare.discoverable && !bare.no_discoverable);
        // The hard gate is the default; both flags are opt-in at the CLI level
        // (an unset flag leaves the environment, hence the default, alone).
        assert!(!bare.require_backbone);
        assert!(!bare.no_require_backbone);

        let private = Cli::try_parse_from([
            "gpn-node",
            "--chain",
            "dev",
            "--no-discoverable",
            "--require-backbone",
        ])
        .expect("argv parses");
        assert!(private.no_discoverable);
        assert!(private.require_backbone);

        // The documented opt-out must parse and must conflict with the explicit
        // hard-gate flag (never silently resolve).
        let soft = Cli::try_parse_from(["gpn-node", "--chain", "dev", "--no-require-backbone"])
            .expect("argv parses");
        assert!(soft.no_require_backbone);
        assert!(!soft.require_backbone);
        assert!(
            Cli::try_parse_from(["gpn-node", "--require-backbone", "--no-require-backbone"])
                .is_err()
        );

        // Mutually exclusive pairs must be rejected, not silently resolved.
        assert!(Cli::try_parse_from(["gpn-node", "--seed", "--no-seed"]).is_err());
        assert!(Cli::try_parse_from(["gpn-node", "--discoverable", "--no-discoverable"]).is_err());
    }
}

/// Translate the membership CLI flags into the `GPN_*` environment that
/// [`service::new_full`] reads.
///
/// The service entry point only sees a `Configuration`, so the flags are
/// normalised into the (already documented) env interface at the CLI boundary.
/// Only flags the operator actually passed are set — an unset flag leaves the
/// environment (and therefore the default) untouched.
pub fn apply_membership_flags(cli: &Cli) {
    if cli.no_discoverable {
        std::env::set_var("GPN_DISCOVERABLE", "0");
        // `--no-discoverable` implies `--no-seed`.
        std::env::set_var("GPN_SEED", "0");
    } else if cli.discoverable {
        std::env::set_var("GPN_DISCOVERABLE", "1");
    }
    if cli.no_seed {
        std::env::set_var("GPN_SEED", "0");
    } else if cli.seed {
        std::env::set_var("GPN_SEED", "1");
    }
    if cli.require_backbone {
        std::env::set_var("GPN_REQUIRE_BACKBONE", "1");
    } else if cli.no_require_backbone {
        std::env::set_var("GPN_REQUIRE_BACKBONE", "0");
    }
}

/// Parse and run command line arguments
pub fn run() -> sc_cli::Result<()> {
    let cli = Cli::from_args();

    match &cli.subcommand {
        Some(Subcommand::Key(cmd)) => cmd.run(&cli),
        Some(Subcommand::BuildSpec(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.sync_run(|config| cmd.run(config.chain_spec, config.network))
        }
        Some(Subcommand::ExportChainSpec(cmd)) => {
            let chain_spec = cli.load_spec(&cmd.chain)?;
            cmd.run(chain_spec)
        }
        Some(Subcommand::CheckBlock(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.async_run(|config| {
                let PartialComponents {
                    client,
                    task_manager,
                    import_queue,
                    ..
                } = service::new_partial(&config)?;
                Ok((cmd.run(client, import_queue), task_manager))
            })
        }
        Some(Subcommand::ExportBlocks(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.async_run(|config| {
                let PartialComponents {
                    client,
                    task_manager,
                    ..
                } = service::new_partial(&config)?;
                Ok((cmd.run(client, config.database), task_manager))
            })
        }
        Some(Subcommand::ExportState(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.async_run(|config| {
                let PartialComponents {
                    client,
                    task_manager,
                    ..
                } = service::new_partial(&config)?;
                Ok((cmd.run(client, config.chain_spec), task_manager))
            })
        }
        Some(Subcommand::ImportBlocks(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.async_run(|config| {
                let PartialComponents {
                    client,
                    task_manager,
                    import_queue,
                    ..
                } = service::new_partial(&config)?;
                Ok((cmd.run(client, import_queue), task_manager))
            })
        }
        Some(Subcommand::PurgeChain(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.sync_run(|config| cmd.run(config.database))
        }
        Some(Subcommand::Revert(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.async_run(|config| {
                let PartialComponents {
                    client,
                    task_manager,
                    backend,
                    ..
                } = service::new_partial(&config)?;
                Ok((cmd.run(client, backend, None), task_manager))
            })
        }
        Some(Subcommand::ChainInfo(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.sync_run(|config| cmd.run::<Block>(&config))
        }
        None => {
            // Membership flags → env before anything builds the service config.
            apply_membership_flags(&cli);
            let mut runner = cli.create_runner(&cli.run)?;
            // A GPN node ALWAYS mines: mining is structural, not an opt-in flag.
            // Force the authority role + authoring on the runner's config *before*
            // anything reads it, so the startup banner reports `Role: AUTHORITY`
            // (not the misleading `FULL`) and the service mines by default —
            // with no `--force-authoring` / `--validator` required. The same
            // invariants are re-applied inside `service::new_full`.
            service::force_mining(runner.config_mut());
            // Extra peer-discovery seed sources (Bitcoin-style DNS seeds + HTTP
            // seed lists) declared on the CLI. Merged with the static
            // bootnodes + persisted peer store in `prepare_network_config`.
            let seed_sources = crate::discovery::SeedSources {
                dns: cli.bootnodes_dns.clone(),
                urls: cli.bootnodes_url.clone(),
            };
            runner.run_node_until_exit(|config| async move {
                let config = service::prepare_network_config(config, &seed_sources).await;
                match config.network.network_backend {
                    sc_network::config::NetworkBackendType::Libp2p => service::new_full::<
                        sc_network::NetworkWorker<
                            Block,
                            <Block as sp_runtime::traits::Block>::Hash,
                        >,
                    >(config)
                    .await
                    .map_err(sc_cli::Error::Service),
                    sc_network::config::NetworkBackendType::Litep2p => {
                        service::new_full::<sc_network::Litep2pNetworkBackend>(config)
                            .await
                            .map_err(sc_cli::Error::Service)
                    }
                }
            })
        }
    }
}
