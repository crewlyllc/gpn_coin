GPN NODE — FIRST RUN
====================

Two steps, one question.

  1) Download + extract this package
  2) Run the installer for your OS:
        Windows:     install.ps1        (or double-click install.cmd)
        Linux:       ./install.sh

That is it. The installer copies the binaries, creates the data folder and the
wallet, makes sure ollama is installed and serving, and registers an autostart
entry so THE NODE (AND OLLAMA) COME BACK BY THEMSELVES AFTER A REBOOT.

  Windows:  a scheduled task "GPN-Node" (/SC ONLOGON /RL HIGHEST, one instance
            only) runs C:\gpn\start-node.cmd, which logs to C:\gpn\node.log.
            Ollama gets its own autostart too (task "GPN-Ollama", or the entry
            Ollama's own installer created — never a duplicate).
  Linux:    systemd user units gpn-node.service + gpn-ollama.service, enabled,
            plus `loginctl enable-linger` so they start at BOOT and not only
            after a login. Also gpn-update.timer, which verifies the signed
            release manifest every 6 h and installs newer binaries in place.

Dashboard after start:  http://127.0.0.1:8099  (loopback only)
The installer prints your payout address when it finishes.

A GPN node is ALWAYS a miner AND an AI provider. Ollama is a REQUIRED
component: `gpn_node` will REFUSE TO START if ollama cannot be reached
(exit code 69, `node=down(no ollama)`).

So the installer INSTALLS ollama for you when it is missing (winget, falling
back to the official OllamaSetup.exe), starts it, and pulls the baseline
models. That can take a few minutes on a fresh machine -- it is not hung.
If you want to install ollama yourself, pass -NoOllamaInstall and the
installer will stop and tell you instead.

Files in this package
---------------------
  gpn-node(.exe)                 the chain node/miner
  gpn_*(.exe)                    the agent binaries (gpn_node is the supervisor)
  install.ps1                    Windows installer (install + autostart)
  install.cmd                    Windows double-click wrapper for install.ps1
  uninstall.ps1                  Windows: remove the autostart in one command
  install.sh                     Linux installer (install + autostart)
  uninstall.sh                   Linux: --remove-autostart / --status
  scripts/gpn-autostart.sh       Linux: systemd user units + enable-linger
  scripts/install-ollama.ps1     ollama installer (Windows)
  scripts/install-ollama.sh      ollama installer (Linux/macOS)
  scripts/install-ollama-amd.sh  AMD/ROCm variant (Linux)
  gpn-node.example.toml          example config (optional — the wizard writes one)
  README-FIRST-RUN.txt           this file

If gpn_node / gpn-node binaries are missing, download the full release zip
from the GitHub Releases page of the repo.

1) Install
----------
  Windows:   powershell -ExecutionPolicy Bypass -File .\install.ps1
             (or just double-click install.cmd)

  Linux:     ./install.sh
             ./install.sh --no-autostart     # do NOT set up autostart
             ./install.sh --no-fast-sync     # skip the snapshot, sync from peers

If Windows refuses to run the script ("running scripts is disabled"), the
install.cmd wrapper already passes -ExecutionPolicy Bypass. You can also
unblock a downloaded file: right-click install.ps1 -> Properties -> Unblock.

You will be asked ONE question:

  Generate a new wallet, or import an existing one?

  * Generate — a 12-word phrase is printed ONCE. Write it down: it controls
    your earnings and cannot be recovered. It is stored in
    <base>/keystore.json with owner-only permissions (0600 on Unix, a
    user-only ACL on Windows).
  * Import   — paste a 12/24-word mnemonic or a 0x-prefixed 32-byte seed.

The resulting SS58 address is printed and used as the payout account.
Re-running the installer keeps the existing wallet (it is never replaced).

2) Nothing else
---------------
The node is already running and will restart by itself after a reboot:

  Windows:   schtasks /End /TN GPN-Node      # stop
             schtasks /Run /TN GPN-Node      # start again now
             powershell -ExecutionPolicy Bypass -File .\install.ps1 -Uninstall

  Linux:     systemctl --user status gpn-node.service
             journalctl --user -u gpn-node.service -f
             ./uninstall.sh --remove-autostart

Uninstalling the AUTOSTART never deletes your data or your wallet: the chain
DB, keystore.json and downloaded models are left exactly where they are, and a
printed note tells you where.

Running the installer twice is safe: tasks are recreated by name and unit files
rewritten, so you never end up with two nodes fighting over one data folder.

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
  autostart      Windows scheduled tasks / systemd user units + linger

Manual run (no autostart, e.g. a one-off test)
---------------------------------------------
  Windows:   C:\gpn\start-node.cmd
  Linux:     gpn_node

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
