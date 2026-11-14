# GPN — Goal vs Implementation

**Audit date:** 2026-11-13 (read-only: no pushes, no release, no restart, no version bump).
**Auditor:** subagent, evidence-gated. Every claim below cites a file:line or a command output produced in this run. Where I could not establish a fact I wrote **not determined** rather than inferring.

**Why this file exists.** Three mechanisms were reported "shipped" this week and were not wired at all:
`gpn_snapshot heal` had no caller, gateway election sat behind `GPN_SITE_ELECT` which nothing sets,
and published multiaddrs named a transport the node does not listen on. All three passed CI, because
CI tests what exists, never what is connected. This is the audit of what else is in that state.

**The four facts, kept separate.** For every capability:
- **W** Written — the code exists (file:line).
- **C** Called — something invokes it (call site). "No caller found" is a first-class result.
- **A** Active — on by default for a normal node, not behind a flag/command nothing sets. Named where it is.
- **V** Verified — observed working on the live node in this run (log line, RPC, state change).

Live node used for V: `gpn-node` pid 952599 (`/home/jeremyg/.local/bin/gpn-node --chain testnet --base-path /home/jeremyg/.gpn/node --network-backend libp2p …`, from `/proc/952599/cmdline`), RPC `:9944`, AP `:8092`, heartbeat `:8093`, GUI `:8099`, spec_version **51** (`state_getRuntimeVersion`).

---

# Part 1 — The goals (derived from the documents, not from memory)

| # | Goal (what GPN is *for*) | Doc citation | "Working" would mean |
|---|---|---|---|
| **G1** | **A GPU does not have to choose**: it mines by default and serves an AI job when one is assigned, mining stopping for the job's duration and resuming after — one node, two earnings, no mode to configure. | GPN-ONE-PAGER.md "The fix"; GPN-WHITEPAPER.md §2 table + "There is **no profitability switch to configure**" | On a live box, a block is produced while idle and a job runs (with mining paused) when assigned, with no operator action between the two. |
| **G2** | **Anyone can join without permission or funds**: no ICO/premine gate, no staking or delegation prerequisite to attest, list or serve; entry can be paid in work. | WP §4 "Stake-free attestation"; WP §6 "Fair launch via mining"; artifacts/design-pow-entry.md §2; runtime `ExistentialDeposit = 0` | A brand-new, unfunded account can attest/register and become routable without a transfer from anyone. |
| **G3** | **Trust-minimal coordination**: the chain holds job lifecycle, escrow and payout; the compute never touches the chain; the chain cannot be talked into paying for work it did not escrow. | WP §5.1, §5.4, §5.5; WP abstract "The chain does coordination, escrow, and settlement" | A job's money moves exactly once, only on delivered work, with a permissionless escape (expire/fail) that cannot be gamed by either side. |
| **G4** | **Price compute from cost/throughput, not from a competitor's price list**: an anchor $/hr for a 4060-class card divided by its realised tok/s, plus margin, converted to GPN at an oracle-fed rate. | WP §6.1 (worked example, `$0.379/1M out`); ONE-PAGER "On price (no overclaim)" | The published price is derivable from the live feed + a measured throughput, and the feed is actually posted. |
| **G5** | **Buyers hit one endpoint and the network finds the GPU**: capability filter → availability → lowest latency, providers discovered on-chain, jobs *pulled*, so a NAT'd box still serves. | WP §5.3 (4 numbered routing steps); ONE-PAGER "Routing"; design-access-points-scheduler.md | A request to the public API lands on an eligible, idle, attested provider chosen by measured latency, with no hand-maintained provider list. |
| **G6** | **GPU-only, provably**: eligibility is proof-of-work + hardware attestation + a capacity challenge, so a CPU/iGPU cannot earn. | WP §3 "GPU-only by construction", §4 PoSG + "Capacity challenge (proof, not claim)"; ONE-PAGER "GPU-only, provably" | A provider with no measured discrete GPU is refused, and a claimed GFLOPS number is checked against a measured one before it can route work. |
| **G7** | **Fair-launch PoW token**: all supply minted per block, halving ~2 years, hard cap ≈520.5 M, block producer keeps the reward. | WP §6; ONE-PAGER "Token (GPN)" | Emission matches the halving schedule on chain; the producer's account is the payout account. |
| **G8** | **A node that diverges repairs itself, and divergence is never treated as a crime**: detect against a signed checkpoint, stop sealing, preserve the DB, re-bootstrap from a signed snapshot, rejoin; never ban a forked peer; publish our own chain state on the wire. | WP §3 "Fork tolerance", §10 "transient forks self-heal"; design-self-heal-divergence.md §1–§3; design-network-identity-fork-tolerance.md | A node whose hash at the signed height is wrong stops producing, backs up, re-imports, and rejoins — unattended, within minutes, with a loud record. |
| **G9** | **The reachability layer self-organizes**: peer-witnessed liveness for full nodes, Bitcoin-style discovery (persistent store + DNS/HTTP seeds + on-chain), dialable-address publication with an evidence ladder, and one verified gateway per NAT. | WP §4 "Self-organizing full-node layer", §9; design-liveness-selforg-fullnode-tiers.md; design-lan-topology.md §3.1/§5 | Bootnodes and gateways emerge from the network's own evidence — a founder's static IP is not load-bearing, and a stranger can always find and dial the network. |
| **G10** | **The operator can see and reclaim the hardware, and the node stays current by itself**: local dashboard, Idle switch (survives reboot), signed auto-update on by default with a fleet-spread schedule, minimum-version enforcement. | WP §9.1 (dashboard, Idle, auto-update whole section); ONE-PAGER "Runs on a gaming PC", "Stays current by itself" | A gaming-PC owner can park the GPU for ≤6 h without leaving the network, and a stale binary updates itself without a stampede. |
| **G11** | **Privacy**: shielded transfers and withdrawals with nullifiers, no value/owner on chain. | WP §7 | A shielded spend moves value without revealing amount or parties, and double-spends are rejected. |
| **G12** | **No single key**: council governance with a timelock, forkless runtime upgrade, treasury spend gated by a separate roster. | WP §8.1, §8.2 | A parameter change requires N-of-M approvals + a timelock; a treasury spend requires the roster — no lone key. |
| **G13** | **The operator (and the reader) sees the true state**: never publish a number that was not measured, never call a gap "done", name the reason and the fix when something is degraded. | WP §12 "honest limitations"; ONE-PAGER "Where it is … labelled honestly"; AGENTS.md Evidence Gate | Every published field is either measured or `null`; every "DONE" has a caller and an observation behind it. |

---

# Part 2 — Capability matrix (W / C / A / V)

Legend: ✔ yes · ✘ no · ? not determined.

### 2.1 Self-heal and fork handling

| Capability | Goal | W | C | A | V | Evidence | Verdict |
|---|---|---|---|---|---|---|---|
| **Self-heal on divergence** (detect at startup + every 5 min, stop sealing, rename `chains/`, re-import signed snapshot, rejoin, one recovery/24 h) | G8 | ✔ `gpn-agent/src/bin/gpn_snapshot.rs:1252` (`heal`), ledger + `EXIT_HEAL_REFUSED` at `:1257` | **✘ no caller** — `grep -rn "gpn_snapshot\|heal"` over `scripts/`, `install.*`, `.github/workflows/*.yml`, `gpn-node/`, `gpn-agent/src/main.rs`, `gpn_node.rs` finds only the binary itself, the weekly *publisher* (`scripts/snapshot-weekly.sh`), and a status *reader* (`gpn_provider_status.rs:1380`) | ✘ manual CLI only; no unit, timer, supervisor child, or in-node task | ✘ never observed | design-self-heal-divergence.md:23 "Check at startup and every 5 minutes"; `scripts/gpn-autostart.sh` writes exactly 4 units (node, ollama, update.service, update.timer) and none runs a check | **written-not-wired** |
| **Checkpoint enforcement / exit 12** | G8 | ✔ `gpn_snapshot.rs:1075` `EXIT_WRONG_CHAIN=12`, `:1113` `checkpoint_verdict`, `:1154` `checkpoint()` | ✘ same as above — nothing invokes it | ✘ deliberately unscheduled: GPN-TASKS.md #156 "**the comparison half shipped; the schedule is deliberately not installed**" | ✔ **the tool works**: `gpn_snapshot checkpoint --meta /tmp/snapmeta.json --rpc http://127.0.0.1:9944` → `checkpoint: OK — local block 294496 matches the signed hash`, `EXIT=0` | signed metadata fetched from `snapshot-latest` (block 294496, genesis `0xb8e463…`) | **written-not-wired** (working tool, no schedule) |
| **Never ban on divergence** | G8 | ✔ `gpn-node/node/src/fork_tolerance.rs` (`divergence_credit`, module doc §"never", lines 20-40) | ✔ `service.rs:1375` spawns `gpn-fork-tolerance` | ✔ unconditional | ✔ **live** `2026-11-13 14:31:47 fork-tolerance: peer 12D3KooWEYMEt8… is on a different chain at #296399 … reputation -16087 -> 0 (divergence is not a protocol violation)` | `journalctl --user -u gpn-fleet` | **working** |
| **Chain-state announce (`gpn/chain/1`)** | G8 | ✔ `chain_state.rs` (protocol const, 9 tests) | ✔ `service.rs:859` registration + `:1884` spawn | ✔ unconditional, INFO on start | ✔ live: `chain-state: peer 12D3KooWEYMEt8… agrees on #296459 0x76d4606e` / `is behind at #280551 (ours #296462); it will sync from us` every ~30 s | journal | **working** |
| **Sealing pause at zero peers** | G8 | ✔ `service.rs:1348-1352` (writes `<base>/gpn-isolated`, resumes when a peer appears) | ✔ `service.rs:1329` spawn `gpn-peer-watch` | ✔ unconditional | ✔ live: `⚠️ No peers — block sealing is PAUSED … (Marker: <base-path>/gpn-isolated)` repeated, then mining resumed at 1 peer | journal 14:15–14:30 | **working**, with a defect: the warn is **level-triggered**, printed every 30 s tick (≈60 identical lines/hour), not edge-triggered — `service.rs:1334-1352` logs inside `loop {}` with no state compare (contrast `dialability::edge_transition`) |

### 2.2 Reachability, gateway, transport

| Capability | Goal | W | C | A | V | Evidence | Verdict |
|---|---|---|---|---|---|---|---|
| **Gateway election / one gateway per NAT** | G9 | ✔ `dialability.rs:1132` `site_elect`, `:1346` `elect_gateway`, `:1366` `elected_as_gateway`, `:1659` `gateway_role` | ✘ **nothing sets the flag**; `grep -rn GPN_SITE_ELECT` over `install.ps1 install.sh install.cmd scripts/*.sh *.ps1 .github/workflows/*.yml` → **0 hits** (only `dialability.rs` + a comment in `service.rs:1032`) | ✘ off by default; the live fleet runs without it | ✘ and it could not work even if set: the caller passes **`&[]`** as the uptime samples (`service.rs:1056`) and, more decisively, **nothing writes `~/.gpn/claims.json`** — `grep -rn "claims_state_path\|write_claims"` finds only the reader (`service.rs:1049`, `dialability.rs:1778`) and tests; `judge_claim` (`dialability.rs:1521`) has **no non-test caller** (`:4151,4174,4182` are tests) | live `cat ~/.gpn/claims.json` → **absent**; `/network` `"gateways":0` | **missing in practice** (written, unreachable from any shipped config) |
| **Dial-back before publishing an address** (only a third-party handshake that returns *our* peer id may publish) | G9 | ✔ `dialability.rs:1389` `DialbackOutcome`, `:1405` `classify`, `:1521` `judge_claim`, `:1543` `SocketRegistry` | ✘ no caller outside tests | ✘ not the active evidence path | **✘ contradicted live**: dialability published `/ip4/142.129.54.88/tcp/30333` from a **self-probe**: `detail: "outbound TCP connect … succeeded (hairpin reachability; weaker than peer observation)"`, `dialable: true`, `reachability: "backbone"` (`~/.gpn/dialability.json`; journal 14:29:17 `[tier: seed-candidate]`) | design-lan-topology.md:135-137 says outright "there is no dial-back verifier anywhere"; §T2 names this exact case (`hairpin_success_is_not_dialability`, `tcp_probe_ok=true` ⇒ `dialable==false`) | **wrong** (live behaviour is the one the design forbids) |
| **`/ws` transport derivation** (publish the transport we listen on) | G8/G9 | ✔ `dialability.rs:2089` `align_public_addresses`, `:2061` `listen_transport`; `gpn_provider_status::base_multiaddr` keeps `/ws` | ✔ `service.rs:788` before `build_network`; ✔ provider-status derived from listen addrs | ✔ unconditional (copied from listen set, never assumed) | ✔ live: `system_localListenAddresses` = `/ip4/127.0.0.1/tcp/30333/ws/p2p/12D3…`, `/ip4/192.168.1.20/tcp/30333/ws/…`; `/providers` publishes `/ip4/142.129.54.88/tcp/30333/ws/p2p/12D3…` | residual: `~/.gpn/dialability.json` `multiaddrs` is still **bare** `/ip4/142.129.54.88/tcp/30333` — `write_state_file` (`dialability.rs:2480`) copies `publication.public` without adding the transport; any consumer other than `gpn_provider_status` inherits a dead address | **working at the two publishing paths; one stale writer remains** |
| **Same-LAN discovery (mDNS)** | G9 | ✔ `lan_discovery.rs` (posture classifier) | ✔ `service.rs:818` `log_posture` | ✔ reports truthfully; **does not make it work** | ✘ **live**: `lan-discovery: mDNS is ON but INERT on the libp2p backend — this sc-network rev drops the mDNS (peer, address) pairs and keeps only the peer id, so no LAN peer ever becomes dialable. Fix: run --network-backend litep2p (the default; GPN's launchers do not pin a backend)` | live cmdline **pins** `--network-backend libp2p` (from `GPN_NODE_ARGS` in `/proc/952599/environ`), i.e. the broken combination is the shipped one | **off/inert on the live fleet** (honest log, wrong default) |
| **Peer priority (tier-0 oracle seeds, 60 s re-dial, ranked slots)** | G9 | ✔ `peer_priority.rs:41` `REDIAL_INTERVAL_SECS = 60`, tiers 0/1/2 (`:229-234`: tier-0 always retried) | ✔ `service.rs:1625` spawn `gpn-peer-redial` | ✔ unconditional, reads `GPN_MAX_PEERS` (default 8) | ✔ live: `peer-priority: dialled 1/4 ranked peer(s) — 1 reserved, 3 on-chain/unknown … 4/8 slot(s) filled` every 30–60 s | journal | **working** |
| **Peer exchange (`gpn/peers/1`, ≤32 entries, 24 h TTL, 3-strike drop)** | G9 | ✔ `peer_exchange.rs` (pure encode/decode/select) | ✔ `service.rs:834` registration, `:1753` spawn | ✔ unconditional | ✔ live: `peer-exchange: substream open with 12D3… (2 peer(s))` / `substream closed` cycling; `PEERS_PROTOCOL` registered before `build_network` | journal | **working** |
| **On-chain liveness registry (register / witness / finalize)** | G9 | ✔ pallet `gpn-liveness` (calls 0-3, `lib.rs:247-351`); runtime `type Liveness` (`runtime/src/lib.rs:597`); client in `chain/mod.rs:2571-2649`; operator tool `gpn-agent/src/bin/gpn_fullnode.rs` | ⚠ **manual binary only** — `grep -rn "gpn_fullnode\|register_fullnode\|witness_reachable"` over `scripts/`, `install.*`, `*.service`, `*.yml`, `gpn_fleet.rs` → **0 scheduling hits** | ✘ nothing registers, witnesses or finalizes automatically; `gpn_fleet` spawns only the node (`gpn_fleet.rs:117`) | ? not determined (a read-only query would have required running `gpn_seedlist`, which publishes) | design-liveness…md §4 "Agent: `gpn_fullnode` binary registers endpoint, runs node, dials/accepts peer witnesses, submits self + witness" — that binary exists and nothing runs it | **off-by-default (no daemon)** |
| **Portmap / UPnP / NAT-PMP** | G9 | ✔ `portmap.rs` (1480 lines), maintenance loop `service.rs:929` | ✔ `service.rs` spawn `gpn-portmap-maintain` when `session.config.enabled` | ✔ on by default (`--no-upnp` is the only flag; `command.rs` test asserts default ON) | ✔ live path executed: `portmap: skipped (public address configured)` — correct behaviour when `GPN_PUBLIC_ADDR`/`--public-addr` is set, **but the mapping itself is therefore unexercised here** | journal 14:29:17; `--public-addr` present in cmdline | **working (skip path verified; mapping not determined on this host)** |

### 2.3 Chain, token, eligibility

| Capability | Goal | W | C | A | V | Evidence | Verdict |
|---|---|---|---|---|---|---|---|
| **Unsigned PoW entry (fee-free, no account)** | G2 | ✔ `compute-marketplace/src/lib.rs:574` `validate_unsigned`, `:786` `register_provider_entry_unsigned`; `gpu-attestation` pre-check `:177`; client `onboard.rs:328,514` | ✔ `gpn_node.rs:3930` `run_entry(..., entry=true)` on first run; `gpn_register --entry` | ✔ automatic on first run (marker `ONBOARD_ATTEMPTED`), with fallback to signed entry then attest+register (`onboard.rs:518-525`) | ? not determined — this box is already registered (`/status` `gflops_verified` true, `onboard` skipped) | design-pow-entry.md "Status: implemented in source, not enacted" | **working (code + caller + default), live re-verification not determined** |
| **ED = 0 accounts** | G2 | ✔ `runtime/src/lib.rs:558` `ExistentialDeposit: Balance = 0` (comment: "unconditionally 0 — no cargo feature can change it; `insecure_zero_ed` always enabled") | ✔ `runtime/src/lib.rs:244` `type ExistentialDeposit = ExistentialDeposit` for `pallet_balances` | ✔ compile-time, no flag | ⚠ partial: live runtime is spec **51** (`state_getRuntimeVersion` → `specVersion:51`), matching `runtime/src/lib.rs:126`, so the constant is what is running; I did not read a zero-balance account's existence from storage (would need a key computation) | — | **working (compile-time), direct observation not determined** |
| **Block-producer-keeps-reward / halving / cap** | G7 | ✔ pallet-gpu-pow + token emission, `MaxFreeEntriesPerBlock`, halving constants | ✔ runtime wiring | ✔ unconditional | ⚠ not re-measured this run; live chain advances and pays (one-pager §"LIVE") | WP §6 | **not determined this run** |
| **Proof-of-GPU attestation (measured GFLOPS + challenge)** | G6 | ✔ `gpu-attestation` pallet; `gpn_attest`; node RPC `gpn_confirmChallenge` (`rpc.rs`) | ✔ heartbeat/attest loop; `GPN_CHALLENGE_VERIFIER_URI` set in the live env | ✔ default on | ✔ live: `~/.gpn/attest-state.json` `{"measured_gflops":824,"spread_pct":0.0,"submitted":true,"on_chain_before":825}`; `/providers` `gflops_verified:true`, `gflops_source:"attested-idle"` | `/proc/952599/environ` carries `GPN_CHALLENGE_VERIFIER_URI` | **working** |
| **Capability gates on routing (min_gflops / vram / ram)** | G6 | ✔ scheduler `pick()` | ✔ every job | ✔ on | ✔ live `/providers` shows `vram_mb:8188`, models filtered (`tuned-*`/`stories260K` advertised by status but hard-excluded from the public catalogue) | WP §5.6 | **working** |

### 2.4 Marketplace, escrow, pricing

| Capability | Goal | W | C | A | V | Evidence | Verdict |
|---|---|---|---|---|---|---|---|
| **On-chain escrow + job lifecycle** | G3 | ✔ compute-marketplace pallet | ✔ AP + provider | ✔ on | ✔ live jobs complete and settle (`[hub] job … billed 29839699 of the 35403046 ceiling (32 prompt + 7 completion tokens)`) | journal `gpn-accesspoint` | **working** |
| **Off-chain escrow channels (default settlement)** | G3 | ✔ `hub_ap.rs`, `hub_channel`, `settle_policy.rs:156` (`GPN_ESCROW_MODE` = settle \| channel \| onchain) | ✔ AP startup | ✔ live mode `settle` with hub enabled | ✔ live: `[hub] off-chain escrow ENABLED (mode=settle, deposit=50000000000000, ttl=9000 blocks, settle_every=86400s …)`; `[hub] job 1794577418119 served off-chain on channel 7 (nonce 2698 …)` | journal + `/proc` env | **working** |
| **Provider heartbeat + assignments** | G1/G5 | ✔ `gpn_provider_status` (`/status`, push + pull) | ✔ `gpn-provider-heartbeat.service` active | ✔ on | ✔ live `:8093/health` `{"status":"ok"}`, `/status` full measured record; `~/.gpn/assignments.json` + `assign-pull-state.json` (mtime 14:54); `/providers` rows `source:"push"` | — | **working** |
| **Pricing from throughput + oracle feed** | G4 | ✔ `profitability/`, pricing constants, `GPN_GPU_ANCHOR_USD_HR` | ✔ AP pricing path | ✔ on | ⚠ oracle operator unit active (`gpn-oracle.service`); I did not read `ProfitabilityOracle::CurrentFeed` this run (task #68 says LIVE) | journal shows unit active | **working, value not re-measured** |
| **Fast sync / snapshot import** | G2 (onboarding) | ✔ `gpn_snapshot import/verify`; `install.sh:108-199` verifies signed metadata then imports | ✔ installer (one-shot) | ✔ `GPN_FAST_SYNC` default 1 **at install time only** | ✔ path exists and is exercised by `scripts/tests/test-fast-sync.sh`; install output logs `chain data: bootstrapped from the weekly snapshot` | docs/fast-sync.md line 8: "the *scheduled* daily check is **not installed yet**" | **working as onboarding; no ongoing verification loop** |

### 2.5 Operator surface, updates, reporting

| Capability | Goal | W | C | A | V | Evidence | Verdict |
|---|---|---|---|---|---|---|---|
| **Dashboard + Idle mode** | G10 | ✔ `gui.rs`, `idle.rs`, `idle_auto.rs` | ✔ `gpn_node` supervisor; `GPN_GUI=1` | ✔ live env sets `GPN_GUI=1` | ✔ live listener `127.0.0.1:8099`; `~/.gpn/idle.json` absent = active (correct default) | `ss -ltn` | **working** |
| **Signed auto-update** | G10 | ✔ `update.rs`, `gpn_update` bin, signed manifest verify | ✔ `gpn-update.timer` (user unit) **and** in-process for installer deployments | ✔ on by default (`GPN_AUTO_UPDATE=0` to opt out) | ✔ live: timer last fired **14:27:38**, next 15:39:12 (hourly); `gpn-update.service` `Result=success` | `systemctl --user list-timers gpn-update.timer` | **working**, but the **cadence disagrees with the docs**: code `update.rs:700` `DEFAULT_CHECK_HOURS = 1` ("TEMPORARY … revert after the review window"), timer comment says hourly, `docs/updates.md:815` still says "every 6 hours" and `:674` "6" — tracked as GPN-TASKS #161 |
| **Minimum-version / outdated detection** | G10 | ✔ `network_registry::classify` compares `latest_version` | ✔ startup banner | ✔ on | **✘ misleading live**: banner says `identity : note: running 0.3.45 (registry latest 0.1.0)`. Cause: an operator override `~/.gpn/network.json` (mtime **2026-10-02**, `"spec_version": 6, "latest_version": "0.1.0"`) shadows the embedded registry (repo `network.json` correctly says `0.3.45`) | `cat ~/.gpn/network.json`; journal 14:29:17 | **wrong on this host** — the outdated-node warning can never fire, and the banner misreports the network's spec |
| **Honest three-state fields (measured / null / unknown)** | G13 | ✔ `ram_mb: null` pattern (#48), `blocked_reason` (#49) | ✔ status + AP | ✔ on | ✔ live `/providers`: unmeasurable boxes read `null`, not 0 | — | **working** |
| **`recovered_from_fork` on /status** | G8/G13 | ✔ `gpn_provider_status.rs:2548,2652` reads the heal ledger | ✔ status handler | ✔ on | ✘ field can only be non-null if `heal` ran; see self-heal (no caller) | — | **written-not-wired** (downstream of §2.1) |

---

# Part 3 — Goal → change mapping, and the three buckets

Sources: `GPN-TASKS.md` (the 155 table rows across both tables, ids as printed; some ids appear twice with different subjects — noted below), `git log --oneline` (**1513 commits**), `change-log.md` (5405 lines / 3875 entries).

Mapping rule: an item is "serves a goal" only if I can name the goal it advances for a user or an operator. Bugfix rows are mapped to the goal whose promise they protect — a job that strands escrow breaks G3 even though the row's title says "janitor".

## 3.1 Serves a stated goal — the large majority

| Goal | Items (GPN-TASKS ids) |
|---|---|
| **G1** mine+serve, one node, no switch | 4, 24, 25, 26, 44, 50, 64, 70, 71, 94, 130, 145 |
| **G2** join without permission or funds | 19, 21, 23, 44, 96, 97, 104, 143, 146 |
| **G3** trust-minimal escrow/settlement | 1, 2, 6, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 47, 49, 51, 53(first: usage metering), 54, 55, 56, 57, 58, 60, 61, 62, 66, 67, 70, 73, 74, 75, 76, 78, 80, 81, 82, 91, 95, 99, 100, 101, 102, 103, 105, 108, 109, 110, 111, 129, 131, 135, 142, 144, 147, 149, 150 |
| **G4** cost-derived pricing + oracle | 11, 68, 106, 117 |
| **G5** one endpoint, discover providers, pull-based | 3, 14, 18, 22, 45, 46, 55, 59, 70, 77, 89(heartbeat/node-endpoint), 115, 131, 141, 147, 149 |
| **G6** GPU-only, attested capability | 6, 14, 19, 65, 87, 93, 97, 104, 128, 137, 140, 143 |
| **G7** fair-launch PoW token | 11, 20, 97 |
| **G8** self-heal / never-ban / explicit identity | 116, 136, 154, 155, 156, 157, 158, 159 |
| **G9** self-organizing reachability | 18, 45, 46, 116, 136, 139, 154, 155, 156, 159 |
| **G10** operator control + stays current | 8, 42, 43, 53(second: signed auto-update), 64, 69, 90, 92, 96, 130, 132, 134, 148, 151, 152, 161 |
| **G11** privacy | 15, 13, 17 |
| **G12** no single key, forkless upgrade | 12, 13, 16, 17, 52, 63, 88, 113, 114 |
| **G13** true state, honest docs | 7, 10, 26, 27, 42, 43, 48, 59, 60, 99, 107, 110, 113, 114, 115, 118, 119, 120, 121, 122, 123, 124, 125, 126, 127, 128, 133, 135, 137, 139, 141, 143, 145, 148, 149, 150 |

(Ids 5, 9, 107, 137, 138, 153 exist in one table only or carry no row text in the printed export; 153 is PARKED streaming = G5/G3, 138 = unit dependency ordering = G10. Where an id appears twice with different subjects — **53** and **89** — both subjects are listed.)

## 3.2 Serves no stated goal (candidate scope creep / build-for-the-sake-of-building)

| Item | What it is | Why it maps to no goal |
|---|---|---|
| **98** | `cargo fmt --all` + 4 clippy lints | Housekeeping. Necessary for CI, not a capability. |
| **134** | Diagnose why a CI run was missing for a commit | Release plumbing; no user-facing promise. |
| **151** | Bump 15 `Cargo.toml` files to get *past* the #148 guard | Version bookkeeping created by the guard itself. |
| **69** | Move fmt/clippy/lib-tests onto self-hosted runners | Delivery integrity. Real, but it serves the *project*, not any G1–G13. |
| **161** | Revert update cadence 1 h → 6 h | Bookkeeping for a temporary decision; it exists only because of a deliberate deviation. |
| **20** | Testnet-participation snapshots → airdrop | No airdrop appears in ONE-PAGER, WHITEPAPER §6, or any design doc I read. It is a growth/marketing instrument. |
| **132** | Installer referral code plumbing | Referral appears nowhere in the intent documents; it is attribution for marketing. |

## 3.3 Serves an unstated but arguably legitimate need (Jeremy decides)

| Item | The need it actually serves |
|---|---|
| **22** abuse & safety guardrails (AUP, moderation, report/ban) | Not a whitepaper goal, but a *public, free, OpenAI-compatible* endpoint needs abuse control or it becomes a spam relay. Legitimate; should be stated as a goal (G5's operating cost) rather than left implicit. |
| **28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 47** (the long janitor/nonce/heal-churn tail) | These protect G3, but they exist because the settlement path has **two** writers (AP hot wallet + provider signer) and an off-chain/on-chain split with no single source of truth for "is this job alive". The unstated need: *one* authoritative job state machine. Worth naming so the 20th bug in the class is treated as a design problem. |
| **42, 43** disk-full chain halt + guard | An availability property (G10/G3) that the docs never promise: "the node will not stop producing because the host filled up". Legitimate; state it. |
| **59** provider burns 91% of a core building a fresh reqwest client per read | Efficiency, not a documented goal — but on a gaming PC (G10's premise) a provider that eats a core is a real harm. |
| **92, 148, 150, 152** deploy/update hardening (flock, version guard, transient-vs-permanent download, HEAD retry) | Delivery integrity for G10. The auto-update promise ("stays current by itself") is only true if these hold; the docs promise the outcome, not the machinery. |
| **134** CI billing block, **69** CI green | The project's ability to *know* whether it works. Given this audit's premise (CI tests what exists, not what is connected), CI health is arguably the highest-leverage unstated need in the whole list. |

---

# Part 4 — What is quietly not happening that the design assumes IS happening

Ordered by risk: blast radius × how silently it fails.

1. **Divergence self-heal does not exist as a behaviour (G8).**
   The design's first line is "Check at startup and every 5 minutes" (`design-self-heal-divergence.md:23`).
   `gpn_snapshot heal` is a complete, tested, live-working CLI (`heal`, 24 h ledger, `EXIT_HEAL_REFUSED`) with
   **no caller anywhere** — not a timer, not a unit in `scripts/gpn-autostart.sh` (which writes exactly four
   units), not a child of `gpn_node`/`gpn_fleet`. **Blast radius:** a forked node keeps serving and sealing
   canonical-looking blocks until a human notices; the only automated brake is the *zero-peer* pause, which does
   not fire for a node that is forked but well connected (the box-1 case: six healthy children, a chain nobody
   else had). This is the single largest gap between promise and behaviour.

2. **Checkpoint enforcement is a tool nobody runs (G8).**
   `gpn_snapshot checkpoint` works — I ran it live: `checkpoint: OK — local block 294496 matches the signed
   hash`, exit 0 — and `EXIT_WRONG_CHAIN=12` is pinned. GPN-TASKS #156 says so explicitly: *"the comparison
   half shipped; the schedule is deliberately not installed."* **Consequence:** items 1 and 2 are the same gap
   seen from two ends. Nothing in the shipped system ever compares a running node to the signed chain. The
   design's exit-12 path is unreachable in production.

3. **"One gateway per NAT" is not a mechanism, it is a comment (G9).**
   Three independent breaks, any one fatal: (a) `GPN_SITE_ELECT` is read (`dialability.rs:1172`) and set by
   **no launcher**; (b) the role computation is handed **`&[]`** for witnessed-uptime samples (`service.rs:1056`);
   (c) the *only* input that could make election mean anything — a verified dial-back claim in
   `~/.gpn/claims.json` — is **never written**; `judge_claim` has no non-test caller and the file does not exist
   on the live host. Live `/network` reports `"gateways":0`. **Blast radius:** two machines behind one router can
   both publish the same public socket, and the design's demotion path ("A's next re-verification returns
   `WrongPeer`; A demotes within one TTL") cannot run because nothing re-verifies.

4. **Address publication still accepts hairpin self-connect as proof (G9).**
   The design states the rule (design-lan-topology.md §3.0: "An address may be published only by the peer id that
   a third party reaches when it dials that address") and names this exact defect as test T2
   (`hairpin_success_is_not_dialability`). The live node publishes on precisely that evidence:
   `dialable: true`, `reachability: "backbone"`, `detail: "outbound TCP connect … succeeded (hairpin
   reachability; weaker than peer observation)"`. **Blast radius:** the directory advertises addresses that may
   not forward; clients dial and hang. Routing still works because it is pull-based — which is exactly why this
   can hide for a long time.

5. **Same-LAN discovery is inert on the configuration the fleet actually runs (G9).**
   `lan_discovery.rs` documents it accurately and logs it at every start: mDNS on **libp2p** keeps the peer id
   and drops the address, so no LAN peer becomes dialable. The live node's argv **pins** `--network-backend
   libp2p` (`GPN_NODE_ARGS`). The code's own fix text says the launchers do not pin a backend — they do.
   **Consequence:** two boxes on one LAN must hairpin through the router; the "D3 same-LAN discovery" row reads
   DONE while the shipped configuration makes it a no-op.

6. **The on-chain liveness layer has no daemon (G9).**
   The pallet, the client calls, and `gpn_fullnode` all exist; nothing schedules `gpn_fullnode`. `gpn_fleet`
   spawns only the node (`gpn_fleet.rs:117`), and no script/unit/workflow references it. **Consequence:** the
   mechanism the whitepaper uses to argue "the bootnode/reachability layer emerges organically rather than
   depending on a founder" is at present 100% founder-dependent (one seed in `network.json`).

7. **The identity/version check on this host is reading a stale hand-made file (G10/G13).**
   Live banner: `running 0.3.45 (registry latest 0.1.0)`. Cause: `~/.gpn/network.json` (2026-10-02,
   `spec_version: 6`, `latest_version: "0.1.0"`) shadows the embedded registry, which correctly says `0.3.45`.
   The override feature is intentional (`network_registry.rs:64-72`), but nothing warns that it is stale, and its
   effect is that **`UPGRADE REQUIRED` / outdated warnings can never fire on this box**. Same class as the three
   bugs that started this audit: a config that looks authoritative and is not.

8. **Auto-update cadence no longer matches its documentation (G10/G13).**
   Code and units are hourly (`update.rs:700`, `gpn-update.timer` `OnUnitActiveSec=1h`), docs say 6 h
   (`docs/updates.md:815`, `:674`). It is a deliberate temporary change with a tracked revert (#161), so the
   behaviour is intended — but every doc a reader meets still promises 6 h.

9. **`~/.gpn/dialability.json` still publishes a bare `/tcp` address (G9 residual).**
   The 0.3.45 transport fix rewrote `config.network.public_addresses` and `gpn_provider_status`'s derivation, and
   both are verified live. `write_state_file` (`dialability.rs:2480`) was not part of that fix: its `multiaddrs`
   are `/ip4/142.129.54.88/tcp/30333` with no `/ws`. Today's consumer compensates; the next consumer will not.
   This is the same defect that cost a week, still present in one writer.

10. **Sealing-pause is level-triggered (G13, minor).**
    `service.rs:1348-1352` writes the marker and logs the warning **every 30 s tick** while isolated; the live
    journal shows ~60 identical lines/hour (14:15:32 → 14:28:32). Contrast `dialability::edge_transition`,
    which exists precisely to avoid this. Noise buries the signal that matters.

11. **CI cannot see any of items 1–10, by construction.** Every capability above is *written and unit-tested*;
    what is missing is a caller, a default, or an observation. A test suite that asserts "the function returns
    the right plan" cannot fail when nothing invokes the function. If one thing changes from this audit, it
    should be that the release gate runs at least one **end-to-end wiring assertion per claimed automatic
    mechanism** (a unit/timer exists and fires; a config flag the code reads is set by a shipped launcher; a
    published field is derived from an observed one).

---

## Appendix — commands whose output is quoted above

```
/proc/952599/cmdline, /proc/952599/environ            # live node flags + env
curl :9944  system_health            → {"peers":1,"isSyncing":false,"shouldHavePeers":true}
curl :9944  system_localListenAddresses → …/tcp/30333/ws/p2p/12D3KooWAFt…
curl :9944  state_getRuntimeVersion  → specVersion 51
curl :8092/providers                 → multiaddrs [ /ip4/142.129.54.88/tcp/30333/ws/p2p/… ]
curl :8092/network                   → counts{gateways:0, chain_registered_now:9, …}
curl :8093/health, /status           → ok; measured gflops 824, listen_addrs …/ws
ss -ltn                              → 30333, 9944, 8082, 8092, 8093, 8099
journalctl --user -u gpn-fleet       → peer-exchange / chain-state / fork-tolerance / portmap /
                                       lan-discovery / dialability / sealing-pause lines quoted
systemctl --user list-timers         → gpn-update.timer LAST 14:27:38, NEXT 15:39:12
~/.local/bin/gpn_snapshot checkpoint --meta /tmp/snapmeta.json --rpc :9944 → EXIT=0, block 294496 matches
cat ~/.gpn/{dialability.json,attest-state.json,network.json}; ls ~/.gpn/claims.json (absent)
grep -rn GPN_SITE_ELECT / claims_state_path / judge_claim / gpn_snapshot / gpn_fullnode
```
