# Design — GPN Groth16 trusted-setup ceremony (`gpn_ceremony`)

**Status:** tool BUILT + TESTED (2026-10-08); transcript-soundness gap fixed
2026-10-09 (§6a — new `verify-chain` recomputes each step; plain `verify` was
self-consistency only and accepted a fabricated chain). The public multi-party
run happens later (once enough independent people can contribute). Embedding the
final CRS is a **separate, gated `set_code`** step (spec 16) — this tool never
touches the chain or genesis.
**Owner:** PM07. **Item:** `GPN-TASKS.md` #17.

## 1. Why

`pallet-shielded-pool::groth16::note_spend_key()` currently uses a
**deterministic testnet CRS** derived from the public seed
`testnet-crs-note-spend-v0`. Its toxic waste (`α,β,γ,δ,τ`) is therefore
derivable by anyone, so a shielded-note spend proof can be forged. Replacing it
with a CRS produced by a **multi-party ceremony** makes the setup trustless:
as long as **at least one** contributor honestly destroys their randomness, the
trapdoor is unknowable.

`gpn-shield` is a from-scratch Groth16 over BLS12-381 (not arkworks); the
ceremony tooling is built directly on that code (`gpn-shield/src/ceremony.rs`,
CLI `gpn-shield/src/bin/gpn_ceremony.rs`, genesis setup in
`gpn-shield/src/groth16.rs::setup_ceremony`).

## 2. Flow

```text
gpn_ceremony init       --circuit note-spend --out phase2-initial.bin --record init.json
gpn_ceremony contribute --in phase2-initial.bin --out crs-1.bin --record c1.json --seeded-from <public>
gpn_ceremony contribute --in crs-1.bin           --out crs-2.bin --record c2.json --seeded-from <public>
gpn_ceremony verify     --crs crs-2.bin --records init.json,c1.json,c2.json     # PASS/FAIL
gpn_ceremony export-vk  --crs crs-2.bin --out vk.bin                            # on-chain artifact
```

* **`init`** builds the phase-2 genesis CRS from the circuit. Its entropy is OS
  entropy by default, or a public `--seeded-from` value. `init` is a trusted
  step; its role is to define `τ` (see §5).
* **`contribute`** reads the current CRS, folds in **fresh** `α',β',γ',δ'`, and
  writes the updated CRS + a **verifiable contribution record**.
* **`verify`** checks the whole transcript (`PASS`/`FAIL`).
* **`export-vk`** emits the verifying key — exactly what would be embedded.

## 3. What a contribution changes (and why it is trustless)

The CRS stores the `βA_k`, `αB_k` and `C_k` components **separately** rather
than as one pre-combined `[(βA_k+αB_k+C_k)/δ]` term. A contributor only needs
their **own** fresh scalars; they never learn the aggregate `α,β,γ,δ`
(indeed nobody does). Each contribution scales:

| Component | × factor |
|---|---|
| `[α]₁` | `α'` |
| `[β]₁`, `[β]₂` | `β'` |
| `[γ]₂` | `γ'` |
| `[δ]₁`, `[δ]₂` | `δ'` |
| `[βA_k/δ]₁` | `β'·δ'⁻¹` |
| `[αB_k/δ]₁` | `α'·δ'⁻¹` |
| `[C_k/δ]₁` | `δ'⁻¹` |
| `[(τ^i)/δ]₁` | `δ'⁻¹` |
| `[βA_i/γ]₁`,`[αB_i/γ]₁`,`[C_i/γ]₁` (public) | `β'·γ'⁻¹`,`α'·γ'⁻¹`,`γ'⁻¹` |

`τ`-only elements (`[A_k(τ)]₁`, `[B_k(τ)]₁`, `[B_k(τ)]₂`) are untouched. The
composition across contributions is exact: the aggregate secrets become
`α∏α'`, `β∏β'`, `γ∏γ'`, `δ∏δ'`. `(α',β',γ',δ') = (1,1,1,1)` is a byte-exact
no-op (a unit-test invariant). This is the standard Groth16 MPC structure; the
only reason it works is the *separability* of the components.

**Security:** knowing `δ` (and `α`/`β`) lets one forge. With ≥1 honest
contributor, `δ` is unknown to everyone ⇒ no forgery.

## 4. Toxic-waste destruction (hard requirement)

`contribute` derives fresh scalars from an **explicit entropy source**:

* default: 32 bytes from `/dev/urandom`;
* `--seeded-from <public value>`: `sha256(value)` — use a value the contributor
  **cannot** control or predict (a future drand round, a future block hash) so
  the contribution is **publicly un-biasable** and auditable.

The scalars are used only to scale group elements and are then **wiped**:
`Zeroizing` for the entropy buffers and a `volatile` overwrite for the
[`FreshSecrets`] scalars (`Drop`). Nothing secret is logged or written to disk.
`init`'s seed is handled the same way (wrapped in `Zeroizing`).

## 5. τ and the powers-of-tau extension

`τ` is fixed by `init` (the genesis). Rotating `τ` across contributors is **not**
done here because, in this from-scratch Groth16 CRS, `[A_k(τ)]₁` etc. are stored
as *evaluations* — to move to `ττ'` a contributor would need the circuit's column
coefficients re-lifted through the raw `[τ^i]₁`/`[τ^i]₂` powers (a group-FFT), and
the `β`-scaled power vectors for the combined terms. That is the classic BGM17
separable form:

* store raw `[τ^i]₁`, `[τ^i]₂` and the separator vectors `[βτ^i]`, `[ατ^i]`,
  `/δ`, `/γ`;
* `contribute` scales those vectors (pure scalar mults) and re-derives the
  per-variable elements with a **group-FFT** over the (public) circuit matrices.

This is specified as the **production extension** and is compatible with the
current file format (add the vectors; `contribute` re-materialises the
variables). For the first public ceremony, either (a) run `init` with strong,
publicly-verifiable entropy and treat `τ` as trusted at genesis, or (b) land the
τ-rotation extension first. Because §3 lets any number of parties re-randomise
`α,β,γ,δ`, the practical trust gap is only `τ`; note that forging needs `δ`,
which *is* rotated, so `≥1` honest contributor already closes the forgery vector.

## 6. What `verify` checks

1. every record is `v1`, with 64-hex digests;
2. the digests form a **contiguous hash chain**
   (`records[i].parent == records[i-1].digest`), anchored for an `init` record at
   the public `genesis_anchor(circuit)`;
3. the **final record's digest equals `sha256(final CRS)`** (canonical
   serialization) — catches any CRS tampering;
4. no replayed digests; and
5. the final CRS is structurally well-formed (dimensions, non-degenerate points)
   and **matches its named circuit** (dimensions recomputed from the circuit).

Canonical CRS bytes: `GPNCRS1\n` + circuit + dims + the six `α/β/γ/δ` points +
length-prefixed compressed G1/G2 vectors. `sha256` over these bytes is the digest.

## 6a. Soundness of the transcript — `verify` vs `verify-chain` (added 2026-10-09)

**Gap found in the initial tool:** `verify` (above) checks only that the record
digests form a *self-consistent hash chain* ending at `sha256(final CRS)`. A
malicious coordinator can therefore **fabricate** a genesis-anchored digest chain
whose last digest is `sha256(evil_crs)` for a CRS whose trapdoor it knows, and
`verify` returns `PASS` — defeating the exact attack the ceremony exists to stop.
(Demonstrated: an OS-entropy contribution whose record is relabelled with a
public `seeded_from` passes `verify` but is not reproducible.)

**Fix — `verify_chain` — `gpn_ceremony verify-chain --crs crs0.bin,crs1.bin,… --records r0.json,r1.json,…`:**
in addition to every `verify` check, it **recomputes** each `contribute` step from
the prior published CRS and the record's public `seeded_from` beacon
(`sha256(beacon)` → fresh `α',β',γ',δ'`) and requires the output CRS to match
byte-for-byte (`ceremony::verify_contribution_step` / `verify_chain`). This is
sound and binding: the final CRS is provably the deterministic function of the
published genesis CRS and the published public beacons.

**Consequences for the ceremony policy:**
* Contributors MUST use `--seeded-from <public un-biasable value>` (drand round /
future block hash) for a publicly verifiable ceremony. An **OS-entropy** step
(`seeded_from == "os-entropy"`) is *not* reproducible and is **rejected** by
`verify_chain` (it would need a pairing-based contribution proof — the BGM17
construction — which is out of scope for this tool).
* `verify_chain` does **not** recompute the `init` genesis step (its seed may be
private); `init` remains the **trusted anchor** (§2/§5). With all contributions
publicly seeded, the final CRS is trustless *conditioned on `init` being honest*.
* Removing that residual `init`/`τ` trust requires either (a) the separable
powers-of-tau τ-rotation extension (§5) or (b) secret contributions plus
pairing-based contribution proofs — both are follow-on work.

## 7. "Run publicly later" plan

1. Publish the tool (this) + the circuit + the `init` transcript & CRS digest.
2. Open a contribution window: each contributor runs `contribute` with
   `--seeded-from <the next drand round>` (un-biasable + auditable), publishes
   their `contribution.json` + the resulting CRS, and destroys their scalars.
3. Anyone runs **`verify-chain`** over the published chain of CRS files + records
   (recomputes every step; a bare `verify` is self-consistency only) → `PASS`.
4. `export-vk` → the final VK, embedded via **gated `set_code`** (spec 16),
   replacing the deterministic testnet CRS. Keep the full transcript for audit.

## 8. Tests

`gpn-shield` (debug **96** / release **95 + 5 ignored**, all green; +9 ceremony):
`genesis_is_deterministic_and_serializes`,
`genesis_recombines_to_a_valid_proving_key` (the split CRS reconstructs the real
key byte-for-byte), `identity_contribution_is_a_noop`,
`contribution_changes_crs_and_produces_record`,
**`proof_roundtrip_against_contributed_vk`** (init → 2 contributions → prove →
verify against the exported VK), **`verify_accepts_honest_chain_and_rejects_tampering`**
(honest chain passes; tampered CRS / broken chain / wrong digest / malformed
record / empty transcript all FAIL), `verify_rejects_wrong_circuit_dimensions`,
`vk_serialization_roundtrip`, `contribution_entropy_is_deterministic_when_seeded`.
`pallet-shielded-pool` stays **32/32** (debug + release). Canonical genesis WASM
untouched; no `set_code`.

**Added 2026-10-09 (+4, `gpn-shield` lib now 100/100 debug):**
`verify_chain_accepts_honest_publicly_seeded_chain`,
**`verify_chain_rejects_substituted_final_crs_that_plain_verify_accepts`** (the
substitution attack: plain `verify` PASSes on a fabricated transcript that
`verify_chain` rejects), `verify_chain_rejects_os_entropy_step`,
`verify_contribution_step_checks_parent_and_digest`.

Verified live (release): `init` (note-spend, ~24 s) → `contribute` (~5 min; the
large `τ`-power vector dominates) → `verify` **PASS** → `export-vk` (7
`gamma_abc` entries). CRS ≈ 40 MB.
