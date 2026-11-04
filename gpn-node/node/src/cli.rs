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
    ///
    /// Defaults to `https://gpncoin.ai/seeds.json` — the live network
    /// directory. Passing any value here **replaces** that default; the
    /// compiled-in `--bootnodes` remain as the hard fallback either way.
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

    /// Hard gate (OPT-IN, legacy): exit (code 78) unless the node is
    /// inbound-reachable.
    ///
    /// Off by default: a non-dialable node warns once and keeps syncing,
    /// mining and serving. Set this only if you need reachability to be a
    /// guarantee (e.g. a seed host under supervision).
    #[arg(long = "require-backbone", conflicts_with = "no_require_backbone")]
    pub require_backbone: bool,

    /// Explicitly select the default soft mode: keep running as a
    /// non-discoverable participant when this node cannot be dialed.
    #[arg(long = "no-require-backbone")]
    pub no_require_backbone: bool,

    /// Mark this machine as the dialable gateway for its LAN/egress
    /// (`GPN_SITE_GATEWAY=1`). It always attempts UPnP/manual forwarding and
    /// publishes a dialable address; other machines on the site stay leaves.
    #[arg(long = "site-gateway")]
    pub site_gateway: bool,

    /// Never contact the router: disable automatic UPnP-IGD / NAT-PMP port
    /// mapping. The port must then be forwarded manually (or `--public-addr`
    /// used). Mapping is on by default and is logged when it happens.
    #[arg(long = "no-upnp")]
    pub no_upnp: bool,
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
