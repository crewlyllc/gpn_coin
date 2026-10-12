# GPN — Off-Chain Escrow (Payment Channels) — Design

**Status:** DESIGN (Jeremy-approved to pursue 2026-10-07) · **Owner:** main / PM07
**Goal:** keep escrow **trust-minimized** while moving it **off-chain**, so an AI job no longer
waits on a block. **Effect: the per-job block delay (from mining cadence) is removed from the AI path.**

## 1. Problem
Today every job is escrowed/settled **on-chain**, one tx per step:
`create_job`(escrow) → `accept_job` → `start_job` → `submit_result` → `complete_job`. Each step
stop-and-waits on the chain (~1 block), so an AI job's e2e is **block-bound** (the fast path just
minimized, not removed, this). With ~4–6 s blocks, that's seconds of pure chain latency per job.

## 2. Idea
Use **payment channels** (a.k.a. state channels): lock funds **once** on-chain, then move value
**off-chain** with signed state updates per job. The chain only sees **channel open/close** (and
disputes) — **not** each job.

```
 open (on-chain, rare)                per-job (OFF-CHAIN, instant)                close (on-chain, rare)
requestor ──deposit──► [channel]  ◄──signed balances / job receipts──►  [channel] ──settle──► requestor
                        escrow-free                                               provider
```

## 3. Trust model (why it stays trust-minimized)
- Funds are locked on-chain in the channel. Neither party can move them unilaterally.
- Each job updates the channel state by **two signatures** (requestor's "release N for job J",
  provider's "J completed with result-hash H"). The latest signed state is settleable on-chain.
- **Either side can force-close** with the latest signed state → no party can cheat.
- **The chain remains the root of trust**: open, close, dispute resolution, and (if we want) slashing.
  We are moving the *per-job money movement* off-chain, not the *trust*.

## 4. Mechanics
- **Channel:** `(requestor, provider)` pair with balances `(a, b)`, a `nonce`, and a timeout.
  - **Open:** one on-chain `open_channel(provider, amount, ttl)` → funds locked (`gpn/cmkt`).
  - **Per job:** off-chain signed update: `state_n = sig_req(balance_req -= price) ; sig_prov(... )`.
    The job's *result* is a separate off-chain receipt `sig_prov(job_id, result_hash, salt)` that the
    requestor verifies (matches the on-chain workload commitment) before signing release.
  - **Close:** cooperative `close_channel(balances, nonce, sigs)` (instant); or **force-close** after
    `timeout` by submitting the latest signed state; the other side may dispute with a newer state
    within a **challenge window**, else funds settle as submitted.
- **Per-job result integrity:** the requestor holds the provider's signed
  `(job_id, result_hash, salt)` receipt; the on-chain `workload_hash` commitment (already in the
  pallet) binds it, so a provider can't deliver garbage and a requestor can't deny a delivered job.
- **Routing / hub (the AP role):** a requestor needn't pre-open a channel with *every* provider — an
  **access point can act as a hub**, holding one channel to each side and forwarding value
  (HTLC-style). Direct requestor↔provider channels also allowed. Hubs are permissionless + multiple.
- **Fees:** the hub/AP takes a tiny routing fee (bps) out of the stream; the 5% protocol fee split
  (staking/treasury/burn/oracle) can be applied on the periodic on-chain settlement instead of
  per job.

## 5. What stays on-chain vs moves off-chain
| On-chain (root of trust) | Off-chain (fast) |
|---|---|
| Channel open / close / force-close | Per-job payment (signed balance updates) |
| Provider registry + attestation + capability gate | Job assignment / routing |
| `workload_hash` commitment (result binding) | Actual inference / image / tune compute |
| Disputes + (optional) slashing | Result delivery + receipts |
| Periodic net settlement | Metering / token counts |
| Token / staking / governance | Prime channel housekeeping |

**Net:** the **AI job path has zero per-job txs** → no block delay. Chain interaction is amortized
(open/close/settle occasionally) + rare disputes.

## 6. Alternatives (if channels are too much to start)
- **Bond + arbitration:** no locked escrow; parties post a bond, disputes go on-chain (slashing).
  Lighter, but needs a dispute/evidence path.
- **Signed IOU + batched net settlement:** requestor signs per-job IOUs; provider net-settles
  periodically. Simplest; trust = requestor credit.

## 7. Tradeoffs / risks
- **Liquidity:** channels must be funded; hubs need capital + routing.
- **Liveness:** the requestor must be online to co-sign releases (or delegate to a hub/relayer).
- **Hub centralization:** mitigate with multiple independent hubs + direct channels (same posture as
  access points — permissionless, redundant).
- **Complexity:** channel state machine + dispute window + watchtowers (to penalize stale states).

## 8. Impact / why it matters
- **Removes the block-bound latency from AI** (Jeremy's point): the job no longer waits for
  create/accept/settle txs.
- **Much higher throughput / lower cost:** no per-job on-chain tx.
- **Keeps trustlessness:** the chain is still the arbiter; only custody-per-job moves off-chain.
- **Composes with #1 (on-chain provider discovery):** discovery/registry stay on-chain; payments go
  off-chain.

## 9. Build order (when picked up)
1. On-chain: `open_channel` / `close_channel` (or reuse `gpn-token` as the channel escrow account) +
   challenge window + fee-on-settle. Runtime upgrade (forkless `set_code`).
2. Agent: channel manager (open/co-sign/close), job receipts, watchtower for stale-state defense.
3. AP hub: routing + liquidity.
4. Migration: run channels alongside per-job escrow; flip the default once verified.

## 10. Open decisions
- Channel (direct pairs) vs hub-first?
- Challenge-window length / dispute bond.
- Watchtower: mandatory or optional?
- Keep per-job on-chain escrow as a fallback mode?
