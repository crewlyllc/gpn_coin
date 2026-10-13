@echo off
REM ============================================================================
REM  GPN Windows — start this box as a GPN node.
REM
REM  A GPN node is ALWAYS both a miner and an AI provider — there is no role
REM  option and no mine-only / provider-only mode. This launches the single-node
REM  supervisor gpn_node.exe, which brings up the chain node/miner, the provider
REM  heartbeat (:8093) and the AI provider claim/infer loop TOGETHER, with one
REM  log stream and clean shutdown (Ctrl-C stops everything).
REM
REM  The one GPU is auto-yielded to an AI job while it runs (mining fully stops
REM  for the job, then resumes — task #24).
REM
REM  Configure via env / a config file (see docs/run-a-node.md). Set:
REM    GPN_CHAIN, GPN_SIGNER, GPN_ACCOUNT, GPN_PROVIDER_ENDPOINT, GPN_OLLAMA,
REM    GPN_GFLOPS, GPN_ASSIGN_TOKEN, GPN_NODE_KEY, GPN_BOOTNODES
REM  Rewards go to the node's own (or the configured payout) account.
REM
REM  NOTE: a second author on the SAME authoring key forks the chain. Only one
REM  designated author should mine per key. The external miner (if used) must run
REM  on a SEPARATE machine — gpn_external_miner refuses a localhost node, so one
REM  GPU can never double-mine.
REM ============================================================================

REM --- Node: miner + AI provider, supervised together --------------------------
set "GPN_CHAIN=testnet"
set "GPN_NODE_KEY=8f9e5ccd99ed3f4516563495d598a7dbcfff5c18b658d3e5cea5718307a4ba74"
set "GPN_RPC_PORT=9944"
set "GPN_BOOTNODES=/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo"
REM  Provider side (AI serving) — set to match this box:
REM set "GPN_SIGNER=//Alice"
REM set "GPN_PROVIDER_ENDPOINT=http://<your-public-ip>:8093/status"
REM set "GPN_ASSIGN_TOKEN=<shared AP-provider secret>"
start "gpn-node" "C:\actions-runner\_work\gpn_coin\gpn_coin\gpn-agent\target\release\gpn_node.exe"

REM --- (OPTIONAL) external miner — run on a SEPARATE machine ----------------
REM  Uncomment the line below on a DIFFERENT PC (it will refuse to run here,
REM  since the node is on the same machine). Replace <NODE_IP> with the node's
REM  public IP, and <WALLET_SEED> with the miner's own wallet seed.
REM start "gpn-miner" "C:\actions-runner\_work\gpn_coin\gpn_coin\gpn-agent\target\release\gpn_external_miner.exe" http://<NODE_IP>:9944 0 1048576 2000 <WALLET_SEED>
