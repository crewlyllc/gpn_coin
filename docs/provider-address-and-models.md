# Provider address publication, VRAM gating and the public model catalogue

Operator-facing reference for three things a provider publishes:

* `multiaddrs` / `listen_addrs` — how peers dial it (`gpn_provider_status`, `GET /status`)
* `models[]` — what it claims it can serve (VRAM-gated)
* `GET /v1/models` — what the public shop window lists (allowlisted)

Everything here is enforced in code; this page explains the rules and the
environment variables that override them.

---

## 1. Publishing a dialable address

The heartbeat (`gpn_provider_status`) reads the node's identity over JSON-RPC
(`system_localPeerId`, `system_localListenAddresses`) and publishes:

| field | meaning |
|---|---|
| `peer_id` | `12D3Koo…` — the node's libp2p identity |
| `multiaddrs` | dialable **public** endpoints, each ending in exactly one `/p2p/<peer_id>` |
| `p2p_port` | the TCP port to combine with the host the access point reached us on |
| `listen_addrs` | the node's own listeners, normalised (diagnostic only) |
| `address_source` | which rung of the ladder produced `multiaddrs` (or `none`) |

### Why `listen_addrs` are normalised

Current litep2p builds report `system_localListenAddresses` **already carrying**
`/p2p/<peer_id>`, and some emit it **doubled**:

```
/ip4/127.0.0.1/tcp/30333/p2p/12D3Koo…/p2p/12D3Koo…     ← upstream cosmetic bug
```

Relaying that verbatim made the access point reject every address (its
sanitizer — correctly — requires exactly one `/p2p/<id>`, last, matching the
advertised `peer_id`). The heartbeat now strips identity from every listener and
appends `/p2p/<peer_id>` exactly once itself. `listen_addrs` never contain
`/p2p/`.

If you see this line in the heartbeat log, you are on an affected node build —
it is harmless now:

```
[p2p] node RPC reported a doubled /p2p/<id>/p2p/<id> listen address (upstream litep2p bug) — normalised before publication
```

### The evidence ladder

A public endpoint is chosen by the same evidence the node's dialability gate
uses, strongest first:

| # | rung | what it means |
|---|---|---|
| 1 | `GPN_PUBLIC_ADDR` / `--public-addr` | the operator declares it (also supports `/dns4/name/tcp/port`) |
| 2 | `GPN_DIALABILITY_STATE` (default `~/.gpn/dialability.json`) | the node's gate proved it: an inbound peer was observed, or UPnP-IGD/NAT-PMP mapped the port |
| 3 | a public **listen** address | the classic VPS case — the node is bound to a public IP |
| 4 | a STUN mapping **on the listen port** | port-preserving NAT; a port-rewriting NAT is reported, not published |

Never published: `127.0.0.0/8`, `::1`, `10/8`, `172.16/12`, `192.168/16`,
`169.254/16`, `100.64/10` (CGNAT), `fc00::/7`, `fe80::/10`, `0.0.0.0`.

If every rung fails, `multiaddrs` is `[]` **and the heartbeat log says why**:

```
[p2p] publishing NO public address for peer 12D3Koo… — peers cannot dial this box:
[p2p]   - GPN_PUBLIC_ADDR unset (no operator-declared endpoint)
[p2p]   - no node dialability state (node never observed an inbound peer or mapped a port)
[p2p]   - node listens only on non-routable addresses (/ip4/192.168.1.20/tcp/30333); set GPN_PUBLIC_ADDR to the forward
[p2p]   - STUN probing unavailable or not port-preserving
[p2p]   fix: set GPN_PUBLIC_ADDR=/ip4/<wan-ip>/tcp/<port> (or enable UPnP) and restart the heartbeat
```

An empty `multiaddrs` is **not** an error: the access point can still route work
over the outbound `GET /assignments` poll. It only means other peers cannot dial
this box directly.

### Environment

| variable | default | purpose |
|---|---|---|
| `GPN_PUBLIC_ADDR` | unset | the endpoint peers should dial (wins over every other rung) |
| `GPN_DIALABILITY_STATE` | `~/.gpn/dialability.json` | state file written by `gpn-node` when the gate passes |
| `GPN_STUN` | on | `off` disables the STUN probe (air-gapped hosts) |
| `GPN_STUN_SERVER` | `stun.l.google.com:19302` | STUN server |
| `GPN_NODE_KEY_FILE` | unset | per-machine libp2p key file (`gpn-node key generate-node-key --file …`) |

### Troubleshooting: two boxes with the SAME `peer_id`

The peer id is the libp2p node key, not the account. A **copied** `--node-key`
(or a shared `run-fleet.sh`) gives every box the same identity: the directory
collapses them into one entry, peers dial whichever host resolved first, and jobs
land on a box that never polled for them.

Check:

```bash
grep -n 'node-key' ~/.gpn/fleet/run-fleet.sh   # a literal 64-hex key = shared
curl -s http://127.0.0.1:8093/status | grep peer_id
```

Fix, on the box that must change identity:

```bash
gpn-node key generate-node-key --file ~/.gpn/node-key     # 64 hex chars, mode 600
sed -i 's|--node-key [0-9a-f]\{64\}|--node-key-file '"$HOME"'/.gpn/node-key|' ~/.gpn/fleet/run-fleet.sh
# or, for the supervisor: unset GPN_NODE_KEY; export GPN_NODE_KEY_FILE=~/.gpn/node-key
gpn-node key inspect-node-key --file ~/.gpn/node-key      # print the new peer id
systemctl --user restart gpn-provider gpn-provider-heartbeat
```

The supervisor now warns loudly if it starts with the shared fleet key, and
supports `GPN_NODE_KEY_FILE` as the preferred (per-machine) shape. Expect the
peer id to change: republish the node's address if it is registered on-chain.

---

## 2. VRAM-aware model gating

A model must not be advertised — or claimed — if its weights cannot fit the
box's VRAM. The size table lives in `gpn_agent::models`:

| model | weights (GB) |
|---|---|
| `qwen2.5:0.5b` | 0.4 |
| `qwen2.5:1.5b` / `deepseek-r1:1.5b` | 1.2 |
| `llama3.2:1b` | 1.3 |
| `qwen2.5:3b` | 2.0 |
| `llama3.2:3b` / `phi3:mini` | 2.4 |
| `mistral:7b` / `deepseek-r1:7b` / `qwen2.5:7b` | 4.4–4.7 |
| `llama3.1:8b` / `deepseek-r1:8b` | 5.0 |
| `deepseek-r1:14b` / `qwen2.5:14b` | 9.0 |

A **headroom factor** of 1.15 is applied (KV cache + runtime), so `qwen2.5:7b`
needs ~5.4 GB and an 8 GB card is fine, a 4 GB card is not.

Applied at three points:

1. `GET /status models[]` — the heartbeat drops models that cannot fit
   (`gpn_provider_status`).
2. The provider's claim/inference path — `resolve_model` removes unfittable
   installed models and falls back to one that fits (`gpn_llm_provider`).
3. The access point's routing — `pick()` skips a provider whose advertised VRAM
   cannot hold the requested model (`gpn_scheduler`).

Unknown model size or unadvertised VRAM (`0`) does **not** gate: blocking on
ignorance would empty every catalogue behind an unprobed box.

Log line (once per model per process):

```
[models] skipping deepseek-r1:14b: needs ~10.3 GB, node has 8.0 GB — not advertised
```

### Where `vram_mb` comes from

`gpn_agent::gpu::detect_vram()` probes in this order and takes the first
success (largest adapter wins on a multi-GPU box):

1. `nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits` — true
   64-bit figure, any card size.
2. `rocm-smi --showmeminfo vram` — true 64-bit figure (AMD).
3. **Windows only:** registry
   `HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\0*`
   → `HardwareInformation.qwMemorySize`. This is a **REG_QWORD** (64-bit), so
   12 GiB / 24 GiB / 48 GiB cards are reported correctly; rows whose
   `DriverDesc` is an iGPU (`Intel(R) UHD/Iris`, `AMD Radeon(TM) Graphics`,
   `Radeon 780M`, …) are skipped, as are zero, `N/A`, non-numeric and
   implausible (>1 TiB) values.
4. **Windows only, last resort:** `Win32_VideoController.AdapterRAM`.

⚠️ **`AdapterRAM` is a uint32 and saturates at 4,294,967,295 bytes = 4095 MiB.**
It cannot represent a 6 GB+ card. Using it understates VRAM, which makes the
gate *wrongly exclude* models the machine can actually serve. When — and only
when — detection falls through to it, the agent logs, once per process:

```
[gpu] VRAM from AdapterRAM (32-bit, capped at 4095 MiB) — value may understate the real total; models larger than 4 GB may be wrongly excluded
```

A probe that finds **nothing** (no vendor CLI, no registry key, no
`AdapterRAM`) logs instead of returning a silent `0`:

```
[gpu] VRAM detection returned 0 MiB (no nvidia-smi, rocm-smi, registry qwMemorySize or AdapterRAM value) — VRAM is NOT advertised and the VRAM model gate is disabled, so oversized models will not be blocked
```

Either way the fix is the same: pass `--vram-mb <MiB>` (or `VRAM_MB`).

### Overrides

| variable | example | effect |
|---|---|---|
| `GPN_MODEL_VRAM_HEADROOM` | `1.3` | raise/lower the headroom (values `< 1.0` are ignored) |
| `GPN_MODEL_VRAM_OVERRIDE` | `qwen2.5:14b` | treat that model as fitting regardless of the table |
| | `qwen2.5:14b=6.5` | use 6.5 GB as its size instead of the table value |
| | `all` | disable the gate entirely (operator knows better) |

---

## 3. The public model catalogue

`GET /v1/models` (and therefore the site's model picker) lists **only**
assistant-grade models. Default allowlist:

```
llama3.2:1b  llama3.2:3b  llama3.1:8b
qwen2.5:0.5b qwen2.5:1.5b qwen2.5:3b qwen2.5:7b qwen2.5:14b
deepseek-r1:1.5b deepseek-r1:7b deepseek-r1:8b deepseek-r1:14b
mistral:7b   phi3:mini   gemma2:2b   gemma2:9b
```

Matched as a prefix, so quantisation tags are fine
(`llama3.2:1b-instruct-q4_K_M` appears as `llama3.2:1b`-family and is listed).

Always excluded, even with `GPN_PUBLIC_MODELS=all`:

* fine-tune artifacts — `tuned-<job_id>` (per-job LoRA merges)
* toy models — `stories260K`, anything under **0.3B** parameters
* embedding/reranker models (`*embed*`, `*rerank*`)

**Excluded ≠ unusable.** A model not in the catalogue is still served when a
caller names it explicitly, and a provider still runs it. The catalogue is a
shop window, not a permission system.

| variable | example | effect |
|---|---|---|
| `GPN_PUBLIC_MODELS` | `llama3.2:1b,qwen2.5:7b` | replaces the allowlist |
| | `all` | publish everything not in the hard-exclusion set |

`/v1/models` never returns an empty list: with no provider online it advertises
the `llama3.2:1b` baseline (clients call it before anything else and treat `[]`
as an outage).

---

## 4. `gflops: 0`

`gflops` is the measured FP32 GEMM throughput written to the attestation
(`compute_gflops`). `0` means one of:

* the benchmark could not open the GPU (`GPU compute measurement FAILED (…)` in
  the agent log — driver/OpenCL/runtime problem), or
* the configured `--gflops`/`GFLOPS`/`GPN_GFLOPS` for the heartbeat is missing.

Consequences, now logged instead of silent:

* the agent warns `attested compute is 0 GFLOPS — every marketplace job with
  min_gflops > 0 will be skipped`;
* `list_claimable_jobs` prints `[marketplace] skipping N pending job(s) whose
  min_gflops exceeds this provider (…)` at most once a minute;
* the heartbeat prints a startup warning explaining both causes and the fix.

Jobs with `min_gflops = 0` (open market) are still claimable, and LLM jobs
delivered through the access point's pull path are unaffected — this gate is the
on-chain marketplace only.
