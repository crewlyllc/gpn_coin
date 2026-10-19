# Run a node (single command)

GPN ships a **single-node supervisor**, `gpn_node`, so an operator runs **one
command** and gets **one node** — the chain node/miner, the provider heartbeat,
and the provider loop(s) — started, logged (one prefixed stream), monitored
(one status line) and stopped cleanly, together.

**A GPN node is always both a miner and an AI provider.** There is no role
option and no mine-only / provider-only mode: every `gpn_node` start brings up
the chain node/miner *and* the AI serving stack. The node mines and serves AI,
**auto-yielding the one GPU** to a job while it runs (mining fully stops for the
job's duration, then resumes — see #24).

> **⚑ “One GPU, one job” = MUTUAL EXCLUSION, not a limit on job types.**
> The single GPU runs **exactly one job at a time** — never two concurrent — so
> it never over-subscribes its compute/VRAM. It does **NOT** mean only some job
> types are available: **every node is equipped by design to serve every job
> type** — LLM chat · image · video · ML inference · fine-tune — and advertises
> all of them. The scheduler routes each incoming job to a node that can serve
> it; that node runs that **one** job to completion (mining paused) before
> accepting another. All capabilities are ON by default; a constrained box can
> opt a capability out with `GPN_DIFFUSION=0` / `GPN_ML=0` / `GPN_FINETUNE=0`
> (or `diffusion = false` etc. in the config file), but the default is
> “everything available”.

Internally the parts stay **separate processes** (fault isolation: a crash in
the AI stack cannot take the chain node down), but the operator never has to
manage them individually.

```
                       ┌──────────────────────────────┐
   one command  ───▶   │          gpn_node            │
   one config          │      (supervisor)            │
   one log             └──┬──────┬──────┬──────┬─────┬┘
   one status line       │      │      │      │     │
                    [node] [heartbeat] [provider] [diffuse] [ml] [finetune]
                   gpn-node  gpn_provider gpn_llm_  gpn_diffusion_  gpn_ml_  gpn_finetune_
                   (miner)    _status    provider   provider      provider  provider
```

## Quick start

```bash
# 1) install (builds gpn-node + the gpn-agent bins, generates a wallet)
curl -fsSL <REPO>/install.sh | sh      # or ./install.sh from a clone

# 2) ollama is REQUIRED (a node = miner + AI provider). `--setup` installs,
#    starts, and pulls the baseline model in one shot; a normal start does the
#    same automatically and REFUSES to run if ollama cannot be reached.
./scripts/install-ollama.sh            # Linux/macOS; Windows: scripts\install-ollama.ps1
./gpn_node --setup                     # verify/bootstrap (exit 0 = ready)

# 3) run ONE command — starts the miner AND the AI provider together
gpn_node
```

That's it. `gpn_node` reads its config from a small file + env, then starts and
supervises all of the children — always: the miner and the AI serving stack.

## REQUIRED: a public address + a forwarded port (every node must be dialable)

**A GPN node must be dialable to join the network.** Peers have to be able to
open an **inbound** connection to your node — not just for your node to reach
them. Every node being dialable is what makes GPN a **full mesh**: any node can
serve blocks/state to any other node, and the bootnode is only a **seed** that
introduces peers to each other. A node behind NAT with no port forward can dial
out and sync, but nobody can dial *it* — that collapses the mesh into a star
around the bootnode — so **it is not allowed to join**: it fails loudly and does
not sync, mine, or serve.

Required setup, on every node:

1. **Forward a TCP port** on your router/firewall to this machine (the node
   listen port, default **30333**; use a different one if you run several nodes).
2. **Tell the node its public address**, either in `~/.gpn/node.toml`:

   ```toml
   public_addr = "/ip4/203.0.113.10/tcp/30333"
   ```

   or via the environment (wins over the config file / `--public-addr`):

   ```bash
   GPN_PUBLIC_ADDR=/ip4/203.0.113.10/tcp/30333
   ```

   Use `/ip6/<addr>/tcp/<port>` for IPv6, and the **same port** you forwarded.
3. **Restart** `gpn_node`. It verifies inbound reachability before it joins.

If you are on a VPS with a directly bound public IP, the gate also accepts a
public **listen** address; still set `public_addr` so peers are told the right
endpoint.

### The dialability gate

After the network starts, the node waits up to **120 s**
(`GPN_DIALABLE_TIMEOUT_SECS`) for evidence that it is inbound-reachable:

- a peer **dials us** (an inbound connection), or
- the **configured public address** is observed by peers (`GPN_PUBLIC_ADDR` /
  `--public-addr`, exact IP **and** port), or
- the node listens directly on a **public** IP.

Only public (globally routable) addresses count — RFC1918 (`10/8`, `172.16/12`,
`192.168/16`), CGNAT (`100.64/10`), loopback, link-local and `0.0.0.0` never do.

If no evidence appears, the node **fails closed** — it logs:

```
❌ this node is NOT dialable — set GPN_PUBLIC_ADDR to /ip4/<public-ip>/tcp/<port>
   and forward that port on your router; refusing to join the network
```

and exits with code **78** (`EX_CONFIG`) *before* starting the miner or the RPC
service. `gpn_node` surfaces this as `node=down(not dialable, restarts N)` in its
`[status]` line and retries with the bounded backoff (≥30 s, no tight loop) —
fix the port forward and it will join on the next attempt.

> **Local / single-node testing only:** set `GPN_REQUIRE_DIALABLE=0` to skip the
gate and restore the old behaviour (the node will run even though it is not
reachable). Do **not** use this on a real network node.

| Env var | Meaning | Default |
|---------|---------|---------|
| `GPN_REQUIRE_DIALABLE` | Enforce the inbound-reachability gate. `0`/`false`/`no`/`off` opts out (local testing only). | `1` (ON) |
| `GPN_PUBLIC_ADDR` | This node's public multiaddress, e.g. `/ip4/203.0.113.10/tcp/30333`. Must match the forwarded port. | _(none → `--public-addr`/`public_addr`)_ |
| `GPN_DIALABLE_TIMEOUT_SECS` | How long to wait for inbound evidence before failing closed. | `120` |

## Ollama is a REQUIRED component (bundled installer + hard gate)

A GPN node is **always miner AND AI provider**. Ollama is therefore a
**first-class, required component**, not an optional extra:

* the installers **ship inside the release package** (`scripts/install-ollama.ps1`,
  `scripts/install-ollama.sh`, `scripts/install-ollama-amd.sh`), together with
  `gpn-node.example.toml` and `README-FIRST-RUN.txt`;
* `gpn_node` **probes `GPN_OLLAMA` on every start**, and if nothing answers it
  runs the **automated first-run bootstrap**: install (winget / official
  installer / bundled script) → start `ollama serve` detached → wait for
  `/api/tags` → pull the baseline model;
* if ollama is **still unreachable**, the node **REFUSES TO START**. It logs an
  actionable error, prints `node=down(no ollama)`, and exits **69**
  (`EX_UNAVAILABLE`) so a supervisor/service manager reports the real cause.
  It never silently runs miner-only.

### One-shot setup

```bash
./gpn_node --setup          # Linux / macOS
```
```powershell
gpn_node.exe --setup        # Windows
```

Exit code `0` means ollama is serving and every baseline model is present;
otherwise it exits `69` with install instructions.

### Manual install (what `--setup` automates)

```bash
# Linux / macOS
./scripts/install-ollama.sh                    # installs, starts, pulls the baseline
# or manually
curl -fsSL https://ollama.com/install.sh | sh
ollama serve &                                 # keep running (listens on 127.0.0.1:11434)
ollama pull llama3.2:1b
```

```powershell
# Windows
powershell -ExecutionPolicy Bypass -File scripts\install-ollama.ps1
# or manually
winget install Ollama.Ollama
ollama serve
ollama pull llama3.2:1b
```

### Gate + environment

| Env var | Meaning | Default |
|---------|---------|---------|
| `GPN_OLLAMA` | ollama base URL | `http://127.0.0.1:11434` |
| `GPN_REQUIRE_OLLAMA` | Hard gate. `0`/`false`/`no`/`off` opts out (**special cases only**) | `1` (ON) |
| `GPN_OLLAMA_SETUP` | Attempt the automated install/start on startup | `1` (ON) |
| `GPN_BASELINE_MODELS` | Comma-separated models to pull | `llama3.2:1b` |
| `GPN_OLLAMA_WAIT_SECS` | Grace window for `/api/tags` after starting | `60` |
| `GPN_OLLAMA_PULL_TIMEOUT_SECS` | Max time for one model pull | `900` |

`GPN_REQUIRE_OLLAMA=0` starts the node in a loud **degraded** mode (mining
only). It exists for CI smoke tests and deliberately mining-only boxes — it is
**not** a supported production configuration.

### First-run flow

1. Unpack the release zip (binaries + `scripts/` + `README-FIRST-RUN.txt`).
2. Run `gpn_node --setup` (or just start `gpn_node` — it does the same).
3. Set `GPN_PUBLIC_ADDR` (+ `GPN_ACCOUNT`, `GPN_SIGNER`, `GPN_NODE_KEY`).
4. Start `gpn_node`: it passes the ollama gate, then starts the miner, the
   heartbeat, and every provider loop.

When ollama is up but a model is merely missing, the node still starts (model
presence is not the gate); the provider pulls/retries and logs a **single**
actionable warning instead of per-poll `pull … failed` spam.

## AMD / Radeon support (ROCm)

GPN is GPU-native and supports **AMD** as a first-class vendor alongside NVIDIA.
Two independent things use the GPU, and they have different requirements:

| Use | Needs | Notes |
|-----|-------|-------|
| **Mining** (Ethash) | an **OpenCL** ICD | vendor-agnostic — AMD, NVIDIA, Intel all mine. Install `rocm-opencl-runtime` on Linux (then reboot). |
| **Serving LLMs** (ollama) | an ollama-supported backend | Linux: **ROCm**; Windows: limited → often **CPU** fallback. |

**GPU detection is automatic and vendor-agnostic.** The heartbeat
(`gpn_provider_status`) and the node probe the local GPU and advertise
`gpu_class` (`discrete`/`integrated`/`unknown`), `vram_mb`, and an FP32 `tflops`
estimate so the min-GPU gate (#19) and the scheduler route correctly:

* Linux — `nvidia-smi`, then **`rocm-smi`** (`--showproductname`,
  `--showmeminfo vram`), then `lspci` + `/sys/class/drm`.
* Windows — `Win32_VideoController` (`Get-CimInstance` / `wmic`) for name +
  `AdapterRAM`. ⚠️ Windows caps `AdapterRAM` at 4 GiB, so set `--vram-mb` (or
  `VRAM_MB`) to the real figure for big cards.

Override anything with `--gpu-class`/`--tflops`/`--vram-mb` (or the `GPU_CLASS`,
`TFLOPS`, `VRAM_MB` env vars).

### Serving LLMs on AMD (Linux — ROCm)

```bash
# 1) the OpenCL ICD (mining) + the ROCm runtime (ollama):
sudo apt install rocm-opencl-runtime          # mining; then reboot
# 2) bring ollama up on ROCm (installs, starts, pulls the baseline):
./scripts/install-ollama-amd.sh
```

Ollama's bundled ROCm supports a **subset** of AMD cards (RDNA2/RDNA3 — RX
6000/7000 series, plus some older GCN). If it errors with *"unsupported gfx"*,
pin the compute target **before** `ollama serve`:

```bash
export HSA_OVERRIDE_GFX_VERSION=11.0.0   # RX 7000 (gfx1100/1101/1102)
# export HSA_OVERRIDE_GFX_VERSION=10.3.0  # RX 6000 (gfx1030/1031/1032)
ollama serve
```

Confirm GPU inference (not CPU): `ollama ps` (the `PROCESSOR` column should say
`ROCm`/`GPU`) — or `grep -i 'inference compute' ~/.gpn/ollama.log`.

### Windows + AMD (ROCm is limited)

On Windows, ollama's ROCm support is limited/experimental. In practice an AMD
Windows box either **(a)** runs ollama on the **CPU — correct but slow**, or
**(b)** needs a DirectML/ROCm build if one exists for the card. **Mining is
unaffected** (pure OpenCL). This is why an AMD **Windows** box may show high
latency on LLM jobs even though it mines fine — set expectations (small models,
quantized) and prefer **Linux** for full AMD GPU serving.

> **CPU-fallback caveat:** when ollama cannot use the GPU it silently falls back
to the CPU, giving ~10–50× lower token/s. The node still works and still mines;
just budget latency accordingly.

## What it starts (all by default)

| Child       | Process                                                                    | Log prefix  |
|-------------|----------------------------------------------------------------------------|-------------|
| miner/node  | the chain node / miner (`gpn-node …` — mines by default)                     | `[node]`    |
| heartbeat   | `gpn_provider_status` (advertises capability + load, `:8093`)               | `[heartbeat]` |
| provider    | `gpn_llm_provider` (claims + runs + settles LLM jobs)                       | `[provider]` |
| diffuse     | `gpn_diffusion_provider` (image/video jobs) — opt-out `GPN_DIFFUSION=0`      | `[diffuse]` |
| ml          | `gpn_ml_provider` (ML-inference jobs) — opt-out `GPN_ML=0`                   | `[ml]`      |
| finetune    | `gpn_finetune_provider` (LoRA fine-tune jobs) — opt-out `GPN_FINETUNE=0`     | `[finetune]` |

There is nothing to select — a node is miner + **every** provider. A node is
**designed to serve every job type** (LLM, image, video, ML inference,
fine-tune); the one-GPU rule only means it does **one job at a time**.
`gpn_node` honours the #24 semantics: **mine-by-default**, and the provider
holds the (separate) job flag so the miner fully stops during a job; the
supervisor runs the miner and the AI stack side by side.

> **Missing optional model/dep is non-fatal.** A provider whose model, helper
> script, or *optional* binary is absent is logged with a clear warning and
> skipped — the node, the miner, and the other providers stay healthy. This
> does **not** apply to ollama: ollama is required and the node will refuse to
> start without it (see above; `GPN_REQUIRE_OLLAMA=0` for special cases).

## Config: one small file + env overrides

Resolution order per field: **`GPN_*` env var → config file → default**.

The config file is, in order:

1. `--config <path>`
2. `$GPN_CONFIG`
3. `$GPN_PREFIX/node.toml` (`$GPN_PREFIX` defaults to `~/.gpn`)
4. `./gpn-node.toml`

Example (`~/.gpn/node.toml`, also see `gpn-node.example.toml`):

```toml
chain        = "testnet"                           # testnet | mainnet | dev | <path to chain-spec.json>
signer       = "//Alice"                           # sr25519 URI or seed (provider identity / authoring key)
account      = "5ENqSp5Q…"                         # payout / advertised ss58 address
endpoint     = "ws://127.0.0.1:9944"               # chain RPC used by the provider loop
base_path    = "~/.gpn/node"                      # or set GPN_BASE_PATH; default: C:\gpn\data (Win) / ~/.gpn/node (Unix)
rpc_port     = 9944
node_key     = "<64-hex ed25519 secret>"           # node's network identity
bootnodes    = ["/ip4/23.241.109.255/tcp/30333/p2p/12D3KooW…"]
public_addr  = "/ip4/<your-public-ip>/tcp/30333"     # REQUIRED (see above; forward this port)
author       = "alice"                             # passed as --alice to gpn-node (authoring key)
node_args    = ["--no-telemetry", "--no-prometheus"]

provider_endpoint = "http://<your-public-ip>:8093/status"   # advertised on-chain
assign_token      = "<shared AP↔provider secret>"           # GPN_ASSIGN_TOKEN (see below)
ollama       = "http://127.0.0.1:11434"
listen       = "0.0.0.0:8093"                      # heartbeat bind
gflops       = 30000
capacity     = 1
poll_ms      = 300
# ── optional providers: ALL ON by design; set false to opt a capability OUT ──
diffusion    = true                                # image/video provider (gpn_diffusion_provider)
diffusion_poll_ms = 2000
ml           = true                                # ML-inference provider (gpn_ml_provider)
ml_poll_ms   = 2000
finetune     = true                                # fine-tune provider (gpn_finetune_provider)
finetune_poll_ms = 4000
```

### Environment variables (overrides)

| Env var | Meaning | Default |
|---------|---------|---------|
| `GPN_CHAIN` | chain spec (`testnet`/`mainnet`/`dev`/path) | `testnet` |
| `GPN_SIGNER` | signer URI / seed | `//Alice` |
| `GPN_ACCOUNT` | payout / advertised address | _(empty)_ |
| `GPN_ENDPOINT` | chain RPC (ws) for the provider loop | `ws://127.0.0.1:9944` |
| `GPN_BASE_PATH` | node data dir | _(node default)_ |
| `GPN_RPC_PORT` | node RPC port | `9944` |
| `GPN_NODE_KEY` | node network key (64-hex) | _(none — node needs one)_ |
| `GPN_BOOTNODES` | comma/space list of bootnodes | _(none)_ |
| `GPN_PUBLIC_ADDR` | `--public-addr` (e.g. `/dns4/host/tcp/30333`). **Required for a public node** — the dialability gate also reads it. | _(none)_ |
| `GPN_REQUIRE_DIALABLE` | Enforce inbound reachability (must be dialable to join). `0` opts out for local testing only. | `1` |
| `GPN_DIALABLE_TIMEOUT_SECS` | Grace window for dialability evidence before exit 78. | `120` |
| `GPN_AUTHOR` | short authoring key name → `--<name>` | _(none)_ |
| `GPN_NODE_ARGS` | extra raw args appended to `gpn-node` | _(none)_ |
| `GPN_PROVIDER_ENDPOINT` | public `/status` URL advertised on-chain | _(none → discovery off)_ |
| `GPN_ASSIGN_TOKEN` | shared AP↔provider secret (`POST /assign` auth) | _(empty → legacy-permissive, warns)_ |
| `GPN_OLLAMA` | ollama base URL | `http://127.0.0.1:11434` |
| `GPN_REQUIRE_OLLAMA` | Hard gate: refuse to start when ollama is unreachable. `0` opts out (special cases only). | `1` |
| `GPN_OLLAMA_SETUP` | Attempt the automated install/start if ollama is missing | `1` |
| `GPN_BASELINE_MODELS` | Comma-separated baseline model(s) to pull on first run | `llama3.2:1b` |
| `GPN_OLLAMA_WAIT_SECS` | Grace window for `/api/tags` after `ollama serve` | `60` |
| `GPN_LISTEN` | heartbeat bind addr | `0.0.0.0:8093` |
| `GPN_GFLOPS` | advertised compute | `0` |
| `GPN_CAPACITY` | concurrent jobs | `1` |
| `GPN_POLL_MS` | provider poll interval | `300` |
| `GPN_DIFFUSION` | image/video provider — set `0` to OPT OUT (default ON) | `1` |
| `GPN_DIFFUSION_POLL_MS` | diffusion poll interval | `2000` |
| `GPN_ML` | ML-inference provider — set `0` to OPT OUT (default ON) | `1` |
| `GPN_ML_POLL_MS` | ML-inference poll interval | `2000` |
| `GPN_FINETUNE` | fine-tune provider — set `0` to OPT OUT (default ON) | `1` |
| `GPN_FINETUNE_POLL_MS` | fine-tune poll interval | `4000` |
| `GPN_BIN` | dir holding the child binaries | dir of `gpn_node` |
| `GPN_STATUS_SECS` | status-line interval | `15` |
| `GPN_STALL_SECS` | sync-stall watchdog: restart node after `best` frozen this long (min 60) | `300` |
| `GPN_STALL_CHECK_SECS` | sync-stall watchdog poll interval (min 10) | `60` |
| `GPN_CONFIG` | config-file path | _(see search order)_ |

> **Secrets stay in the environment.** `gpn_node` reads the signer/seed and the
> assign token from env / the config file and never writes them anywhere. Put
> the token in `GPN_ASSIGN_TOKEN` (or the service `EnvironmentFile`), and keep
> the config file `chmod 600`.

### Sync-robustness defaults (why your node won't wedge)

GPN has **no finality gadget** — the finalized head stays at `#0`. That breaks
both of the things a node normally leans on to sync, so `gpn_node` bakes in the
one reliable combination (**on by default, overridable via `GPN_NODE_ARGS` /
`node_args`**):

| Flag | Why |
|------|-----|
| `--sync full` | `fast`/`warp` build their proof from a **finalized** block; with no finality they burst then stall early. (`full` is also the node default, but we pin it explicitly so a stray `--sync fast` can't re-introduce the wedge.) |
| `--state-pruning archive` | The node default is `256`, pruned **relative to finality** — i.e. it discards state almost immediately on GPN, so a peer that asks for it gets *"State already discarded"*, the peer bans us, and our best block freezes. `archive` never discards state. |
| `--blocks-pruning archive` | Same finality-relative problem for block bodies (default `archive-canonical`). `archive` keeps every body so we can always serve a syncing peer. |

Archive is cheap here (the whole testnet DB is well under a gigabyte); there is
no reason to prune on a chain this small.

> ⚠️ **Legacy DB caveat — and the pinned base path.** Substrate **locks the
> state-pruning mode into the DB at first creation**, and requesting `archive`
> on a DB that was created with the old `256` default **hard-fails**:
> `Incompatible pruning modes [stored: Constrained { max_blocks: Some(256) };
> requested: ArchiveAll]`.
>
> So `gpn_node` now **pins a single base path** and uses it for *both* the DB
> check and the node's `--base-path` (resolution: `GPN_BASE_PATH` → config
> `base_path` → default `C:\gpn\data` on Windows / `~/.gpn/node` on Unix). This
> matters because a node launched with **no** `--base-path` silently uses the
> OS-default data dir (`%LOCALAPPDATA%\gpn-node.exe\data` on Windows) — a guard
> that only inspected an *explicit* base path therefore looked at the wrong
> directory and forced `archive` onto a legacy DB → crash-loop.
>
> The archive default is applied only when the DB is **fresh**, or when it
> carries the `.gpn-pruning-archive` marker `gpn_node` drops next to a DB *it*
> created with archive. For any **existing legacy/unknown DB** it requests **no**
> pruning flags (so the node's compiled defaults match the stored mode) and logs
> a clear warning — it can no longer crash-loop on `Incompatible pruning modes`.
> For backward-compat, if no DB exists at the pinned path but one exists at the
> OS-default path, the OS-default DB is detected and used (with a warning).
> Such a legacy DB must still be **re-created** (see the Troubleshooting entry
> below) before it can be stall-proof.

### Disk headroom (why your chain can halt with a full disk)

On 2026-10-19 the root filesystem filled to **100%** and the only symptom was a
**chain halt**: the node mined a block but could not persist its header
(`ENOSPC`), so its in-memory best chain kept a hash that could never be fetched
again. Nothing warned beforehand.

- **Check it:** `df -h <your data dir>` (Linux/macOS) or
  `Get-PSDrive C` (Windows). Keep **≥10% free** (≈ 20 GB on a typical build box).
- **The heartbeat reports it:** `GET /status` on the provider heartbeat (`:8093`)
  now includes `disk_free_mb`, `disk_total_mb`, `disk_free_pct` (probe of
  `$GPN_DATA_DIR` or `$HOME`; `0`/`null` = probe unavailable, **not** full). The
  heartbeat also prints a `⚠ LOW DISK:` line at startup below 10%. Point a
  monitor at `disk_free_pct`.
- **Build hosts:** Rust `target/debug` is the usual culprit (it reached **217 GB**
  here). Run `cargo clean --profile dev` — or `cargo clean` — regularly, and keep
  `target/` on a **different filesystem** from the node's data directory where
  possible.
- **Recovery if it already halted:** free space, then restart the node
  (`systemctl --user restart gpn-fleet`). Freeing space alone does **not** clear
  the stale in-memory best-chain hash.

## What the supervisor does

- **Spawns each child with a label** and forwards its stdout/stderr with a
  `[node]` / `[heartbeat]` / `[provider]` / `[diffuse]` / `[ml]` / `[finetune]`
  prefix.
- **Restarts a crashed child** with bounded backoff (1s → 2s → … → 30s); the
  backoff resets once a child has been healthy for 60s. Every start / exit /
  restart is logged as `[supervisor HH:MM:SS] <label>: …`.
- **Stops all children gracefully** on `Ctrl-C` / `SIGTERM`: each child gets
  `SIGTERM`, is waited on for up to 10s, then `SIGKILL`ed, and the supervisor
  exits.
- **Prints one aggregated status line** every `status_secs` (default 15s):

  ```
  [status] node_count=6 node=up(pid 123) heartbeat=up(pid 456) provider=up(pid 789) diffuse=up(pid 790) ml=up(pid 791) finetune=up(pid 792) best=#107535
  ```

  `best` is the node's best block, read cheaply from its own RPC
  (`chain_getHeader`); `?` if the node's RPC isn't answering yet.

  A node that failed the **dialability gate** is shown distinctly as
  `node=down(not dialable, restarts N)` (the child exited 78), with an
  actionable hint in the log — set `GPN_PUBLIC_ADDR` and forward the port.

## Linux

```bash
# foreground (Ctrl-C stops everything) — always miner + provider
GPN_SIGNER=//Alice \
GPN_PROVIDER_ENDPOINT=http://<public-ip>:8093/status \
GPN_ASSIGN_TOKEN=$GPN_ASSIGN_TOKEN \
gpn_node
```

As a **systemd (user) service** — one unit replaces the per-part units:

```ini
# ~/.config/systemd/user/gpn-node.service
[Unit]
Description=GPN node (supervised: miner + heartbeat + provider)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile=%h/.gpn/node.env        # GPN_SIGNER, GPN_ASSIGN_TOKEN, …
ExecStart=%h/.local/bin/gpn_node --config %h/.gpn/node.toml
Restart=on-failure
RestartSec=5
KillSignal=SIGTERM
TimeoutStopSec=30

[Install]
WantedBy=default.target
```

```bash
systemctl --user daemon-reload
systemctl --user enable --now gpn-node.service
journalctl --user -u gpn-node.service -f     # the one prefixed log stream
```

## Windows

`gpn_node.exe` is the same binary. From the release bundle (`.\bin\`):

```powershell
# everything supervised under one console window (miner + provider)
$env:GPN_SIGNER           = "//Alice"          # or your seed
$env:GPN_CHAIN            = "testnet"
$env:GPN_PROVIDER_ENDPOINT= "http://<public-ip>:8093/status"
$env:GPN_ASSIGN_TOKEN     = "<shared secret>"
.\bin\gpn_node.exe --config .\gpn-node.toml
```

Use **Ctrl-C** in the window (or close it) to stop all children cleanly. To run
it as a service, wrap `gpn_node.exe` with NSSM / a Windows Service.

> Windows note: the connect step for a syncing node (`run-windows.bat`) still
> applies — a second node authoring on the **same key** would fork the chain.
> Only the single designated author should mine. Every node mines by default
> (`gpn-node` is an authority miner on its own key — no `--force-authoring`
> needed); the flag is accepted but no longer required.

### Console encoding on Windows (emoji / mojibake)

`gpn_node` prints a few non-ASCII glyphs (warning signs, a sleep/zzz marker,
the winner trophy, …). On a **legacy Windows console** these mojibake even with
`chcp 65001` (`⚠️` shows as `ΓÜá∩╕Å`, `💤` as `≡ƒÆñ`) because the text round-trips
the PowerShell pipeline (e.g. `Tee-Object`) through the OEM code page.

The supervisor now **sanitises every line it prints to ASCII** when needed:
known symbols map to readable tokens (`⚠️` → `!`, `💤` → `[zzz]`, `🏆` → `[win]`,
`📦` → `[pkg]`), dashes/ellipses map to `-`/`...`, and anything else becomes
`?`. Nothing ever mojibakes.

* **Default:** ASCII mode is **on for Windows**, **off for Linux/macOS** (modern
  terminals render UTF-8 fine).
* **Force it on** anywhere: pass `--no-emoji`, or set `GPN_ASCII_LOG=1`.
* **Force it off** (keep emoji): set `GPN_ASCII_LOG=0`.

```powershell
# plain ASCII on any console (e.g. a legacy WS2016/2019 box):
$env:GPN_ASCII_LOG = "1"
.\bin\gpn_node.exe --config .\gpn-node.toml
#   …or one-shot:  .\bin\gpn_node.exe --config .\gpn-node.toml --no-emoji
```

## Watchdog — auto-recover a silently wedged node

GPN has no finality, so a node can stay **p2p-connected** while its best block
silently freezes (see the Troubleshooting entry below). `gpn_node` watches for
exactly this: it polls the node's own RPC (`chain_getHeader` → best number)
every `GPN_STALL_CHECK_SECS` (default **60 s**). If `best` is **unchanged for
more than `GPN_STALL_SECS` (default 300 s = 5 min)** *while at least one peer is
connected*, it logs a loud `SYNC STALL` alert and **restarts the node child**
(SIGTERM → the supervisor restarts it with its normal backoff → it re-syncs).

It is deliberately conservative so it never fights healthy behaviour:

* normal syncing advances `best` on every check → never triggers;
* if the node's RPC isn't answering yet (startup / mid-restart) the clock is
  **reset**, not counted;
* with **zero** peers it logs but does **not** restart (a peerless node can't
  sync anyway — that's a different problem);
* after a restart it resets its timer, so it can't spiral into a tight kill loop.

Tune it with `GPN_STALL_SECS` (min 60) and `GPN_STALL_CHECK_SECS` (min 10).

## Troubleshooting — node connected but not syncing

**Symptom.** The node shows peers at the p2p layer — `system_peers` lists them as
`FULL`/`AUTHORITY` — but its **best block is frozen** while the network head
keeps advancing. `system_syncState` shows `currentBlock` stuck. Logs may show
`State already discarded` followed by `Same block request multiple times.
Banned, disconnecting` — the sync peer bans the node, it has no other peer, and
it never imports another block. This is the *silent wedge*.

**Mechanism (why).** GPN has **no finality gadget** — the finalized head is
`#0` forever. Two things then go wrong:

1. **Finality-relative pruning.** The node's default pruning is
   `--state-pruning 256` / `--blocks-pruning archive-canonical`, both measured
   *from the finalized block*. With finality pinned at `#0`, the node discards
   state and block bodies almost immediately. A peer that requests them gets
   *"State already discarded"*, the request repeats, and the peer bans us.
2. **Finality-dependent sync modes.** `--sync fast` / `warp` need a *finalized*
   block to build their state proof. With no finality they burst for a moment
   then stall early.

Because the node loses its only peer to a ban, it can no longer sync and its
best block freezes — while still looking "connected" in the peer list.

**Fix.** Run with the reliable combination (which `gpn_node` now defaults to):

```bash
gpn-node --chain testnet --base-path ~/.gpn/node --sync full \
  --state-pruning archive --blocks-pruning archive …
```

**If the node already has a (legacy `256`) DB**, the pruning mode is locked in
and `archive` will hard-fail — re-create the data dir and let it re-sync, or
import a fresh **archive** DB snapshot (fast — see
[`docs/fast-sync.md`](fast-sync.md)):

```bash
# option A — re-sync from scratch (keep the node key / config):
mv ~/.gpn/node/chains/gpn_testnet/db ~/.gpn/node/chains/gpn_testnet/db.old
#   …then restart gpn_node: it creates a FRESH archive DB and full-syncs.

# option B — fast onboarding from a snapshot (recommended for a slow box):
scripts/gpn-snapshot.sh import --snapshot gpn-testnet-<tip>.tar.gz --base-path ~/.gpn/node --force
```

On Windows the legacy DB often lives at `%LOCALAPPDATA%\gpn-node.exe\data`
(the OS default, used before the base path was pinned). `gpn_node` auto-detects
it, but to migrate it cleanly: stop the `gpn*` processes, then either **wipe
`chains/`** (and let it re-sync) or import the archive snapshot with
`--base-path C:\gpn\data` (see the fast-sync doc and `docs/downloads.md` for the
snapshot URL).

> **Snapshot pruning marker.** A `gpn_snapshot` export from a modern `gpn-node`
> is always an **archive** DB, so `gpn_snapshot import` now **stamps the
> `.gpn-pruning-archive` marker by default** — including for older snapshots
> whose manifest predates the marker feature and carries no `pruning` field.
> `gpn_node` then knows the imported DB is safe to run with
> `--state-pruning archive` (without the marker it conservatively leaves pruning
> untouched). Override with `--pruning archive|none` (e.g.
> `--pruning none` to import a legacy non-archive DB).

> **Deeper root cause:** this failure mode only exists because GPN has **no
> finality gadget**. Archive pruning + `--sync full` are the correct *workaround*
> and make nodes robust today, but the *real* fix is to add finality at mainnet
> — see the finality-gadget design cross-reference in
> [`docs/mainnet-launch-runbook.md`](mainnet-launch-runbook.md). Until then,
> assume no finality and never rely on `finalized #> 0`.

## Troubleshooting — slow LLM jobs on a dual-stack / AAAA-laggy host (the `/result` stall)

**Symptom.** End-to-end text/image jobs through the access point work, but are
**slow** — routinely 10–25s instead of <5s — even though the provider itself
finishes inference in ~1s. The AP log shows it falling back to its ~11s
on-chain *reconcile* instead of learning the result immediately.

**Mechanism (why).** When the AP and provider are **co-located**, the AP still
advertises a *public* callback URL (`GPN_PUBLIC_URL`, e.g.
`http://gpn-testnet.duckdns.org:8092`) and the provider POSTs the finished
result back to `{callback}/result/{id}`. `reqwest` resolves that host with
`getaddrinfo(AF_UNSPEC)` — **both A and AAAA**. If the host resolver does *not*
answer AAAA (a common router/DNS quirk — e.g. `192.168.1.1`), the AAAA leg
blocks until it times out:

```bash
$ getent ahostsv6 gpn-testnet.duckdns.org    # hangs → rc=124 (timeout)
$ getent ahostsv4 gpn-testnet.duckdns.org    # instant → A record, rc=0
$ curl    http://gpn-testnet.duckdns.org:8092/health   # http=000 after ~8s (stalls)
$ curl -4 http://gpn-testnet.duckdns.org:8092/health   # http=200 in ~0.02s
```

Because the `/result` return is best-effort with a hard **5s** budget, the stall
makes it time out, the AP never learns the result off-chain, and it falls back
to the slow reconcile — so e2e blows the <5s target. It is **not** an inference
or chain problem; it is pure name resolution.

**Fix (no root needed).** GPN is now robust to this: the provider's `/result`
(and the AP's `/assign` push) use a **custom resolver that only issues A (IPv4)
queries** plus a bounded connect timeout, so a slow/absent AAAA can never block
them (see `gpn-agent/src/net.rs`). No host DNS change is required.

**Optional — remove the dependency entirely (co-located boxes).** If your AP and
provider run on the same machine, point the provider at loopback so no public
lookup happens at all. In `~/.gpn/provider.env`:

```bash
# either form (a bare base, or the full .../result URL) works:
GPN_RESULT_URL=http://127.0.0.1:8092
# GPN_AP_URL=http://127.0.0.1:8092
```

Precedence is `GPN_RESULT_URL` > `GPN_AP_URL` > the AP-advertised `callback`
(remote providers keep using the advertised URL). The AP itself now advertises
loopback automatically when its `--listen` is a wildcard bind (`0.0.0.0:8092`)
and no `GPN_PUBLIC_URL`/`GPN_AP_CALLBACK_URL`/`--callback-url` is set.

**Verify.** Reproduce the raw stall and confirm the fix in one shot:

```bash
cd gpn-agent && cargo run --example dns_probe -- http://gpn-testnet.duckdns.org:8092/health
# dual-stack   ERROR in 8.000s: error sending request …   ← the stall
# ipv4-only    http=200 in 0.013s                        ← the fix
```

**Real host fix (needs root — optional; the above makes GPN robust without it).**
Point the host resolver at a resolver that answers AAAA, then flush:

```bash
sudo resolvectl dns <iface> 1.1.1.1 192.168.1.1 && resolvectl flush-caches
# e.g. iface = enp42s0
```

## Fault isolation / why separate processes

The supervisor deliberately keeps one process per part: an ollama/diffusion
crash, an OOM in the AI stack, or a wedged inference never takes the chain node
down — the supervisor restarts just that child. Folding everything into one
binary is tracked separately (task #26, deferred).
