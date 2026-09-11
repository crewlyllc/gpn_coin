# Security Policy — GPN Network

**Project:** GPN — GPU-Native Crypto & AI Compute Network
**Components:** `gpn-node` (Substrate node + runtime + custom pallets), `gpn-agent`
(GPU miner / compute agent and operator CLIs), `gpn-shield` (shielded-pool prover/verifier),
and the ops tooling (explorer, faucet, treasury CLI).

We take security reports seriously and appreciate responsible disclosure. This policy follows
the spirit of `SECURITY.md` / coordinated disclosure used by mature open-source projects.

---

## 1. Supported versions

| Version | Supported |
|---|---|
| `main` / latest tagged release | ✅ |
| Public testnet builds | ✅ |
| Older tags / unmaintained forks | ❌ |

Only the latest release on `main` is supported. Always test against the tip before reporting.

---

## 2. How to report a vulnerability

**Preferred channel (private):** open a **GitHub Security Advisory** (private vulnerability
reporting) on the project repository. This keeps the discussion private until a fix ships.

> A dedicated security contact email will be published here before mainnet launch. Until then,
> GitHub private advisories are the only sanctioned channel. **Do not** open a public issue,
> Discord message, or PR that discloses a vulnerability.

Please include:
- a clear description and the affected component/version (commit hash),
- impact assessment (funds at risk? chain halt? data leak? privilege escalation?),
- reproduction steps or a PoC (minimal and self-contained where possible),
- any suggested fix, and
- how you wish to be credited.

**Encrypt if sensitive.** If you need to send secrets (keys, exploit payloads), say so in the
advisory and we will arrange an encrypted channel.

---

## 3. Response timeline (best effort)

| Stage | Target |
|---|---|
| Acknowledge receipt | within **48 h** |
| Initial triage + severity | within **5 business days** |
| Fix or mitigation for Critical/High | as fast as a governed release allows (see §6) |
| Public disclosure | coordinated, default **90 days** after a fix is available |

We will keep you updated and will not take legal action against good-faith research that follows
this policy (see §7, safe harbor).

---

## 4. Scope

**In scope:**
- Consensus / mining: Ethash PoW seal verification, difficulty retargeting, block-reward
  minting, supply-cap enforcement.
- Runtime / pallets: `gpn-token`, `gpu-pow`, `gpu-attestation`, `compute-marketplace`,
  `profitability-oracle`, `gpn-treasury`, `gpn-liveness`, `shielded-pool`.
- Cryptography: `gpn-shield` prover/verifier (soundness, zero-knowledge, proof malleability,
  constant-time handling of secret scalars).
- Node/network: p2p, RPC exposure, sync safety.
- Agent/tooling: GPU miner, compute executor, faucet, explorer, treasury CLI (input handling,
  panic/DoS, key handling).
- Economic/supply logic: fee split, escrow, vesting, governance flows.

**Out of scope:**
- Findings requiring a compromised host / physical access / stolen keys (report anyway — see §6).
- Social engineering, spam, brute-force against login.
- Third-party dependency issues already tracked upstream (please link the upstream advisory).
- Testnet-only issues with **no** mainnet impact are lower priority (still welcome).
- Missing hardening headers on static marketing pages.

---

## 5. Critical invariants (highest rewards)

1. **Supply cap holds:** `treasury_allocation (200,000,000 GPN) + all minted ≤ 1,000,000,000 GPN`.
2. **Treasury is keyless:** 200M GPN moves **only** through the 13-of-25
   `pallet-gpn-treasury` flow, bounded by unlocked tranches.
3. **No mainnet privileged key:** the mainnet sudo key is an inert all-zeros placeholder; a way
   to make it usable without governance is a Critical finding.
4. **Block reward is paid exactly once per block** — no double-pay between the automatic
   `on_initialize` path and a manual `submit_solution` claim.
5. **Proof soundness:** no way to produce an accepted shielded-pool proof without the witness, or
   a *different* proof that verifies under the value-balance VK.
6. **Deterministic, panic-free** runtime paths under adversarial input (bad extrinsics, malformed
   RPC args, weird block numbers/balances).

**Do not** change, and do not ask us to change, the **proof bytes** or the **value-balance
verification key** — these are on-chain compatibility surfaces.

---

## 6. Coordinated disclosure & emergency releases

GPN is **PoW with no validator authority set**, and mainnet's sudo is inert. There is **no
break-glass superuser**: fixes ship through a **governed runtime upgrade** (root/governance) or
the 13-of-25 treasury flow, which is intentionally slow. This shapes timelines:

- We may deploy a **mitigation** (config, node-side, or client-side) before the governed fix
  lands; we will tell you what we did.
- **Testnet** may be patched and re-tested quickly (ops sudo available there) before mainnet.
- Please give us the chance to roll a governed fix + coordinated comms before public disclosure.

---

## 7. Safe harbor

We will not pursue or support legal action against researchers who:
- act in **good faith** and in **scope**,
- avoid privacy violations, data destruction, and service disruption beyond what's needed to
  demonstrate the issue,
- do not exfiltrate or retain user/operator data or funds,
- give us **reasonable time** to fix before disclosure, and
- follow this policy.

---

## 8. Recognition

With your permission we will credit you in the public post-incident write-up and/or a
Hall of Thanks. Testnet/exploit-range reports are welcome and valued.
