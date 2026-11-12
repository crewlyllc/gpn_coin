# Testing GPN, including across networks

Two test surfaces exist. Use both; a change that only passes one of them is not tested.

## 1. Single-host tests (fast, no network truth)

- `cargo test --release -p gpn-node` / `-p gpn-agent` — unit and integration.
- `scripts/argv_contract.rs` (in `gpn-agent/tests/`) — the supervisor may only pass flags
  the chain binary actually implements. It caught `--network-backend` being added to a
  stale allow-list; before that it caught invented flags twice.
- `tests/launcher_bootnodes.rs` — `install.ps1` and `chainspec::launcher_bootnodes()` must
  agree. It caught the LAN seed being removed from one list but not the other.
- `worker/worker.test.mjs`, `website/tests/*.test.js` — publishing and display rules.
- **A probe node on a spare port is the cheapest integration test there is**
  (`--base-path /tmp/x --port 3036N --rpc-port 99NN`), and it must never bind 30333.

## 2. Cross-network tests (slow, the only source of network truth)

The Windows self-hosted runner (`DESKTOP-USBUADL`) is **box 1** — a machine on a
different LAN from this host. `./.github/workflows/windows-node-ops.yml` runs a supplied
PowerShell snippet there and returns its output in the job log:

```
POST /repos/jeremyzguo/gpn_coin/actions/workflows/windows-node-ops.yml/dispatches
{"ref":"main","inputs":{"script":"<powershell>","allow_service_control":"false"}}
```

Use it for anything that is genuinely two-sided:

- dial `142.129.54.88:30333` from box 1 and report what it reaches
- install a downloaded release artifact there, verify sha256, restart, report peer count
- confirm the peer id a node presents, versus the one the registry publishes for it
- confirm whether a node is dialable at all, and from which side

**Why this matters:** on 2026-11-11/12 every conclusion drawn from one side of the handshake
was wrong at least twice (self-hairpin mistaken for a remote peer; a backend mismatch
declared from 28 bytes that both a working and a failing peer sent). A claim about peering
must be observed from **both** ends, or it is a hypothesis.

Guardrail: the workflow refuses `Stop-Process`/`taskkill`/service control unless the caller
sets `allow_service_control: true`, and it never runs on push.

## 3. Rules that have earned their place

- **One variable at a time.** Changing the seed list and the backend together produced two
  days of misattribution.
- **Never test on the production port.** Probes bind 3036x; 30333 belongs to the live node.
- **Prefer observed behaviour to declared config**: run history beats a `runs-on` line, a
  built artifact beats a version string, a socket table beats a believed port.
- **Publish nothing you have not verified**: an address is only publishable by the peer id
  a third party reaches when it dials it (`design-lan-topology.md`).
- **A green suite is not an end-to-end pass.** The 2026-11-12 startup crash passed every
  unit test and killed every fresh node, because no test started a node with the live
  published seed list.
