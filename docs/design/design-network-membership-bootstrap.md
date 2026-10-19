# Design — Network Membership, Bootstrap & Pull-Based Jobs

**Status:** ✅ **APPROVED — v2, 2026-10-19, Jeremy.** Design only; no code yet.
**Author:** PM07 + Jeremy
**v2 changes (supersede v1):** backbone/seed is now the **DEFAULT** (opt-out, not opt-in); every node
ships with **two bootstrap sources** — a hardcoded **≤5-seed list** and a **DDNS/HTTP bootstrap list**;
seed-by-default dialability (UPnP/NAT-PMP, then graceful degradation) is specified; tiers/rewards
rebalanced accordingly.
**v2.1 (2026-10-19, Jeremy):** §3.1 resolved — the 5 seeds are a **dedicated bootstrap set** (option i);
the §5.2 operator notice and the "now dialable" re-check message are finalized verbatim.
**v2.2 (2026-10-19, Jeremy — supersedes the v2 default):** **dialable is REQUIRED by default.**
The network is still growing, so the hard gate is the default posture: a node that cannot be dialled
prints the §5.2 notice and exits `EXIT_NOT_DIALABLE` (78). Opting out is now `--no-require-backbone` /
`GPN_REQUIRE_BACKBONE=0`, which restores the v2 "run degraded" behavior (plus §5.3 re-check).
**Supersedes:** the "every node must be dialable" policy in `node/src/dialability.rs` (queue #45), the
mandatory `POST /assign` dispatch in `design-access-points-scheduler.md` / master-ref §6.1, and v1's
framing of backbone as an *optional* tier.
**Unchanged from v1:** **pull-based job transport** (§4) — providers poll the AP, the AP never dials a
provider. That is orthogonal to membership and is retained verbatim.

---

## 1. Problem & the v2 correction

GPN today hard-requires **inbound reachability to participate at all**:

- `gpn-node/node/src/dialability.rs` — default-ON gate. A node that cannot prove inbound reachability
  within a 120 s grace window **exits with code 78** (`EXIT_NOT_DIALABLE`) *before* it syncs, mines or
  serves. Operator opt-out is `GPN_REQUIRE_DIALABLE=0`.
- `gpn_scheduler` dispatches a job by **`POST /assign` to the chosen provider's `:8093`** heartbeat
  (`dispatch_assignment`). No inbound `:8093` ⇒ the assignment never arrives.

Both are the same policy in two places: *you are a member only if we can dial you.* That excludes home
users behind CGNAT/NAT who will never set up DDNS or a static IP — the exact population GPN needs for
GPU supply.

**v1's correction** was to demote dialability to an *optional* tier and make outbound-only the default.
**v2 goes further, in the other direction:** dialability is still **not a gate**, but it is the
**default ambition**. A node is expected to be a backbone node/seed; it only stops being one when the
operator says so, or when the network physically prevents it (CGNAT with no working mapping). The
difference from v1 is intent and inversion:

| | v1 (superseded) | **v2 (this doc)** |
|---|---|---|
| Default posture | participant (outbound-only) | **backbone / seed** (announce + accept inbound) |
| How you leave the backbone | never joined | explicit `--no-seed` / `GPN_DISCOVERABLE=0`, or automatic degradation |
| Seed candidacy | opt-in by being dialable | **default-on**; `--no-seed` opts out of publication only |
| Failure to become dialable | normal | **fatal by default** — full notice + exit 78; `--no-require-backbone` opts into a loud degraded warning |

Rationale: a network where backbone is *optional* and defaults off converges to a star (everyone behind
NAT, a handful of volunteers carrying all inbound). A network where backbone is the default, and only
the physically-incapable degrade, converges to a mesh. Bitcoin settled the membership question in 2009
(outbound-only nodes are full nodes); it never settled the *topology* question, because 8333 is open by
default on a server and a minority of nodes listen. GPN should copy the membership answer **and** aim
higher on the topology answer, because GPU supply is consumer hardware behind consumer routers.

**Bitcoin comparison**

| Concern | Bitcoin | GPN today | GPN v2 |
|---|---|---|---|
| Inbound port required to participate | No (8333 optional) | **Yes** (exit 78) | **No** — but default ON |
| Inbound port expected | No (minority listen) | Yes (everyone) | **Yes, by default**; degrade if impossible |
| Backbone / dialable set | Minority, emergent | Enforced: everyone | **Default: every node tries**; emergent majority |
| Bootstrap | Hardcoded DNS seeds + `addr` gossip | static bootnode + DNS-TXT/URL seeds + on-chain registry (already built) | **hardcoded ≤5 IP seeds + DDNS list + on-chain**, all present |
| Membership / identity | None | Attestation (hardware) only | Attestation only (unchanged) |
| How a job reaches a worker | n/a (miners dial out) | **AP dials provider** | **Provider polls AP** (unchanged) |
| Consequence of being NAT'd | Slightly worse peer set | **Cannot join** | Slightly worse peer set + no liveness multiplier |

The asymmetry that makes this cheap: **GPN already has most of the machinery.** Peer discovery
(`discovery.rs`: addrman file, DNS-TXT seeds, HTTP seed lists, on-chain `ProviderEndpoints` +
`gpn-liveness`) and an outbound result path (`POST /result/{id}` with a per-job `callback_token`)
already assume providers can live anywhere. Only two things are inbound-only: the dialability gate and
the assignment push. The gate becomes a **capability probe with a fallback**, and the push is deleted.

---

## 2. Membership model

One class of member; three **capability states**. There is **no membership gate, allowlist, or
identity**. "Tier" describes what your box *can* do, not whether it is welcome.

Posture note (v2.2): being **dialable is required by default** — the network is still growing and
every node is expected to be part of the backbone. A node that cannot be dialled prints the §5.2
notice and exits unless the operator opts out with `--no-require-backbone`. The table below describes
the capability states; the non-discoverable column is the opt-out mode.

| | **Non-discoverable** (opt-out) | **Backbone** (**DEFAULT**) | **Seed** (published) |
|---|---|---|---|
| How you get here | `--no-seed` / `GPN_DISCOVERABLE=0` | default, if dialable (or made dialable) | backbone + in the published seed list |
| Network position | outbound-only | public IP + inbound `30333` | same, and listed as a bootstrap target |
| Sync chain | ✅ | ✅ | ✅ |
| Mine (PoW) | ✅ | ✅ | ✅ |
| Accept AI jobs (pull) | ✅ | ✅ | ✅ |
| Relay/serve blocks | limited | ✅ full mesh | ✅ full mesh |
| Announced in `ProviderEndpoints` / liveness | ❌ | ✅ | ✅ |
| Latency-probed by an AP | ❌ | ✅ | ✅ |
| In `seeds.json` | ❌ | candidate | ✅ top-N by uptime |
| Earnings | base rewards + job payouts | **base + liveness credit** | **same + routing tie-break + free peer inflow** |

**Nothing is taken away economically for being non-discoverable.** A non-discoverable node earns exactly
what a backbone node earns per block and per job. The only things dialability buys are (a) the
`gpn-liveness` uptime credit that gates the Silver/Gold staking multiplier, (b) seed-list eligibility,
and (c) a small routing tie-break. That must stay true: the majority of nodes may end up non-discoverable
after all, and they are the GPU supply.

**Seed is earned, not asserted.** `GPN_SEED=1` (default) means *willing and trying*. Actual
`tier: seed` / `tier: backbone` labels come from **peer witnesses** in `gpn-liveness`
(`reachable_count`, `uptime_bps`), never from a self-reported flag. Otherwise every NAT'd node claims
to be a seed and the list is worthless.

Corollary for the scheduler: a provider's `endpoint` is **optional metadata**. `ProviderEndpoints` stays,
populated by backbone nodes; routing must never *depend* on it (§4.6).

---

## 3. Bootstrap: two sources, always shipped

A new node must answer one question: *who do I dial first?* It needs ~1 honest address; gossip does the
rest. **Every build ships with both sources below**, and both are best-effort/non-fatal.

### 3.0 The two sources

| # | Source | Lives in | Purpose | Changes without a release? |
|---|---|---|---|---|
| **A** | **Hardcoded ≤5 seed nodes (IP addresses)** | `network.json` → `bootstrap_seeds[]`, compiled in | **Floor.** Works with no DNS, no HTTP, no chain, cold start | No (by definition) |
| **B** | **DDNS / bootstrap site list** | `bootstrap_urls[]` + `_gpn-seeds._tcp` TXT | **Freshness.** Current live node list | **Yes** |
| C | Persistent peer store (`gpn-peers.json`) | local | Fast restart | yes (local) |
| D | On-chain registry (`gpn-liveness` ∪ `ProviderEndpoints`) | chain | Trust-minimized, no publisher | yes |

Load order at startup: **C (if present) and A immediately**, then B and D concurrently. A is the floor:
if B and D both fail, the node still has 5 addresses. Never block startup on B or D.

### 3.1 Source A — the hardcoded 5-seed list

**Spec.** Up to five entries, each an `ip4` multiaddr plus a fixed peer id. Five is a *maximum for the
shipped default*, not a limit on the network — operators may append their own. Rationale for 5:
enough that any two being down still bootstraps; few enough to stay reviewable and to keep the capture
surface small.

Format (`network.json`):

```json
{
  "network": "gpn-mainnet",
  "genesis_hash": "0x…",
  "bootstrap_seeds": [
    { "id": "seed-a", "ip4": "203.0.113.10", "port": 30333,
      "peer_id": "12D3KooW…", "dns4": "seed-a.gpn.network", "operator": "gpn-core" },
    { "id": "seed-b", "ip4": "198.51.100.22", "port": 30333,
      "peer_id": "12D3KooX…", "dns4": "seed-b.gpn.network", "operator": "partner-eu" }
  ],
  "bootstrap_urls": [
    "https://seeds.gpn.network/seeds.json",
    "https://seeds2.gpn.network/seeds.json"
  ]
}
```

- `ip4` is authoritative for the hardcoded path (Jeremy's requirement: *IP addresses*).
- `dns4` is an **optional alias in the same entry**: if present, the client dials the IP **and** the
  name, so an IP change survives without a release once DNS is updated. The IP remains the fallback.
- `peer_id` is mandatory and is the real identity check: a dial to a wrong/rogue host fails the libp2p
  handshake and is discarded. A hijacked seed IP can waste a timeout, not forge a peer.
- Entries without `peer_id` are rejected at load time (log once, skip).

**Selection criteria** (published, so the set is auditable):

1. **Distinct operators**, ideally ≥3 legal jurisdictions. No operator holds >2 of the 5.
2. **Static public IPv4**, always-on, monitored; target ≥99.5% monthly reachability.
3. Same `genesis_hash`; runs a **released, unmodified** binary; no special privileges.
4. A published abuse/ops contact and a named maintainer.
5. Held the `backbone` tier with `uptime_bps ≥ 9900` for **≥30 consecutive days** before inclusion.
6. Cannot influence consensus, escrow, or job routing. A seed is a *dial target*, nothing more.

**Rotation.** Reviewed quarterly, and immediately on: 30 days cumulative downtime, a security incident,
operator request, or a `genesis`/network change. Rotation = normal client release (the list is compiled
in) **plus** the DNS alias, so a pure IP change never needs a release. Never rotate all 5 at once —
change at most 2 per release, so no two adjacent releases share zero seeds.

**Decision (Jeremy, 2026-10-19): option (i) — a dedicated bootstrap set.** The five seeds are **five
separate cheap VPSes, controlled and rotated by GPN, used only as dial targets**. They are **not
co-located with the five oracle operators**, and share no role, box or key with them. Rationale: an
oracle operator is a *consensus-adjacent, bonded, identity-bearing* role with a genesis-defined closed
set; a bootstrap seed is a *disposable, stateless dial target*. Coupling them would (i) turn a seed
outage into an oracle-ops incident, (ii) put oracle key material and seed hosting in one blast radius,
and (iii) make the seed set unrotatable without touching a locked launch decision.

The same *people* may operate both, but never the same box, the same process, or the same `peer_id`.
The five are enumerated in Phase 2; until then `bootstrap_seeds[]` ships with the entries it has (fewer
than five is valid — a floor of one is still a floor).

**Operational consequence:** the cost of this decision is five small VPSes and a quarterly rotation
chore. That is the price of keeping bootstrap independent of consensus; it is budgeted as an ops line,
not a protocol cost.

### 3.2 Source B — the DDNS / bootstrap list

A node queries a small list of well-known URLs (and DNS) to learn **who is currently available**. This is
what lets the peer set change without shipping a client.

**HTTP list format** (`seeds.json`, static file behind CDN/nginx, `Cache-Control: max-age=60`):

```json
{
  "network": "gpn-mainnet",
  "genesis": "0x…",
  "updated": 1760990400,
  "ttl": 300,
  "multiaddrs": [
    "/dns4/seed-a.gpn.network/tcp/30333/p2p/12D3KooW…",
    "/ip4/198.51.100.22/tcp/30333/p2p/12D3KooX…"
  ],
  "publisher": "gpn-core",
  "sig": "0x…"
}
```

- Bare JSON arrays remain accepted (v1 clients already parse them). Object form is preferred.
- `genesis` mismatch ⇒ discard the whole list (wrong network). `ttl` bounds cache reuse; the client
  clamps to 60 s–1 h.
- `sig` is a detached signature over the canonical JSON (sorted keys, `sig` removed) by a publisher key
  shipped in `network.json` (`bootstrap_publishers[]`). **Verification is optional and only prevents
  junk-list spam** — the libp2p handshake and `genesis` check are the real gate. Unsigned lists are
  accepted with a debug log.
- Two **independent publishers** (`seeds.gpn.network`, `seeds2.gpn.network`) are the availability
  target; a GitHub-raw mirror may be added as a third. Clients union the results.

**DNS form** (for clients that prefer DNS, and for IP rotation):

```
_gpn-seeds._tcp.gpn.network.  TXT  "gpn=v1;addr=/dns4/seed-a.gpn.network/tcp/30333/p2p/12D3KooW…"
seed-a.gpn.network.           A    203.0.113.10
seed-a.gpn.network.           AAAA 2001:db8::10        ; optional
```

Multiple TXT records, one multiaddr each; the client unions TXT ∪ HTTP. `--bootnodes-dns <name>` already
exists and needs no change.

**Query policy.**

- On startup: query all `bootstrap_urls` + DNS, in parallel, 5 s timeout each.
- Steady state: re-query when outbound peer count `< 8` (every 15 min, jittered), else hourly. This
  matters — a node with 30 peers must not hammer the seed service.
- Failures: exponential backoff 5 s → 10 min, never fatal, never a retry storm. Log once per state change.
- Cache the last good list to `<base-path>/gpn-seeds-cache.json`; on a cold, offline start the cache is
  used if `< 7 days` old, otherwise Source A only.

**Why this is safe to centralize:** a seed list confers no trust. A malicious entry wastes a dial
timeout or, at worst, contributes to an eclipse — mitigated by dialing **all** returned addresses
(shuffled) rather than the first one, keeping Source A as an independent floor, and requiring ≥2 distinct
operators/`/24`s before the node treats its peer set as healthy.

### 3.3 Creating a seed (operator recipe — a 5-minute job)

1. Any always-on box with a **public IPv4** (cheap VPS is the intended case). Residential is fine if the
   forward works (§5).
2. Open **TCP 30333** inbound (optionally 30334 for the second protocol).
3. Run:
   ```
   gpn_node --base-path … --public-addr /ip4/<ip>/tcp/30333 \
            --bootnodes-url https://seeds.gpn.network/seeds.json
   ```
   (Seeding is the default; no flag is needed. `--no-seed` would opt *out*.)
4. That's it. It is a normal full node: it mines, it serves jobs, it earns, and it **auto-registers its
   endpoint**, so it enters the seed-candidate pool on its own (§3.4).

No registration transaction, no permission, no key ceremony. The node's `peer_id` is its identity.
**To be listed in the hardcoded 5** (Source A) is a separate, deliberate act (§3.1) — running a seed gets
you into `seeds.json` automatically; it does not get you compiled into clients.

### 3.4 Auto-verification and promotion (who enters `seeds.json`)

Nobody hand-curates the dynamic list. `pallet-gpn-liveness` already records, per account/endpoint:
`first_seen`, `last_proven_online`, `cumulative_online_blocks`, `reachable_count` (distinct bonded peer
witnesses) and `uptime_bps`. Promotion rule (off-chain exporter `gpn_seedlist`, permissionless to run):

```
candidate = endpoint registered on-chain (announced, i.e. backbone)
          ∧ reachable_count ≥ K            (K=3, distinct witnesses, existing pallet param)
          ∧ uptime_bps   ≥ 9500            (95% over the liveness window)
          ∧ last_proven_online within 1 window
rank by (uptime_bps, cumulative_online_blocks); publish top 16 to seeds.json
```

The exporter is a ~100-line bin that reads chain state and writes the JSON; hosting it is unprivileged.
Because the inputs are on-chain, **anyone can audit or re-publish the list**, and a captured publisher
cannot inject a node that peers will accept. This is the "self-organizing bootnode layer" from the
liveness design, made concrete. Source A (≤5) is deliberately *outside* this pipeline: it must be stable
when the pipeline is broken.

---

## 4. Pull-based job protocol (unchanged from v1)

### 4.1 Principle

**The AP never initiates a connection to a provider.** All provider↔AP traffic is provider-initiated;
the on-chain job record remains the source of truth for state and money.

```
                 (1) GET /assignments?provider=…&since=…   [long-poll, outbound]
   provider  ────────────────────────────────────────────────▶  access point
             ◀───────────────────────────────────────────────
                 (2) 200 {lease_id, job{…}, callback_token} | 204

                 (3) accept_job / start_job   (on-chain, provider signs)
   provider  ────────────────────────────────────────────────▶  chain
                 (4) POST /result/{id}  {lease_id, token, output}
   provider  ────────────────────────────────────────────────▶  access point
                 (5) complete_job   (on-chain)
   provider  ────────────────────────────────────────────────▶  chain
```

Steps 3 and 5 already exist and are already provider-signed. Step 4 already exists (`POST /result/{id}`
+ `callback_token`). **Only step 1–2 is new**, plus the heartbeat inversion in §4.5.

### 4.2 API

```
GET /assignments?provider=<ss58>&since=<seq>&wait=<0..30>&caps=<json>
  Headers: X-Gpn-Assign-Token: <shared fleet token>     (v1)
           X-Gpn-Provider-Sig: <sr25519 over "assign|provider|seq|ts">   (v2, per-provider)
  200 → { "seq": 42,
          "leases": [ { "lease_id": "<uuid>", "job_id": 5731, "model": "llama3.2:1b",
                        "pull": true, "deadline_ms": 600000,
                        "callback": "https://ap…/result/5731",
                        "callback_token": "<32B hex>",
                        "job": { …full LlmJob incl. prompt… } } ] }
  204 → no work within `wait` (client re-polls immediately)
  401 → bad token/signature;  409 → provider not registered/attested
```

- **`since`/`seq`**: per-provider monotonic sequence in the AP's assignment store. The provider persists
  the last `seq` it processed, so a lost response or a restart cannot silently drop a lease; re-delivery
  is idempotent (same `lease_id`).
- **Long-poll** (`wait` ≤ 30 s) instead of tight polling: one HTTP request per idle provider per 30 s.
  With 1k providers that is ~33 req/s — trivial. A 60 s max backoff on error.
- **`caps`** lets the provider self-report load/capacity/models in the same request, removing the need
  for the AP to poll anything (§4.5).
- **Lease**, not assignment: `lease_id` + `deadline_ms` (default 60 s to reach `Accepted`). Exactly one
  live lease per job. If the provider doesn't land `accept_job` before the lease expires, the AP voids
  it and re-offers the job to the next candidate. Leases are AP-local and need no chain transaction.

### 4.3 AP-side state machine

```
                 ┌───────── lease expiry / no accept ─────────┐
                 ▼                                            │
  Queued ──offer──▶ Offered ──accept_job seen──▶ Claimed ──start──▶ Running
    ▲                 │                            │                 │
    │                 └── provider gone ───────────┴─────────────────┤
    │                                                               ▼
    └──────── re-offer (only if still Pending on-chain) ────── ResultPosted ──▶ Settled
                                                                     │
                                            result too late / invalid ▼
                                                                  Expired (expire_job → refund)
```

Mapping to the chain (unchanged): `Pending → Accepted → Running → AwaitingVerification → Completed`
(+ `Cancelled`, `Failed`). The AP's `Queued/Offered/Claimed/ResultPosted/Settled` are **off-chain
bookkeeping only**; every transition is confirmed by reading the chain, never assumed from an HTTP 200.

### 4.4 Auth

- **v1 (ship first):** shared fleet secret `GPN_ASSIGN_TOKEN` (already implemented for the push path) +
  the `provider` ss58 must be in `RegisteredProviders` with a fresh attestation. The AP refuses to hand
  a payload to an unknown account. Transport is HTTPS when the AP has a public URL.
- **v2 (hardening):** per-provider signature. The provider signs `assign|provider|seq|ts` with the same
  sr25519 key it signs transactions with; the AP verifies against the on-chain account. This removes the
  shared secret and gives non-repudiation of "you were offered job N". The runtime already has sr25519
  verify (`ChannelAuth`), and the node exposes raw signing.
- **Result auth is unchanged:** the per-job `callback_token` binds the result to the lease; the AP still
  rejects a mismatched/absent token in constant time (`ct_eq`).

### 4.5 Heartbeat inversion (required, easy to miss)

Today the AP learns load/latency by polling `GET /provider/status` on `:8093`. That is **inbound to the
provider**. Replace it with:

- `caps` in the assignment poll → load, models, capacity, `active_jobs`, disk/VRAM (the existing
  `/status` JSON body, unchanged schema).
- **`latency_ms`** = the AP's measured RTT of the poll request itself (provider→AP, which is the
  direction that matters: that is the path the result travels). Optionally report the provider's own
  measured RTT to the AP for symmetry.
- `POST /providers/{addr}/heartbeat` and AP-side latency probing remain as a **backbone-only** fast path.
  Non-discoverable nodes never need it.

`gpn_provider_status`'s `GET /status` stays (local monitoring, LAN, backbone probing) but is no longer
load-bearing for routing.

### 4.6 Provider selection (routing, new transport)

`pick()` keeps its current order — capability filter (`gflops ≥ min_gflops`, `min_vram_mb`,
`min_ram_mb`, model availability, `active == 0`) → readiness → lowest latency — with one addition:
**backbone/seed nodes get a small score bonus / tie-break** (§7). Only the *delivery* mechanism changes:
the winner gets a lease instead of a POST.

### 4.7 Failure & timeout paths

| Failure | Detection | Recovery |
|---|---|---|
| Provider never polls | no poll for `> N` s | AP marks it stale, stops offering (heartbeat freshness) |
| Lease offered, no `accept_job` | lease deadline (60 s) | void lease, re-offer to next candidate |
| `Accepted`, provider dies | no result by `result_deadline` | AP calls `expire_job` (permissionless) → refund; chain sweep (`MaxExpirySweepPerBlock=25`) is the backstop |
| Result posted, `complete_job` fails | AP sees `AwaitingVerification` past deadline | existing heal path (`gpn_llm_provider` one-step heal) |
| AP restarts mid-lease | leases persisted in `assignments.json` with `lease_id` + expiry | re-offer expired leases; on-chain state decides what is still live |
| Duplicate `POST /result` | `lease_id` + token already consumed | 200 idempotent, no second settlement |
| Provider claims on-chain without a lease (legacy/dual path) | `list_claimable_jobs` finds a Pending job | still allowed — the chain is authoritative; the AP observes `Accepted` and adopts it |

The last row matters: the **existing first-claim fallback is already a pull mechanism** and should be
kept. It is what makes the migration incremental and what keeps the network alive if an AP's lease
bookkeeping is wrong.

### 4.8 What changes where

**`gpn-scheduler` (AP)**
- `GET /assignments` + long-poll + lease table (new).
- `dispatch_assignment` → demoted to an optional best-effort **wake hint** (`POST /assign` becomes
  "you have work, poll now"); a failure is no longer a lost job.
- No longer requires a provider `endpoint` to route. Registry gains providers discovered from polls.
- `heartbeat` polling of non-discoverable providers removed; `caps` accepted in-band.
- Persist leases in the existing cross-process-locked `assignments.json` (`StoreLock`, item #28).

**Provider binaries** (`gpn_llm_provider`, `gpn_diffusion_provider`, `gpn_finetune_provider`,
`gpn_ml_provider`)
- New config `GPN_AP_URL` (or `--ap-url`); a poll loop replacing "wait for `/assign` then
  `read_assigned()`".
- `assignments.json` stays as the **cache** of leased work, so the run/settle code is untouched.
- `GPN_ASSIGN_SOURCE=push|pull|both` (default `both` during migration).
- Heartbeat `/status` server may be disabled entirely (`--no-status`) for pure participants.

**Chain / runtime: none.** No new pallet, no `set_code`, no genesis change. `ProviderEndpoints` becomes
optional metadata. `gpn-liveness` is used exactly as designed.

---

## 5. Becoming dialable — the seed-by-default path

This is the section v2 changes most. V1 said "detect, then settle quietly into participant mode; UPnP is
out of scope." V2 says: **try hard, tell the operator exactly what to do, and only then degrade.**

### 5.1 Startup sequence

```
1. Explicit config?   GPN_PUBLIC_ADDR / --public-addr set → use it, verify by dial-back, done.
2. Inbound works?     classify local IPs (classify_ip) + libp2p Identify external_addresses
                      + an actual inbound peer connecting on :30333
                      → public & inbound-verified ⇒ backbone. Done.
3. NAT mapping         if (2) failed and GPN_UPNP=auto → try UPnP-IGD / NAT-PMP (igd-next), 10 s budget.
                      Runs BEFORE build_network so the learnt external ip:port can be
                      injected into public_addresses. On success: verify externally
                      (inbound peer / Identify), then adopt the mapped address. Done.
4. CGNAT check         WAN address in 100.64.0.0/10, or observed ≠ local and no mapping
                      → no forward is possible. Skip to 5.
5. Degrade             print the §5.2 notice. Default: exit EXIT_NOT_DIALABLE (78).
                      With --no-require-backbone: continue as a NON-DISCOVERABLE
                      participant and re-check every 10 min (§5.3).
```

**In the default posture, failing step 2–4 is fatal** (exit 78) after printing the full notice.
`--no-require-backbone` / `GPN_REQUIRE_BACKBONE=0` opts into the soft mode where the sequence is
non-fatal. §5.3's re-check/promotion path exists **only** in that opt-out mode (a hard-gated node
exits, so there is nothing to promote).

### 5.2 Degradation notice (exact behavior)

The notice is **the product**, not a log line: it is the only channel by which a non-expert operator
learns that their node is not dialable and what to do about it. Requirements:

- Printed **once per degraded transition** (startup, or dialable → not dialable) to **stderr** and to
  the console at `warn` level (`error` in the default hard-gate mode), so it survives the default log
  filter; each line is emitted verbatim so the box is never mangled by the logger. It must not be
  interleaved with per-block output.
- Summarized in `gpn_node --status` as `tier: private (degraded)`.
- Emitted **after** the UPnP/NAT-PMP attempt, so a successful mapping never shows it.
- Suppressed entirely when the operator passed `--no-seed` / `GPN_SEED=0` (they asked to be private).
- The LAN IP and port are filled in from the detected interface and the actual listen port.
- **ASCII only** (Windows consoles; see the mojibake incident in `docs/run-a-node.md`).

Exact text (`{lan}` = detected LAN IPv4, `{port}` = listen port, default 30333):

```
================================================================================
  YOUR NODE IS NOT DIALABLE
================================================================================
Other nodes cannot open a connection to this machine on TCP {port}.

This node will NOT start until it is dialable. Nothing else on this machine
is affected - fix the forward/firewall below and start it again.
What you lose: it is not listed as a backbone/seed node, so it earns no
liveness credit, gets no seed-list eligibility, and cannot be a bootstrap target.
Nothing is taken away from block rewards or job payouts.

To fix it (about 5 minutes):

  1. Find this machine's LAN IP. Detected: {lan}
     (Windows: ipconfig | Linux/macOS: ip addr or ifconfig)

  2. On your router, add a port-forwarding rule:
        Protocol ............ TCP
        External port ....... {port}
        Internal port ....... {port}
        Internal IP/host .... {lan}
     Router admin is usually http://192.168.1.1 or http://192.168.0.1.

     Recommended setup (EXAMPLE - these are example values, not detected):
       - Give this machine a DHCP reservation so its LAN IP never changes
         (example: 192.168.1.50).
       - Forward exactly one TCP port, external = internal ({port}).
       - Example router rule:  TCP {port} -> 192.168.1.50:{port}
     Detected on this machine: LAN {lan}, port {port}.

  3. Allow the port in this machine's firewall.
     Windows (Administrator PowerShell):
       New-NetFirewallRule -DisplayName "GPN node" -Direction Inbound `
         -Protocol TCP -LocalPort {port} -Action Allow
     Linux (ufw):
       sudo ufw allow {port}/tcp
     Linux (firewalld):
       sudo firewall-cmd --add-port={port}/tcp --permanent && sudo firewall-cmd --reload

  4. Or let the node ask the router via UPnP/NAT-PMP (already attempted at
     startup; on by default). If your router has UPnP disabled, enable it or
     use step 2. Disable this behavior with --no-upnp.

  5. CGNAT: if your router's WAN address is in 100.64.0.0/10, no port forward
     can work. Run the node on a cheap VPS with a public IP.

Full guide with per-OS detail:
  https://gpn.network/docs/run-a-node#make-your-node-dialable

This node will exit until it is dialable.
To run anyway (not recommended while the network is growing):
  --no-require-backbone
================================================================================
```

In soft mode (`--no-require-backbone`) the body line becomes "Your node is running ..." and the tail
is:

```
This node re-checks every 10 minutes and will print NOW DIALABLE here when
the fix works. You can also restart to force a check.

Soft mode is active (--no-require-backbone / GPN_REQUIRE_BACKBONE=0).
Run private, silence:      --no-seed   (or GPN_SEED=0)
================================================================================
```

### 5.3 Re-detection and the "now dialable" message (**soft mode only**)

This section applies **only** when the hard gate is opted out
(`--no-require-backbone` / `GPN_REQUIRE_BACKBONE=0`). A hard-gated node exits on the first failed
probe, so there is nothing to re-check in-process.

- Re-evaluate every **10 min** and on `external_addresses` change. A residential IP change is normal.
- On change: update the on-chain endpoint via `set_provider_endpoint` (rate-limited to once per hour to
  avoid tx churn) and re-run the probe.
- **State is edge-triggered, not level-triggered.** Keep `last_dialable: bool`. Print the §5.2 notice
  only on `dialable → not dialable`; print the message below only on `not dialable → dialable`. Never
  repeat either while the state is unchanged.
- The promotion message is printed **once per transition**, to stderr/console (same visibility rules as
  §5.2), so the operator knows their router/firewall change worked without reading logs:

```
================================================================================
  NOW DIALABLE - your node is reachable on TCP {port}
================================================================================
Inbound reachability was confirmed. This node now announces itself as a backbone
node and is eligible for seed listing. Nothing else to do.

(This message is shown once.)
================================================================================
```

- If the node later becomes unreachable (IP change, router reboot, CGNAT change), the §5.2 notice
  fires once on that transition — not on every probe.
- **CGNAT is terminal but not an error.** Documented as a normal, supported outcome — the same class as
  "IPv6-only". A degraded node is a first-class member (§2), not a failure.

### 5.4 Flags

| Flag / env | Default | Meaning |
|---|---|---|
| `--seed` / `GPN_SEED=1` | **on** | Willing to be a seed: announce, accept inbound, attempt NAT mapping, enter seed-candidate pool |
| `--no-seed` / `GPN_SEED=0` | — | Do **not** publish me as a seed. Still discovers and participates. Suppresses the §5.2 notice |
| `--discoverable` / `GPN_DISCOVERABLE=1` | **on** | Announce endpoint + accept inbound (backbone behavior) |
| `--no-discoverable` / `GPN_DISCOVERABLE=0` | — | **Privacy mode.** No announce, no inbound, not in any list. Implies `--no-seed`. Job pull, mining, sync unchanged |
| `GPN_UPNP=auto\|off` (`--no-upnp`) | `auto` | Try UPnP-IGD/NAT-PMP; `off` never touches the router |
| `--public-addr <multiaddr>` / `GPN_PUBLIC_ADDR` | unset | Explicit address; **always wins** over detection (needed when a forward rewrites the port) |
| `--require-backbone` / `GPN_REQUIRE_BACKBONE=1` | **on (default)** | Hard gate: print the §5.2 notice and exit 78 when not dialable |
| `--no-require-backbone` / `GPN_REQUIRE_BACKBONE=0` | — | Opt out: run degraded, re-check every 10 min, print NOW DIALABLE on promotion |
| `GPN_SEED_PORT` | `30333` | Inbound port to open/probe |
| `--bootnodes-url` / `--bootnodes-dns` | from `network.json` | Override/extend Source B |

Semantics worth stating: `GPN_SEED` controls **publication as a bootstrap target**;
`GPN_DISCOVERABLE` controls **being announced/accepting inbound at all**. `--no-seed` on a dialable box
gives a normal backbone node that is never listed; `--no-discoverable` gives a private node. The two are
not synonyms, and the docs must not conflate them.

### 5.5 UPnP/NAT-PMP — implemented (v2 change)

V1 excluded it (security surface, opens a router port without informed consent, often broken). V2
includes it **because seed-by-default otherwise silently fails for the majority of home users**, which
is the whole point of the change. Guardrails:

- **Crates / protocols.** UPnP-IGD via `igd-next` 0.14 (already in the dependency graph through
  `libp2p-upnp`, so no new transitive crates; blocking API driven on `spawn_blocking`, 5 s SSDP
  search). NAT-PMP (RFC 6886) is hand-rolled on a plain `UdpSocket` (port 5351, 3 × 400 ms) — no
  crate, `natpmp` is unmaintained. Pure PCP is not implemented; NAT-PMP covers the routers it needs
  to. STUN remains useless here (the listener is TCP; GPN does not do TCP hole-punching).
- **Ladder order:** explicit `GPN_PUBLIC_ADDR` (skip — do not touch the router) → UPnP-IGD → NAT-PMP.
  Each backend: WAN address → CGNAT/private check → `AddPortMapping` TCP. A CGNAT or private WAN
  short-circuits the ladder (`MapOutcome::Cgnat`), since no later backend can beat it.
- **Timing / advertisement:** the attempt runs **before `build_network`**. A successful mapping's
  router-reported external `ip:port` is appended to `NetworkConfiguration::public_addresses` and
  becomes the address the dialability gate must see observed. This is what makes a router-assigned
  external port (`45123 -> 30333`) work with no configuration. `GPN_PUBLIC_ADDR` still wins.
- **Consent is explicit:** enabled by default only because the product's stated default is "be a
  backbone node". `--no-upnp` / `GPN_UPNP=off` fully disables it, and every attempt and result is
  logged (`portmap: UPnP: mapped TCP 30333 -> 192.168.1.42:30333 via 192.168.1.1 (UPnP-IGD);
  external address is 203.0.113.7:30333. Disable with GPN_UPNP=off or --no-upnp`). The log line is the
  consent record — the mapping itself is never silent.
- **Scope is minimal:** one TCP port, lease 3600 s (`GPN_UPNP_LEASE_SECS`), renewed at half-life,
  expires on its own if the node dies; never UDP, never a port range, never SSDP beyond the discovery
  probe. Windows NAT-PMP needs `GPN_NATPMP_GATEWAY` (no portable gateway detection); UPnP does not.
- **Failure is normal:** ~10 s budget, then fall through to §5.2. Many routers have IGD disabled.
- **Truth model:** a mapping is not proof. §5.2 still fires unless an inbound peer or a matching
  Identify observation arrives within the grace window.
- **Renewal / change:** renewed at half-life; a WAN IP or external-port change is logged at `warn`.
  libp2p's announced address set is fixed at construction, so the new address is authoritative on the
  next restart (peers can still find it via Identify in the meantime).

---

## 6. Migration

**Phase 0 — design (this doc).** No code.

**Phase 1 — soft gate + pull in parallel (highest value, no flag day).**
- Dialability gate: **dialable is REQUIRED by default** — probe-and-exit 78 with the full §5.2 notice.
  `--no-require-backbone` / `GPN_REQUIRE_BACKBONE=0` opts into probe-and-degrade (v2 behavior).
  This is the single highest-value change and is independent of everything else — ship it first.
- Add §5.1 NAT mapping (UPnP) + the §5.2 warning + the flags in §5.4. **Done:** `node/src/portmap.rs`
  (UPnP-IGD + NAT-PMP), pre-`build_network` advertisement, lease renewal, CGNAT short-circuit.
- AP: add `GET /assignments` + leases. Keep `POST /assign` dispatch. `GPN_JOB_TRANSPORT=push|pull|both`
  (default `both`). **Done (2026-10-19):** `GET /assignments?provider=&since=&wait=` in
  `gpn_scheduler.rs` — bounded (≤25 s) long-poll, cursor by job id, cap 20 leases/poll, joined with the
  AP's own `LlmStore` payload, authenticated by `GPN_ASSIGN_TOKEN` off-loopback.
- Providers: poll loop behind `GPN_ASSIGN_SOURCE` (default `both`). **Done (2026-10-19):**
  `gpn-agent/src/assign_pull.rs` (persists into `assignments.json` + `LlmStore` exactly like `POST
  /assign`, never overwriting a payload we already hold, cursor in `~/.gpn/assign-cursor.json` seeded
  from the store on first run) wired into all four provider bins; `GPN_AP_URL` selects the AP.
- **Still open for Phase-1 sign-off:** the two acceptance checks below (they need a real NAT'd box).
- Verify: (a) a NAT'd box with no forward runs a job end-to-end and prints the warning; (b) a box with
  UPnP available auto-promotes to backbone with no config.

**Phase 2 — bootstrap lists + pull default.**
- Ship `bootstrap_seeds[]` (Source A) and `bootstrap_urls[]` (Source B) in `network.json`; publish
  `seeds.json` from `gpn_seedlist`; add `_gpn-seeds._tcp` TXT.
- `GPN_JOB_TRANSPORT=pull`. `POST /assign` is a wake hint only; `dispatch_assignment` failures log at
  debug, never count as job failures.
- AP stops polling non-discoverable `/status`; routing uses `caps` + poll RTT.
- **The 5-seed decision (§3.1) is settled:** five separately operated cheap VPSes, controlled and
  rotated by GPN, used only as dial targets. Provisioning them is a Phase 2 prerequisite.

**Phase 3 — remove push.** Delete the mandatory dispatch path and the inbound-status dependency. Keep
`POST /assign` as a no-op/wake endpoint for one release, then delete.

**Backward compatibility**
- Old providers (no poll loop) keep working while `GPN_JOB_TRANSPORT=both`, because push still runs.
- Old APs + new providers: providers still self-claim `Pending` jobs from the chain (the dual path), so
  the network degrades to first-claim-wins rather than breaking.
- New clients ignore unknown `network.json` keys, so adding `bootstrap_seeds`/`bootstrap_urls` is safe
  for old builds (they simply keep their compiled-in bootnode).
- Mixed fleets are safe at every step; no flag day.

**What happens to the dialability gate:** it stops being `EXIT_NOT_DIALABLE` and becomes the **backbone
capability probe**. Same evidence code (`classify_ip`, `evaluate`, inbound-peer detection), different
consequence: tier label + seed eligibility + routing bonus, instead of refusing to start.

---

## 7. Tiers & incentives (backbone is the default)

| Tier | Requirement | Badge | Routing | Rewards |
|---|---|---|---|---|
| **Non-discoverable** | explicit opt-out, or dialability impossible | `tier: private` (local only, not advertised) | normal score, **never excluded** | block rewards + job payouts |
| **Backbone** (**default**) | dialable (or mapped), endpoint announced | `tier: backbone` + `/providers` field | small tie-break bonus | base **+ liveness credit → Silver/Gold staking multiplier (1.5×/2.5×)** |
| **Seed** | backbone, top-16 by uptime, published in `seeds.json` | `tier: seed` | same as backbone | same + free peer inflow; optional ops grant (open, §8 Q4) |

- The **default expectation is backbone**, so the reward for it must be *real but not coercive*: the
  liveness credit is the existing Silver/Gold gate and is the main carrot. Do not add a separate
  "backbone block bonus" — that would tax the non-discoverable majority for a topology property.
- The badge is **observable and honest**: derived from peer witnesses, not self-reported. A node that
  claims `backbone` in a heartbeat but has no witnesses is ignored.
- Never a hard exclusion: non-discoverable nodes are always routable and always paid per block/job. The
  routing bonus is deliberately small (a tie-break, not a preference that starves them).
- Links to `pallet-gpn-liveness` (implemented): `reachable_count`, `uptime_bps`,
  `cumulative_online_blocks` are exactly the promotion inputs in §3.4.
- **Open incentive question:** whether seeds get a fee share or a treasury ops grant (§8 Q4). v1 answer:
  no fee share; tier multiplier only, plus a possible small grant once the 5-seed set is formalized.

---

## 8. Open questions / risks

1. ~~**The 5-seed ambiguity (§3.1).**~~ **Resolved 2026-10-19: dedicated bootstrap set** — five
   separately operated VPSes, not co-located with the oracle operators, distinct `peer_id` from any
   oracle identity. See §3.1.
2. **Prompt confidentiality at the AP.** Pull does not change it — the AP already holds the plaintext
   prompt. But `GET /assignments` now returns prompts over the network to providers. Require HTTPS, and
   consider end-to-end encryption (customer key → provider) as a follow-up; token auth is not
   confidentiality.
3. **Lease races.** Two APs may both lease the same Pending job (APs are permissionless and do not share
   state). The chain resolves it — the second `accept_job` fails — so the cost is a wasted lease, not a
   double run. Acceptable; document it. Do **not** add a global lease registry (centralization).
4. **AP as DoS target / resource sink.** Long-polls hold a connection for up to 30 s. Bound concurrent
   polls per IP and per provider; 204 fast when the queue is empty.
5. **Seed centralization.** Source A is 5 IPs; Source B is 2–3 publishers. Mitigations: dial all
   returned addresses shuffled, keep Source A/B/D independent, never let one peer be the only peer,
   require ≥2 distinct operators/`/24`s before treating the peer set as healthy. A seed cannot forge
   chain state; the worst case is an eclipse, not a consensus break.
6. **UPnP is a real security surface.** It is on by default in v2. Mitigations in §5.5; `--no-upnp`
   must work and must be documented in the install guide. Revisit if abuse is observed.
7. **Noisy warnings.** The §5.2 block must fire once, not per reconnect. A CGNAT user who ignores it
   should see it at most once per start plus in `--status`.
8. **Latency semantics change.** AP-measured `latency_ms` was AP→provider; it becomes provider→AP (poll
   RTT). Client-supplied RTT hints (`routing.rs`) are unaffected and remain the better signal.
9. **`can_pull` ambiguity.** The field currently means "will pull a missing model from Ollama". It now
   collides with "pulls jobs". Rename to `can_pull_model` in the next breaking heartbeat change.
10. **Provider identity for pull auth.** v1's shared `GPN_ASSIGN_TOKEN` is fleet-wide: any holder can
    poll as any registered provider and see that provider's queued prompts. v2 signature (§4.4) closes
    it. Until then, restrict the token to the fleet and prefer HTTPS.
11. **IPv6-only / CGNAT hosts** can never be backbone. Correct and supported — document it as a normal
    outcome, not an error (§5.2).
12. **On-chain endpoint churn** from auto-detected address changes; rate-limit (§5.3).
13. **Default-on seed + default-on UPnP is a behavior change on upgrade.** A fleet restart will cause
    every node to attempt a NAT mapping and announce. Land it deliberately (not bundled with an
    unrelated restart), and pre-announce it in the install guide.

---

## 9. References

- `design-liveness-selforg-fullnode-tiers.md` — liveness pallet, Silver/Gold full-node gate.
- `design-access-points-scheduler.md` — AP/scheduler, heartbeat, `pick()`.
- `gpn-node/node/src/dialability.rs` — the gate being demoted to a capability probe.
- `gpn-node/node/src/discovery.rs`, `network_registry.rs`, `service.rs` — seed sources.
- `gpn-agent/src/bin/gpn_scheduler.rs` — `dispatch_assignment`, `assignment_token`, `POST /result/{id}`.
- `GPN-MASTER-REFERENCE.md` §3, §3.1, §5, §6.1, §11 (D1 oracle set).
- `network.json` — `bootstrap_seeds`, `bootstrap_urls`, `bootstrap_publishers`.
