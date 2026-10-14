# GPN — Downloads

Every GPN build publishes a **rolling GitHub Release**, so there is always a stable, downloadable URL.

## Releases (rolling "latest")
| Platform | Release | Asset |
|---|---|---|
| Windows | [`windows-latest`](https://github.com/jeremyzguo/gpn_coin/releases/tag/windows-latest) | `gpn-windows.zip` |
| Linux | [`linux-latest`](https://github.com/jeremyzguo/gpn_coin/releases/tag/linux-latest) | `gpn-linux.tar.gz` |
| macOS | [`macos-latest`](https://github.com/jeremyzguo/gpn_coin/releases/tag/macos-latest) | `gpn-macos.tar.gz` |

Each build **replaces** the asset on the same tag, so the URL never changes — it always points at the newest build.

## ⚠️ Private repo → downloads need auth
This repo is currently **private**, so release assets require a GitHub token:
```bash
curl -L -H "Authorization: token <YOUR_GITHUB_TOKEN>" \
  -o gpn-windows.zip \
  https://github.com/jeremyzguo/gpn_coin/releases/download/windows-latest/gpn-windows.zip
```
**Once the repo is made public, the same URLs download anonymously** (no token) — that's the launch step that turns these into public download links.

## Quick start by OS
**Windows** — unzip and run (see `docs/deploy` on the website):
```powershell
curl.exe -L -H "Authorization: token <TOKEN>" -o gpn-windows.zip https://github.com/jeremyzguo/gpn_coin/releases/download/windows-latest/gpn-windows.zip
Expand-Archive gpn-windows.zip -DestinationPath C:\gpn\bin -Force
```

**Linux / macOS** — untar and use the one-command installer, or run the binary directly:
```bash
curl -L -H "Authorization: token <TOKEN>" -o gpn-linux.tar.gz https://github.com/jeremyzguo/gpn_coin/releases/download/linux-latest/gpn-linux.tar.gz
tar -xzf gpn-linux.tar.gz
```

## From source
```bash
git clone https://github.com/jeremyzguo/gpn_coin.git
cd gpn_coin && ./install.sh            # node + agent
./install-accesspoint.sh               # just an access point
```

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
