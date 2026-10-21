# GPN — Downloads

Every GPN build publishes a **rolling GitHub Release**, so there is always a stable, downloadable URL.

**Downloads are public.** The official public repository is
[`crewlyllc/gpn_coin`](https://github.com/crewlyllc/gpn_coin) — all release assets below live there
and download **anonymously, with no GitHub account and no token**. It is a one-way mirror of the
private development repo [`jeremyzguo/gpn_coin`](https://github.com/jeremyzguo/gpn_coin).

## Releases (rolling "latest")
| Platform | Release | Asset |
|---|---|---|
| Windows | [`windows-latest`](https://github.com/crewlyllc/gpn_coin/releases/tag/windows-latest) | `gpn-windows.zip` |
| Linux | [`linux-latest`](https://github.com/crewlyllc/gpn_coin/releases/tag/linux-latest) | `gpn-linux.tar.gz` |
| macOS | `macos-latest` (not published yet) | `gpn-macos.tar.gz` |

Each build **replaces** the asset on the same tag, so the URL never changes — it always points at the newest build.

## No auth needed
```bash
curl -L -o gpn-linux.tar.gz \
  https://github.com/crewlyllc/gpn_coin/releases/download/linux-latest/gpn-linux.tar.gz
```
Release pages: <https://github.com/crewlyllc/gpn_coin/releases>

## Quick start by OS
**Windows** — unzip and run (see `docs/deploy` on the website):
```powershell
curl.exe -L -o gpn-windows.zip https://github.com/crewlyllc/gpn_coin/releases/download/windows-latest/gpn-windows.zip
Expand-Archive gpn-windows.zip -DestinationPath C:\gpn\bin -Force
```

**Linux / macOS** — untar and use the one-command installer, or run the binary directly:
```bash
curl -L -o gpn-linux.tar.gz https://github.com/crewlyllc/gpn_coin/releases/download/linux-latest/gpn-linux.tar.gz
tar -xzf gpn-linux.tar.gz
```

## From source
```bash
git clone https://github.com/crewlyllc/gpn_coin.git
cd gpn_coin && ./install.sh            # node + agent
./install-accesspoint.sh               # just an access point
```

Development happens in the private `jeremyzguo/gpn_coin` repository; `crewlyllc/gpn_coin` is synced
from it automatically (code + release assets), so the public history always matches the dev history.

## Also available
- **CI artifacts** (`gpn-windows`, `gpn-linux`, …) under **Actions** — but these **expire** (retention-days) and are quota-limited; the **Releases** above are the durable distribution.
- **Chain spec:** `GET /genesis-spec` on any access point (e.g. `http://gpn-testnet.duckdns.org:8092/genesis-spec`).
- **Whitepaper:** `docs/GPN-WHITEPAPER.md` / `.html`.

## DB snapshot (fast node onboarding / recovery)
A pre-synced **archive** database snapshot of the testnet, for onboarding a new
node in seconds (or recovering a wedged one) instead of full-syncing from
genesis. Served by the access point over plain HTTP:

```
http://gpn-testnet.duckdns.org:8083/gpn-testnet-snapshot.tar.gz
http://gpn-testnet.duckdns.org:8083/gpn-testnet-snapshot.tar.gz.manifest.json
```

Import it (see [`fast-sync.md`](fast-sync.md) for the full flow + the Windows
recovery one-liner):

```bash
gpn_snapshot import --snapshot gpn-testnet-snapshot.tar.gz --base-path ~/.gpn/node
```

Check the manifest's `genesis_hash` (`0xb8e4…9a5c` = canonical GPN Public
Testnet) and `best_block_number` before trusting it. The snapshot is exported
from an **archive**-pruned node so it is compatible with the node's default
`--state-pruning archive` (a legacy `256` snapshot would hard-fail).
