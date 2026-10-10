# GPN node-free wallet (`gpn_wallet`)

A lightweight **sr25519** wallet so an AI user / trader can create, hold and move GPN
**without running a full GPN node**. Standard Substrate key derivation — the address and
mnemonic are interoperable with any Substrate wallet (Polkadot.js, `subkey`, …). There is no
GPN-specific crypto.

The chain stays the coordination + escrow + settlement layer; the wallet just talks to a plain
RPC endpoint (a public access point or any node) for **balance** and **send**. Everything else
(`keygen`, `address`, `sign`) is **fully offline**.

## Mental model

| Command   | Needs a node? | What it does |
|-----------|---------------|--------------|
| `keygen`  | no            | fresh sr25519 mnemonic → ss58 address; optional 0600 JSON keystore |
| `address` | no            | derive the ss58 address from a keystore / mnemonic / URI |
| `sign`    | no            | sign a message/payload (e.g. authorize a job payment) → signature |
| `balance` | yes (RPC)     | read the free balance of an address |
| `send`    | yes (RPC)     | sign + submit `Balances.transfer_allow_death` |

## Usage

```bash
gpn_wallet keygen  [--out <file>] [--password <pw>]
gpn_wallet address <keystore.json|mnemonic|//URI> [--password <pw>]
gpn_wallet balance <endpoint> <ss58|0xhex|keystore.json|//URI> [--password <pw>]
gpn_wallet send    <endpoint> <keystore.json|mnemonic|//URI> <dest> <amount_gpn> [--password <pw>]
gpn_wallet sign    <keystore.json|mnemonic|//URI> <message> [--hex] [--password <pw>]
```

Environment: `ENDPOINT` (default `ws://127.0.0.1:9944`), `GPN_WALLET_PASSWORD`.

### Keystore format

`keygen --out` writes a JSON document (mode `0600`):

```json
{
  "address": "5…",
  "crypto": "sr25519",
  "mnemonic": "twelve word recovery phrase …"
}
```

The mnemonic is the private key — back it up offline, never share it. A wallet source can be a
keystore path, a raw mnemonic, or a dev/derivation URI (`//Alice`, `phrase//hard//soft`). A
password-protected source uses `--password` (or `GPN_WALLET_PASSWORD`); a password changes the
derived address, so it must be supplied on every use.

## Examples

```bash
# offline: make a wallet
gpn_wallet keygen --out wallet.json

# offline: address + signature
gpn_wallet address wallet.json
gpn_wallet sign wallet.json "pay:job=42;amount=1.5"

# online: balance + send (1 GPN)
gpn_wallet balance ws://127.0.0.1:9944 wallet.json
gpn_wallet send    ws://127.0.0.1:9944 wallet.json 5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY 1.0
```

## Security notes

- **Never log or transmit the mnemonic.** The keystore is written `0600`; keep it that way.
- `send` signs **locally** — the private key never leaves the machine; only the signed extrinsic
  is submitted to the RPC endpoint.
- An amount **below the runtime existential deposit** is rejected by the chain (the transfer
  fails dispatch). Use an amount ≥ the ED for a first-time recipient.
- Interoperability is by construction: this is standard Substrate sr25519 derivation, verified
  against the well-known dev vectors in the unit tests.

## Related

- `USE-GPN.md` §2.1 — the short how-to.
- `gpn_send` — the older ops-only transfer tool (takes a signer URI); `gpn_wallet` is the
  user-facing, node-free replacement that also does keygen and offline signing.
