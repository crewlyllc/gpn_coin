GPN NODE — FIRST RUN
====================

Three steps. One question.

  1) Download + extract this package
  2) ./gpn_node --setup      (answer: generate or import a wallet)
  3) ./gpn_node

Nothing else needs to be configured. `--setup` downloads the chain spec,
creates the data folder, creates (or imports) your wallet, derives the payout
address, and installs/starts ollama with the baseline model. The second run
reads ~/.gpn/node.toml (Windows: %USERPROFILE%\.gpn\node.toml) and starts with
no flags and no environment variables.

A GPN node is ALWAYS a miner AND an AI provider. Ollama is a REQUIRED
component: `gpn_node` will REFUSE TO START if ollama cannot be reached
(exit code 69, `node=down(no ollama)`).

Files in this package
---------------------
  gpn-node(.exe)          the chain node/miner
  gpn_*(.exe)             the agent binaries (gpn_node is the one supervisor)
  scripts/install-ollama.ps1     ollama installer (Windows)
  scripts/install-ollama.sh      ollama installer (Linux/macOS)
  scripts/install-ollama-amd.sh  AMD/ROCm variant (Linux)
  gpn-node.example.toml   example config (optional — the wizard writes one)
  README-FIRST-RUN.txt    this file

If gpn_node / gpn-node binaries are missing, download the full release zip
from the GitHub Releases page of the repo.

1) Setup
--------
  Windows:   gpn_node.exe --setup
  Linux:     ./gpn_node --setup

You will be asked ONE question:

  Generate a new wallet, or import an existing one?

  * Generate — a 12-word phrase is printed ONCE. Write it down: it controls
    your earnings and cannot be recovered. It is stored in
    <base>/keystore.json with owner-only permissions (0600 on Unix, a
    user-only ACL on Windows).
  * Import   — paste a 12/24-word mnemonic or a 0x-prefixed 32-byte seed.

The resulting SS58 address is printed and used as the payout account.
Run `--setup` again any time; it keeps the existing wallet by default.

2) Start the node
-----------------
  Windows:   gpn_node.exe
  Linux:     ./gpn_node

One command starts the miner + heartbeat + every AI provider, and supervises
them as one node with one log stream and one status line.

What is automatic
-----------------
  data folder    ~/.gpn/node (Unix) / C:\gpn\data (Windows); DB + wallet
  chain spec     fetched from http://gpn-testnet.duckdns.org:8092/genesis-spec
                 and pinned at <base>/testnet-spec.json
  payout account derived from the wallet
  peers          https://gpncoin.ai/seeds.json + built-in bootnodes
  public address configured GPN_PUBLIC_ADDR -> public-IP detection ->
                 UPnP/NAT-PMP port mapping (on by default)
  ollama         installed/started + baseline model pulled

Fallbacks / manual overrides
----------------------------
  GPN_BASE_PATH     use a different data folder (e.g. another disk)
  GPN_CHAIN         use your own chain spec instead of the auto-fetched one
  GPN_SIGNER        mnemonic/seed (skips the keystore)
  GPN_SIGNER_FILE   path to a keystore JSON
  GPN_PUBLIC_ADDR   /ip4/203.0.113.10/tcp/30333 when the router rewrites ports
  --no-upnp          never contact the router (forward TCP 30333 manually)
  --no-require-backbone  keep running when the node cannot be dialled

Servers with no terminal (systemd, containers, CI): the wizard never hangs —
it exits with "run `gpn_node --setup` once interactively". Do that once, then
the service starts with no prompts.

Environment (ollama)
--------------------
  GPN_OLLAMA                 ollama URL        default http://127.0.0.1:11434
  GPN_REQUIRE_OLLAMA         hard gate         default 1 (ON; 0 = degraded)
  GPN_OLLAMA_SETUP           auto-bootstrap    default 1 (ON)
  GPN_BASELINE_MODELS        models to pull    default llama3.2:1b

Special cases only: GPN_REQUIRE_OLLAMA=0 lets the node start miner-only with a
loud warning. That is NOT a supported production configuration.

Docs: docs/run-a-node.md
