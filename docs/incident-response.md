# GPN Incident Response Plan

**Audience:** the on-call operator(s) of GPN mainnet / testnet. **Status:** active.

This document defines how GPN detects, triages, contains, and recovers from incidents, and how
it communicates. It is the operational companion to `docs/mainnet-launch-runbook.md` and
`SECURITY.md`.

> **Key structural fact:** GPN mainnet is **PoW with no validator authority set**, and its sudo
> key is an **inert all-zeros placeholder** with no private key. There is therefore **no
> break-glass `sudo`** on mainnet: emergency actions run through **root/governance** (a governed
> runtime call / the oracle-governance committee) or the on-chain **13-of-25 treasury** flow.
> Containment is therefore *slow by design* — this is intentional. Plan the response assuming
> governance latency, not instant powers.

---

## 1. Severity levels

| Sev | Name | Definition | Response target |
|---|---|---|---|
| **SEV1** | Critical | Chain halted / forked / finality lost; supply or treasury accounting broken; key compromise; loss of funds | Immediate, all hands, page |
| **SEV2** | Major | A core subsystem degraded (mining stalls but recovers, oracle feed dead, marketplace escrow stuck, explorer/faucet down) | < 1 h |
| **SEV3** | Minor | Cosmetic / single-operator issue; docs wrong; testnet-only bug | < 1 business day |
| **SEV4** | Info | Suspicious pattern, no confirmed impact | Log & monitor |

Severity can be **raised** at any time by the responder; only downgraded by the incident
commander.

---

## 2. Roles

- **Incident Commander (IC)** — decides severity, owns the timeline, calls abort/rollback.
- **Scribe** — keeps a timestamped log (a file or channel), captures every action and decision.
- **Comms Lead** — drafts and sends user/operator communications (single channel of truth).
- **Subject-matter responders** — mining/consensus, runtime/pallets, infra/network, security.

One person may hold multiple roles. On a small team the IC defaults to the launch operator
(Jeremy) unless delegated.

---

## 3. Detection sources

- **Prometheus (`:9615`)** — block height, peer count, import-queue depth, finality lag.
- **`gpn_explorer`** + JSON API — public-facing chain state and job list.
- **Node logs** — `BlockRewardPaid`, difficulty retargets, `check_expired` finalization, seal
  validity, treasury events (`SpendProposed/Approved/Executed`, `RosterUpdated`,
  `ThresholdUpdated`).
- **Community** — users, operators, and the disclosure channel in `SECURITY.md`.

Trigger a triage whenever: no new block for > 2× target (30 s), peer count 0, `TotalMinted`
stalls, difficulty degenerates, a treasury proposal approaches execution, or a security report
arrives.

---

## 4. First 15 minutes (universal triage)

1. Declare severity and start a **timeline file**; appoint IC + scribe.
2. **Preserve evidence** before "fixing": save node logs, `chain_getHeader` output, peer list,
   Prometheus snapshots. Never restart nodes before capturing state if a fork/zombie is suspected.
3. Determine **blast radius**: mainnet? testnet only? how many operators/users affected?
4. Open a single **incident channel**; comms flows there.
5. For SEV1, hold the "announce vs contain" decision with the IC — silence is better than a wrong
   statement.

---

## 5. Communications

- **Internal:** one incident channel; scribe logs everything.
- **External (SEV1/SEV2):** status message within **60 min** (acknowledge, scope, next update
  time), then updates at least every 2 h until resolved. Plain language; no unverified root cause.
- **Post-incident:** public write-up within **7 days** for SEV1/SEV2 (timeline, impact,
  root cause, fixes, follow-ups). SEV3 optional.
- **Never** publish exploit specifics or reproduction steps before a fix has shipped and
  operators have upgraded.

---

## 6. Playbooks

### 6.1 Chain halt / no blocks (SEV1)
*Symptom:* height frozen > 10 min; no `BlockRewardPaid`; peers connected but no seals.
*Likely causes:* miners offline/failing GPU seals; difficulty mis-tuned; timestamp rejection
(instant-mine chaos); all bootnodes down.
*Actions:*
1. Confirm from ≥2 independent nodes that height is frozen (rule out local RPC/storage issues).
2. Check difficulty + target (`get_mining_info`); check node logs for seal/timestamp errors.
3. Restore mining capacity: bring GPU miners/bootnodes back; verify Ethash (OpenCL) path is in
   use, not the silent CPU fallback.
4. If difficulty is stuck too high, coordinate a **governed difficulty adjustment** — there is no
   mainnet sudo; use the governance path. Testnet may use its functional ops sudo for speed.
5. Do **not** hand-edit chain DB.

### 6.2 Chain split / zombie chain (SEV1)
*Symptom:* nodes report different best hashes; two coexisting heights.
*Actions:*
1. Freeze writes; capture best hash per node and their peer graphs.
2. Identify the divergent block and the node/miner set that produced it.
3. Isolate the minority partition (stop those bootnodes) so the network converges on one fork.
4. Publish the canonical chain hash; have operators resync.
5. If the split is caused by a runtime/consensus bug, prepare a **governed runtime upgrade**, and
   in the meantime document manual mitigation.

### 6.3 Supply / accounting anomaly (SEV1)
*Symptom:* `TotalMinted` jumps unexpectedly, disagrees with `BlocksMined × reward`, or exceeds
the **1B cap**; treasury balance mutates outside the pallet.
*Actions:*
1. Snapshot `gpn_token::TotalMinted`, treasury balance, and recent block rewards.
2. Compare against `initial_minted = 200M (treasury) + 50 GPN/block`. Any divergence is a P0.
3. **Invariant:** `TotalMinted ≤ 1_000_000_000 GPN`; issuance = treasury allocation + mined.
   If violated, treat as a critical runtime bug — freeze, preserve evidence, prepare an urgent
   governed fix. Do **not** paper over with a manual mint.
4. Do **not** touch the proof bytes or the value-balance verification key as a "fix".

### 6.4 Treasury governance anomaly (SEV1)
*Symptom:* unexpected `SpendProposed/Executed`, roster/threshold change, or a suspected roster-key
compromise.
*Actions:*
1. Enumerate pending proposals (`gpn_treasury proposals`) and approvals.
2. If a proposal is malicious, coordinate roster members to **not** approve / to stand down; if a
   proposal has already executed, the funds are gone — escalate to legal/comms.
3. If a roster key is compromised, do **not** assume you can remove it: roster/threshold changes
   themselves require governance approval. Convene the honest roster, and if the threshold can no
   longer be trusted, treat as key-compromise (6.5) and plan a governed runtime/spec response.
4. Remember nothing unlocks at launch — the first spendable tranche is at block **1,036,800**, so
   early-window exposure is low *by design*.

### 6.5 Key compromise (SEV1)
*Triage which key:* binary/release signing key, bootnode node-key, treasury roster member key,
oracle-operator key, agent/marketplace signing key.
*Actions:*
1. Treasury roster key → §6.4. Oracle key → de-list via governance `add_operator`/removal per the
   oracle-governance spec. Bootnode node-key → rotate key file, republish peer id.
2. Release/binary key → stop distributing that artifact, publish the compromise, re-cut a signed
   release. (Never sign with a key you cannot verify.)
3. Rotate, revoke, re-issue. Preserve evidence. Assume anything the key could sign is suspect.

### 6.6 Oracle feed manipulation or staleness (SEV2)
*Symptom:* `CurrentFeed` outliers, stale `LastUpdateBlock`, or agents auto-switching on bad data.
*Actions:*
1. Check `OracleOperators` membership and per-operator last-update; cross-check the outlier
   threshold (20% bps) — the pallet rejects outliers, so a *stale* feed is more common than a
   single corrupted one.
2. De-list misbehaving operators via governed `add_operator`/removal; ask honest operators to
   re-post.
3. Clients: agents should treat a stale feed as an error and fall back to safe defaults; if they
   don't, that's a bug to fix (see gaps in `design/protocols/05-oracle-governance.md` §6).

### 6.7 Marketplace escrow stuck / payout bug (SEV2)
*Symptom:* jobs stuck in Accepted/Started/Submitted; escrow not released; fee split wrong.
*Actions:*
1. Inspect the job lifecycle events and escrow balance for affected job ids.
2. Distinguish a **client/provider** issue from a **pallet** issue (deterministic state).
3. If pallet logic is wrong, gather reproduction steps and prepare a governed runtime fix.
4. Do not "fix" balances by moving funds outside the pallet.

### 6.8 Infrastructure compromise (explorer / RPC / faucet) (SEV2)
*Symptom:* defaced explorer, exposed RPC abuse, faucet draining.
*Actions:* take the service down, rotate its signing/API keys, redeploy from the pinned release,
post a status note. Reminder: faucet is **testnet-only**; if it exists on a mainnet host, that's
already a finding.

### 6.9 Testnet-only bug (SEV3)
Fix on testnet first, add a regression test, re-run the two-node GPU p2p testnet + the
mine→attest→paid-job loop, commit, then promote. Testnet's ops sudo may be used for speed.

---

## 7. Recovery & post-incident

1. Confirm **stable for 2× the incident duration** before declaring resolved.
2. Ensure a **regression test** exists for the root cause (property/target where possible — see
   the fuzz/differential item in the work plan).
3. Update runbooks/docs with what was learned; file follow-up work.
4. Publish the post-incident write-up (§5).

---

## 8. Non-negotiable invariants

- The **shielded-pool proof bytes** and the **value-balance verification key** must not change
  without explicit sign-off (on-chain compatibility).
- The **1B GPN cap** holds: `treasury_allocation (200M) + all mined issuance ≤ 1B`.
- The treasury is **keyless**; funds move only via 13-of-25 governance within unlocked tranches.
- Mainnet sudo is **inert**; never re-introduce a usable privileged key without governed approval.
