# Design — referral codes derived from the node's wallet (owner decision 2026-11-20)

**Supersedes:** the "any 4–32 char string" behaviour in `gpn-agent/src/referral.rs`.
**Problem with what exists:** a code is whatever the operator types. So it is not tied to a node or
a wallet, two people can use the same string, nobody can verify who a code belongs to, and the
operator has no way to *find* their own code — it exists only if they invented one.

---

## 1. The rule

> **A referral code is a short, checkable fingerprint of the node's wallet address. Every node has
> exactly one, it is printed wherever the operator can see it, and it can be verified offline
> without a registry.**

Properties this buys:

| Property | How |
|---|---|
| unique | derived from the 32-byte account id — collisions are ~2⁻⁴⁸ at 12 characters |
| stable | deterministic in the address; reinstalling the same wallet reproduces it |
| verifiable | anyone can recompute it from the ss58 address; no issuance list needed |
| non-squattable | you cannot claim someone else's code without their wallet |
| self-identifying | a node presenting `ref=GPN…` proves the code belongs to a wallet, and the AP checks it |

## 2. Derivation

```
addr   = ss58 address of the node's reward wallet (the same one block rewards go to)
digest = blake2b-256("gpn-referral-v1" || 32-byte account id)
code   = "GPN" + base32(digest[0..8]) with I/O/0/1 removed  →  11 characters
```

- **Uppercase base32, no padding, ambiguous characters removed** so it can be read aloud and typed
  from a screenshot.
- The `gpn-referral-v1` domain string means a future scheme can coexist instead of invalidating
  every code in the wild.
- **Display form** adds a checksum character (Crockford-style) so a typo is caught locally:
  `GPN-7K2X-9QMD-4T`.
- `--ref` accepts either form. Validation recomputes the digest from the *presented* payment
  address and compares — the code is checked, not merely stored.

## 3. Where the operator sees it (the part that was missing)

1. **Dashboard** (`:8099`): a "Your referral code" card — the code, the shareable link
   `https://gpncoin.ai/join?ref=<code>`, and a **Copy** button. Shown only when a wallet is
   configured; if not, it says so and links to `gpn_node --setup`.
2. **Startup summary** printed by `gpn_node`: one line, `referral code GPN-7K2X-9QMD-4T (share:
   https://gpncoin.ai/join?ref=GPN7K2X9QMD4T)`.
3. **`gpn_node --referral`** (non-starting subcommand): prints the code and link only — so an
   operator can retrieve it without running a node.
4. **Info page** on gpncoin.ai (`ai.html` / a new `referrals.html`): what a code is, how it is
   derived from your wallet, how to share it, and the fact that it records *attribution only* —
   no rewards are live until mainnet.
5. **`GET /api/referral`** on the dashboard's API: `{code, link, address}` for tooling.

## 3b. The link must not promise a download (owner, 2026-11-20)

`/download?ref=…` reads as *"clicking this downloads software"* — the opposite of what a referral
link should suggest to someone deciding whether to trust you.

- **Canonical share link: `https://gpncoin.ai/join?ref=<code>`**
- **Short alias: `https://gpncoin.ai/r/<code>`** — for spoken or printed URLs; 301 to `/join`.
- `/join` is a **landing page**, not a download page: what GPN is in three sentences, what the
  referrer's code means, the four install commands *with the code already baked in*, and the
  buttons that lead to `/download` only if the visitor chooses.
- The code is shown on `/join` as text (`your code: GPN-7K2X-9QMD-4T`) so a screenshot of the page
  still carries the attribution if the visitor types the commands by hand later.
- `/download?ref=…` keeps working (it already bakes the flag into the commands) but is no longer the
  link anyone is asked to share.

Acceptance additions: `/join?ref=CODE` renders the landing page with the code baked into all four
commands; `/r/CODE` redirects to it preserving the code; `/join?ref=` with a malformed code shows
the landing page with a clear "this code is not valid" note rather than silently ignoring it.

## 4. What the AP does with a presented code

- Validates it against the provider's own wallet address (below). A code that does not match the
  presenting node is recorded as `invalid_for_address` — **not** silently dropped, because that is
  the signature of someone else's code being replayed.
- Self-referral (code derived from the node's own address) is rejected as today.
- Registry stays keyed by ss58 address, and now carries `code`, `derived_from`, and `first_seen`.

## 4b. Visibility is operator-only (owner decision, 2026-11-20)

Attribution is **not public**. There is no leaderboard, no page listing who referred whom, and no
endpoint that answers "who referred this address" to a stranger.

| Who | Sees |
|---|---|
| an operator | **their own count only** — how many nodes were referred with *their* code, and when each was first seen. No other operator's data, and no identities of the nodes they referred |
| the network admin (Jeremy, on the AP host) | the whole registry, via local tooling only — never an HTTP endpoint |
| anyone else | nothing. `GET /api/referrals` without proof of the code returns `401` |

Concrete shape:

```
GET /api/referrals?code=<my code>&sig=<signature over code by my wallet>
→ { "code": "GPN-7K2X-9QMD-4T", "referred": 7, "first_seen": …, "recent": [ {first_seen} … ] }
```

- Proof is a **wallet signature**, not a password: it must verify against the address the code
  derives from (row 223), so only the wallet owner can read their own referrals.
- The response **omits addresses** of referees by default; a count and timestamps are enough to know
  your referrals landed, and not enough to build a map of other people's machines.
- The admin view is `gpn-referrals --all` run on the AP host (reads
  `~/.gpn/referral-registry.json` directly). No route, no token to leak.
- Rationale to record alongside: referral counts are useful to the operator who earned them and to
  the admin who pays them out. Published counts would turn a wallet address into a marketing asset
  and invite sybil farming of a number anyone can read.

## 5. Migration (existing codes are not lost)

The registry may already contain arbitrary-string codes from testnet installs. Rule:

- **Keep them.** A stored code is never rewritten (write-once is already enforced).
- New installs get derived codes. `~/.gpn/referral` still stores whatever was given, so an old
  node keeps its old string.
- The dashboard shows a derived code **only** when the node has no stored code; if one exists, it
  shows that one and labels it `legacy`.

## 6. Acceptance

1. Two nodes with different wallets get different codes; the same wallet on a reinstall reproduces
   the same code — proven by printing both.
2. A code presented with a mismatched address is recorded `invalid_for_address` and does not credit
   the referrer.
3. The dashboard shows the code and link for a configured wallet, and the Copy button puts exactly
   the link on the clipboard (checked in a browser, pasted into a shell).
4. `gpn_node --referral` prints the code without starting a node (exit 0, no RPC listener).
5. A typo in the checksum character is rejected with a message that names the expected character.
6. gpncoin.ai explains the scheme and shows a real example code from a live node.
7. Row 202-style end-to-end: an install with `--ref <someone else's code>` lands an entry in
   `referral-registry.json` whose `derived_from` equals that wallet's address.

## 7b. Why this exists: correct airdrop accounting at mainnet (owner, 2026-11-20)

The referral record is **the input to the mainnet airdrop**, not a marketing feature. The genesis
plan gives a slice of the treasury to testnet participants and a share to whoever referred them;
once the network is live, that history cannot be reconstructed. So the record has to be:

- **complete** — every node that ever presented a code is recorded, even if it went offline later
  (the entry is written on first sighting and never removed);
- **unique** — keyed by ss58 address, so the same operator reinstalling five times counts once;
- **derivable** — a code recomputes from the referrer's wallet (row 223), so a payout list can be
  checked without trusting our database;
- **reproducible** — the export is a plain JSON file with a schema version and a sha256, produced by
  a documented command, so the allocation can be recomputed and audited years later;
- **bounded** — self-referral is rejected, and a code that does not match the presenting address is
  recorded as `invalid_for_address` and never credited (that entry is evidence, not a payout).

Export shape (admin-only, on the AP host):

```
gpn-referrals --export airdrop-input.json
{ "schema": "gpn-airdrop-input-v1", "generated": "...", "registry_sha256": "...",
  "participants": [ {"address": "5…", "first_seen": 1700000000, "last_seen": …,
                     "referred_by": "GPN-7K2X-9QMD-4T", "referred_by_address": "5…",
                     "derivation_verified": true} ],
  "referrers": [ {"address": "5…", "code": "…", "referred": 7, "verified_referrals": 7} ] }
```

`derivation_verified` is the important field: it means the code recomputed from
`referred_by_address`, so the airdrop maths can distinguish *proven* attribution from a legacy
typed string that cannot be checked.

**Amounts are still undecided** and no promise is made here; this section defines the *record*, so
that when the decision is made the data is complete, deduplicated and auditable.
