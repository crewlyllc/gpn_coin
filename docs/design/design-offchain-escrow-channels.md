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

---

# v2 — the AP-hub protocol (implemented)

**Status:** IMPLEMENTED 2026-10-21 · on-chain primitives unchanged (pallet
`compute-marketplace`, spec 13) · **no runtime change, no genesis, no WASM**

Section 4 sketched "an AP can act as a hub". This section is the concrete
protocol, the wire format, and the guarantees. Code:

| Piece | Location |
|---|---|
| Message schemas, signing payloads, both state machines | `gpn-agent/src/hub_channel.rs` |
| AP hub: channel reuse/open/settle, off-chain routing | `gpn-agent/src/hub_ap.rs` + the `/jobs` branch in `gpn-agent/src/bin/gpn_scheduler.rs` |
| Provider: `POST /channel/job` | `gpn-agent/src/bin/gpn_llm_provider.rs` |
| Client: deposit / withdraw / inspect | `gpn-agent/src/bin/gpn_channel.rs` (`open`/`close` + `deposit`/`withdraw`/`info`/`id`) |
| On-chain reads (channel index, channel + escrow) | `ChainClient::get_channel_id` / `get_channel` |
| Integration test | `scripts/test-channel-e2e.sh` |

## 11. Roles and channels

Three parties, two kinds of channel:

```
   client ──[channel A: client → AP]──► AP ──[channel B: AP → provider]──► provider
           (optional; the AP's own      |
            hot wallet may be the       |  only channel B is needed for the
            requestor on testnet)       |  AI path to leave the block clock
```

- The **AI job path** only needs channel **B** (AP → provider): the AP is the
  requestor and the provider is the provider. That is what removes the per-job
  tx from a job.
- A client that wants to pay per request can open channel **A** to the AP (the
  AP must be a registered provider to be the provider side) and sign one
  payment update per request; the AP then routes off-chain on channel B. On the
  testnet "free AI" mode the AP's hot wallet is the requestor on B and channel A
  is not used. Both channels use the same primitives — no new pallet call.
- `open_channel` enforces **one live channel per (requestor, provider) pair**
  (`ChannelIndex`), so a hub is naturally one channel per peer; multiple hubs
  are just multiple APs with their own channels (permissionless, redundant).

## 12. Message schemas

All messages are JSON on the wire; every signature is over a **canonical,
domain-separated byte string**, never over JSON (field order/float formatting
cannot change a signature).

| Message | Direction | Signed by | Binds |
|---|---|---|---|
| `Assignment` | AP → provider | AP (`sig_requestor` over `gpn/hub-assign-v1`) **+** `sig_settle` over the post-state | channel id, job id, requestor, provider, deposit, `state_before`, `state_after`, model, prompt, max_tokens, temperature, price, deadline, request_nonce (+ optional `billing` rates, item #63) |
| `JobResultMsg` | provider → AP | provider (`sig_provider` over `gpn/hub-result-v1`) **+** `sig_settle` over the result state | channel id, job id, provider, `ok`, output, `result_hash`, prompt/completion tokens, `state_after` |

`Assignment` (abridged):

```json
{ "channel_id": 2, "job_id": 1792609125087,
  "requestor": "0x90b5…fe22", "provider": "0x664c…3d01",
  "deposit": "100000000000000",
  "state_before": {"balance_requestor":"100000000000000","balance_provider":"0","nonce":0},
  "state_after":  {"balance_requestor":"999969986817328","balance_provider":"3013182672","nonce":1},
  "model": "llama3.2:1b", "prompt": "say hi in three words",
  "max_tokens": 24, "temperature_milli": 700, "price": "3013182672",
  "deadline_ms": 1792609305087, "request_nonce": 1,
  "sig_requestor": "0x…64 bytes…", "sig_settle": "0x…64 bytes…" }
```

`JobResultMsg` (abridged):

```json
{ "channel_id": 2, "job_id": 1792609125087, "provider": "0x664c…3d01",
  "ok": true, "output": "Hello!", "result_hash": "0x…",
  "prompt_tokens": 29, "completion_tokens": 4,
  "state_after": {"balance_requestor":"999969986817328","balance_provider":"3013182672","nonce":1},
  "sig_provider": "0x…64 bytes…", "sig_settle": "0x…64 bytes…" }
```

**Signatures.** sr25519 (`ChannelAuth` on chain already verifies sr25519 over
`channel_settle_payload`). Payload hashing is `blake2_256(domain || LE fields ||
u32-len-prefixed strings)`. `sig_settle` is the pallet's
`channel_settle_payload(channel_id, br, bp, nonce)` — the exact 32 bytes
`close_channel` / `force_close_channel` verify. A frozen cross-implementation
vector pins the encoding (both crates assert it), so an agent-generated
signature is accepted on chain.

## 13. Job flow (zero per-job txs)

```
AP                                                              provider
──                                                              ────────
pick provider (existing routing)
ensure channel (open ONCE on-chain, or adopt via ChannelIndex)
price = per-token price × (prompt×input_frac + max_tokens)   # ceiling
Assignment{state_before, state_after=before+price, billing{in,out}, sig_requestor, sig_settle}
      ─────────────── POST /channel/job ───────────────────────►
                                        verify both sigs, nonce, price,
                                        deadline; commit state_after; persist
                                        run inference (off-chain)
                                        sign result at min(actual, ceiling)
      ◄──────────────── 200 JobResultMsg ───────────────────────
verify provider sig over result, result_hash, state_after == before + charge,
  charge <= promised ceiling
advance local state, keep provider's sig_settle for the close
reply to the caller (OpenAI shape + usage)
```

The **only** on-chain txs are `open_channel` (once per provider, amortized over
the channel's whole life) and `close_channel` (when the provider's balance or
the update count crosses a threshold, or on shutdown/expiry).

## 14. Replay, timeouts, disputes

- **Replay / double-spend.** Each `Assignment` carries a strictly increasing
  per-channel `request_nonce` **and** a unique `job_id`; the provider rejects a
  repeated nonce or job id, and rejects a `state_before` that is not its current
  state. Each `JobResultMsg` advances the channel `nonce`; the pallet rejects
  any settle with `nonce <= on-chain nonce`, so a stale state can never be
  replayed on chain.
- **Timeouts.** `deadline_ms` is wall clock: the provider refuses an expired
  assignment, the AP drops pending entries after it (no value moved — the AP's
  channel state only advances when a *result* applies). The channel's own
  `deadline` is the block-height TTL supplied at open; after it, either party
  may `force_close_channel` with the counterparty's latest signature.
- **Dispute.** The dispute is the chain: submit the **latest mutually-signed
  state**. Because each update is signed by both parties, "latest" is
  unambiguous (highest nonce). A provider that did the work holds an
  AP-signed post-state and can force-close after the deadline; an AP that paid
  can prove what it authorised. There is no slashing concept to add: the chain
  only ever sees a state both agreed to.
- **Failure/refund.** A job that fails produces a provider-signed **refund**
  state (before-balances, nonce+1), which the AP applies; a provider cannot
  charge for work it did not deliver, and the AP cannot claw back a charge the
  provider already signed.
- **Untouched-channel reclaim.** If a provider never co-signs anything, the
  requestor can `refund_expired_channel` only after `ChannelRefundDelay` past
  the deadline (finding #30) — the provider's window to force-close a co-signed
  claim first.

## 15. Why the AP cannot steal

The AP is the party holding the money side, so this is the property that
matters:

1. **The payment is pre-signed, not post-approved.** `Assignment.sig_settle` is
   the AP's signature over the *post-job* state. As soon as the provider accepts
   the assignment it holds a signature that pays it `price`, and it can
   `force_close_channel` with it after the channel deadline — even if the AP
   disappears. The AP cannot "use the work and not pay".
2. **Provider balance is monotone.** The AP only ever accepts (a) exactly the
   pre-authorised `state_after` or (b) a provider-signed refund to the pre-job
   balances. Any state that lowers `balance_provider` is rejected
   (`ProviderBalanceDecreased`), so there is no clawback path.
3. **No unilateral close.** `close_channel` needs both signatures;
   `force_close_channel` needs the **counterparty's** signature over the exact
   final state; `refund_expired_channel` works only on an untouched channel and
   only after the dispute window. There is no way for the AP to extract escrow
   while a provider holds a co-signed claim.

The dual property — the provider cannot overcharge — holds because the
assignment fixes `state_after == state_before + price`, `price` is signed, and
the AP checks the provider's signature over the result before applying it.

## 16. e2e budget (measured)

What is left on the job path: routing (in-process), one HTTP round trip to the
provider, the provider's inference, one HTTP response. **No block wait.**

| Path | Measured |
|---|---|
| First job after opening a channel | 89.8 s *(includes the one-time `open_channel` block inclusion; on this testnet a single slow seal routinely takes 20–110 s)* |
| Steady-state job over an open channel | **0.18 s – 0.37 s** (llama3.2:1b / qwen2.5:0.5b, 16–24 tokens) |
| On-chain escrow path (today) | ~15–20 s typical, 56–83 s with a retarget gap (`TOOLS.md`) |

The steady-state number is the claim: **two orders of magnitude**, with the
block time removed from the path. `open_channel` is amortized (one per provider
per channel lifetime, TTL default 25 920 blocks ≈ 4.5 days) and `close_channel`
happens once per settlement batch.

## 17. Operating modes (safe fallback)

| Env | Default | Meaning |
|---|---|---|
| `GPN_ESCROW_MODE` | `channel` | `channel` = try the off-chain path first; `onchain` = never use channels |
| `GPN_HUB_CHANNEL_DEPOSIT` | `100 GPN` (1e14) | deposit locked when a hub channel is opened; `0` disables the AP hub entirely |
| `GPN_HUB_CHANNEL_TTL` | `25920` blocks | channel deadline (the force-close window) |
| `GPN_HUB_CLOSE_AT` | `deposit/2` | settle on chain when the provider balance reaches this |
| `GPN_HUB_CLOSE_NONCE` | `500` | ...or after this many off-chain updates |
| `GPN_HUB_STATE` | `~/.gpn/hub_channels.json` | AP channel registry (survives restarts) |
| `GPN_HUB_JOB_DEADLINE_MS` | `180000` | per-job deadline (inference + return) |
| `GPN_PROVIDER_CHANNEL_STATE` | `~/.gpn/provider_channels.json` | provider channel state (persisted before inference runs) |
| `GPN_CHANNEL_SKIP_CHAIN_CHECK` | `0` | dev only: trust the assignment's deposit without verifying the channel on chain |

The provider exposes two endpoints on `status_port + 1`:
`GET /channel/status` (capability probe: `{ok, channel, provider, channels[]}`)
and `POST /channel/job` (the assignment).

**The fallback is per-request and silent-safe.** The channel path is attempted
only when all of the following hold: mode is `channel`, the job is a text job,
the provider's heartbeat port yields a derivable channel endpoint
(`status_port + 1`), and — when no channel exists yet — the provider answers
`GET /channel/status` with `{"channel":true}`. That probe is what stops the AP
from **locking a deposit against a provider that cannot serve channel jobs**
(an older build 404s the probe; no `open_channel` is submitted). Any error — connect
refused, 404 (provider built without channel support), bad signature, timeout —
marks that provider "unsupported" for 60 s and the job continues down the
**existing on-chain escrow path**. A job is therefore never lost by enabling
channels; the worst case is the old latency.

Provider side: `POST /channel/job` is served by the same loopback server as the
chain-free `/infer` endpoint (`status_port + 1`), so a NAT'd provider needs no
new inbound port — the AP already reaches it through the same
address it uses for `/infer`. Providers that are only reachable by pull have no
inbound endpoint and therefore simply never get channel jobs (fallback).

## 18. What remains

- **Client → AP channel (A)** is supported by the primitives (the AP registers
  as a provider, the client uses `gpn_channel deposit`/`sign`) but the AP does
  not yet *require* it — on testnet the hot wallet is the requestor.
- **Watchtower.** Not implemented. The provider/AP each persist their latest
  signed state; a watchtower would submit it if the peer vanished. The dispute
  window in the pallet already gives the wronged party a race-free chance to
  act while online.
- **Routing fee.** A hub fee (bps) is not charged yet; it can be taken out of
  the per-job price the AP signs.
- **Multihop.** Hub-to-hub forwarding (A→AP1→AP2→provider) needs the HTLC-style
  conditional transfer sketched in §4; the single-hub path is what shipped.
