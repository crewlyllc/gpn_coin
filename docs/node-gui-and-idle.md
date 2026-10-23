# Node dashboard + Idle mode

A GPN node is meant to run on a gaming PC. **Idle mode** is the "I want to play
a game now" switch: it pauses the two things that compete for the GPU — Ethash
mining and AI job work — while the node **stays a node**: syncing, peering and
serving the chain to everyone else. At the end of the window it **catches up and
resumes by itself**; a **Resume now** button (and a CLI verb) ends it early.

> Idle is **not** "turn GPN off". Nothing is stopped, uninstalled or
> de-registered. Your node is still online, still synced, still reachable — it
> just stops burning the GPU for a while.

**Max one window: 6 hours.** Longer requests are refused with a message that
names the limit (config-file values are clamped instead of refused).

---

## 1. Launch the dashboard

```bash
gpn_node --gui                # http://127.0.0.1:8099
GPN_GUI=1 gpn_node            # same thing via env
GPN_GUI=1 GPN_GUI_LISTEN=127.0.0.1:9000 gpn_node   # another port
```

`--gui` adds a child-free HTTP thread inside the **supervisor** (`gpn_node`), so
it sees every child process, the chain height, the peer count and the GPU.
Without `--gui` the node prints one hint line at startup:

```
  dashboard: off — add --gui to serve the local control panel on 127.0.0.1:8099
```

With `--gui`:

```
  dashboard: http://127.0.0.1:8099 (loopback only — open it in a browser)
```

Open that URL in any browser. There is no build step and no CDN: the page is
compiled into the binary (`gpn-agent/src/gui/index.html`, `include_str!`), so it
works offline and on a fresh machine.

### Fleet-supervised nodes (`gpn_fleet`)

The same dashboard is available when the node is supervised by `gpn_fleet`
(the fleet/seed deployments) — same flags, same port, same page:

```bash
# locally
gpn_fleet --gui
# or in the unit / run-fleet.sh
export GPN_GUI=1
```

Idle needs no extra wiring there: it is **one file** (`~/.gpn/idle.json`) read by
the node's miner (`gpn-node`, every 500 ms) and by every provider's claim gate,
so a file written by the dashboard parks the miner and stops new claims no
matter which supervisor launched the processes. `gpn_fleet` adds the node
child's state to `GET /api/status` (the provider services are separate systemd
units, so they are not listed as children).

### Loopback only, by design

The dashboard can stop the miner, which makes it a control surface. It therefore
**only** binds `127.0.0.1` (`localhost` is normalised to `127.0.0.1`). Any other
address — `0.0.0.0`, `192.168.x.x`, a hostname — is **refused at startup** with a
clear error, and the node keeps running without the dashboard:

```
[gpn_node] dashboard not started: refusing to bind the dashboard to 0.0.0.0:8099:
it only ever listens on 127.0.0.1 (a LAN-reachable dashboard would be an
unauthenticated remote control for the miner)
```

There is no authentication because there is no network exposure. If you need
remote access, use an SSH tunnel (`ssh -L 8099:127.0.0.1:8099 user@host`) — do
not try to bind the dashboard to a LAN address.

### Windows shortcut

`gpn-agent/windows/gpn-dashboard.cmd` starts `gpn_node.exe --gui` (idempotently —
it checks `tasklist` first) and opens the dashboard in the default browser.
Right-click → *Send to* → *Desktop (create shortcut)* and rename it "GPN Node".

### What the page shows

```
┌ GPN node  v0.2.0                                   ● active ─────────────┐
│                                                                          │
│  MODE      PEERS     BLOCK HEIGHT   MINING   GPU          CURRENT JOB     │
│  Active    12        #107535        on       RTX 4070     none            │
│  mining +  p2p       synced                  VRAM 512/…                   │
│  serving                                                                  │
│  MODELS SERVED 1     EARNINGS TODAY —                                     │
│                                                                          │
│  ┌ Idle mode ──────────────────────────────── ● resuming in 1h 59m ────┐  │
│  │ Pauses mining and AI job claims so games get the GPU — the node     │  │
│  │ keeps syncing, peering and serving the chain.                       │  │
│  │ How long?  [30 min] [1 hour] [2 hours] [4 hours] [6 hours]          │  │
│  │ [ Go idle ]  [ Resume now ]                                         │  │
│  │ ── Step aside automatically when my other GPU work runs  [ on ]     │  │
│  │ Why idle: gaming (manual)                                           │  │
│  └─────────────────────────────────────────────────────────────────────┘  │
│                                                                          │
│  PROCESSES   node up · heartbeat up · provider up · diffuse up · …        │
└──────────────────────────────────────────────────────────────────────────┘
```

It refreshes every 3 s (the countdown ticks locally every second and re-syncs on
each poll). Everything is labelled, keyboard reachable (`Tab` to the duration
radio group, `Space` to select, `Enter` on a button), and readable at 1280 px
and at phone width (the cards wrap, the buttons go full width). Dark theme
matches gpncoin.ai (`#07090f` background, `#7c6cff` → `#22d3ee` accent).

---

## 2. Idle semantics — precisely what stops and what does not

| Component | While idle | Why |
|---|---|---|
| Chain node (`gpn-node`) | **keeps running**, syncing, peering, serving blocks/state | it is the network's value; idle must never degrade it |
| Ethash miner (GPU or CPU thread) | **parked** — searches no nonces (0 % GPU) | this is the thing games fight with |
| `gpn_llm_provider` | **no new claims** (no poll of the AP, no `accept`, no `start`) | no AI work starts |
| `gpn_diffusion_provider` / `gpn_ml_provider` / `gpn_finetune_provider` | **no new claims** | same |
| Job already running | **finishes normally** | a half-run job would wedge escrow |
| Heartbeat (`:8093`), discovery, RPC | **unchanged** | the node must stay visible |
| Wallet, staking, on-chain identity | **unchanged** | idle is local and temporary |

A job is claimed **and** executed inside a single provider pass, so nothing is
ever cut in half: the pass either claims or it does not. The reconcile pass at
the top of every LLM cycle still runs while idle, so a job that was left in
`AwaitingVerification` (e.g. a tx confirmed after a timeout) is still driven to
its terminal, payable state — idle never strands a job.

### The state file

`~/.gpn/idle.json` (mode `0600`; override the path with `GPN_IDLE_FILE`):

```json
{"until": 1760003600, "reason": "gaming", "started": 1760000000}
```

- `until` is unix seconds. **`now < until` ⇒ idle**; at `until` the node is
  active again (the deadline is inclusive).
- An **expired file is not idle**: every reader treats it as absent and removes
  it. So a node that was powered off past the deadline simply starts **active**
  — there is no way to leave a box idled forever by mistake.
- A missing, empty or corrupt file is also "active" — an unreadable file must
  never be able to park the miner.
- The file is the *only* state, so idle survives a supervisor restart, a crash,
  or a reboot. Start `gpn_node` again inside the window and the startup banner
  says `IDLE: resuming in 42m 10s (until …)`.

The same file is read by three independent processes:

- the miner (`gpn-node` → `service.rs`, `park_reason()`), checked every 500 ms
  in the miner loop — the miner thread stays alive and parked, so resuming is
  immediate (no process or thread restart, no re-DAG);
- every provider (at its claim boundary, `gpn_agent::idle::claims_allowed()`);
- the dashboard (`GET /api/status`, the countdown).

### Auto-resume

At `until` the file is pruned and the miner's park loop sees "no reason → mine",
logging:

```
⛏  idle ended, resuming mining
[provider] idle ended, resuming job claims
```

"Catches up" is automatic: the node never stopped importing blocks, so there is
nothing to replay — it was already at tip. If the box was suspended/asleep
during the idle window, the node syncs normally on wake (the miner is parked
until the deadline either way).

### Manual resume

- **Dashboard:** the *Resume now* button (`POST /api/resume`).
- **CLI:** `gpn_node resume` — takes effect within a second, no restart.
- **File:** deleting `~/.gpn/idle.json` works too (documented, not recommended).

### Auto-idle — step aside for *your other* GPU work

Idle was built for "I want the GPU now". Auto-idle removes the click: when
something that is **not GPN** starts using the card, the node steps aside, and
when that work stops it comes back by itself.

> Auto-idle steps aside for your other GPU work; it is **opt-in** and **never
> overrides you**.

Enable it once, either way:

```bash
GPN_IDLE_AUTO=1 gpn_node --gui      # for this run
```

or flip **“Step aside automatically when my other GPU work runs”** in the Idle
panel — that toggle is persisted in `~/.gpn/idle-auto.json`, so it survives a
restart. Default is **off**.

**What counts as foreign load.** All three must hold:

1. `nvidia-smi` utilisation ≥ **40 %** (`GPN_IDLE_AUTO_UTIL_PCT`), **and**
2. GPN's own GPU lock is **free** — a job of ours is expected load, not foreign, **and**
3. at least **512 MiB** of VRAM (`GPN_IDLE_AUTO_MIN_FOREIGN_VRAM_MB`) is held by
   processes that are not ours. "Ours" means `gpn-*` and anything under
   `~/.gpn/` (including the bundled `llama-server`). Failing that, only when no
   GPN process holds GPU memory at all.

Rule 3 is what stops the desktop from triggering it: Xorg + the compositor sit
at ~275 MiB permanently, below the threshold. A game or a second model loads
gigabytes.

**Debouncing.** One sample is never enough:

| What | Samples | Spacing | Total |
|---|---|---|---|
| enter idle | `GPN_IDLE_AUTO_BUSY_SAMPLES` = 3 | `GPN_IDLE_AUTO_BUSY_SAMPLE_SECS` = 5 s | ~15 s |
| resume | `GPN_IDLE_AUTO_QUIET_SAMPLES` = 3 | `GPN_IDLE_AUTO_QUIET_SAMPLE_SECS` = 20 s | ~60 s |

The window is `GPN_IDLE_AUTO_MINUTES` (default **30**, clamped to the same 6 h
ceiling as a manual idle). Auto-idle resumes on whichever comes first: quiet for
the debounce, or the deadline.

**It never fights you.**

- A **manual** idle (any reason not starting with `auto:`) is **never** auto-resumed.
- If you click *Resume now* while auto-idle is active, auto-idle stays disarmed
  for `GPN_IDLE_AUTO_COOLDOWN_SECS` (default **600 s**) — no immediate
  re-engagement while you are still at the keyboard.
- The **sole-authority** valve applies: if this node has no peers (or its RPC is
  unreadable) it will not auto-idle, even under full load. It logs one line per
  load episode instead.
- Your other idle choices are untouched: choosing 2 hours by hand behaves
  exactly as before.

**Why it is idle** is shown in the panel (`Why idle: auto: foreign GPU load
87% (auto — foreign GPU work detected)` vs `… gaming (manual)`), and
`idle.reason` in `/api/status` carries the same string. The toggle's hint line
shows the live evidence (`GPU now: 87% util, 2304 MiB non-GPN`).

Log lines, with the evidence attached:

```
[idle-auto] idle: foreign GPU load (87% util): util 87%, VRAM 6000/8188 MiB,
  2304 MiB non-GPN (2 foreign PIDs), GPU lock free — stepping aside for 30 min (until 1760003600)
[idle-auto] resume: foreign load gone for 3 samples (util 3%, VRAM 1200/8188 MiB,
  80 MiB non-GPN (1 foreign PIDs), GPU lock free) — mining and job claiming resume
[idle-auto] manual resume seen — auto-idle disarmed for 600s (the operator is in charge)
[idle-auto] NOT stepping aside despite …: this node has no peers, so it may be the
  only block producer — idling would stop the chain.
```

---

## 3. Safety valve — never idle the last block producer

If this node is the **only** block-producing authority, idling it halts the
chain for everyone. GPN's miner is the node itself, so "no miner" really does
mean "no blocks".

Before honouring a request, the supervisor reads the node's own
`system_health` RPC:

- **peers ≥ 1** → allowed. (We cannot prove a peer is an authority, but a lone
  authority has *no peers at all*, so this rules the dangerous case out.)
- **peers = 0** → **refused** (HTTP `409`, CLI exit code `3`) with:

  ```
  this node has no peers, so it may be the only block producer — idling would
  stop the chain. Start another authority (or pass --force-idle /
  GPN_IDLE_FORCE=1 for a single-box dev chain).
  ```

- **RPC unreachable** (node starting, crashed, wrong port) → **refused**: "I
  cannot see another node" is exactly the state in which idling is dangerous.

Overrides, for a single-box dev chain or an operator who knows better:

```bash
gpn_node --gui --force-idle
GPN_IDLE_FORCE=1 gpn_node --gui
```

A refused request writes **no** state file, logs loudly, and leaves the node
mining. The dashboard shows the refusal message in the idle panel (and a
`no peers — idle is blocked` hint under the Peers card).

---

## 4. Endpoints

All JSON, same-origin (no CORS needed), `Cache-Control: no-store`.

| Method | Path | Body | Response |
|---|---|---|---|
| `GET` | `/` | — | the dashboard (`text/html`) |
| `GET` | `/api/status` | — | `200` status document |
| `POST` | `/api/idle` | `{"minutes": 60, "reason": "gaming"}` | `200` `{ok, minutes, until, started, reason, idle}` · `400` bad body/duration · `409` sole-authority refusal · `500` cannot write the file |
| `POST` | `/api/resume` | — | `200` `{ok, resumed: <bool>, idle}` (`resumed:false` = was already active) |
| `POST` | `/api/idle/auto` | `{"enabled": true}` | `200` `{ok, auto_idle}` (persists the toggle) · `400` non-boolean · `409` sampler not running |

`GET /api/status` (trimmed):

```json
{
  "node": "up",
  "peers": 12,
  "syncing": false,
  "best": 107535,
  "mining": true,
  "standby": false,
  "gpu": "NVIDIA GeForce RTX 4070",
  "vram": "VRAM 512/12282 MiB · 3% util · 41C",
  "current_job": null,
  "models": ["llama3.2:1b"],
  "children": [{"label": "node", "up": true, "pid": 1234, "restarts": 0, "detail": ""}],
  "chain": "testnet",
  "rpc_port": 9944,
  "account": "5Grwva…",
  "earnings_today": null,
  "earnings_note": "not tracked locally — see the explorer",
  "spec": "0.2.0",
  "idle": {"active": true, "until": 1760003600, "started": 1760000000,
           "reason": "auto: foreign GPU load 87%", "auto": true,
           "remaining_secs": 3517},
  "auto_idle": {"enabled": true, "engaged": true, "foreign": true,
                "util_percent": 87, "foreign_vram_mb": 2304, "threshold_pct": 40,
                "min_foreign_vram_mb": 512, "minutes": 30, "cooldown_secs": 0},
  "now": 1760000083,
  "max_idle_minutes": 360,
  "durations": [30, 60, 120, 240, 360],
  "force_idle": false,
  "version": "0.2.0",
  "dashboard": {"listen": "127.0.0.1:8099", "loopback": true}
}
```

Notes on honesty of the numbers:

- `mining` is `false` during an idle window, during version **standby**, and
  when the node child is down. It is not a GPU probe.
- `current_job` is non-null only while a provider holds the one-GPU job flag
  (`~/.gpn/gpu-yield`) — it reports `{"kind":"ai job"}` because the flag carries
  no job id.
- `earnings_today` is `null`: per-block reward accounting is not tracked
  off-chain. The dashboard renders `—` with the note above rather than inventing
  a number. (Making this real would need the explorer or a reward indexer — a
  product decision, not a dashboard one.)
- `models` comes from the local ollama `/api/tags` with a 700 ms timeout; an
  unreachable ollama yields an empty list, never a hang.

---

## 5. CLI

```bash
gpn_node idle 30      # pause mining + claims for 30 min (max 360)
gpn_node idle 360     # the max: 6 hours
gpn_node idle 600     # -> refused: "…longer than the 360 min (6 h) maximum" (exit 2)
gpn_node resume       # back to active immediately
gpn_node --gui        # start normally, with the dashboard
```

Both verbs answer **before** the update / ollama / network gates: pausing the
machine must never depend on a manifest fetch or a GPU probe. They exit
immediately and do not start a node — they edit the same file the running node
watches.

Exit codes: `0` done, `2` bad duration/usage, `3` refused by the safety valve.

---

## 6. Config

| Env | Flag | Config file | Default |
|---|---|---|---|
| `GPN_GUI=1` | `--gui` | `gui = true` | off |
| `GPN_GUI_LISTEN` | `--gui-listen` | `gui_listen = "127.0.0.1:8099"` | `127.0.0.1:8099` (loopback only) |
| `GPN_IDLE_FORCE=1` | `--force-idle` | `force_idle = true` | off |
| `GPN_IDLE_FILE` | — | — | `~/.gpn/idle.json` |
| `GPN_IDLE_AUTO=1` | — | — | **off** (also flipped by the dashboard toggle) |
| `GPN_IDLE_AUTO_FILE` | — | — | `~/.gpn/idle-auto.json` (persisted toggle) |
| `GPN_IDLE_AUTO_UTIL_PCT` | — | — | `40` (clamped 10…100) |
| `GPN_IDLE_AUTO_MIN_FOREIGN_VRAM_MB` | — | — | `512` |
| `GPN_IDLE_AUTO_BUSY_SAMPLES` | — | — | `3` (clamped 1…20) |
| `GPN_IDLE_AUTO_QUIET_SAMPLES` | — | — | `3` (clamped 1…20) |
| `GPN_IDLE_AUTO_BUSY_SAMPLE_SECS` | — | — | `5` |
| `GPN_IDLE_AUTO_QUIET_SAMPLE_SECS` | — | — | `20` |
| `GPN_IDLE_AUTO_MINUTES` | — | — | `30` (clamped to the 6 h ceiling) |
| `GPN_IDLE_AUTO_COOLDOWN_SECS` | — | — | `600` (after a manual resume) |

Typical `~/.gpn/node.toml` for a home gaming box:

```toml
# local dashboard for the idle switch
gui = true
gui_listen = "127.0.0.1:8099"
```

Then the desktop shortcut (or `http://127.0.0.1:8099`) is all a family member
needs: pick "2 hours", click *Go idle*, play, and the node comes back on its own.

### Running it as a service

The dashboard is served by the supervisor, so with a systemd user unit it is
available whenever the node is:

```bash
systemctl --user restart gpn-node     # after adding gui = true
curl -s http://127.0.0.1:8099/api/status | jq .idle
```

`gpn_node idle 120` works from any shell as that user — the running service
picks the change up from the file within a second. No `sudo`, no restart.

---

## 7. Tests

All of this is unit/integration tested without a chain, GPU or ollama:

- `gpn-agent/src/idle.rs` (14): file round-trip + `0600`, 6 h ceiling and
  zero/over-long rejection with the ceiling in the message, config clamping,
  deadline boundary (just-before/at/after), restart with an expired file starts
  active, corrupt/missing file ⇒ active, clear semantics, `claims_allowed()`.
- `gpn-agent/src/gui.rs` (13): routing (`/`, `/api/status`, `/api/idle`,
  `/api/resume`, `/api/idle/auto`), 200/400/404/405/409 shapes, over-long refusal text, loopback
  enforcement (`0.0.0.0`, LAN IPs, hostnames refused), a real socket round-trip
  including `POST` + stop, and no external asset in the page.
- `gpn-agent/src/idle_auto.rs` (23): foreign-load classification (our own job,
  the desktop baseline, the VRAM threshold, unreadable probe), debounce
  (one spike vs 3 spaced samples, samples inside the spacing window), auto-resume
  after M quiet samples, deadline expiry vs manual resume (cooldown), manual idle
  wins and is never resumed, restart adopting a live auto window, sole-authority
  refusal, clamped env config, default-off, and the `nvidia-smi` table parser.
- `gpn-agent/tests/idle_auto.rs` (5): the real sampler thread with a fake probe —
  sustained load writes an `auto:` `idle.json` and quiet removes it; a single
  spike never writes one; the dashboard toggle gates it; a manual idle is left
  untouched; a manual resume wins and cools down.
- `gpn-agent/tests/idle_mode.rs` (6): public-API state machine, claim gate,
  HTTP endpoints end-to-end, lone-authority 409 and its force override,
  non-loopback bind refusal.
- `gpn-node/node/src/service.rs` (`idle_mode_tests`, 4): deadline boundary,
  expired/corrupt file ⇒ mining, `mining_allowed(role, idle)` respects the role
  gate, and one log line per parking transition.
- `gpn-agent/src/bin/gpn_node.rs` (`idle_cli_tests`, 4): `idle`/`resume` verbs —
  over-long rejected, safety-valve refusal (exit 3), forced write + resume
  clears, and a plain start runs no verb.

---

## 8. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `dashboard: off` and no page | started without `--gui`; add the flag or `gui = true` |
| `dashboard not started: refusing to bind … 0.0.0.0` | the listen address is not loopback — use `127.0.0.1:<port>` |
| *Go idle* answers 409 `sole-authority` | `peers = 0`: start a second node, or `--force-idle` on a dev chain |
| `cannot read the node's peer count` | the node child is down/starting; check the `[node]` log |
| Idle "does nothing" (`mining` still on) | you are running an older binary; the miner gate needs this release |
| Countdown is wrong after a long suspend | the file stores absolute `until`; the page re-syncs every 3 s |
| Node still warm while idle | idle stops the *miner*; ollama may keep a model resident — `ollama stop <model>` if it matters |
