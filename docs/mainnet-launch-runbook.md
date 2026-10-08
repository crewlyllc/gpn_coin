# GPN Mainnet Launch Runbook

**Audience:** the launch operator (currently Jeremy). **Status:** ready to execute, pending a
go/no-go and the open business decisions listed in §10.

This runbook covers standing up the **`--chain=mainnet`** network: the genesis artifact, the
bootnode/full-node set, post-launch governance actions (block beneficiary, oracle operators),
treasury operations, monitoring, verification, and abort/rollback criteria.

Companion docs:
- Tokenomics (source of truth): `design/tokenomics/01-gpn-tokenomics.md`
- Pallet map: `design/pallets/00-pallet-overview.md`
- Public **testnet** runbook: `shared/artifacts/project7/testnet-public-runbook.md`
- Incident response: `docs/incident-response.md`
- Security policy: `SECURITY.md`

> Nothing in this runbook uses `sudo` on mainnet — the mainnet sudo key is an **inert
> all-zeros placeholder** (`AccountId::new([0u8;32])`) with no private key. Every privileged
> action listed below is done through **root/governance** (`Sudo`-only calls are replaced by a
> governance runtime call) or the on-chain **`pallet-gpn-treasury` 13-of-25** flow. See §6.

---

## 1. What mainnet actually is (facts to verify, not assume)

| Property | Value | Where it comes from |
|---|---|---|
| Chain id / name | `gpn_mainnet` / "GPN Mainnet" | `chain_spec::mainnet_chain_spec()` |
| Chain type | `Live` | same |
| Genesis supply | **0 mined**, **100,000,000 GPN** disclosed vested treasury | `TREASURY_ALLOC_GPN` |
| Hard cap | **1,000,000,000 GPN** (`MaxSupply`) | `pallet-gpn-token` (canonical) |
| Max *mineable* | **900,000,000 GPN** (cap − treasury allocation) | `initial_minted = treasury_alloc` |
| Block reward | **50 GPN/block**, halving-only (**no** inflation curve) | `gpu_po_w.initial_block_reward` |
| Block time | **~4 s target** (shared runtime `TargetBlockTime=4`; self-tuning from 2^232 difficulty) | `initial_difficulty` |

> **Cadence note (open decision §10):** the runtime is shared by testnet and mainnet, so
> `TargetBlockTime=4 s` applies to **both** today — mainnet has **no separate 15 s cadence** yet.
> The `HalvingInterval` (4,204,800 blocks) is ~2 yr at 15 s but only **~194 days at 4 s**; resolve
> via §10 before launch.
| Consensus | Ethash PoW (`gpu-pow`) + PoSG attestation | pallet runtime |
| Oracle operators at genesis | **none** (added by governance) | `initial_operators = vec![]` |
| Endowed accounts | **none** (fair launch) | `endowed_accounts = vec![]` |
| Sudo | **inert all-zeros key** | `AccountId::new([0u8;32])` |
| Treasury account | keyless `gpn/trsy` `PalletId` account | `chain_spec::treasury_account()` |
| Treasury governance | **13-of-25** roster | `TREASURY_THRESHOLD`, `GPN_TREASURY_ROSTER` |

**Invariants that must never change after genesis** (on-chain compatibility): the proof bytes of
the shielded-pool verifier and the **value-balance verification key**. See `SECURITY.md`.

---

## 2. Preconditions (T-7 days and earlier)

1. **Code freeze + clean tree.** `git status` clean on the release commit; record the commit hash.
2. **Green workspace.**
   ```bash
   cd gpn-node && cargo test --workspace          # expect 252 passed / 4 ignored
   cd ../gpn-agent && cargo test --workspace      # expect lib + bins green
   cd .. && cargo fmt --all --check
   cd gpn-node && cargo clippy --workspace --all-targets -- -D warnings
   ```
3. **Reproducible release binary.**
   ```bash
   cd gpn-node && cargo build --release
   sha256sum target/release/gpn-node | tee ../artifacts/gpn-node.sha256
   ```
   Publish the binary + hash so bootnode operators can verify the artifact they run.
4. **Treasury roster finalized.** Produce the 25 governance addresses as ss58:
   ```bash
   # one ss58 per line (or comma-separated via GPN_TREASURY_ROSTER)
   cat treasury-roster-addrs.txt
   ```
   Confirm **25** distinct addresses, each independently controlled, and that 13 of them are
   reachable at launch time. If `GPN_TREASURY_ROSTER` is unset the spec falls back to dev
   placeholder keys — **never launch mainnet with placeholders.**
5. **Decision log closed.** Update/replace the inert sudo path decision and the business items in §10.
6. **Testnet rehearsal passed.** At minimum: the two-node GPU p2p testnet and a full
   mine → attest → paid-job loop on `--chain testnet` (see public testnet runbook §7).

---

## 3. Build the mainnet genesis artifact (T-1 day)

```bash
cd gpn-node
# Build the spec. Export GPN_TREASURY_ROSTER first if the roster is not baked in.
export GPN_TREASURY_ROSTER="$(paste -sd, treasury-roster-addrs.txt)"   # 25 ss58 addrs
cargo run --release -- build-spec --chain mainnet --disable-default-bootnode > /tmp/gpn-mainnet.json
```

**Verify the genesis, line by line (do NOT skip):**

```bash
python3 - <<'PY'
import json
# NB: the runtime GenesisConfig serializes with **camelCase** keys
# (`gpuPoW`, `gpnToken`, `gpnTreasury`, `profitabilityOracle`, …).
g = json.load(open('/tmp/gpn-mainnet.json'))['genesis']['runtimeGenesis']['config']
b = g['balances']['balances']
print('endowed accounts (must be treasury-only):', b)
print('gpuPoW.blockRewardBeneficiary (must be null):', g['gpuPoW']['blockRewardBeneficiary'])
print('gpuPoW.initialBlockReward (must be 50e12):', g['gpuPoW']['initialBlockReward'])
print('gpnToken.initialMinted (must be 100e6 * 1e12):', g['gpnToken']['initialMinted'])
print('treasury threshold (must be 13):', g['gpnTreasury']['threshold'])
print('treasury roster size (must be 25):', len(g['gpnTreasury']['roster']))
print('treasury tranches (must be 21):', len(g['gpnTreasury']['schedule']))
print('oracle operators (must be empty):', g['profitabilityOracle']['initialOperators'])
print('sudo key (must be the all-zeros placeholder):', g['sudo']['key'])
PY
```

Expected:
- `balances` = the single `gpn/trsy` pallet account with `100000000000000000000` raw
  (`100,000,000 GPN × 10¹²`). No other account.
- `block_reward_beneficiary` = `null` (set post-launch via governance, §6).
- `gpn_token.initial_minted` = the treasury allocation (counts against the 1B cap).
- `sudo.key` = `5C4hrfjw9DjXZTzV3MwzrrAr9P1MJhSrvWGWqi1eSuyUpnhM` (all-zeros).
- 21 schedule entries: **70M unlocked at block 0**, then 20 × 1.5M six-month tranches (30M over
  10 yr). 70M is spendable at launch.

Hash and publish the canonical spec:
```bash
sha256sum /tmp/gpn-mainnet.json | tee artifacts/gpn-mainnet-genesis.sha256
# distribute gpn-mainnet.json ONLY to operators you trust to verify the hash jointly in §5
```

---

## 4. Stand up the network (T-0)

There is **no validator authority set** — GPN is PoW; the "bootnode layer" is a set of
always-on, publicly reachable full nodes. A permissionless liveness scheme
(`pallet-gpn-liveness`) rewards reachable full nodes and slashes unreachable ones, so
bootnodes emerge organically rather than being founder-funded.

1. **Seed bootnodes (2–4).** Each operator runs:
   ```bash
   gpn-node --chain /tmp/gpn-mainnet.json \
     --base-path /var/lib/gpn --name <human-name> \
     --node-key-file /etc/gpn/node.key \
     --listen-addr /ip4/0.0.0.0/tcp/30333 --listen-addr /ip4/0.0.0.0/tcp/30334/ws \
     --rpc-port 9944 --rpc-external --rpc-methods=safe \
     --prometheus-port 9615 --prometheus-external

   # publish the real peer id
   gpn-node key inspect-node-key --file /etc/gpn/node.key
   ```
   Note: a **fresh `--base-path` requires a `--node-key`/`--node-key-file`**, otherwise the node
   fails with `NetworkKeyNotFound`. Reuse the same key file across restarts so peer IDs are stable.
2. **Announce the bootnode list:** `--bootnodes /ip4/<IP>/tcp/30333/p2p/<PEER_ID>` for each.
3. **Confirm chain progress.** Blocks are only produced by GPU PoW; watch for the first seals:
   ```bash
   gpn-node --chain /tmp/gpn-mainnet.json ...   # any full node
   # separate terminal
   curl -s -H 'Content-Type: application/json' \
     -d '{"id":1,"jsonrpc":"2.0","method":"chain_getHeader","params":[]}' \
     http://127.0.0.1:9944 | python3 -m json.tool
   ```
   A healthy launch has **new blocks every ~10–15 s**. If no block appears within ~10 min, treat
   as a launch incident (see `docs/incident-response.md` §6.1).

---

## 5. Independent verification (within the first hour)

Run on a node that is **not** the one that built the spec, and compare notes publicly:

```bash
# genesis hash must match across all nodes
curl -s -H 'Content-Type: application/json' \
  -d '{"id":1,"jsonrpc":"2.0","method":"chain_getBlockHash","params":[0]}' \
  http://127.0.0.1:9944

# supply counter: seeds at 200M and must rise by exactly 50 GPN per mined block
python3 - <<'PY'
# gpn_token.TotalMinted via storage; use gpn_check_state / gpn_explorer
PY
gpn-agent/target/release/gpn_check_state --endpoint ws://127.0.0.1:9944 | head
```

Checklist:
- [ ] All nodes report the **same genesis hash**.
- [ ] `TotalMinted` seeds at `100e6 GPN` and increases by `50 GPN` per block.
- [ ] `Balances` shows the treasury `gpn/trsy` account holding 100M, **70M spendable**
      (70M unlocked at genesis; the remaining 30M vests over 10 yr).
- [ ] No endowed user accounts exist.
- [ ] Sudo is unreachable (the all-zeros key is not funded and has no known private key).

---

## 6. Post-launch governance actions (first days)

All of these are **root/governance** calls (mainnet has no usable sudo). Queue them as a
governed `Sudo.sudo → <call>` or via the governance committee per
`design/protocols/05-oracle-governance.md`.

1. **Set the block-reward beneficiary.** Without this, mined blocks pay **no one** (the genesis
   beneficiary is `None` — deliberate, no hardcoded dev backdoor). Set the mining pool / ops
   account:
   ```text
   gpu_pow.set_block_beneficiary(account)   # call_index 1, root-only
   ```
   Verify: mine a block, confirm the beneficiary balance rose by exactly the block reward and a
   `BlockRewardPaid` event fired (one payment per block — a manual `submit_solution` claim and the
   automatic path can never double-pay).
2. **Admit oracle operators** so the profitability feed is populated:
   ```text
   profitability_oracle.add_operator(account)   # root; then operator posts update_feed
   ```
   Client-side: an operator runs `gpn_operate_oracle ws://<node>:9944 <values.toml> <interval> //<key>`.
3. **Fund/operate the treasury** only through the 13-of-25 flow (§7). **70M is unlocked at
   genesis** (spendable immediately), and 1.5M unlocks every `SIX_MONTHS_BLOCKS` = **1,036,800
   blocks** thereafter until the 30M vest completes (10 yr).
4. **Publish operator onboarding** (`operator-onboarding.md`) and the RPC endpoint list.

---

## 7. Treasury operations (`pallet-gpn-treasury`)

The 200M allocation sits in the **keyless** `gpn/trsy` pallet account. There is no private key;
funds move only through the 13-of-25 roster flow, bounded by unlocked tranches.

```bash
# read-only
gpn_treasury status      # balance, unlocked, spendable, threshold, roster size
gpn_treasury schedule    # tranches: unlock block, amount, unlocked?
gpn_treasury roster      # roster size + threshold
gpn_treasury proposals   # pending spend proposals

# signed — each signer must be a roster member (real seed phrase or secure URI)
gpn_treasury propose <dest-ss58> <amount-gpn>        --signer <uri1>   # auto-approves as proposer
gpn_treasury approve <proposal-id>                   --signer <uri2>   # repeat to reach 13
gpn_treasury execute <proposal-id>                   --signer <uri1>   # after ≥13 approvals
```

Rules the pallet enforces: `NotInRoster`, `NotEnoughApprovals` (< 13), `InsufficientUnlocked`
(beyond unlocked tranche total), `AlreadyExecuted`, `RosterEmpty`. Amounts are **whole GPN**
(converted to 12-decimal base units). Coordinate signers out-of-band; never collect keys in one
place.

---

## 8. Monitoring

| Surface | What | Notes |
|---|---|---|
| Prometheus (`:9615`) | Substrate metrics: block height, peers, import queue, finality lag | scrape into your TSDB; alert on height stall |
| `gpn_explorer` | Human dashboard + JSON API (`/api/chain`, `/api/blocks`, `/api/account`, `/api/jobs`) | `--endpoint ws://<node>:9944 --listen 0.0.0.0:8082` |
| Node logs | `BlockRewardPaid`, difficulty retarget, seal validity, `check_expired` finalization | ship to a log store |
| `gpn_check_state` / `gpn_check_feed` | Quick CLI health probes | CI/ops |

Minimum alerts: **no new block for >2× target (30 s)**; **peer count 0**; **`TotalMinted` not
advancing**; **any treasury proposal nearing execution**; **difficulty collapsing or exploding**.
The faucet (`gpn_faucet`) is **testnet-only — do NOT run it on mainnet.**

---

## 9. Abort / rollback criteria

Launch is aborted (announce, stop bootnodes, keep genesis private) if any of:
- the genesis hash differs between operators (spec mismatch), or
- genesis verification in §3 fails (wrong endowment / non-null beneficiary / wrong
  `initial_minted` / oversized roster), or
- the WASM runtime hash does not match the published release binary, or
- > 10 min with zero blocks and no mining explanation.

There is **no chain rollback tool** worse than re-genesis: if a fatal genesis bug ships, the
network must be restarted from a corrected spec at block 0 (nothing of value exists yet). After
value exists, recovery is governance-driven and slow by design — which is why §3/§5 verification
is non-negotiable.

---

## 10. Open business decisions (need Jeremy)

1. Rotate the mainnet sudo placeholder into a **governed** path (or leave permanently inert) —
   see `design/protocols/05-oracle-governance.md`.
2. Bootnode + full-node set and the decentralization plan (who runs the first nodes, where).
3. Token listing / liquidity plan (CEX/DEX).
4. Marketplace go-live: first paying customers + SLA / reputation policy
   (`marketplace-sla-reputation-proposal.md`).
5. The 25 treasury roster members and the 13-of-25 signer operational runbook.
6. **Block-time / halving parity.** The shared runtime currently sets `TargetBlockTime = 4 s`
   (labelled "fast testnet cadence") for **both** testnet and mainnet, while `HalvingInterval`
   (4,204,800 blocks) is documented as "~2 years" (true only at 15 s). Decide the launch cadence;
   if mainnet should differ from testnet, the runtime constant must change (forkless `set_code`,
   reviewed) before the mainnet genesis is cut.
