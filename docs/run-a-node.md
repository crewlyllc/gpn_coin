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

Internally the parts stay **separate processes** (fault isolation: a crash in
the AI stack cannot take the chain node down), but the operator never has to
manage them individually.

```
                       ┌──────────────────────────────┐
   one command  ───▶   │          gpn_node            │
   one config          │      (supervisor)            │
   one log             └───┬───────────┬──────────┬───┘
   one status line         │           │          │
                       [node]     [heartbeat] [provider]
                      gpn-node   gpn_provider  gpn_llm_provider
                      (miner)      _status     (+ gpn_diffusion_provider)
```

## Quick start

```bash
# 1) install (builds gpn-node + the gpn-agent bins, generates a wallet)
curl -fsSL <REPO>/install.sh | sh      # or ./install.sh from a clone

# 2) make sure ollama is running (REQUIRED to serve LLM jobs — see below)
./scripts/install-ollama.sh            # Linux/macOS; Windows: scripts\install-ollama.ps1

# 3) run ONE command — starts the miner AND the AI provider together
gpn_node
```

That's it. `gpn_node` reads its config from a small file + env, then starts and
supervises all of the children — always: the miner and the AI serving stack.

## Ollama is a prerequisite (for serving LLMs)

A GPN node **cannot serve LLM jobs without ollama** — the provider talks to
`http://127.0.0.1:11434`. **Mining still works** without it, but every AI job
fails (`pull … failed: error sending request for url (http://127.0.0.1:11434/api/pull)`),
and `gpn_node` prints a clear warning at startup telling you exactly this.

Install + start it once (idempotent helpers are in `scripts/`):

```bash
# Linux / macOS
./scripts/install-ollama.sh                    # installs, starts, pulls llama3.2:1b
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

The baseline model(s) can be overridden with `GPN_BASELINE_MODELS`
(default `llama3.2:1b,qwen2.5:0.5b`). When ollama is down the provider logs a
**single** actionable warning (never a per-poll `pull … failed` spam).

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

## What it starts (always)

| Child       | Process                                                                    | Log prefix  |
|-------------|----------------------------------------------------------------------------|-------------|
| miner/node  | the chain node / miner (`gpn-node …` — mines by default)                     | `[node]`    |
| heartbeat   | `gpn_provider_status` (advertises capability + load, `:8093`)               | `[heartbeat]` |
| provider    | `gpn_llm_provider` (claims + runs + settles LLM jobs)                       | `[provider]` |
| diffuse     | `gpn_diffusion_provider` (only when `diffusion = true`)                     | `[diffuse]` |

There is nothing to select — a node is miner + provider. `gpn_node` honours the
#24 semantics: **mine-by-default**, and the provider holds the (separate) job
flag so the miner fully stops during a job; the supervisor runs the miner and
the AI stack side by side.

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
base_path    = "~/.gpn/node"
rpc_port     = 9944
node_key     = "<64-hex ed25519 secret>"           # node's network identity
bootnodes    = ["/ip4/23.241.109.255/tcp/30333/p2p/12D3KooW…"]
author       = "alice"                             # passed as --alice to gpn-node (authoring key)
node_args    = ["--no-telemetry", "--no-prometheus"]

provider_endpoint = "http://<your-public-ip>:8093/status"   # advertised on-chain
assign_token      = "<shared AP↔provider secret>"           # GPN_ASSIGN_TOKEN (see below)
ollama       = "http://127.0.0.1:11434"
listen       = "0.0.0.0:8093"                      # heartbeat bind
gflops       = 30000
capacity     = 1
poll_ms      = 300
diffusion    = false                               # also run gpn_diffusion_provider
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
| `GPN_PUBLIC_ADDR` | `--public-addr` (e.g. `/dns4/host/tcp/30333`) | _(none)_ |
| `GPN_AUTHOR` | short authoring key name → `--<name>` | _(none)_ |
| `GPN_NODE_ARGS` | extra raw args appended to `gpn-node` | _(none)_ |
| `GPN_PROVIDER_ENDPOINT` | public `/status` URL advertised on-chain | _(none → discovery off)_ |
| `GPN_ASSIGN_TOKEN` | shared AP↔provider secret (`POST /assign` auth) | _(empty → legacy-permissive, warns)_ |
| `GPN_OLLAMA` | ollama base URL | `http://127.0.0.1:11434` |
| `GPN_LISTEN` | heartbeat bind addr | `0.0.0.0:8093` |
| `GPN_GFLOPS` | advertised compute | `0` |
| `GPN_CAPACITY` | concurrent jobs | `1` |
| `GPN_POLL_MS` | provider poll interval | `300` |
| `GPN_DIFFUSION` | also run the diffusion provider | `false` |
| `GPN_DIFFUSION_POLL_MS` | diffusion poll interval | `2000` |
| `GPN_BIN` | dir holding the child binaries | dir of `gpn_node` |
| `GPN_STATUS_SECS` | status-line interval | `15` |
| `GPN_CONFIG` | config-file path | _(see search order)_ |

> **Secrets stay in the environment.** `gpn_node` reads the signer/seed and the
> assign token from env / the config file and never writes them anywhere. Put
> the token in `GPN_ASSIGN_TOKEN` (or the service `EnvironmentFile`), and keep
> the config file `chmod 600`.

## What the supervisor does

- **Spawns each child with a label** and forwards its stdout/stderr with a
  `[node]` / `[heartbeat]` / `[provider]` / `[diffuse]` prefix.
- **Restarts a crashed child** with bounded backoff (1s → 2s → … → 30s); the
  backoff resets once a child has been healthy for 60s. Every start / exit /
  restart is logged as `[supervisor HH:MM:SS] <label>: …`.
- **Stops all children gracefully** on `Ctrl-C` / `SIGTERM`: each child gets
  `SIGTERM`, is waited on for up to 10s, then `SIGKILL`ed, and the supervisor
  exits.
- **Prints one aggregated status line** every `status_secs` (default 15s):

  ```
  [status] node_count=3 node=up(pid 123) heartbeat=up(pid 456) provider=up(pid 789) best=#107535
  ```

  `best` is the node's best block, read cheaply from its own RPC
  (`chain_getHeader`); `?` if the node's RPC isn't answering yet.

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

## Fault isolation / why separate processes

The supervisor deliberately keeps one process per part: an ollama/diffusion
crash, an OOM in the AI stack, or a wedged inference never takes the chain node
down — the supervisor restarts just that child. Folding everything into one
binary is tracked separately (task #26, deferred).
