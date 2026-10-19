GPN NODE — FIRST RUN
====================

A GPN node is ALWAYS a miner AND an AI provider. Ollama is a REQUIRED
component: `gpn_node` will REFUSE TO START if ollama cannot be reached
(exit code 69, `node=down(no ollama)`).

Files in this package
---------------------
  gpn-node.exe            the chain node/miner
  gpn_*.exe               the agent binaries (gpn_node is the one supervisor)
  scripts/install-ollama.ps1   ollama installer (Windows)
  scripts/install-ollama.sh    ollama installer (Linux/macOS)
  scripts/install-ollama-amd.sh  AMD/ROCm variant (Linux)
  gpn-node.example.toml   example config — copy to %USERPROFILE%\.gpn\node.toml
  README-FIRST-RUN.txt    this file

If gpn_node / gpn-agent binaries are missing, download the full release zip
from the GitHub Releases page of the repo.

1) Install + start ollama + pull the baseline model
---------------------------------------------------
Easiest: run the one-shot bootstrap — it does all three steps for you.

  Windows:   gpn_node.exe --setup
  Linux:     ./gpn_node --setup

Manual equivalent:

  Windows:
    powershell -ExecutionPolicy Bypass -File scripts\install-ollama.ps1
    (or) winget install Ollama.Ollama
    ollama serve
    ollama pull llama3.2:1b

  Linux/macOS:
    ./scripts/install-ollama.sh
    (or) curl -fsSL https://ollama.com/install.sh | sh
    ollama serve &
    ollama pull llama3.2:1b

Verify:  curl http://127.0.0.1:11434/api/tags      (must list a model)

2) Set your public address
--------------------------
Every node must be inbound-dialable (public IP + forwarded port 30333):

  set GPN_PUBLIC_ADDR=/ip4/203.0.113.10/tcp/30333        (Windows cmd)
  export GPN_PUBLIC_ADDR=/ip4/203.0.113.10/tcp/30333     (Linux/macOS)

Also set a payout account (GPN_ACCOUNT), a signer (GPN_SIGNER) and a network
key (GPN_NODE_KEY) — see gpn-node.example.toml and docs/run-a-node.md.

3) Start the node
-----------------
  Windows:   gpn_node.exe
  Linux:     ./gpn_node

One command starts the miner + heartbeat + every AI provider, and supervises
them as one node with one log stream and one status line.

Environment (ollama)
--------------------
  GPN_OLLAMA                 ollama URL        default http://127.0.0.1:11434
  GPN_REQUIRE_OLLAMA         hard gate         default 1 (ON; 0 = degraded)
  GPN_OLLAMA_SETUP           auto-bootstrap    default 1 (ON)
  GPN_BASELINE_MODELS        models to pull    default llama3.2:1b

Special cases only: GPN_REQUIRE_OLLAMA=0 lets the node start miner-only with a
loud warning. That is NOT a supported production configuration.

Docs: docs/run-a-node.md
