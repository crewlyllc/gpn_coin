# GPN Pricing — how to set the price of a job

## The two anchors
Price has to satisfy two constraints at once:

1. **Floor — provider economics (cost-plus).** A provider must cover:
   - GPU time (the dominant cost)
   - electricity (watts × $/kWh)
   - amortized hardware (GPU $ / lifetime hours)
   - model download (one-time, amortized over jobs)
   - a **margin** so running a node is worth it
   If the price is below the floor, supply disappears.

2. **Ceiling — market parity.** What a centralized provider (OpenAI, RunPod,
   Replicate) charges for comparable work. If GPN is above it, no one uses it,
   except for the decentralization/privacy/sovereignty angle.

**GPN's price should sit between the floor and the market, closer to the floor
for adoption, and it should float with supply/demand.**

## The unit: price per token
Bill on **output tokens** (and optionally input tokens), because that's what
correlates with GPU time and is what the user values:

```
charge = in_rate × prompt_tokens + out_rate × output_tokens
escrow = in_rate × max_input  + out_rate × max_output     ← the cap
```

## The formula
```
rate(model, load) = base_rate × size_factor(model) × demand_factor(load) × quality_factor
```

- **`base_rate`** — GPN per token for a reference model (say `llama3.2:1b`).
- **`size_factor`** — scales with model size: `1B → 1×`, `8B → ~4×`, `70B → ~30×`
  (roughly GPU-seconds, not params — a 70B is ~30× the compute of a 1B).
- **`demand_factor`** — a spot multiplier from the scheduler: `1.0` balanced,
  `>1` when GPUs are scarce/queued, `<1` when supply is idle (incentivizes filling
  idle time).
- **`quality_factor`** — optional: attested/verified providers (Gold tier,
  high uptime) can command a premium.

## How to set `base_rate` concretely
Pick a **fiat anchor**, then convert to GPN at the token's price:

> "1M output tokens of llama3.2:1b ≈ $0.005–0.02" (between the raw cost floor
> and what a hosted model charges). Convert at the GPN/USD rate → `base_rate`.

Then a bigger model scales up by `size_factor`. Example anchor table
(illustrative, tune to real GPU benches):

| Model | size_factor | out rate (GPN/token) | ~1M tokens |
|---|---|---|---|
| `qwen2.5:0.5b` | 0.6× | r × 0.6 | cheap |
| `llama3.2:1b` | 1× | base | base |
| a 8B | 4× | r × 4 | 4× |
| a 70B | 30× | r × 30 | 30× |

## How it behaves on-chain
- The requestor picks a **rate** + a **max_tokens**; the escrow is `rate × cap`.
- The provider meters the **actual** tokens; the charge is `rate × actual`.
- **Surplus refunds** automatically (that's the per-token escrow we just built).
- So the client is never overcharged past the cap, and pays only for what's used.

## Who sets it (governance)
Three options, in increasing decentralization:
1. **Protocol-set rate card** (governance sets `base_rate` + the size table) — simple,
   predictable; a parameter update.
2. **Provider-set prices** (each provider advertises its rate in the heartbeat; the
   scheduler routes on price × latency) — market-driven, needs the scheduler to
   rank on cost.
3. **Free market** (providers post asks, requestors post bids) — most decentralized,
   most complex.

**Recommendation:** start with (1) a governance rate card + a demand multiplier,
then graduate to (2) provider-advertised rates once there's enough supply.

## Guardrails
- A **min/max rate** clamp so nobody prices at 0 (spam) or absurdly high.
- The **escrow cap** prevents runaway spend (already enforced).
- **Token metering** trust: v1 the provider declares tokens (≤ cap); add a
  recount/challenge later.
- **Free tier:** a small treasury-funded quota keeps onboarding alive without a
  permanent hot-wallet bleed (the testnet "free AI" productized).
