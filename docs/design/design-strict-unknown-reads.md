# Strict reads — an unreadable read is `Unknown`, never `0`

**Status:** implemented (`gpn-agent/src/strict.rs`), items #100–#111.
**Enforced by:** `cargo test -p gpn-agent --test strict_reads_no_collapse`.

## The bug class

Every defect in the #100–#111 family had the same shape: a **failed read** was
collapsed into a perfectly ordinary value, and a decision was then taken on
that value as if the chain had reported it.

```rust
let balance = read_balance().await;            // 0.0 — node down
let gflops  = record.map(|(_, f, _)| f).unwrap_or(0); // 0 — and we advertise it
let feed    = read_feed().await.ok().flatten(); // None — or a dead socket
let nonce   = get_nonce(addr).await;            // 0 — then we sign with it
```

`None`, `0`, `false` and `[]` are **legitimate answers from a chain that was
read** (unfunded account, fresh chain, unattested provider). Using them for
failure too is unrecoverable: the caller cannot tell "the chain says no" from
"we never asked the chain", and both the log and the charge look normal.

## The rule

1. A read you cannot complete is `Strict::Unknown(reason)` — never a value.
2. A **publication** path publishes **nothing** (or `null`) on `Unknown`, and
   carries the reason (`chain_read_error`, `chain_attestation: "unknown"`).
   It never republishes a stale value.
3. A **decision** path (charge, sign, register, claim, route, switch mode) takes
   no decision on `Unknown`; it skips the tick and says why.
4. A dead or never-established connection is *"not yet"*, not *"never"*: use
   `strict::retry_read` (backoff, capped) instead of latching the failure.
5. Scope is **decision-bearing reads only** — money, identity, capacity, feeds
   and prices. Cosmetic/logging/diagnostic reads may keep `Option`.

## API

```rust
pub enum Strict<T> { Known(T), Unknown(String) }

Strict::known(v)                  Strict::unknown(site, why)   // logs once
Strict::from_result(site, res)    Strict::from_option(site, opt, why)
.map(f)  .and_then(f)  .is_known()  .reason()  .into_option()

// display/logging ONLY (the name is the fence):
.or_default_for_display_only()    .or_for_display_only(placeholder)

// shared retry for the #105/#111 shape:
retry_read(site, RetryPolicy::default(), || fut).await   // 3 tries, 250ms→2s
retry_read_blocking(site, policy, || fallible_sync_call)
```

There is deliberately **no** `unwrap_or*`. `impl Default for Strict<T>` is
`Unknown`, so `Default::default()` can never fabricate a `Known`.

## Converted sites

| Site | Was | Now |
| --- | --- | --- |
| `chain::signing_nonce` (all `submit_*`) | nonce `0` on failure | refuses to sign; retries first |
| `chain::get_feed_strict` / `get_difficulty_strict` | fallback constants | three states preserved |
| `profitability::analyze` | `FALLBACK_DIFFICULTY`, stale-as-fresh | error: no mode decision |
| `main` registration (`registration_gate`) | bond spent with `1 GPU / 0 GFLOPS` | skip on `Unknown` |
| `main` claiming (`claim_gate`) | claim at `0 GFLOPS` | skip on `Unknown` |
| `GET /provider/{addr}` (`provider_payload`) | `false/false/"Unattested"` | `null` + `chain_read_error` |
| heartbeat `advertised_gflops` | fell through to `--gflops` | `gflops=0` + `chain_attestation:"unknown"` |
| `gpn_llm_provider` / `gpn_diffusion_provider` claim | `0 GFLOPS` window | skip the pass |

## Deliberately not converted

* Peer id / multiaddrs: already publishes `null` / `[]` explicitly on failure
  (see `attach_p2p` + its tests) — converting would add nothing.
* `gpn_explorer` (diagnostic CLI) and the demo binaries: display only.
* `ChainClient::get_*` value-form accessors stay as thin wrappers for those
  display callers; every decision path uses the `_result` form.
