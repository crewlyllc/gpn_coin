@echo off
REM ============================================================================
REM  GPN Windows — start the node (and an optional remote external miner).
REM
REM  The node mines + produces blocks; its rewards go to the on-chain
REM  BlockRewardBeneficiary (your reward wallet 5D1sqRBmSZ...).
REM
REM  NOTE: the external miner MUST run on a SEPARATE machine. gpn_external_miner
REM  refuses to run against a localhost node, so one GPU can never double-mine.
REM ============================================================================

REM --- 1. The node (mines + peers with the Linux bootnode) ---------------------
start "gpn-node" "C:\actions-runner\_work\gpn_coin\gpn_coin\gpn-node\target\release\gpn-node.exe" --chain testnet --alice --force-authoring --node-key 8f9e5ccd99ed3f4516563495d598a7dbcfff5c18b658d3e5cea5718307a4ba74 --rpc-port 9944 --bootnodes "/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo"

REM --- 2. (OPTIONAL) external miner — run on a SEPARATE machine ----------------
REM  Uncomment the line below on a DIFFERENT PC (it will refuse to run here,
REM  since the node is on the same machine). Replace <NODE_IP> with the node's
REM  public IP, and <WALLET_SEED> with the miner's own wallet seed.
REM start "gpn-miner" "C:\actions-runner\_work\gpn_coin\gpn_coin\gpn-agent\target\release\gpn_external_miner.exe" http://<NODE_IP>:9944 0 1048576 2000 <WALLET_SEED>
