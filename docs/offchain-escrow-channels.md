# Off-chain escrow channels — operator & API guide

**Status:** LIVE on the testnet access point (`:8092`), default payment mode for
`/v1/chat/completions`.
**Design:** [`docs/design/design-offchain-escrow-channels.md`](design/design-offchain-escrow-channels.md)
**Code:** `gpn-agent/src/hub_channel.rs` (wire format + both state machines),
`gpn-agent/src/hub_ap.rs` (AP side), `gpn-agent/src/bin/gpn_llm_provider.rs`
(provider side, `POST /channel/job`).

---

## 1. The problem it solves

The on-chain path locks escrow per request:

```
create_job (escrow) → block inclusion → accept → start → submit_result → complete
```

Every arrow waits on a block. On this testnet the measured block cadence is
p50 ≈ 12 s, p95 ≈ 54 s, so a chat request pays **tens of seconds of pure chain
wait before inference even starts**. Measured on the live AP, same model and
prompt:

| mode | p50 | p95 |
|---|---|---|
| `passthrough` (no billing) | **72 ms** | 73 ms |
| `channel` (off-chain escrow) | **73 ms** | 219 ms |
| `settle` (on-chain escrow) | **37 570 ms** | 89 812 ms |

(n = 5 each, `llama3.2:1b`, `max_tokens: 16`, box-local AP → box-local provider.)

A channel moves the money **once per channel**, not once per job: `open_channel`
locks a deposit on chain, then each job is a pair of signatures
(`Assignment` → `JobResultMsg`). The chain only sees open/close (and disputes).

---

## 2. The three modes

`POST /v1/chat/completions` accepts `"mode"` in the body:

| `mode` | What it does | Money | Use it when |
|---|---|---|---|
| `"channel"` *(default)* | Off-chain signed settlement against a hub channel. No `create_job`, no block wait. | Escrow locked in the channel; per-job transfer is two signatures. | Normal API traffic. |
| `"settle"` / `"onchain"` | Classic per-request on-chain escrow (`create_job` → claim → settle). | Escrow + settlement on chain, per job. | Auditing, debugging, or when you *want* a per-job on-chain receipt. |
| `"passthrough"` | Direct provider `/infer` call. | **Nothing** — free/unbilled, nothing recorded. | Latency probes and the free tier. Not a market: no payment, no receipt. |

`POST /jobs` (the native, non-OpenAI endpoint) uses the same machinery; it has
no `mode` field and always prefers a channel, falling back to on-chain escrow.

Responses say which path paid for the request:

```json
"gpn": {
  "payment": "channel",
  "channel_id": 5,
  "job_id": "1792695433200",
  "channel_balance_provider": "2663953986048",
  "channel_charge": "2119018",
  "channel_charge_ceiling": "2663953986048"
}
```

`payment` is `channel`, `onchain`, or `passthrough`. `channel_charge` is what the
job actually cost and `channel_charge_ceiling` is the maximum the AP committed
to it (see §2.1).

### 2.1 Billing: actual usage, capped by the committed ceiling

One price card is used by all paid modes (`gpn_agent::pricing`): the caller's
`max_tokens` (default **256**, clamped to 4096) sets the *output allowance* and
the escrow commitment is `prompt_tokens × GPN_INPUT_RATE_FRAC + max_tokens`
output-equivalents (input default 0.15 × the output rate).

| | Commitment (ceiling) | Charge |
|---|---|---|
| on-chain (`settle`) | `rate × max_tokens` locked in `create_job` | `rate × min(actual_tokens, cap)`; the pallet refunds the difference on settle |
| channel (default) | `price` in the AP-signed `Assignment` | provider signs `min(in_rate·prompt + out_rate·completion, price)`; the unused ceiling stays with the requestor in that same state |

So **the ceiling is a cap, never the bill.** Measured before the fix: a 4-token
answer was billed 1 297 268 757 units (0.001297 GPN ≈ 512 output tokens) for
*any* `max_tokens`, because the AP ignored the request and committed the
provider's internal 512 default — a ~58× over-charge. After the fix the same
answer costs its actual tokens and `max_tokens=8` / `64` / `256` produce three
different charges.

The escrow ceiling is sized from an **estimate** of the prompt (`4 chars/token`
plus `pricing::CHAT_TEMPLATE_TOKENS = 32` for the role/marker framing the
provider's tokenizer adds), because the provider's real count is only known
after it runs. Getting that estimate too low does not over-charge anyone, but it
does *cap* a short job at the estimate — measured live with `max_tokens=8`: a
42-token prompt was estimated at 15, so the charge was clamped to the ceiling
instead of reaching its usage. Hence the template allowance.

How the channel does it (`hub_channel::BillingRates`):

* The AP puts `in_rate` / `out_rate` (base units per token) in the assignment;
  they are inside the signed pre-image, so neither side can swap the rate after
  the price is agreed.
* The provider reports `prompt_tokens` + `completion_tokens` — already inside the
  signed `JobResultMsg` payload — and settles to `state_before + charge` on the
  **same nonce** the ceiling state would have used. No extra round-trip, no new
  channel update, no extra nonce.
* `min(actual, ceiling)` is safe with **no new trust assumption**: under-reporting
  only costs the provider (it is paid for what it reports) and over-reporting is
  bounded by the ceiling the requestor pre-signed. It is strictly better than
  charging the ceiling.
* If the backend reports **no usage**, the provider falls back to the ceiling and
  logs it (`backend reported no usage — billing the ceiling`); the AP logs
  `provider reported no usage — charged the full ceiling`. A missing measurement
  is never a silent free job.
* A pre-fix provider (one that does not understand the rates extension) hashes
  the assignment differently and rejects it at accept-time with HTTP 4xx — a
  `NotAttempted` fallback to on-chain escrow, never a silent over-charge.

### Pricing is identical in both paid modes

Both paid paths use the same price card, so switching modes can never become a
way to pay less (or be charged more): the ceiling is
`per_token × (prompt_tokens × input_frac + max_tokens)` and both paths settle at
the provider's reported usage bounded by that number.

---

## 3. Safety properties (all preserved off-chain)

| Property | How |
|---|---|
| **Escrow before work** | The AP signs `Assignment{state_before, state_after = before + price, sig_settle}` *before* the provider runs anything. The provider `accept()`s (verifying both signatures, the exact price, the deposit conservation and nonce monotonicity) and **persists that state before inference**. The AP's `sig_settle` over the post-job state is the provider's liveness proof: if the AP goes dark, the provider force-closes after the channel deadline and still gets paid. |
| **Result binding** | `JobResultMsg.result_hash` = `sha256(output)`; the AP recomputes it and refuses a mismatch. |
| **Replay guard** | Two independent counters: per-channel `request_nonce` (strictly increasing, provider rejects `<=` its last) **and** a unique `job_id`; every state update advances the channel `nonce` (the pallet rejects `nonce <= on-chain nonce`). Settled job ids are remembered, so `complete_job` on an already-settled job returns `UnknownJob`. |
| **No AP clawback** | A settlement state is either exactly `state_before + price` (the ceiling) or `state_before + charge` with `charge ≤ price` on the same nonce — the latter is only ever signed by the *provider* (billing itself less), so the AP still cannot lower the provider's balance. `balance_provider` can therefore never decrease without the provider's signature. |
| **No double settlement** | A channel job never gets an on-chain id (channel job ids are AP-local millisecond values and are never passed to `create_job`), so there is nothing for the on-chain path to settle. Within the channel, `complete_job` consumes the pending entry — a replay is refused. |

### Failure semantics — what the caller sees

The AP distinguishes two failure classes, because they have different money
consequences (`hub_ap::ChannelFail`):

| Class | When | AP behaviour |
|---|---|---|
| `NotAttempted` | hub disabled, no channel capacity, provider does not serve the channel protocol (`/channel/status` false or a 60 s negative cache), connect error, HTTP **4xx** at accept-time, or the provider signed a **refund** after failing mid-run | Pending entry is aborted, **then the request automatically falls back to on-chain escrow**, logged: `[chat] channel path unavailable (<reason>) — falling back to on-chain escrow`. No value moved, so the caller is charged once. |
| `Ambiguous` | HTTP **5xx** (provider may have accepted and crashed), or a 200 whose body will not parse/verify | **No fallback.** The AP returns **502** with `channel settlement failed ambiguously: …; not retrying on-chain to avoid a double charge`. The pending entry is left to expire after `GPN_HUB_JOB_DEADLINE_MS` (default 180 s). Rationale: the provider may hold the AP's pre-signed `sig_settle` and could force-close; paying on-chain as well would charge the caller twice. |

Provider-side failure is a **refund**: `ProviderChannel::sign_refund` reverts the
balances to the pre-job state with a fresh nonce (`state.nonce += 2` for the
job). The AP applies it, so the caller pays nothing for the failed attempt and
falls back on-chain if it wants a retry.

Escrow on a channel can never be silently lost: the AP's `hub_channels.json`
and the provider's `provider_channels.json` both persist the signed state, and
the latest mutually-signed state is closeable on chain at any time.

---

## 4. Lifecycle & operations

**Open** — once per `(AP, provider)` pair, on the first job that needs it:
`open_channel(provider, deposit, ttl)`. The AP then adopts the channel from
`ChannelIndex`, so an AP restart re-uses it instead of re-funding.

**Per job** — no extrinsics at all.

**Close** — a background settler thread closes cooperatively when the provider's
off-chain balance reaches `GPN_HUB_CLOSE_AT` (default deposit/2) or the update
count reaches `GPN_HUB_CLOSE_NONCE` (default 500). The next job opens a fresh
channel. A channel whose provider never returns a result simply ages out; the
deposit stays on chain and is recoverable.

### Environment

| Variable | Default | Meaning |
|---|---|---|
| `GPN_ESCROW_MODE` | `channel` | `channel` = enable; `onchain`/`off` = disable the module entirely (AP and provider). |
| `GPN_HUB_CHANNEL_DEPOSIT` | `100 GPN` | Deposit locked per channel open. |
| `GPN_HUB_CHANNEL_TTL` | `25920` blocks (~4.5 d) | Channel lifetime. |
| `GPN_HUB_CLOSE_AT` | deposit/2 | Settle on chain once the provider's balance reaches this. |
| `GPN_HUB_CLOSE_NONCE` | 500 | …or after this many off-chain updates. |
| `GPN_HUB_JOB_DEADLINE_MS` | 180000 | Per-job deadline (also the HTTP timeout for `/channel/job`). |
| `GPN_HUB_STATE` | `~/.gpn/hub_channels.json` | AP-side channel registry. |
| `GPN_PROVIDER_CHANNEL_STATE` | `~/.gpn/provider_channels.json` | Provider-side channel state. |
| `GPN_CHANNEL_SKIP_CHAIN_CHECK` | off | Dev only: accept a channel without verifying it on chain. |

**Provider requirement:** the provider must serve `POST /channel/job` and
`GET /channel/status` on `heartbeat_port + 1` (8093 → 8094). The AP probes
`/channel/status` **before** opening (and funding) a channel, so an older
provider build never costs a locked deposit; it stays on-chain and is retried
after 60 s.

### Health checks

```bash
curl -s localhost:8094/channel/status      # provider: {"channel":true,"channels":[5]}
jq '.channels[] | {channel_id, state}' ~/.gpn/hub_channels.json
jq '.[].accepted | length' ~/.gpn/provider_channels.json   # must be 0 when idle
```

AP and provider state must match (`balance_requestor`, `balance_provider`,
`nonce`). `accepted` / `pending` must be empty once traffic stops — a non-empty
entry is either in flight or waiting to expire.

---

## 5. Known limits

* **The AP is trusted for liveness, not for correctness.** It pre-signs the
  post-job state, so a provider can always collect for work it did; but a
  provider that goes dark after `accept` holds a payment it never earned until
  the job deadline expires the pending entry (funds stay in the channel, not
  lost — the AP simply does not advance its own state).
* **Channel capacity is a working balance**, not a per-user limit: it is the
  AP's deposit. When it is exhausted (`InsufficientChannelBalance`) the request
  falls back on-chain — automatically and logged.
* **One GPU, one job:** the provider serialises jobs, so channel latency under
  concurrency is bounded by inference, not by the channel.
* **Rolling back** is a config change, not a data change: `GPN_ESCROW_MODE=onchain`
  on both sides returns every request to per-job on-chain escrow. The on-chain
  channel remains open and can be closed cooperatively at any time.
