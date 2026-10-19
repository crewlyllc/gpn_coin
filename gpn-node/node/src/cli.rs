//! CLI configuration for the GPN node.

use clap::Parser;
use sc_cli::SubstrateCli;

#[derive(Debug, Parser)]
pub struct Cli {
    #[command(subcommand)]
    pub subcommand: Option<Subcommand>,

    #[clap(flatten)]
    pub run: sc_cli::RunCmd,

    /// Enable GPU attestation on this node
    #[arg(long)]
    pub gpu_attestation: bool,

    /// GPU device index for mining (0-indexed)
    #[arg(long, default_value = "0")]
    pub gpu_device: u8,

    /// Bitcoin-style DNS seed: resolve this name and merge the peer multiaddrs
    /// it publishes as DNS TXT records into the bootnode set. Repeatable.
    #[arg(long = "bootnodes-dns", value_name = "NAME")]
    pub bootnodes_dns: Vec<String>,

    /// Published seed-list JSON to fetch and merge into the bootnode set
    /// (accepts `{"multiaddrs": [...]}` or a plain array). Repeatable.
    #[arg(long = "bootnodes-url", value_name = "URL")]
    pub bootnodes_url: Vec<String>,

    /// Willing to be published as a seed candidate (network default).
    #[arg(long = "seed", conflicts_with = "no_seed")]
    pub seed: bool,

    /// Do not publish this node as a seed candidate; also silences the
    /// "non-discoverable mode" notice. Syncing, mining and jobs are unaffected.
    #[arg(long = "no-seed")]
    pub no_seed: bool,

    /// Announce this node's endpoint and accept inbound peers (default).
    #[arg(long = "discoverable", conflicts_with = "no_discoverable")]
    pub discoverable: bool,

    /// Privacy mode: no endpoint announcement, no inbound expectation, not in
    /// any published list. Implies `--no-seed`.
    #[arg(long = "no-discoverable")]
    pub no_discoverable: bool,

    /// Old hard gate: exit (code 78) unless the node is inbound-reachable.
    #[arg(long = "require-backbone")]
    pub require_backbone: bool,
}

#[derive(Debug, clap::Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum Subcommand {
    /// Key management cli utilities
    #[command(subcommand)]
    Key(sc_cli::KeySubcommand),

    /// Build a chain specification.
    #[command(name = "build-spec")]
    BuildSpec(sc_cli::BuildSpecCmd),

    /// Export the chain specification.
    ExportChainSpec(sc_cli::ExportChainSpecCmd),

    /// Validate blocks.
    CheckBlock(sc_cli::CheckBlockCmd),

    /// Export blocks.
    ExportBlocks(sc_cli::ExportBlocksCmd),

    /// Export the state of a given block into a chain spec.
    ExportState(sc_cli::ExportStateCmd),

    /// Import blocks.
    ImportBlocks(sc_cli::ImportBlocksCmd),

    /// Remove the whole chain.
    PurgeChain(sc_cli::PurgeChainCmd),

    /// Revert the chain to a previous state.
    Revert(sc_cli::RevertCmd),

    /// Db meta columns information.
    ChainInfo(sc_cli::ChainInfoCmd),
}
