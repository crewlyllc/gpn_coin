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

## Referral code (optional, data collection only)

If somebody referred you to the testnet, pass their code at install time:

```bash
./install.sh --ref team-alpha          # or: GPN_REF=team-alpha ./install.sh
```

Windows: `.\run-windows-provider.ps1 -Ref team-alpha` (env `GPN_REF` also works),
or the binary directly: `.\bin\gpn_node.exe --setup --ref team-alpha`.

If you installed a prebuilt release and never ran `install.sh`, the node binary
takes the same flag itself, on first run:

```bash
./gpn_node --setup --ref team-alpha    # wallet + ollama bootstrap; code recorded
./gpn_node --ref team-alpha            # standalone (no --setup)
./gpn_node --ref=team-alpha            # --ref=CODE works too
```

**Share links.** `https://gpncoin.ai/download?ref=team-alpha` (also `?r=`) renders
the code into the page's copy-paste commands: the Windows block becomes
`.\bin\gpn_node.exe --setup --ref team-alpha` and the Linux block
`./install.sh --ref team-alpha`. The page validates the code against
`^[A-Za-z0-9_-]{4,32}$` and silently ignores anything else (that is the XSS
boundary: the value is written as text, never as markup), and remembers it for
the browser session so it survives clicking around the site.

- Write-once: the binary stores the code only if `$GPN_PREFIX/referral` does not
  exist yet; an existing value wins and the new `--ref` is ignored with a log
  line (`[referral] already set to …; ignoring --ref …`).
- Format: **4–32 characters**, letters/digits/`-`/`_`. An invalid code only
  prints a warning — it never fails an install.
- Stored **once** in `~/.gpn/referral` (one line). A later `--ref` is ignored
  (`[referral] already set to …; ignoring new value`), so the attribution cannot
  be rewritten afterwards. Delete the file only if you *want* to lose it.
- Published on the provider heartbeat as `"ref"` in `/status`, and recorded by
  the access point once per provider address (it is not a secret).
- Purely for the testnet-participation allocation at mainnet launch. Amounts
  are undecided and no reward is promised — see
  `shared/artifacts/project7/airdrop/REFERRAL-AND-MIGRATION-POLICY.md`.
- A node installed without a code behaves exactly as before (the field is
  omitted entirely).

## Fast sync (skip the ~200k-block catch-up)

A fresh node normally verifies ~200k blocks from peers (hours on a PoW testnet).
A **weekly snapshot** is published to the public mirror, and **an installer uses
it automatically** — you do not have to do anything:

```bash
./install.sh                 # fast sync is the default
```

At install time it:

1. fetches `snapshot.json` — **signed** with the GPN release ed25519 key (the same
   key compiled into `gpn_node` for `releases.json`). The release *description* is
   for humans and is never trusted;
2. checks the signature, then the archive's exact **size** and **sha256** against
   that signed document *before* importing a single byte;
3. imports into a scratch directory with `--pruning archive`, and only then moves
   `chains/` into the node base path;
4. prints, at the end, either `chain data: bootstrapped from the weekly snapshot
   (block N)` or `chain data: none yet — the node will full-sync from peers`.

URLs (both are overridable with `GPN_SNAPSHOT_URL` / `GPN_SNAPSHOT_META_URL`):

```
https://github.com/crewlyllc/gpn_coin/releases/download/snapshot-latest/gpn-testnet-snapshot.tar.gz
https://github.com/crewlyllc/gpn_coin/releases/download/snapshot-latest/snapshot.json
```

### Disabling it

```bash
./install.sh --no-fast-sync          # CLI
GPN_FAST_SYNC=0 ./install.sh         # env (same effect)
```

In that case the node simply syncs every block from peers as before.

### What the fallback does

Fast sync is **best-effort by design: it can never fail an install**. Any of
these — metadata 404/timeout, bad or missing signature, size/sha256 mismatch, a
dropped download, less than 3× the snapshot size free on disk, or an import
error — prints one line

```
! fast sync unavailable (<reason>) — falling back to a normal full peer sync (slower, still correct).
```

deletes only its own scratch directory, and continues with a normal sync. A
**pre-existing chain DB is never touched**: if `<base>/chains` already has
content the installer skips fast sync entirely (and says whether it found the
`.gpn-pruning-archive` marker), so a DB created with a different pruning mode
cannot be corrupted into `Incompatible pruning modes` / `Column families not
opened` failures.

### Doing it by hand

```bash
curl -fLO https://github.com/crewlyllc/gpn_coin/releases/download/snapshot-latest/snapshot.json
curl -fLO https://github.com/crewlyllc/gpn_coin/releases/download/snapshot-latest/gpn-testnet-snapshot.tar.gz
gpn_snapshot verify --meta snapshot.json --snapshot gpn-testnet-snapshot.tar.gz
gpn_snapshot import --snapshot gpn-testnet-snapshot.tar.gz --base-path ~/.gpn/node --pruning archive
```

`gpn_snapshot verify` exits non-zero and imports nothing if the signature, size
or hash does not match. Windows is identical (`gpn_snapshot.exe`; the installer's
launcher performs the same checks with `curl.exe`).

The snapshot is rebuilt every Monday at 04:00 PT (`scripts/snapshot-weekly.sh`);
its block height, size and sha256 are in the signed `snapshot.json` and echoed in
the release description for convenience.

## Install on a new computer

Three steps, one question.

```
1. Download + extract
2. ./gpn_node --setup        (answer: generate or import a wallet)
3. ./gpn_node
```

That is the whole install. Nothing else has to be set: the chain spec, data
folder, payout address, peer list and public address are all resolved
automatically. `--setup` also installs/starts ollama and pulls the baseline
model, because a GPN node is always **miner + AI provider**.

### Check the version / all options

```
$ ./gpn_node --version
gpn_node 0.1.0 (spec 41)

$ ./gpn_node --help      # usage, --setup/--version/--help, key GPN_* env vars
```

Both flags answer immediately and exit `0`: no config is read, no data dir is
created, nothing is downloaded and no wallet prompt appears. Use them to confirm
which build a machine is running before debugging anything else.

### 1. Download + extract

Grab the release archive for your OS (Linux, macOS, Windows) and unpack it
anywhere. It contains `gpn_node`, `gpn-node`, the `gpn_*` agent binaries, the
`scripts/` folder and `gpn-node.example.toml`.

Building from source instead: see [Appendix A](#appendix-a--manual--detailed-installation).

### 2. `./gpn_node --setup`

```
$ ./gpn_node --setup
[gpn_node] base path: /home/you/.gpn/node (node database + wallet — where GPN keeps its data; default)
[gpn_node] chain spec: source https://gpncoin.ai/chain-spec · genesis 0xb8e4636a… (verified) · bootnodes /dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooWAFtUNxc…,/ip4/142.129.54.88/tcp/30333/p2p/12D3KooWAFtUNxc…,/ip4/23.241.109.255/tcp/30333/p2p/12D3KooWFqZ2msYzP3263AKGSUfLzUC7Am7FyRDqi2KKfgREmxGz
[gpn_node] chain spec: saved -> /home/you/.gpn/node/testnet-spec.json (auto)
[gpn_node] no wallet configured — starting the one-time wallet setup (stores /home/you/.gpn/node/keystore.json; owner-only)

Set up your wallet
Your wallet is your node identity and receives all mining/job earnings.
  1) Generate a new wallet (recommended)
  2) Import an existing wallet (mnemonic or 0x seed)
Choose [1]: 1

========================================================================
  WRITE THIS DOWN — IT CONTROLS YOUR EARNINGS
  It cannot be recovered. Anyone who has it can take your funds.
========================================================================

  secret phrase (12 words):

    word1 word2 word3 word4 word5 word6 word7 word8 word9 word10 word11 word12

========================================================================

  address (SS58): 5EWTxdpdiaSydouhVkGmnxvNe6DrX8VHBvcuWMCb9vznBkje
  keystore: /home/you/.gpn/node/keystore.json  (owner-only: 0600 / user ACL)
[gpn_node] config written: /home/you/.gpn/node.toml (second run is zero-config)
…
  Start the node:  ./gpn_node
```

The wallet question is the **only** prompt. Choosing *import* asks for the
mnemonic or `0x` seed and validates it (3 attempts). The secret is written to
`<base>/keystore.json` with owner-only permissions (`0600` on Unix; inheritance
removed and only the current user granted on Windows) and is **never** stored in
the config file or the repo.

Run `--setup` more than once and it is idempotent: an existing wallet is kept
(press Enter), an existing spec is reused, and ollama is re-verified.

After the ollama check, `--setup` asks **one** question about updates:

```
  Keep this node auto-updated? [y/N]
    yes: verify + install GPN releases automatically (signed manifest,
         ed25519), restarting briefly when one is published. Recommended:
         the network drops nodes that fall below the minimum version, and an
         old node cannot mine.
    no : you update manually. This node will REFUSE TO MINE once its
         version is below the published minimum (it keeps syncing).
  Currently: off
  >
```

`y` writes `auto_update = true` into `~/.gpn/node.toml`; pressing Enter (or `n`)
leaves it off. It is genuinely optional — but see
[Updates](#updates-auto-update-and-the-minimum-version-cutoff) for what "off"
means once a release raises the floor.

### 3. `./gpn_node`

One command starts the miner, the heartbeat and every AI provider, supervised
as one node with one log stream and one status line. The second run needs no
flags and no environment variables — it reads `~/.gpn/node.toml`.

### Your node has two identities — never copy either from another node

Every GPN node owns **two** machine-generated identities, and neither is a
password you set: they are keys created **on this machine**, on first run.

| Identity | What it is | Where it lives | What happens if two nodes share it |
|---|---|---|---|
| **Network identity** (peer id) | the libp2p key that names this box on the p2p network | `~/.gpn/node-key` (`%USERPROFILE%\.gpn\node-key` on Windows) | both boxes announce the same peer id, the directory treats them as one node and they cannot peer with each other |
| **Money identity** (account) | the sr25519 account this box signs with and is **paid** to | `<base>/keystore.json`, mode `0600` / user-only ACL | two boxes with the same account **split and steal each other's rewards** — and whoever else knows the key can take the funds |

Both are generated per machine and are **never** shipped in the bundle: no
dev URI (`//Alice`, `//Bob`), no seed, no fixed payout address. The Windows
launcher runs `gpn_node --ensure-wallet` on every start, which

* **creates** the wallet on first run and prints
  `this node mines and is paid to 5XXXX… — back up the keystore at …` (once, in
the console and again at startup as `payout account: 5XXXX…`);
* **reuses** an existing keystore unchanged — it is never rewritten, because
  that would change the payout address and orphan the earnings already mined
to it;
* **refuses to start** if the keystore is corrupt, rather than silently
  generating a new one (the node would quietly mine to a new account);
* honours `GPN_SIGNER` / `GPN_ACCOUNT` when an operator (or a fleet install)
sets them explicitly, and logs that they **override** the local keystore.

> **Fleet installs are the one deliberate exception.** A fleet script passes
> `--signer-uri <seed>` (or `GPN_SIGNER=<seed>`) on purpose: one operator, many
> boxes, one treasury account. That is an *override*, and every log line says so.
> A **public bundle install** (the Windows launcher, `install.sh`) must never
> ship a signer — a stranger's box must never mine for somebody else's account.

Back up `keystore.json` (and its 12-word phrase) offline. It is the only copy of
the key; losing it loses the earnings, and copying it to a second node makes the
two fight over the same rewards.

The dashboard (`gpn_node --gui`, <http://127.0.0.1:8099>) shows the payout
account as **Payout account** — check it is yours after an install.

### What happens automatically

| Item | Automatic behaviour | Override |
|---|---|---|
| **Data folder** | `~/.gpn/node` (Unix) / `C:\gpn\data` (Windows), created on first run. Holds the chain DB **and** the wallet. | `GPN_BASE_PATH` |
| **Chain spec** | If no `GPN_CHAIN` and no `<base>/testnet-spec.json`, it is downloaded once over **HTTPS** from `https://gpncoin.ai/chain-spec` (legacy `http://gpn-testnet.duckdns.org:8092/genesis-spec` as fallback), its **genesis is verified against the pinned testnet genesis hash** — a mismatch is refused, never silently used — and its `bootNodes` are cleaned of loopback/private entries. Then pinned in the base dir. One log line, no prompt. | `GPN_CHAIN`, `GPN_SPEC_URL` |
| **Wallet** | Generated (headless: `gpn_node --ensure-wallet`) or imported by the wizard; SS58 address printed once and on every start as `payout account:`. Later runs read the keystore silently and never rewrite it. | `GPN_SIGNER`, `GPN_SIGNER_FILE` |
| **Payout account** | Derived from the wallet, so earnings can never go to a placeholder. A corrupt keystore aborts the start instead of inventing a new account. | `GPN_ACCOUNT` |
| **Peers** | `https://gpncoin.ai/seeds.json` (live directory) + compiled-in bootnodes; failing URLs are logged and ignored. | `GPN_BOOTNODES`, `GPN_BOOTNODES_URL` |
| **Public address** | `GPN_PUBLIC_ADDR` → public-IP detection → **UPnP-IGD / NAT-PMP auto-mapping** (on by default). A successful mapping needs no configuration at all. | `GPN_PUBLIC_ADDR`, `GPN_UPNP=off`, `--no-upnp` |
| **Ollama** | Installed/started and the baseline model pulled on `--setup` (and on a normal start if missing). | `GPN_OLLAMA`, `GPN_REQUIRE_OLLAMA=0` |
| **Updates** | Auto-update is **ON by default** (answer `n` at `--setup` or set `GPN_AUTO_UPDATE=0` to opt out). The minimum-version check runs either way: an outdated node syncs but does not mine. | `GPN_AUTO_UPDATE`, see [Updates](#updates-auto-update-and-the-minimum-version-cutoff) |
| **Config** | `~/.gpn/node.toml` written once (base path, chain, keystore path, autostart). | `--config`, `GPN_CONFIG`, `GPN_PREFIX` |

### Fallbacks (the rare cases)

* **First run with no internet** — the spec fetch fails after ~45 s and the
  node starts on its built-in `testnet` spec with a warning. Set `GPN_CHAIN`
  to a local spec file if the genesis hash must match a specific network.
* **Router with UPnP disabled** — the node prints the NOT-DIALABLE notice with
  the exact port-forward rule (TCP 30333 → this machine) and exits `78`. See
  [Make your node dialable](#make-your-node-dialable). If the router rewrites
  the external port, set `GPN_PUBLIC_ADDR=/ip4/<ip>/tcp/<port>`.
* **CGNAT** (`100.64.0.0/10` WAN address) — no forward can work; run the node
  on a host with a public IP, or opt out of the gate with
  `--no-require-backbone` (degraded mode).
* **systemd / container / CI (no TTY)** — the wizard never hangs: with no wallet
  configured it exits `2` and tells you to run `gpn_node --setup` once
  interactively (or `gpn_node --ensure-wallet`, which needs no terminal at all).
  After that the service starts with no prompts.
* **Different data disk** — set `GPN_BASE_PATH` (or `base_path` in the config)
  before the first run.

## Appendix A — manual / detailed installation

### Build from a clone

```bash
# builds gpn-node + the gpn-agent bins, generates a wallet
curl -fsSL <REPO>/install.sh | sh      # or ./install.sh from a clone

# ollama is REQUIRED (a node = miner + AI provider)
./scripts/install-ollama.sh            # Linux/macOS; Windows: scripts\install-ollama.ps1
./gpn_node --setup                     # verify/bootstrap (exit 0 = ready)

gpn_node                               # one command: miner + AI provider
```

### Everything the wizard would otherwise do, by hand

```bash
export GPN_BASE_PATH="$HOME/.gpn/node"                    # data + wallet
mkdir -p "$GPN_BASE_PATH"
curl -fsS https://gpncoin.ai/chain-spec \
  -o "$GPN_BASE_PATH/testnet-spec.json"                   # pin the spec (HTTPS)
export GPN_CHAIN="$GPN_BASE_PATH/testnet-spec.json"
export GPN_SIGNER="<your mnemonic or 0x seed>"           # or GPN_SIGNER_FILE
# optional: export GPN_PUBLIC_ADDR=/ip4/203.0.113.10/tcp/30333
```

Secrets can live in `~/.gpn/node.toml` (`signer = "…"`) or, better, in a
systemd `EnvironmentFile` / Windows environment variable. Compromise: put the
seed in a keystore file (`0600`) and set `signer_file = "<path>"` — the wizard
does exactly this.

See the rest of this document for the full environment-variable reference,
systemd/Windows service units, snapshots, and troubleshooting.

## Backbone by default — dialable is REQUIRED

Every GPN node is a **backbone** node: it announces its endpoint, accepts
inbound peers, relays and serves blocks for the mesh, and earns the
`gpn-liveness` uptime credit that gates the Silver/Gold staking multiplier. The
best nodes become **seeds** (bootstrap targets) automatically — see
[How to become a seed](#how-to-become-a-seed--bootstrap-node).

The network is still growing, so **inbound reachability is required by
default**. A node that cannot be dialled prints the notice below and **exits
with code 78** (`EX_CONFIG`, reported as `node=down(not dialable)`). Nothing is
silently half-joined.

> **Opting out (not recommended while the network is growing).**
> `--no-require-backbone` (or `GPN_REQUIRE_BACKBONE=0`) keeps the node running
> when it cannot be dialled: it prints the same notice and continues in
> **degraded, non-discoverable mode** — syncing, mining and serving AI jobs with
> the same block rewards and job payouts. The only things it does not get are
> liveness credit, seed-list eligibility, and a small routing tie-break. In
> this mode it re-checks every **10 minutes** and prints `NOW DIALABLE` once
> when the forward starts working.

At startup the node probes inbound reachability for up to **120 s**
(`GPN_DIALABLE_TIMEOUT_SECS`), in this order:

1. explicit `GPN_PUBLIC_ADDR` / `--public-addr` → match it against what peers
   report about us, a direct bind, an **external STUN binding**, or a bounded
   outbound TCP reachability probe (see the evidence model below);
2. direct public listen address, libp2p `Identify` external addresses, or an
   actual inbound peer on `:30333`;
3. UPnP-IGD / NAT-PMP mapping attempt (~10 s) if `GPN_UPNP=auto` (the
   default). If the router accepts the rule, its reported **external
   `ip:port`** becomes this node's advertised public address. CGNAT is detected
   here first: a WAN address in `100.64.0.0/10` (or an RFC1918 WAN address)
   means no forward can work, and the attempt is skipped;
4. CGNAT/private WAN → no forward is possible (reported by the notice);
5. not dialable: print the notice and **exit 78** (default), or continue in
   degraded mode with `--no-require-backbone`.

### Evidence model

The gate passes on **any one** of these (strongest first):

| Evidence | Meaning |
|---|---|
| `inbound_peer` | a peer opened a connection to us |
| `configured_addr_matches_observation` | our configured `GPN_PUBLIC_ADDR` endpoint was reported by peers (Identify) as our external address |
| `observed_public_addr` | no configured address, but peers see us at a public `ip:port` on our listen port |
| `stun_mapped_addr` | an **external** STUN binding says the NAT maps our socket to exactly the configured endpoint |
| `tcp_probe_ok` | a bounded (5 s) outbound TCP connect to the configured public `ip:port` succeeded — hairpin, so the weakest signal |

`/dns4` and `/dns6` are first-class public addresses:
`GPN_PUBLIC_ADDR=/dns4/gpn-testnet.duckdns.org/tcp/30333` is accepted, the name
is **resolved at check time**, every resolved IP must be globally routable, and
the *resolved IPs* (not the hostname) are compared with what peers report.

> **The gate never halts the chain.** Before refusing to start, the node checks
> whether it is the last authoring node the network can see (0–1 connected
> peers) or a dev/solo chain. If so it prints `DIALABILITY GATE OVERRIDDEN -
> KEEPING THE CHAIN ALIVE`, continues in degraded mode and keeps re-checking.
> Set `GPN_DIALABILITY_SAFETY=0` to force the hard failure anyway.
> `GPN_STUN_SERVER=<host:port>` overrides the STUN server (default
> `stun.l.google.com:19302`); `GPN_STUN_SERVER=off` disables that probe.

A successful mapping is **not** proof of reachability on its own: the gate still
waits for an inbound peer (or a matching `Identify` observation) before the node
declares itself dialable. The mapping is renewed at half its lease (default
3600 s) and re-reads the WAN address each time.

Only globally routable addresses count — RFC1918 (`10/8`, `172.16/12`,
`192.168/16`), CGNAT (`100.64/10`), loopback, link-local and `0.0.0.0` never do.

> **Opting out.** `--no-require-backbone` (or `GPN_REQUIRE_BACKBONE=0`)
> disables the hard gate: the node logs the same notice and continues as a
> non-discoverable participant instead of exiting. Use it for a machine that
> physically cannot be forwarded (CGNAT) and must still run.

| Flag / env | Meaning | Default |
|---|---|---|
| `--seed` / `GPN_SEED=1` | Willing to be published as a seed candidate. | **on** |
| `--no-seed` / `GPN_SEED=0` | Never publish as a seed; also silences the not-dialable notice. | — |
| `--discoverable` / `GPN_DISCOVERABLE=1` | Announce endpoint, accept inbound. | **on** |
| `--no-discoverable` / `GPN_DISCOVERABLE=0` | Privacy mode: no announce, no inbound, not in any list. Implies `--no-seed`. | — |
| `--no-upnp` / `GPN_UPNP=off` | Never contact the router; no UPnP/NAT-PMP attempt. The attempt is logged when it happens. | `auto` |
| `--public-addr` / `GPN_PUBLIC_ADDR` | Explicit public multiaddr; always wins over detection. | unset |
| `--require-backbone` / `GPN_REQUIRE_BACKBONE=1` | **Default.** Exit 78 instead of degrading. | **on** |
| `--no-require-backbone` / `GPN_REQUIRE_BACKBONE=0` | Opt out: run degraded when not dialable (re-checks every 10 min). | — |
| `GPN_DIALABLE_TIMEOUT_SECS` | Probe grace window. | `120` |
| `GPN_STUN_SERVER` | External STUN server for the mapping probe; `off` disables it. | `stun.l.google.com:19302` |
| `GPN_DIALABILITY_SAFETY` | `0` lets the hard gate exit even when that would stop the last authority. | on |
| `GPN_SEED_PORT` | Inbound port to open/probe. | `30333` |
| `GPN_DIALABILITY_STATE` | Where the gate writes its verdict for the provider heartbeat (`public_endpoint`, evidence). | `~/.gpn/dialability.json` |
| `GPN_NODE_KEY_FILE` | Per-machine libp2p key file; preferred over a literal `--node-key` (a copied key = a duplicated `peer_id`). | unset |

> The provider side of this — what the heartbeat publishes (`multiaddrs`,
> `listen_addrs`), the VRAM model gate and the public `/v1/models` catalogue —
> is documented in [provider-address-and-models.md](provider-address-and-models.md).

If the node is not dialable you will see this (exact text, LAN IP and port
filled in) and, by default, the process exits:

```
================================================================================
  YOUR NODE IS NOT DIALABLE
================================================================================
Other nodes cannot open a connection to this machine on TCP 30333.

Your node is running and fully functional: it syncs, mines and serves AI jobs.
What you lose: it is not listed as a backbone/seed node, so it earns no
liveness credit, gets no seed-list eligibility, and cannot be a bootstrap target.
Nothing is taken away from block rewards or job payouts.

To fix it (about 5 minutes):

  1. Find this machine's LAN IP. Detected: 192.168.1.42
     (Windows: ipconfig | Linux/macOS: ip addr or ifconfig)

  2. On your router, add a port-forwarding rule:
        Protocol ............ TCP
        External port ....... 30333
        Internal port ....... 30333
        Internal IP/host .... 192.168.1.42
     Router admin is usually http://192.168.1.1 or http://192.168.0.1.

     Recommended setup (EXAMPLE - these are example values, not detected):
       - Give this machine a DHCP reservation so its LAN IP never changes
         (example: 192.168.1.50).
       - Forward exactly one TCP port, external = internal (30333).
       - Example router rule:  TCP 30333 -> 192.168.1.50:30333
     Detected on this machine: LAN 192.168.1.42, port 30333.

  3. Allow the port in this machine's firewall.
     Windows (Administrator PowerShell):
       New-NetFirewallRule -DisplayName "GPN node" -Direction Inbound `
         -Protocol TCP -LocalPort 30333 -Action Allow
     Linux (ufw):
       sudo ufw allow 30333/tcp
     Linux (firewalld):
       sudo firewall-cmd --add-port=30333/tcp --permanent && sudo firewall-cmd --reload

  4. UPnP/NAT-PMP: the node already tried this automatically (GPN_UPNP=auto is
     the default). If your router has UPnP disabled, either enable it and
     restart, or use step 2. Disable the attempt with --no-upnp / GPN_UPNP=off.

  5. CGNAT: if your router's WAN address is in 100.64.0.0/10, no port forward
     can work. Run the node on a cheap VPS with a public IP.

Full guide with per-OS detail:
  https://gpncoin.ai/docs/run-a-node#make-your-node-dialable

This node will exit until it is dialable.
To run anyway (not recommended while the network is growing):
  --no-require-backbone
================================================================================
```

In the opt-out mode (`--no-require-backbone`) the last block reads instead:

```
This node re-checks every 10 minutes and will print NOW DIALABLE here when
the fix works. You can also restart to force a check.

Soft mode is active (--no-require-backbone / GPN_REQUIRE_BACKBONE=0).
Run private, silence:      --no-seed   (or GPN_SEED=0)
================================================================================
```

When a fix succeeds, the node confirms it once (opt-out mode only — in the
default mode you just restart into a clean start):

```
================================================================================
  NOW DIALABLE - your node is reachable on TCP 30333
================================================================================
Inbound reachability was confirmed. This node now announces itself as a backbone
node and is eligible for seed listing. Nothing else to do.

(This message is shown once.)
================================================================================
```

## Make your node dialable

The walkthrough below is what the notice above tells you to do. You need it on
any host that is not already directly reachable on a public IP.

### Recommended setup (example)

A setup that works on most home routers, in three steps:

1. **Pin the LAN IP with a DHCP reservation** on the router, so the address the
   forward points at never changes. Example: `192.168.1.50` (the node prints
   the address it actually detected, which may be e.g. `192.168.1.42`).
2. **Forward one port, external = internal: `30333` → `30333`.** Keeping the
   numbers equal means no `GPN_PUBLIC_ADDR` is needed when the probe sees the
   mapping.
3. **Allow `30333/tcp` in the host firewall** (commands in step 2 below).

Concrete example rule, as it appears on a typical router page:

```
Name ............ GPN node
Protocol ........ TCP
External port ... 30333
Internal port ... 30333
Internal IP ..... 192.168.1.50
```

That is the whole recipe. The rest of this section explains each step and the
failure modes (multiple nodes, CGNAT, port rewritten by the router).

### 1. Forward TCP 30333 on your router

1. Find this machine's **LAN IP**: `ipconfig` (Windows) or `ip addr` /
   `ifconfig` (Linux/macOS). Prefer a **DHCP reservation** or static lease so it
   does not change.
2. Open the router admin page — usually `http://192.168.1.1` or
   `http://192.168.0.1`; the address is printed on the router label.
3. Find **Port Forwarding** (sometimes *Virtual Server*, *NAT*, or *Games &
   Applications*).
4. Add a rule:
   - **Protocol:** TCP (not UDP)
   - **External / WAN port:** `30333`
   - **Internal / LAN port:** `30333`
   - **Internal host / IP:** this machine's LAN IP (e.g. `192.168.1.42`)
5. Save and reboot the router if it asks.

Running more than one node on the same network? Give each a distinct
`GPN_SEED_PORT` and forward each port to its own machine.

### 2. Open the host firewall

Forwarding at the router is not enough if the OS firewall drops the packet.

**Windows** (Administrator PowerShell — note the backtick line continuation):

```powershell
New-NetFirewallRule -DisplayName "GPN node" -Direction Inbound `
  -Protocol TCP -LocalPort 30333 -Action Allow
```

Verify: `Get-NetFirewallRule -DisplayName "GPN node"`.

**Linux — ufw** (Debian/Ubuntu):

```bash
sudo ufw allow 30333/tcp
sudo ufw status verbose
```

**Linux — firewalld** (Fedora/RHEL/CentOS):

```bash
sudo firewall-cmd --add-port=30333/tcp --permanent
sudo firewall-cmd --reload
sudo firewall-cmd --list-ports
```

**macOS:** System Settings → Network → Firewall → Options → allow the
`gpn_node` binary (or `sudo /usr/libexec/ApplicationFirewall/socketfilterfw
--add /path/to/gpn_node`).

### 3. UPnP / NAT-PMP — automatic (on by default)

Most home users do **not** need step 1. Before the node decides it is not
dialable, it asks the router to open the port for it:

1. **UPnP-IGD** (SSDP discovery + `AddPortMapping` TCP `30333 -> this host`,
   lease 3600 s), then
2. **NAT-PMP** (RFC 6886, UDP/5351) as a fallback for Apple and other routers
   that implement NAT-PMP but not UPnP.

Only **one TCP port** is opened — never UDP, never a range, never a DMZ. The
rule is renewed at half-life and expires on its own if the node dies, so a
crashed node never leaves a permanent hole in the router.

It is never silent. A successful mapping logs (the log line is the consent
record):

```
portmap: UPnP: mapped TCP 30333 -> 192.168.1.42:30333 via 192.168.1.1 (UPnP-IGD); external address is 23.241.109.255:30333. Disable with GPN_UPNP=off or --no-upnp
```

and the router-reported external `ip:port` becomes the node's advertised
address — including when the router assigns a **different external port**
(`45123 -> 30333` is advertised as `…:45123`, no `GPN_PUBLIC_ADDR` needed).

A failed attempt is also logged, and the node falls through to the manual
instructions in the notice:

```
portmap: automatic port mapping FAILED (no gateway: ...); TCP 30333 must be forwarded manually. If your router has UPnP disabled, enabling it - or adding the forward - is the fix.
```

**Turn it off:** `--no-upnp` (or `GPN_UPNP=off`). The node then never sends a
packet to the router and behaves exactly like a manually-forwarded host. Use
this if you consider UPnP a security surface you do not want exposed; a manual
forward (step 1) is equally valid.

**It will not work when:**

- the router has UPnP/IGD (and NAT-PMP) disabled — common on ISP-supplied
  routers, and the default on many business routers;
- the WAN address is CGNAT (`100.64.0.0/10`) or private (double NAT) — detected
  before the attempt, reported as CGNAT;
- the router is on a different L2 segment (guest/VLAN isolation, Docker
  bridge networking, some VPNs) so SSDP never reaches it;
- Windows: NAT-PMP needs `GPN_NATPMP_GATEWAY=<router IP>` (gateway detection is
  implemented for Linux/macOS only; UPnP needs no such hint).

When a mapping succeeds but the node is still not dialable after 120 s, the
mapping was accepted but the inbound path is blocked (host firewall, ISP
filtering, or the router applied it to the wrong interface). Step 2 (host
firewall) is then the usual fix.

**Renewal / IP change.** The rule is renewed at half the lease. If the ISP
changes the WAN IP, or the router assigns a different external port, the new
address is logged and used for subsequent checks; the running libp2p instance
cannot change its announced address set, so **restart the node to advertise the
new address**. Peers that learn the address through `Identify` will still find
the new path in the meantime.

**Re-detection (no restart needed).** With **no** live mapping the node re-runs
the whole UPnP-IGD → NAT-PMP ladder every **10 minutes** (`GPN_UPNP_RETRY_SECS`,
floor 30 s, cap 3600 s). Three cases this covers, all previously "restart the
node":

- you enable UPnP on the router after the node started;
- the router reboots or is replaced and drops the rule (the stale handle is
discarded and discovery starts over);
- the WAN address moves off CGNAT (e.g. the ISP hands you a public IP).

If a **live** rule stops renewing, the node immediately re-runs discovery
rather than waiting for the next 10-minute tick. Logging is edge-triggered: a
node that stays unmappable says so **once** (plus a single line when it
recovers), never every 10 minutes.

**Advanced knobs** (rarely needed):

| Variable | Meaning | Default |
|---|---|---|
| `GPN_UPNP` | `auto` or `off`. | `auto` |
| `GPN_UPNP_LEASE_SECS` | Mapping lease requested from the router (minimum 60). | `3600` |
| `GPN_UPNP_RETRY_SECS` | Re-detection cadence while no mapping is live (30–3600). | `600` |
| `GPN_NATPMP_GATEWAY` | Router IP for NAT-PMP when OS gateway detection is unavailable (e.g. Windows). | auto |
| `GPN_SEED_PORT` | The TCP port to open; also the internal port. | `30333` |
| `GPN_SEED_REFRESH_SECS` | How often the published seed list is re-read while the node has fewer than 8 peers (30–14400). The list is cached for 7 days, so a publisher outage does not strand a restart. | `900` |

### 4. CGNAT — when forwarding is impossible

Check the **WAN address** on the router status page (not the LAN address):

- `100.64.0.0/10` → **carrier-grade NAT**. Your ISP does not give you a public
  IPv4; no port forward can work. Options: ask the ISP for a public/static IP,
  use IPv6 if your ISP routes a public prefix, or run the node on a cheap VPS.
- A public WAN address that differs from what a "what is my IP" site reports →
  also CGNAT (double NAT).

CGNAT is **not** an error. By default the node exits with the notice (it cannot
be a backbone member); with `--no-require-backbone` it keeps syncing, mining and
serving jobs as a non-discoverable participant.

### 5. Tell the node its public address (only if detection fails)

Auto-detection covers most hosts. Set it explicitly when the router rewrites
ports or detection is ambiguous:

```toml
# ~/.gpn/node.toml
public_addr = "/ip4/203.0.113.10/tcp/30333"
```

or:

```bash
GPN_PUBLIC_ADDR=/ip4/203.0.113.10/tcp/30333   # env wins over the config file
```

Use `/ip6/<addr>/tcp/<port>` for IPv6. The port must be the **external** port you
forwarded, not necessarily the listen port. On a VPS with a directly bound
public IP this is usually unnecessary, but setting it is harmless and makes the
announced endpoint exact.

### 6. Verify from outside

The node re-checks every 10 minutes **when it is running in the opt-out mode**
(`--no-require-backbone`); by default it exits instead, so restart it after the
forward is in place. You can also confirm the forward yourself:

- **From another network** (phone hotspot, a friend's house, a VPS) — not from
  the same LAN, where NAT hairpinning can give a false positive:

  ```powershell
  # Windows
  Test-NetConnection <your-public-ip> -Port 30333
  # => TcpTestSucceeded : True
  ```

  ```bash
  # Linux/macOS
  nc -vz <your-public-ip> 30333
  ```

- **Online port checker** (e.g. `portchecker.io`, `canyouseeme.org`): check
  TCP `30333` against your public IP.
- The authoritative check is the node itself: `gpn_node --status` shows
  `tier: backbone` once reachable, and (in opt-out mode) the console prints
  `NOW DIALABLE`.

If it still fails: confirm the rule is **TCP** (not UDP), the internal IP is
current (DHCP may have moved it), the host firewall allows it, and the ISP is
not blocking inbound ports (some block 30333; use another `GPN_SEED_PORT`).

## Pull vs push — work behind NAT

A provider used to learn about its jobs only when the access point **pushed**
them to the provider's heartbeat port (`POST /assign` on `:8093`). That needs
an **inbound** port, which a CGNAT or locked-down router cannot give you — the
node would sync, mine, and be reachable *outbound*, yet never receive a job.

The **pull** transport inverts it: the provider asks the access point for its
own assignments (`GET /assignments`, a bounded long-poll) and writes them into
the same local stores the claim loop already reads. Nothing downstream changes:
claiming, inference, result return and settlement are identical, and the
provider behind NAT now earns.

**The poll also carries your capabilities** (`caps`, read from your local
`/status` over loopback), so the access point learns your models, GFLOPS, VRAM,
GPU class and live `active` count **without ever dialling your `:8093`**. That
is what lets a NAT'd provider actually be *routed to* — before this, an
unreachable `/status` meant the box was pollable but never picked.

```toml
# ~/.gpn/node.toml — the only setting you need
ap_url = "http://gpn-testnet.duckdns.org:8092"
assign_source = "pull"          # push | pull | both  (default: pull)
```

| Setting | Meaning | Default |
|---|---|---|
| `GPN_AP_URL` | Access point base URL used for the poll. **Your node serves AI by default**: unset → the canonical AP below. Set it to point at a different AP; set it **empty** (`GPN_AP_URL=`) to disable AI serving (the node warns loudly). | `http://gpn-testnet.duckdns.org:8092` |
| `GPN_ASSIGN_SOURCE` | `pull` (the only delivery path since #46 Phase 3), `both` (same, kept for config compatibility), `push` (**degraded**: no delivery) | `pull` |
| `GPN_ASSIGN_WAIT_MS` | Long-poll budget. The AP answers the instant it has work, so this only sets how long an **idle** poll waits before returning empty. | `25000` |
| `GPN_ASSIGN_TOKEN` | Shared AP↔provider secret for `GET /assignments`; a remote provider must present the AP's token. | _(empty → permissive, warns)_ |
| `GPN_STATUS_URL` | Local heartbeat read to build the `caps` snapshot each poll (loopback — it does not need to be reachable from outside). | `http://127.0.0.1:8093/status` |

Notes:

- **Your node serves AI by default.** It polls the canonical access point
  (`http://gpn-testnet.duckdns.org:8092`) unless `GPN_AP_URL` says otherwise, so
  a plain install receives work with no extra configuration. At startup the
  log says `[assign] no GPN_AP_URL set — defaulting to <url>` when the default
  is used, so the choice is never silent.
- **`GPN_AP_URL` points it at an access point.** Set it (env or `ap_url` in
  `~/.gpn/node.toml`) to use a private/test AP; an explicit value always wins
  over the default.
- **Setting it empty disables serving.** `GPN_AP_URL=` means "this box must not
  take AI work". That is a deliberate choice, so the node does **not** re-apply
  the default; it prints
  `[provider] WARNING: no access point URL — this node will NOT serve AI jobs
  (mining only)` once at startup. The dashboard then shows **AI serving: no —
  no ap url**.
- **`POST /assign` no longer exists on the heartbeat** (#46 Phase 3 step 2).
  An old AP's push gets a `404` and the job is picked up by the poll — the
  lease row is written *before* any delivery attempt, so nothing is lost. The
  endpoint was removed rather than kept as a no-op because a listener that
  answers a public port with a shared secret is exactly the inbound surface the
  pull transport exists to eliminate.
- **Mixed fleets are safe.** An old AP's push is 404'd while the job is picked
  up by the poll; a provider that cannot poll still self-claims `Pending` jobs
  from the chain (first-claim-wins), so nothing breaks.
- **The cursor survives restarts** (`~/.gpn/assign-cursor.json`). On the first
start of a pull-enabled build the cursor is seeded from the highest id already
in `~/.gpn/assignments.json`, so upgrading does not re-deliver history.
- **One request per idle poll.** A healthy idle provider makes one request per
`GPN_ASSIGN_WAIT_MS` (25 s); a busy one is answered immediately.
- CGNAT users should still read [§4](#4-cgnat--when-forwarding-is-impossible):
pull keeps you *earning*, but only a dialable node can be a backbone seed.
- Check it worked: the provider log prints
  `[assign] pull transport starting — ap=… since=…` at boot and
  `[assign] pulled #<id> (payload stored)` per assignment. `[assign] AP rejected
the pull (401)` means `GPN_ASSIGN_TOKEN` does not match the AP's.

## Off-chain escrow (payment channels) — AI jobs without a block wait

By default an AI job is escrowed **per job** on chain
(`create_job` → `claim` → `submit_result` → `complete_job`), so it waits on the
block clock (typically ~15–20 s, occasionally 60–140 s on a slow seal).

**Channel mode** locks a deposit **once** per provider and then moves the
per-job payment off-chain as two signatures. Measured steady state on the dev
fleet: **~0.2–0.4 s** end-to-end for a short LLM answer, with **zero per-job
transactions**. Design and guarantees:
`docs/design/design-offchain-escrow-channels.md` (§11–18).

It is on by default and **falls back per request** to the on-chain path if
anything is unavailable (provider doesn't serve `/channel/job`, the channel
can't be opened, a signature fails), so enabling it can never lose a job.

**Default policy since 2026-10-22 — value-based routing.** Expensive jobs
settle **per-job on chain** (a real receipt each), cheap high-frequency jobs
batch over a channel:

```
explicit "mode" in the request      → honour it
job value >= GPN_SETTLE_MIN_GPN     → per-job on-chain settle
provider speaks channels            → channel (batched)
otherwise                           → per-job on-chain settle
```

The threshold is derived, not guessed: the measured `create_job` +
`complete_job` fee on this chain is **216 314 315 base units** (0.000216 GPN,
read from `TransactionPayment::TransactionFeePaid`), so the default is
**2 163 143 150 base units (0.00216 GPN) = 10× that cost**. On this testnet a
`max_tokens: 8` chat quotes ~3.2e7 (→ channel) and a `max_tokens: 2000` chat
~5.07e9 (→ on chain).

A channel is between **this AP and one provider** — not between a user and the
AP. The operator decides to batch; users never hold channels and there are no
per-user tiers.

### Access point

```bash
# defaults: settle threshold 0.00216 GPN, 50 GPN per (AP, provider) channel,
# 24 h settle interval, 9000-block TTL
gpn_scheduler --listen 0.0.0.0:8092 --signer-uri <ap-seed> --allow-hot-wallet
```

#### Auth token — generated on the first boot

Non-loopback callers must send `Authorization: Bearer <token>` (or
`X-Api-Key: <token>`) on POSTs; loopback is exempt, so local crons keep working.
You do not have to invent that token — the access point bootstraps it:

* **On the first boot**, if `GPN_AP_TOKEN` is unset in the environment *and*
  absent from `~/.gpn/accesspoint.env`, the AP generates 24 random bytes
  (48 lowercase hex chars, same format as a hand-rolled token), writes it to
  `~/.gpn/accesspoint.env` at mode `0600` (creating `~/.gpn` if needed, written
  atomically via temp-file + rename, other variables in the file preserved), and
  prints it **once**:

  ```
  generated a new AP token — set this as the AP_TOKEN secret on the Cloudflare
  Worker (or run scripts/set-ap-token.sh); it will not be printed again:

      GPN_AP_TOKEN=3f9c…
  ```

  Copy it somewhere safe at that moment (the Cloudflare Worker holds the same
  value as its `AP_TOKEN` secret, so `/api/*` via gpncoin.ai keeps working).
* **Every later boot** prints only the prefix — `AP token loaded from
  ~/.gpn/accesspoint.env (3f9c1a2b…)`. The full value never appears in the logs
  again.
* **Precedence:** environment > env file > generated. An explicitly supplied
  token (exported, or set in the file) is **never** overwritten.
* **Retrieve it later:**

  ```bash
  grep GPN_AP_TOKEN ~/.gpn/accesspoint.env
  ./scripts/set-ap-token.sh        # push it to the Cloudflare Worker
  ```

A missing or unreadable env file is not fatal: the AP keeps running. If the
file cannot be written, the printed token is marked **EPHEMERAL** (it will
change on the next start) — fix the permissions on `~/.gpn` and restart. An
unparseable file is moved aside as `accesspoint.env.corrupt.<epoch>` rather
than being overwritten. Set `GPN_AP_TOKEN_FILE` to relocate the file (tests and
multi-instance hosts).

| Env | Default | Meaning |
|---|---|---|
| `GPN_ESCROW_MODE` | `settle` | `settle` = value-based routing (see above); `channel` = batch everything (opt-in for volume); `onchain`/`off` = always per-job escrow |
| `GPN_SETTLE_MIN_GPN` | `2163143150` | value threshold in base units (10× the measured settle fee) |
| `GPN_HUB_CHANNEL_DEPOSIT` | `5e13` (50 GPN) | locked **per (AP, provider) pair**; `0` disables channel mode. Users share the channel, so this does not scale with users |
| `GPN_HUB_SETTLE_SECS` | `86400` (24 h) | scheduled cooperative settlement interval |
| `GPN_HUB_CLOSE_AT` | `2.5e13` (25 GPN = deposit/2) | early settle once the provider's credit reaches this |
| `GPN_HUB_CLOSE_NONCE` | `50000` | ...or this many off-chain updates (flood valve) |
| `GPN_HUB_CHANNEL_TTL` | `9000` blocks (~30 h @ 12 s median) | the channel deadline (force-close window). Must exceed `GPN_HUB_SETTLE_SECS`; block times vary 3–90 s so wall-clock TTL drifts |
| `GPN_HUB_STATE` | `~/.gpn/hub_channels.json` | channel registry (restart-safe) |
| `GPN_PENDING_WARN` | `12` | warn when this many extrinsics sit in the pool |

**Capital:** total float = (providers with an open channel) × 50 GPN. One
provider → 50 GPN, three → 150 GPN, ten → 500 GPN. At 1 000 providers this
would lock 50 000 GPN, so the documented next step at that scale is a 10 GPN
deposit with a 5 GPN early trigger (or user-signed escrow).

Watch it work:

```
[pricing] settle threshold=2163143150 base units (0.00216314315 GPN) = 10x measured per-job settle cost (216314315 base units); mode=settle, channel=on
[hub] capital: 1 channel(s) × 50 GPN = 50 GPN locked (per AP-provider pair; users share these)
[hub] off-chain escrow ENABLED (mode=settle, deposit=50000000000000, ttl=9000 blocks, settle_every=86400s, close_at=25000000000000, ...)
[hub] job 1792706257296 served off-chain on channel 7 (nonce 1, provider balance 39954841)
[hub] channel 7 due for scheduled settlement (86400s interval)
[hub] channel 7 settled on chain
```

`POST /jobs` responses gain `"escrow":"channel"`, `channel_id`, and the
off-chain `job_id` (a local id, **not** a chain job id). The channel's on-chain
state stays at its last settled nonce until the settler runs — that is correct,
not a bug.

### Provider

The provider serves `GET /channel/status` (a capability probe) and
`POST /channel/job` on **heartbeat port + 1** (the same loopback server as the
chain-free `/infer`, i.e. `:8094` next to `:8093`), so no new inbound port is
needed. The AP probes `/channel/status` **before** it opens (and funds) a
channel, so an older provider never costs it a locked deposit. Actions:

- accept a signed assignment (verify both signatures, nonce, exact price,
  deadline) and persist the pre-paid state **before** running inference;
- sign the result (or a full refund) plus its half of the channel settlement;
- keep state in `GPN_PROVIDER_CHANNEL_STATE` (default
  `~/.gpn/provider_channels.json`).

`GPN_ESCROW_MODE=onchain` on the provider disables the endpoint (it 404s and
the AP falls back). `GPN_CHANNEL_SKIP_CHAIN_CHECK=1` skips the one-time
on-chain channel verification — **dev only**.

### Client: deposit and withdraw

```bash
# once per provider — this is the only funding tx
# (50 GPN deposit, 9000-block TTL)
gpn_channel deposit ws://127.0.0.1:9944 //Charlie 5ENq…Sp5Q 50000000000000 9000
gpn_channel id      ws://127.0.0.1:9944 //Charlie 5ENq…Sp5Q
gpn_channel info    ws://127.0.0.1:9944 1
# settle back
gpn_channel withdraw ws://127.0.0.1:9944 //Charlie 1 <br> <bp> <nonce> <sig_req> <sig_prov>
```

(`open`/`close` are the same commands; `sign` produces one party's half for an
out-of-band exchange.)

### Prove it end-to-end

```bash
PROVIDER_SIGNER=<registered-provider-seed> PROVIDER_ADDR=<its ss58> JOBS=3 \
  bash scripts/test-channel-e2e.sh
```

The script snapshots `ComputeMarketplace::NextJobId`, serves N jobs through a
channel, asserts `NextJobId` **did not move** (no per-job tx), prints each job's
wall time and the off-chain channel state, then settles on chain cooperatively.

## How to become a seed / bootstrap node

A **seed** is a backbone node that is published as a bootstrap dial target.
Nothing is registered and no permission is granted — a seed has no privileges.

**What qualifies:**

- inbound-reachable on TCP `30333` with a **stable public IPv4** (static, or a
  DDNS name kept current);
- same `genesis_hash`, released unmodified binary, always-on and monitored;
  target **≥ 99.5% monthly reachability**;
- `backbone` tier with `uptime_bps ≥ 9500` (95%) over the liveness window and
  `reachable_count ≥ 3` distinct peer witnesses;
- a published abuse/ops contact and a named maintainer.

**How promotion works:** the node announces its endpoint on-chain by itself.
An off-chain exporter (`gpn_seedlist`, permissionless to run) reads
`pallet-gpn-liveness` and publishes the top **16** eligible nodes by
`(uptime_bps, cumulative_online_blocks)` to `seeds.json`, which every client
fetches at startup. There is nothing to apply for; running a healthy dialable
node is the application. Anyone can audit or re-publish the list from chain
state.

**The hardcoded 5-seed list is separate.** Every client also ships with up to
five compiled-in seed IPs as a DNS/HTTP-independent floor. Those five are a
**dedicated bootstrap set**: separately operated cheap VPSes controlled and
rotated by GPN, used only as dial targets — not co-located with the oracle
operators. Inclusion is by invitation against the criteria above (distinct
operators/jurisdictions, ≤2 changes per release), not by uptime ranking alone.

### The live network directory — `https://gpncoin.ai/seeds.json`

Every node also fetches a live directory at startup, with **no flag needed**:

```
https://gpncoin.ai/seeds.json      # JSON  (what gpn-node fetches)
https://gpncoin.ai/bootnodes.txt   # the same dialable multiaddrs, one per line
```

The directory is a Cloudflare Worker that unions (a) the compiled-in seed list
with (b) the access point's **currently healthy providers**, and caches the
result for 60 s (`Cache-Control: public, max-age=60`). The node merges it with
its static bootnodes and persisted peers exactly like any other seed source:
entries are deduplicated, invalid ones dropped, and **a failed fetch is logged
and ignored** — the compiled-in bootnodes still get it online.

```bash
curl -s https://gpncoin.ai/seeds.json | head -40
```

```json
{
  "network": "GPN Public Testnet",
  "chain_id": "gpn_testnet",
  "genesis": "0xb8e4636a65d0c812abb437c38dd7e937bd7c3355c54c54d63060f26a33519a5c",
  "updated_at": "2026-10-19T22:00:00.000Z",
  "ttl": 60,
  "multiaddrs": ["/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooW…"],
  "nodes": [{ "multiaddr": "/dns4/…/p2p/12D3KooW…", "role": "seed" }]
}
```

**Override / add sources.** `--bootnodes-url <URL>` (or `bootnodes_url = […]`
in `node.toml`, `GPN_BOOTNODES_URL`) **replaces** the default directory; pass
several to union them. `--bootnodes-dns` works alongside it.

**Publishing a node.** There is no registration form and no permission: run a
healthy, dialable node (see the criteria above). It announces its endpoint
on-chain by itself, appears in `GET /providers` on the access point once it is
serving, and therefore in the live directory. Only multiaddrs that carry a
`/p2p/<peer-id>` are dialable by `gpn-node`; provider entries that publish only
an HTTP `status_url` are listed under `nodes[]` for tooling but are not dialled.
The hardcoded seed set changes only with a release (or a `network.json`
update) — being in the live directory does not compile you into clients.

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
2. Run `gpn_node --setup` — one question: generate or import a wallet.
   (A plain `gpn_node` start asks the same question when no wallet is
   configured, so the two commands are interchangeable.)
3. Run `gpn_node`: chain spec, data folder, payout address, peers and the
   public address are already resolved; it passes the ollama gate, then starts
   the miner, the heartbeat, and every provider loop.

`GPN_PUBLIC_ADDR` is only needed when the router rewrites the external port —
otherwise public-IP detection + UPnP/NAT-PMP handle it (see
[Make your node dialable](#make-your-node-dialable)).

When ollama is up but a model is merely missing, the node still starts (model
presence is not the gate); the provider pulls/retries and logs a **single**
actionable warning instead of per-poll `pull … failed` spam.

## Updates — auto-update and the minimum-version cutoff

Short version: **opt in and forget it, or update by hand — but a node that falls
too far behind stops mining.** Full detail: [`docs/updates.md`](updates.md).

```bash
# (default: on — this line is only needed to opt back in after disabling)
GPN_AUTO_UPDATE=1 gpn_node

# manual (the default): nothing is downloaded, ever
#   ...but when a release raises min_version this node will SYNC and NOT MINE
```

**Auto-update** (**on by default**; opt out with `GPN_AUTO_UPDATE=0` / `auto_update = false`):

* checks `https://gpncoin.ai/releases.json` at start and every
  `GPN_UPDATE_CHECK_HOURS` (default 6);
* **verifies an ed25519 signature** over the manifest with the key compiled into
  the node — a forged manifest (or a compromised web host) is rejected before a
  single byte is downloaded;
* verifies the artifact's `sha256` **twice**: in memory after the download, and
  again from disk immediately before the swap;
* stages it as `gpn_node.new`, keeps the old binary as `gpn_node.bak`, and swaps
  atomically;
* restarts to run it. **That is a short outage** (seconds: children stop
  cleanly, sync resumes) — it is the only way to execute new code. Prefer to
  choose the moment? `GPN_UPDATE_RESTART=0` stages it and applies it on your next
  restart.

**The cutoff** (not optional): every start — auto-update or not — the signed
manifest's `min_version` is checked. Below it:

```
  !! UPGRADE REQUIRED — this node is 0.1.0, the network requires >= 0.2.0
  This node will SYNC ... but will NOT MINE and will NOT ANNOUNCE itself.
```

The supervisor launches the chain node with `--no-seed --no-discoverable` and
`GPN_STANDBY=1`, which stops the proposer and the miner. The node keeps syncing,
keeps its RPC, and can be upgraded in place. The last verified cutoff is cached
in `<base>/update/update-state.json`, so it still applies if the manifest is
unreachable.

**Rollback:** if a release is bad, stop the node, move `gpn_node.bak` back over
`gpn_node`, and restart (§6 of `docs/updates.md`).

> The on-chain half of the cutoff (a `MinNodeVersion` storage item) is **staged,
> not applied**: `docs/design/design-min-node-version.md`.

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
* Windows — `nvidia-smi`, then `rocm-smi`, then the display-class registry
  value `HardwareInformation.qwMemorySize` (**REG_QWORD, real 64-bit** figure
  per adapter, iGPUs skipped), and only as a **last resort**
  `Win32_VideoController.AdapterRAM`.

> ⚠️ **Windows `AdapterRAM` is a uint32 and saturates at 4,294,967,295 bytes
> = 4095 MiB.** A 12 GB or 24 GB card reads as ~4 GB from that field, which
> used to make the VRAM model gate exclude models the box can actually run.
> Detection now prefers `nvidia-smi`/`rocm-smi`, then the 64-bit registry
> `qwMemorySize`, and uses `AdapterRAM` only when nothing else works. When it
> does, the agent says so explicitly:
>
> ```
> [gpu] VRAM from AdapterRAM (32-bit, capped at 4095 MiB) — value may understate the real total; models larger than 4 GB may be wrongly excluded
> ```
>
> If neither vendor CLI nor the registry key is available, set `--vram-mb`
> (or `VRAM_MB`) to the real figure. A probe that finds nothing at all now
> warns loudly (`[gpu] VRAM detection returned 0 MiB …`) instead of silently
> disabling the gate.

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
chain        = "/home/you/.gpn/node/testnet-spec.json"   # testnet | mainnet | dev | <path to chain-spec.json>
# signer     = "<seed/mnemonic>"                     # leave OUT to use this machine's wallet;
#                                                    # setting it OVERRIDES the keystore (fleet only)
signer_file  = "/home/you/.gpn/node/keystore.json" # owner-only keystore; the normal choice
account      = "5ENqSp5Q…"                         # payout / advertised ss58 address (auto-derived from the wallet)
endpoint     = "ws://127.0.0.1:9944"               # chain RPC used by the provider loop
base_path    = "~/.gpn/node"                      # or set GPN_BASE_PATH; default: C:\gpn\data (Win) / ~/.gpn/node (Unix)
rpc_port     = 9944
node_key     = "<64-hex ed25519 secret>"           # node's network identity
bootnodes    = ["/ip4/23.241.109.255/tcp/30333/p2p/12D3KooW…"]
bootnodes_url = []                                 # [] → https://gpncoin.ai/seeds.json (live directory)
public_addr  = "/ip4/<your-public-ip>/tcp/30333"     # REQUIRED (see above; forward this port)
author       = "alice"                             # passed as --alice to gpn-node (authoring key)
node_args    = ["--no-telemetry", "--no-prometheus"]

provider_endpoint = "http://<your-public-ip>:8093/status"   # advertised on-chain
assign_token      = "<shared AP↔provider secret>"           # GPN_ASSIGN_TOKEN (see below)
ollama       = "http://127.0.0.1:11434"
listen       = "0.0.0.0:8093"                      # heartbeat bind
gflops       = 0                                   # UPPER BOUND, in the measured unit
                                                   # (GPN-GEMM-F32-1024: ~823 on an RTX 4060).
                                                   # The heartbeat advertises min(this, on-chain
                                                   # attestation). Leave 0/unset: the attestation
                                                   # is measured, and any hand-typed value above
                                                   # 1.5x this box's own measurement is rejected
                                                   # and replaced (item #97). 30000 here is a unit
                                                   # bug, not a faster card.
capacity     = 1
poll_ms      = 300
# ── optional providers: ALL ON by design; set false to opt a capability OUT ──
diffusion    = true                                # image/video provider (gpn_diffusion_provider)
diffusion_poll_ms = 2000
ml           = true                                # ML-inference provider (gpn_ml_provider)
ml_poll_ms   = 2000
finetune     = true                                # fine-tune provider (gpn_finetune_provider)
finetune_poll_ms = 4000

# ── updates: auto-update is OPT-IN; the version cutoff applies regardless ──
auto_update         = false   # true = install signed GPN releases automatically
update_url          = "https://gpncoin.ai/releases.json"
update_check_hours  = 6       # how often to look for a release (1-168)
update_restart      = true    # false = stage it, apply on the next start
update_check        = true    # false = air-gapped: never fetch the manifest
min_version_enforce = true    # false = mine below the minimum (unsupported)
```

### Environment variables (overrides)

| Env var | Meaning | Default |
|---------|---------|---------|
| `GPN_CHAIN` | chain spec (`testnet`/`mainnet`/`dev`/path) | auto: `<base>/testnet-spec.json`, fetched if missing |
| `GPN_SIGNER` | signer URI / seed / mnemonic — **overrides** the local keystore and is logged as an override (fleet/operator use only) | _(none — the wallet on this machine, else the first-run wizard)_ |
| `GPN_SIGNER_FILE` | path to an sr25519 JSON keystore (owner-only) | `<base>/keystore.json` when it exists |
| `GPN_SPEC_URL` | where to fetch the chain spec from | `https://gpncoin.ai/chain-spec` (falls back to `http://gpn-testnet.duckdns.org:8092/genesis-spec`) |
| `GPN_AUTOSTART` | informational: operator intends to install a service unit | `false` |
| `GPN_ACCOUNT` | payout / advertised address — **overrides** the address derived from the wallet (operator use only) | _(empty → derived from the wallet)_ |
| `GPN_ENDPOINT` | chain RPC (ws) for the provider loop | `ws://127.0.0.1:9944` |
| `GPN_BASE_PATH` | node data dir (**database + wallet**) | `~/.gpn/node` (Unix) / `C:\gpn\data` (Windows) |
| `GPN_RPC_PORT` | node RPC port | `9944` |
| `GPN_NODE_KEY` | node network key (64-hex) | _(none — node needs one)_ |
| `GPN_BOOTNODES` | comma/space list of bootnodes | _(none)_ |
| `GPN_BOOTNODES_URL` | comma/space list of published seed-list URLs. Replaces the built-in live directory. | `https://gpncoin.ai/seeds.json` |
| `GPN_PUBLIC_ADDR` | `--public-addr` (e.g. `/dns4/host/tcp/30333`). Recommended for a public node; the capability probe also reads it. | _(none)_ |
| `GPN_REQUIRE_DIALABLE` | Run the inbound-reachability probe. `0` skips it entirely (local testing only). | `1` |
| `GPN_REQUIRE_BACKBONE` | Hard gate: exit 78 when not dialable (instead of degrading). | `0` |
| `GPN_SEED` | Willing to be published as a seed candidate; `0` also silences the not-dialable notice. | `1` |
| `GPN_DISCOVERABLE` | Announce endpoint / accept inbound; `0` implies `GPN_SEED=0`. | `1` |
| `GPN_UPNP` | Try UPnP-IGD/NAT-PMP (`auto`/`off`). See [UPnP / NAT-PMP](#3-upnp--nat-pmp--automatic-on-by-default). | `auto` |
| `GPN_UPNP_RETRY_SECS` | Re-detection cadence while no mapping is live (30–3600). | `600` |
| `GPN_UPNP_LEASE_SECS` | Mapping lease requested from the router. | `3600` |
| `GPN_NATPMP_GATEWAY` | Router IP for NAT-PMP when OS detection is unavailable. | auto |
| `GPN_DIALABLE_TIMEOUT_SECS` | Grace window for dialability evidence. | `120` |
| `GPN_AUTHOR` | short authoring key name → `--<name>` | _(none)_ |
| `GPN_NODE_ARGS` | extra raw args appended to `gpn-node` | _(none)_ |
| `GPN_PROVIDER_ENDPOINT` | public `/status` URL advertised on-chain | _(none → discovery off)_ |
| `GPN_ASSIGN_TOKEN` | shared AP↔provider secret (`GET /assignments` auth) | _(empty → legacy-permissive, warns)_ |
| `GPN_AP_TOKEN` | access-point bearer token for non-loopback callers. Unset → read from the env file → generate on first boot and print once. | _(generated)_ |
| `GPN_AP_TOKEN_FILE` | where the AP reads/writes `GPN_AP_TOKEN` | `~/.gpn/accesspoint.env` |
| `GPN_AP_URL` | access point base URL for the **pull** transport. Unset → the canonical `http://gpn-testnet.duckdns.org:8092`; empty → AI serving disabled (loud warning). | `http://gpn-testnet.duckdns.org:8092` |
| `GPN_ASSIGN_SOURCE` | `pull` (the only delivery path) \| `both` \| `push` (**degraded**, no delivery) — see [Pull vs push](#pull-vs-push--work-behind-nat). | `pull` |
| `GPN_ASSIGN_WAIT_MS` | pull long-poll budget (the AP answers immediately when there is work) | `25000` |
| `GPN_OLLAMA` | ollama base URL | `http://127.0.0.1:11434` |
| `GPN_AUTO_UPDATE` | `1` = install signed GPN releases automatically (**opt-in**) | `0` |
| `GPN_UPDATE_URL` | signed release manifest URL | `https://gpncoin.ai/releases.json` |
| `GPN_UPDATE_CHECK_HOURS` | hours between auto-update checks (1–168) | `6` |
| `GPN_UPDATE_RESTART` | `0` = stage an update but apply it at the next start | `1` |
| `GPN_UPDATE_CHECK` | `0` = never fetch the manifest (air-gapped; the cutoff is then not evaluated) | `1` |
| `GPN_MIN_VERSION_ENFORCE` | `0` = mine even when below the published minimum (emergency only, unsupported) | `1` |
| `GPN_REQUIRE_OLLAMA` | Hard gate: refuse to start when ollama is unreachable. `0` opts out (special cases only). | `1` |
| `GPN_OLLAMA_SETUP` | Attempt the automated install/start if ollama is missing | `1` |
| `GPN_BASELINE_MODELS` | Comma-separated baseline model(s) to pull on first run | `llama3.2:1b` |
| `GPN_OLLAMA_WAIT_SECS` | Grace window for `/api/tags` after `ollama serve` | `60` |
| `GPN_LISTEN` | heartbeat bind addr | `0.0.0.0:8093` |
| `GPN_GFLOPS` | upper bound on advertised compute; the heartbeat advertises `min(this, attested)` — leave it `0`/unset to advertise the on-chain attestation as-is | `0` |
| `GPN_ATTEST_REFRESH` | `0` = never (re)attest from the provider loop | `1` |
| `GPN_ATTEST_REFRESH_BLOCKS` | re-attest when the on-chain record is this old (half of `MaxAttestationAge` = 14 400) | `7200` |
| `GPN_ATTEST_SAMPLES` | timed GEMM runs per attestation measurement (1–10); the best run is attested when they disagree | `3` |
| `GPN_ATTEST_SETTLE_MS` | wait after taking the GPU lock before reading utilisation, so the co-located miner has stood down (0–30 000) | `2000` |
| `GPN_ATTEST_MAX_UTIL_PCT` | device counts as **busy** above this utilisation; a busy GPU defers the attestation (0–100) | `10` |
| `GPN_ATTEST_MAX_VRAM_FRACTION` | device counts as busy above this VRAM fraction even if utilisation reads low (a loaded model leaves no room for a stable run) | `0.5` |
| `GPN_ATTEST_RETRY_SECS` | first retry delay after a skipped (busy) attestation; doubles per attempt | `300` |
| `GPN_ATTEST_RETRY_MAX_SECS` | cap for the attestation retry backoff | `3600` |

### What `compute_gflops` actually measures

The attested number is the throughput of GPN's own FP32 GEMM benchmark — the
canonical unit **`GPN-GEMM-F32-1024`** (`gpn_agent::compute_unit`):

```
dtype      f32 (OpenCL float)          matrix   N = 1024, C = A·B
flops/run  2·N³ = 2.147 GFLOP          value    best of 3 timed runs (1 warm-up)
conditions GPU idle: GPU lock held, miner yielded, util ≤ 10 %, VRAM < 50 %
```

It is real FP32 GFLOP/s, but **not the card's spec-sheet peak**: the shipped
16×16 shared-memory tile reaches only ~5–7 % of peak. Expect ±12 % between power
states on the same card, and ~1 % across repeats in one state. That is plenty to
separate device classes — which is all routing needs — but it means a floor must
be written in *this* unit, never in TFLOPS.

| device | published FP32 peak | measured (`GPN-GEMM-F32-1024`) |
|---|---|---|
| RTX 4060 8 GB | 15.1 TFLOPS | **823–912** (822/825/825 attested) |
| RTX 5070 Ti 16 GB | 44.0 TFLOPS | ~2 300–2 650 (estimate — not measured idle) |
| integrated GPU (Iris Xe / Vega 8 class) | 1.5–2 TFLOPS | ~50–110 (estimate) |

**Why the old numbers were wrong.** Three defects, all fixed: the field was once
written from raw FLOP/s, leaving `30000` on a 15 TFLOPS card (above its own
hardware peak); box1 attested `188` from a sample taken while another workload
held the card; and only an *upward* re-attestation was ever accepted, so neither
record could be replaced. `30000` is not a fast card — it is a unit bug, and it
is now replaced automatically (a record above 1.5× the card's estimated FP32 peak
is treated as a legacy artefact, not a capability claim).

Measurement rules that matter when you compare two boxes:

* **attestation runs only when the GPU is idle.** Before measuring, the provider
  takes the same cross-process GPU lock a job takes (no waiting: if a job holds
  it, the attempt is skipped), waits `GPN_ATTEST_SETTLE_MS` for the co-located
  miner to stand down, then reads utilisation and VRAM. If the card is still
  loaded by anything else — an operator CUDA/ollama job, a desktop session, a
  second miner — the attempt is logged as
  `[attest] skipping: GPU busy (util …%) — will retry later` and retried with
  backoff (`GPN_ATTEST_RETRY_SECS`, doubling, capped). **No measurement taken
  under contention is ever submitted.** If your card also runs other AI work,
  expect the value to update during quiet periods, not on the first attempt;
* the number is a **best-of-N** (`GPN_ATTEST_SAMPLES`, default 3) and the spread
  is logged: a >20 % spread is contention or thermal throttling, so the highest
  sample is used, and `/status` publishes `gflops_samples`,
  `gflops_spread_pct`, `gflops_conditions` and (when the advertised value is
  this box's own idle measurement that was actually submitted)
  `gflops_source: "attested-idle"`. A record that disagrees with the idle
  measurement by more than 30 % is named in a loud warning
  (`gflops_stale_warning` too);
* the **discrete** card is measured (device 0 is ordered discrete-first, so the
  benchmark and the miner always use the same GPU — before 2026-10-23 they did
  not, and a Windows box attested an iGPU-class 188 GFLOPS for a 16 GB card);
* **a state warning is logged once, not once per poll.** `/status` is polled
  every ~3 s, so the "on-chain attestation disagrees" / "`--gflops` implausible"
  notices fire on the **transition** (they appear when the state first holds, and
  again only if the record or the measurement actually changes). The same
  information is in the body of every response as `gflops_stale_warning`, so a
  monitor never needs the journal. Before 2026-10-23 these lines were emitted
  from the value resolver — twice per request — and a single box wrote ~1 800
  identical lines an hour, which crowded out everything else the heartbeat logs;
* **only `gflops_source: "attested-idle"` is usable evidence.** The access point
  will not route a `min_gflops > 0` job to a box whose number came from a
  hand-typed `--gflops`, an older chain record or a contended sample: that value
  is treated as *unknown* (logged, never silent) and the box serves ungated jobs
  until it re-attests on an idle GPU. Never set `--gflops`/`GPN_GFLOPS` unless
  you are deliberately advertising a *lower* bound than the measurement.

**The eligibility floor is 250 GFLOPS** (`MinEligibilityGflops`, also in this
unit): 30 % of the reference card's lowest idle measurement (823 on an RTX 4060
8 GB), which admits any discrete 8 GB card and excludes every integrated GPU. It
used to be `3000` on the spec-sheet scale, which no consumer card could reach —
so a new box could not attest at all and kept whatever record it had.

> **Runtime status:** the constant is changed in source and **staged** as part of
the next runtime upgrade (`shared/artifacts/project7/staged/gflops-floor-v1.md`).
> Until it enacts, the chain still answers `BelowEligibilityFloor` for values
> below 3 000: the provider logs that once and stops retrying (it does **not**
> submit a fabricated number). A restart after the upgrade re-attests
> automatically; `GPN_ATTEST_FORCE=1` forces a re-measure on the next start for a
> record that is wrong but not provably impossible.


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

> **Secrets stay out of the config file.** `gpn_node` reads the signer/seed and
> the assign token from env / the config file and only ever writes a secret when
> you ask the wizard to create/import a wallet — into `<base>/keystore.json` with
> owner-only permissions. Put the token in `GPN_ASSIGN_TOKEN` (or the service
> `EnvironmentFile`), and keep the config file `chmod 600`.

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

  A node that failed the **dialability gate** with `--require-backbone` set is
  shown distinctly as `node=down(not dialable, restarts N)` (the child exited
  78), with an actionable hint in the log — set `GPN_PUBLIC_ADDR` and forward
  the port. Without `--require-backbone`, a non-dialable node stays `up` and
  simply runs degraded.

## Local dashboard + Idle mode ("let me play a game")

A node belongs on a gaming PC, so it has a switch for when the PC is being used
for games: **idle mode** pauses mining and AI job claims for up to **6 hours**,
keeps the node syncing/peering/serving the chain the whole time, and resumes
automatically at the end (or on a *Resume now* click).

```bash
# serve the dashboard (loopback only, no auth needed — it is not on the network)
gpn_node --gui            # http://127.0.0.1:8099
GPN_GUI=1 gpn_node        # same, via env

# idle / resume from a shell (works against a running node, no restart)
gpn_node idle 120         # 2 hours (max 360 = 6 h; longer is refused)
gpn_node resume
```

Without `--gui` the node prints one hint line at startup:
`dashboard: off — add --gui to serve the local control panel on 127.0.0.1:8099`.

**What idle does:** the miner thread parks (no nonces searched, GPU free), and
every provider stops claiming new jobs at its claim boundary. **What it does
not do:** it does not stop the node — syncing, peering, RPC, the heartbeat on
`:8093`, discovery and your on-chain identity all keep working. A job already
running finishes normally; idle never strands one.

State is one file, `~/.gpn/idle.json` (`{"until": …, "reason": …, "started": …}`,
0600, override with `GPN_IDLE_FILE`), read by the miner and every provider — so
idle survives a restart. An expired file reads as *active*, so a box that was
off past the deadline simply starts mining again.

**Safety valve.** If the node has **no peers** (it may be the only block
producer) or its RPC is unreachable, idle is **refused** — idling the last
authority would halt the chain. The dashboard answers `409` and the CLI exits
`3`, both explaining why. Override for a single-box dev chain with
`--force-idle` / `GPN_IDLE_FORCE=1`.

Windows: `gpn-agent/windows/gpn-dashboard.cmd` starts the supervisor with
`--gui` and opens the page — put a shortcut to it on the desktop.

Full details (endpoints, semantics table, screenshots-in-words, tests):
[`node-gui-and-idle.md`](node-gui-and-idle.md).

## Linux

```bash
# foreground (Ctrl-C stops everything) — always miner + provider
# No GPN_SIGNER: the node uses the wallet on THIS machine (run
# `gpn_node --ensure-wallet` once if there is none). Setting GPN_SIGNER is an
# operator/fleet override and the node logs it as such.
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

## Node identity — one key per machine, generated on first run

Your node generates its own libp2p identity on first run. **Never copy a node
key file between machines.**

* **Windows (release bundle / `start-node.cmd`):** the launcher creates
  `%USERPROFILE%\.gpn\node-key` once per Windows user with
  `gpn-node.exe key generate-node-key --file …` and exports it as
  `GPN_NODE_KEY_FILE`. If that generation fails the launcher **aborts** rather
  than starting a node with a default/shared identity.
* **Windows money identity:** the same launcher calls
  `gpn_node.exe --ensure-wallet` before starting anything, which creates
  `C:\gpn\data\keystore.json` on first run (owner-only ACL), reuses it forever
  afterwards, and prints `payout account: 5XXXX…`. If the keystore is corrupt
  the **launcher aborts** instead of mining to a new account.
* **Linux / macOS:** if you set neither `GPN_NODE_KEY` nor `GPN_NODE_KEY_FILE`,
  `gpn-node` creates and persists its network secret under
  `<base-path>/chains/<chain>/network/secret_ed25519` on first start. To keep an
  explicit, backup-able identity file use `GPN_NODE_KEY_FILE=~/.gpn/node-key`
  (create it with `gpn-node key generate-node-key --file ~/.gpn/node-key`).

> **Symptom of a copied key:** two nodes with the same `peer_id`. Peers dial the
> wrong host, the directory collapses the two entries into one, and neither box
> is reliably reachable. If you see two nodes sharing a peer id, one of them
> copied a key file — delete `node-key` (`secret_ed25519` on Linux) on the
> copied box and restart it so it generates a fresh identity.

`GPN_NODE_KEY` (a 64-hex literal) still works but is only for a single
hand-managed box; never bake it into an installer, an image, or a fleet
checkout — that is how every machine ends up with the same identity.

**The other identity — the one that holds the money — follows the same rule.**
The peer id names the box on the network; the **account** (money identity) is
what signs and receives earnings, and it is a separate key in
`<base>/keystore.json` (created by `gpn_node --ensure-wallet` / `--setup`).
Never copy it between machines and never pin one in a bundle: two nodes sharing
an account split and steal each other's rewards, and a dev URI (`//Alice`,
`//Bob`) is derivable by anyone. See
[Your node has two identities](#your-node-has-two-identities--never-copy-either-from-another-node).

## Windows

`gpn_node.exe` is the same binary. From the release bundle (`.\bin\`):

```powershell
# everything supervised under one console window (miner + provider)
$env:GPN_SIGNER           = "<your seed/mnemonic>"   # OPTIONAL override; leave unset to use this machine's keystore
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

## Watchdog — auto-recover a silently wedged **provider**

A provider can wedge the same way the node can, and the symptom is worse for a
customer: on 2026-10-22 `gpn-provider` made no progress for ~23 minutes while a
job sat on-chain `Pending`. The unit stayed `active`, nothing was logged, and a
plain `systemctl --user restart gpn-provider` fixed it instantly — it was a
zombie, not a crash. Nothing bounded the provider's waits (a chain read over a
half-open websocket has no request timeout), so any hung `await` parked the
claim loop forever.

`gpn_llm_provider` now reports **progress**, not just liveness:

* every phase change and every completed pass bumps `~/.gpn/provider-tick.json`
  (`{"pid","loops","phase","updated_unix"}`) — a stale tick therefore **names
  the step that hung** (`reconcile`, `claim`, `run #N`, `gpu-wait`, `infer`,
  `settle`, `idle`);
* a plain watchdog thread (not a tokio task — a wedged runtime must not be able
  to wedge its own supervisor) polls that state and, if nothing moved for the
  phase's budget, logs ONE loud `⚠ [watchdog]` line naming the phase, the idle
  time and the completed-pass count, then exits with code **75** so
  `Restart=always` performs a real restart.

Budgets are per phase because some steps are legitimately slow: `gpu-wait` uses
`GPN_GPU_LOCK_TIMEOUT_SECS` + 300 s (default 1200 s), `infer` uses the 300 s
ollama timeout + 300 s, `startup` gets 30 min (a cold model pull), and every
other phase uses `GPN_PROVIDER_WATCHDOG_SECS` (default **300 s**, floor 60 s).

Knobs: `GPN_PROVIDER_WATCHDOG=0` disables it, `GPN_PROVIDER_WATCHDOG_SECS` sets
the default-phase budget, `GPN_PROVIDER_TICK` moves the tick file. Check the
state with `cat ~/.gpn/provider-tick.json` (a healthy provider's `loops` grows
about three times a second at the default 300 ms poll).

## Provider restart — stale `Running`/`Accepted` jobs are adopted at startup

A restart in the middle of work used to leave the job in limbo: the claim and
reconcile loops only ever look at the **last 128 chain ids**, so a job claimed by
the previous process was re-run only if a later tick happened to land on it
(`Running` #97), and an `Accepted` job whose next step never came (`#105`) sat
there until its on-chain deadline. Nothing enumerated "the jobs I actually own".

Every provider start now runs one **adoption sweep** before the claim loop takes
its first nonce, over the ids in its own assignment store, and decides once per
id:

| chain state | payload we hold | action |
|---|---|---|
| `Pending` | any | left to the claim loop, which **waits for create inclusion** (no optimistic accept) |
| `Accepted` / `Running` | queued | **resume inference** → submit result → complete |
| `Accepted` / `Running` | stored result | **resume settlement** → submit result → complete |
| `AwaitingVerification` | any | `complete_job` (frees the capacity slot) |
| `Accepted` / `Running` | none, or not runnable here | `fail_job` → escrow refunded **now**, slot freed |

A job we hold a live (`queued`/`running`) payload for is examined regardless of
age; ids without one are only examined within 512 of the tip (the store grows
forever — an O(history) startup scan is the finding-#34 trap, and an older id
cannot still be in flight). Every id that needs action gets one log line naming
the state, the payload shape and the action, and the sweep always ends with a
summary — a quiet start is now provably "nothing to adopt":

```
[reconcile] startup adopt #6503 state=Running payload=queued model=llama3.2:1b action=resume-inference
[reconcile] startup adopt #6504 state=Running payload=none model=- action=fail
[reconcile] startup adopt #6504: failed on-chain (reason 4: no off-chain payload (cannot ever run it)) — escrow refunded, slot freed
[reconcile] startup adoption: examined 506 assigned id(s) below tip 6505 (2358 older than 512 ids without a live payload, left to the expiry sweep); 1 non-terminal → 0 resumed, 0 settled, 0 completed, 1 failed
```

`fail_job` reasons used by the sweep: **4** = no off-chain payload, **5** = the
payload cannot run here (foreign/diffusion model, or the per-job attempt cap).
`GPN_STARTUP_ADOPT=0` disables the sweep.

The same start also clears a **stale miner-yield flag**: a `SIGKILL`ed provider
never runs `GpuYieldGuard`'s `Drop`, so `~/.gpn/gpu-yield` survived and kept the
co-located miner parked forever. The flag is only meaningful while a GPU lock
holder exists, so startup takes the lock briefly (1 s): if it succeeds the flag
is stale and is removed; if not, another provider is genuinely mid-job.

## Image/video lane — a cold model is refused, then warmed in the background

A channel (pre-paid) render carries the AP's wall-clock deadline — **180 s** by
default (`GPN_HUB_JOB_DEADLINE_MS`). A cold HuggingFace pull is ~13 GB, i.e.
20–45 minutes, so accepting that job means the caller waits out its own timeout,
gets `every candidate refused`, and the download restarts from scratch on the
next attempt (this is what wedged the image lane on 2026-10-23).

`gpn_diffusion_provider` now decides **before** it accepts anything:

| model state | remaining deadline | result |
|---|---|---|
| on disk (warm) | any | served normally |
| recorded unloadable on this host | any | refused instantly, never re-pulled |
| needs credentials (401/403) | any | refused instantly **until the verdict lapses** (default 6 h) |
| cold | ≥ `GPN_COLD_PULL_BUDGET_MS` (default **900000** = 15 min) | pulled in-band, then rendered |
| cold | < the budget | **refused in ~50 ms** with `HTTP 425`, a background pull starts |

The refusal is a **4xx on purpose**: the AP treats it as *not attempted*, so
failover moves to the next provider, and nothing is charged (`payment:
not_charged`). The caller sees an actionable reason instead of a timeout:

```
model stabilityai/sdxl-turbo is not warm on this host (15 min download) and the
request leaves 179s — downloading it now in the background; retry when warm
```

One background pull per model per process (a retry storm cannot spawn more), and
a failure no retry can fix is recorded so later requests refuse instantly instead
of downloading again. Two verdicts, deliberately different lifetimes:

* **`~/.gpn/diffusion_unloadable.json`** — the repo/loader itself is broken
  (`404`, not a model id, `cannot load model`, OOM on this box). **Forever**: no
  local action can change it, and re-attempting burned ~1 min of RAM and risked
  an OOM kill. Message: `… is unloadable on this host (recorded …)`.
* **`~/.gpn/diffusion_needs_auth.json`** — HuggingFace answered **`401`/`403`**.
  That is *this host*, not the repo: HF returns `401` identically for a private
  repo, a gated repo, a retired repo and a bad token. **Timed**
  (`GPN_DIFFUSION_AUTH_RETRY_SECS`, default **21600** = 6 h), so setting
  `HF_TOKEN` heals the model with no state editing. Message: `… needs HuggingFace
  credentials on this host — or the repo is private, gated or does not exist …`

So if a model you know is public is refused for credentials, export `HF_TOKEN`
for `gpn-diffusion` (or delete the entry) — do **not** add it to the unloadable
file, which would condemn it permanently.

Knobs: `GPN_COLD_PULL_BUDGET_MS` (how long a cold pull may take before we refuse
instead of accepting the job), `GPN_DIFFUSION_AUTH_RETRY_SECS` (how long a
`401`/`403` verdict silences a model), `GPN_DIFFUSION_NEEDS_AUTH` /
`GPN_DIFFUSION_UNLOADABLE` (registry paths), `GPN_DIFFUSE_TIMEOUT_SECS` (the
render budget — now progress-aware, extended while the helper keeps reading
bytes).

**Operator advice:** for a model you plan to serve, warm it before announcing the
capability — a first request for an uncached large model will be refused by
design rather than making the customer wait 20 minutes.

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

## Rebuilding and deploying a binary (one at a time)

**Deploy one binary at a time; the lane lock exists because two deploys once raced and shipped the wrong build.**
Two agents once ran `cargo build --release` from the same `target/release` and
installed at the same moment; a parallel build replaced the installed provider
with a binary that did not contain the change under test, and the test passed
against stale code. Build/install only through the lane:

```bash
cd gpn-agent
../scripts/deploy-bin.sh --bin gpn_llm_provider --unit gpn-provider --expect-marker gpu-lock
```

It takes an exclusive `flock` on `~/.gpn/deploy.lock` (a second run waits and
says so), refuses a stale artifact, asserts `--expect-marker` substrings in the
built binary **before** installing, backs the old binary up as
`<name>.bak-<UTC ts>`, installs by atomic rename, restarts the named
`systemctl --user` unit(s) until `is-active`, and prints sha256 built-vs-installed.

Then confirm the **running process**, not just the file:

```bash
strings -a /proc/$(systemctl --user show -p MainPID --value gpn-provider)/exe | grep -F gpu-lock
```

## Fault isolation / why separate processes

The supervisor deliberately keeps one process per part: an ollama/diffusion
crash, an OOM in the AI stack, or a wedged inference never takes the chain node
down — the supervisor restarts just that child. Folding everything into one
binary is tracked separately (task #26, deferred).
