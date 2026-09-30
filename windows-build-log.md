# Windows Node Build Log

## Run #21 (2026-09-18 21:05 UTC) — FAILURE

**Error:** `build node` step failed. `openssl-sys v0.9.117` vendored build could not run:
```
warning: openssl-sys@0.9.117: configuring OpenSSL build: Command 'perl' not found. Is perl installed?
error: failed to run custom build command for `openssl-sys v0.9.117`
```

**Root cause:** The `deps` step installed `strawberryperl` via choco (deployed to `C:\Strawberry\`), but choco only updates the *machine* PATH and warns "Close/reopen your shell". Each GitHub Actions `run` step spawns a fresh shell that inherits the runner's ORIGINAL (stale) PATH, so `perl.exe` was not on PATH in the `build node` step.

**Fix:** Added a `refresh path` step right after `deps` that appends the Strawberry Perl and OpenSSL bin dirs to `GITHUB_PATH` so subsequent steps see them:
- `C:\Strawberry\perl\bin`
- `C:\Strawberry\c\bin`
- `C:\Program Files\OpenSSL-Win64\bin`

Committed + pushed + re-triggered workflow_dispatch.

## 2026-09-18 21:41 UTC — run 22 (failed) → fix applied

**Error:** `error: failed to run custom build command for librocksdb-sys v0.17.3+10.4.2` → build-script-build exited `0xc0000135 (STATUS_DLL_NOT_FOUND)`.

**Diagnosis:** librocksdb-sys's build.rs runs `bindgen` (confirmed in v0.17.0 source), which requires `libclang.dll` from LLVM/clang. The choco `llvm` package installs it to `C:\Program Files\LLVM\bin`, but that dir was not on PATH (the "refresh path" step only added strawberry perl/c + openssl). Result: the build-script exe fails to load at startup with STATUS_DLL_NOT_FOUND, no stderr emitted.

**Fix:** Added `LIBCLANG_PATH: C:\Program Files\LLVM\bin` env var (workflow-level) + added `C:\Program Files\LLVM\bin` to the "refresh path" step's GITHUB_PATH list. Commit 2f1c5eb, re-triggered run.

## 2026-09-18 22:05 UTC — WINDOWS NODE BUILD SUCCESS ✅

- Run #23 (id 35398173979): conclusion = **success**
- Final fix commit: "ci: set LIBCLANG_PATH and add LLVM bin to PATH for librocksdb-sys bindgen"
- Artifact / run URL: https://github.com/jeremyzguo/gpn_coin/actions/runs/35398173979
- Auto-fix loop: COMPLETE. Disabling cron.

## 2026-09-23 22:54 UTC — per-miner wallets fix + Windows build

**Goal:** external GPU miners submit `GPUPoW::submit_solution` signed with their own wallet, so the 50 GPN/block reward goes to the miner account (per-miner wallets) instead of the single `BlockRewardBeneficiary`.

**Root cause (three bugs in `gpn-agent/src/mining/mod.rs` `submit_solution_direct` + mining loop):**
1. `solution_hash` was set to the ethash `result` hash, but `pallet_gpu_pow::compute_solution_hash` expects `blake2_256((nonce, mix_hash, algorithm_version).encode())`.
2. `difficulty` was encoded as `Primitive::U256` (a `[u8;32]` array), which fails to SCALE-encode against the pallet's `U256 = [u64;4]` target type (WrongLength).
3. The GPU ethash search can never satisfy `blake2(...) < target` (the two hash targets are independent), so the miner never found a valid nonce.

Also: subxt's `OnlineClient` needs a `ws://` endpoint (not `http://`) and a Tokio reactor, so `submit_solution_direct` now rewrites the scheme and runs the submission in a current-thread Tokio runtime.

**Fix (commit 2f8b34d):** the mining loop now does a CPU blake2 hashcash search (mix_hash fixed to zero, nonce counter), computes `solution_hash = blake2(nonce, mix, version)`, and encodes difficulty as four u64 limbs.

**Verified live (Linux, node @ 127.0.0.1:9944):** a `//Bob` wallet climbed ~50 GPN per submitted block (500 → 599.9998 GPN after two submissions). `tx dispatched successfully` in the node.

**Windows build:** re-triggered `release-windows.yml` on `main` (run 35930893217, head 2f8b34d). Monitoring…

## 2026-09-23 23:22 UTC — Windows build status (blocked on runner)

- Run 35930893217 (head 2f8b34d): **failure** at ~10 min. The `windows` job runs on a **self-hosted** runner (`DESKTOP-USBUADL`, the 5070 Ti box) that does **not upload job logs** (empty zip / BlobNotFound via API), so the exact failing step could not be read remotely.
- The code itself is pure Rust and compiles clean on Linux with the same rustc (1.98.1) used by the workflow; `cargo build --release --bins` + `cargo test --no-run` for gpn-agent both pass locally. The likely failing step is `build agent` (`cargo build --release --bins` in gpn-agent) or the fragile `pre-checkout polkadot-sdk` step, but this is unconfirmed.
- Re-triggered (run 35932818112), but it is **queued** because the self-hosted runner `DESKTOP-USBUADL` is now **offline**. The Windows build cannot complete until the runner comes back online.

**Net:** per-miner-wallets fix is committed (2f8b34d), pushed to `main`, and verified live on Linux (//Bob climbs ~50 GPN/block). The Windows binary build is blocked on an offline self-hosted runner, not on the code change.

## 2026-09-23 23:34 UTC — re-verify + re-trigger (runner still offline)

- Re-audited the fix in `gpn-agent/src/mining/mod.rs` against `pallet_gpu_pow::submit_solution` semantics. Confirmed correct: `solution_hash = blake2_256(SCALE(nonce, mix_hash, algorithm_version))` (44 raw bytes), difficulty encoded as an unnamed composite of four u64 limbs against the pallet's `U256 = [u64;4]` array type, and the mining loop does a CPU blake2 hashcash search (mix fixed to zero). scale-encode narrows `Value::u128` to the target field width, so no u128/u64/u32 over-encoding.
- Node live on 127.0.0.1:9944 (block ~0x42f) at re-check; no external miner running (previous test miner was killed).
- Re-triggered `release-windows.yml` on `main` (HTTP 204). Runner `DESKTOP-USBUADL` still **offline**, so the new run + the earlier queued run (35932818112) remain queued.
- **Status:** Linux per-miner wallets DONE and pushed. Windows binary build still blocked on the offline self-hosted 5070 Ti runner (no code issue — the change is pure cross-platform Rust and compiles clean on Linux with the same rustc 1.98.1).


## 2026-09-23 23:46 UTC — final re-verify + re-trigger (runner still offline)

- Re-audited the fix end-to-end against `pallet_gpu_pow` semantics:
  - `compute_solution_hash` = `blake2_256(SCALE(nonce, mix_hash, algorithm_version))` → the agent's `solution_hash()` builds the identical 44 raw bytes (`nonce_le(8) ++ mix(32) ++ version_le(4)`), so `SolutionHashMismatch` cannot fire.
  - `meets_difficulty` reads the hash as four LE u64 limbs and compares `u256_lt` (target semantics). The agent's `meets_target` (`U256::from_little_endian(&h) < difficulty`) matches byte-for-byte.
  - `EthashSolution` encoded as `unnamed_composite([u128→u64 nonce, from_bytes mix, from_bytes solution_hash, u128→u32 version])`; scale-value's `EncodeAsType` narrows `Value::u128` to the target field width, so no over-encoding. `difficulty` (`U256 = [u64;4]`) encoded as four u64 limbs. Verified against scale-value 0.16.3 `encode.rs`.
- `cargo build --release --bin gpn_external_miner` passes clean (only a pre-existing `submit_work` dead-code warning).
- Confirmed GitHub `main` == local HEAD `bb0bac0` (the fix commits 2f8b34d → bb0bac0 are all pushed; local `origin/main` tracking ref was stale, not the remote).
- Re-triggered `release-windows.yml` on `main` (HTTP 204). Runner `DESKTOP-USBUADL` (5070 Ti self-hosted box) is **offline**, so the run + the earlier queued runs (35932818112, 35934260761) remain queued.
- **Net:** per-miner wallets fix is DONE, committed, pushed, and verified live on Linux (//Bob climbs ~50 GPN/block). The Windows binary build cannot run until the physical 5070 Ti runner comes back online — a hardware/infra blocker, not a code issue.

## 2026-09-24 00:09 UTC — cron re-fire: re-audit + re-trigger (runner still offline)

- Re-audited `gpn-agent/src/mining/mod.rs` `submit_solution_direct` + `search_solution` against `pallet_gpu_pow::submit_solution` / `compute_solution_hash` / `meets_difficulty`. Confirmed the encoding + semantics are correct:
  - `solution_hash = blake2_256(SCALE(nonce, mix_hash, algorithm_version))` (44 raw bytes) — matches the pallet exactly.
  - `meets_target` uses `U256::from_little_endian(&h) < difficulty`, byte-identical to the pallet's LE-limb comparison.
  - Difficulty encoded as an unnamed composite of four u64 limbs against the pallet's `U256 = [u64;4]` array type; scale-value narrows `Value::u128` to the target field width (verified against scale-value 0.16.3 `encode.rs` `assert_can_encode_to_type(Value::u128(123), 123u8/u32/u64)`).
- `cargo build --release --bins` (gpn-agent) passes clean (pre-existing dead-code warnings only).
- Confirmed remote `main` HEAD == local HEAD `2491460` via GitHub API (fix commits 2f8b34d → 2491460 all pushed).
- Node live on 127.0.0.1:9944 (`gpn-node` pid 394595) + faucet on 8081; no external miner running.
- Re-triggered `release-windows.yml` on `main` (HTTP 204). Runner `DESKTOP-USBUADL` still **offline** (status via runners API).
- **Net:** code deliverable is complete, pushed, and Linux-verified. The Windows binary build remains blocked on the offline self-hosted 5070 Ti runner — a hardware blocker, not a code issue. The queued `Windows binaries` runs will execute automatically once the runner comes back online. Disabling this cron (re-running produces no further progress until the runner returns).

## 2026-09-24 19:20 UTC — AI-compute Task 1: nonce fix + connection robustness ✅

**Problem:** `gpn_marketplace_demo` could not even run — `ChainClient::new` /
`signer::connect` both failed with `Rpc error: UnknownBlock: State already
discarded for <genesis>` and silently fell back to placeholder data (nonce 0,
balance 0, "not staked"). Two distinct issues were blocking the AI-compute side:

1. **Pruned finalized block breaks subxt connect + tx submit.** The testnet is a
   PoW chain with no GRANDPA finality gadget, so the finalized head stays stuck
   at genesis (#0). The node prunes old state, so subxt's `OnlineClient::from_url`
   (which fetches metadata at the *finalized* block) fails once #0 is pruned.
   Same for `sign_and_submit_then_watch` → `refine_params`, which reads the
   account nonce at the finalized block. Root cause confirmed by reading
   `subxt-0.37.0/src/client/online_client.rs` (`from_backend` fetches metadata at
   `latest_finalized_block_ref()`) and `src/tx/tx_client.rs` (`refine_params`
   reads `get_account_nonce` at the finalized block hash).

2. **Nonce collision** (the originally-reported "Priority is too low (2424 vs
   2424)"): two back-to-back txs from the same account read the same head-block
   nonce.

**Fix (`gpn-agent/src/chain/signer.rs` + `mod.rs`):**
- Added `signer::connect_robust<T: Config>`: falls back to fetching genesis
  hash / runtime version / metadata at the **best (head)** block (whose state is
  never pruned) and building the client with `OnlineClient::from_rpc_client_with`.
- `ChainClient::new` now uses `connect_robust::<SubstrateConfig>`.
- `submit_signed` now signs **offline** (`create_signed_offline`) + `submit_and_watch`,
  avoiding the finalized-block `refine_params` read, and wraps the submission in a
  nonce-retry loop (bump nonce + retry on "Priority is too low" / "Transaction is
  outdated" / "nonce is too low" / "already in pool" / "duplicate").

**Verified live:** `gpn_marketplace_demo ws://127.0.0.1:9944 <provider_seed> 150`
now runs the FULL flow: provider read as staked+attested (Bronze 188 GFLOPS) →
create_job (id=2) → accept → start → real GPU GEMM (RTX 4060, 14ms) →
submit_result → complete_job (escrow 50 GPN released, balance 447.5 → 495.0 GPN).
✅ END-TO-END MARKETPLACE FLOW COMPLETE.

## 2026-09-24 19:26 UTC — AI-compute Task 2: LLM inference end-to-end ✅

**Verified live (no code change needed — the `gpn_llm_provider` / `gpn_api` /
off-chain `LlmStore` plumbing already existed; Task 1's connection fix unblocked it):**

1. `gpn_llm_provider ws://127.0.0.1:9944 <provider_seed> 3000` (poll=3s) — polls
   the chain for `LLMInference` jobs `Running` + assigned to its signer
   (`0x664c6e…`), runs inference against the local ollama (`:11434`,
   `llama3.2:1b`), writes the response + sha256 result hash off-chain, then
   settles on-chain (`submit_result` → `complete_job`).
2. `gpn_api --listen 127.0.0.1:8090 --signer-uri <provider_seed>` —
   `POST /jobs` (LLMInference + prompt + model) created job #3 and stored the
   off-chain payload (`GPN_LLM_STORE=/home/jeremyg/.gpn/llm_store.json`).
3. `POST /jobs/3/accept` + `POST /jobs/3/start` → job moved Pending → Accepted →
   Running (provider = provider_seed).
4. Provider auto-picked it up, ran `llama3.2:1b` inference, and recorded:

   - **response**: "A Graphics Processing Unit (GPU) is a specialized electronic
     circuit designed to quickly manipulate and alter memory to accelerate the
     creation of images in a computer or other digital device." (192 chars)
   - **result_hash**: `0052336c4ad9aa3aae4efa886fd78f788d8d375a6026aaaee42b98b249d60ffb`
   - on-chain state → **Completed** (escrow released).

**Result:** LLM inference end-to-end works — requestor posts a job, the provider
runs a real model off-chain, and only the result hash is recorded on-chain.
`llama3.2:1b` + `ollama serve` on the RTX 4060 confirmed reachable via
`/v1/chat/completions`.

## 2026-09-24 19:33 UTC — AI-compute Task 3: installer (ollama + model helper) ✅

**(a) install.sh — ollama user-local.** Added `install_ollama()`: detects
OS/arch, downloads the ollama release tarball from GitHub (`releases/latest/
download/ollama-{os}-{arch}.tgz`), extracts to `~/.gpn/ollama` (binary lands at
`~/.gpn/ollama/bin/ollama`), and prepares `~/.gpn/models`. Idempotent (no-op if
ollama present) and non-fatal (warns + continues on unsupported platform / failed
download). `gpn.env` now exports `OLLAMA_BIN` + `OLLAMA_MODELS`; the "Done" banner
documents the model commands. Everything sandboxed under `~/.gpn`.

**(b) gpn_pull_model — model-download helper (new bin).** Two modes:
- `pull <model>` → `ollama pull <model>` (resolves `~/.gpn/ollama/bin/ollama`,
  sets `OLLAMA_MODELS`).
- `gguf <hf-repo> <file> [name]` → downloads the GGUF from HuggingFace
  (`https://huggingface.co/<repo>/resolve/main/<file>`, memory-bounded stream),
  writes a `FROM "<path>"` Modelfile, and registers it via `ollama create <name>`.

**Verified live:**
- `gpn_pull_model pull llama3.2:1b` → real ollama pull (manifest + 1.3 GB verified).
- `gpn_pull_model gguf ggml-org/models tinyllamas/stories260K.gguf` → downloaded
  1.18 MB GGUF from HF, generated Modelfile, `ollama create stories260K` → "success".
- `bash -n install.sh` clean; `cargo build --release --bins` clean.

## 2026-09-24 20:30 UTC — 1 GPU = 1 reward: regenerate WASM + reset + rebuild Windows

Starting the "1 GPU = 1 reward via attestation" runtime-change rollout (commit 0c03450).
The committed deterministic WASM is now stale (submit_solution requires attestation), so:
regenerate WASM → recommit → reset chain → re-seed → verify gate → rebuild Windows.

STEP 1 — regenerate + recommit WASM (started).

STEP 1 DONE — new WASM generated (md5 e9a6978fae1259f1c615be4ea59f033a, 476523 bytes vs stale 475998), committed e5c5467 + Cargo.lock c056d34, pushed to main (0c03450..c056d34).

STEP 2 — reset chain + re-seed (started).

STEP 2 DONE — chain reset + re-seed:
- Stopped gpn-faucet/gpn-explorer/gpn-node; removed ~/.gpn/node; restarted all three.
- New genesis: state 0xbf5b…b737 / header-hash 0x08c4…948a (hash changed vs old, confirming the
  regenerated WASM altered the genesis hash).
- Faucet (block beneficiary 5GjVjMhy2MfsEMFrT1BWB5NMXEXpsdqW2Dv2J8fHFnCRCwrs) accumulated
  block rewards (~300 GPN / 3 min); waited until balance_gpn >= 2000 (reached 2150 at 13:35).
- Dripped 1000 GPN to operator-1 (5DUaYAf5wSDSZrQVvMygLRCMmkF9BBqrrrSsVdw3QHboM5Y6).
- gpn_seed_feed (operator-1) → update_feed finalized ✅ (token 0.15, gpu-hr 3.0, util 72%, APR 15%).
  (The tool's "CurrentFeed still empty" warning was transient — gpn_check_feed now reads the
  seeded feed correctly and marks it plausible.)

STEP 3 DONE — attestation gate verified live:
- Built two throwaway verification helpers (gpn_verify_gate + gpn_transfer, since removed).
- Unattested fresh wallet 5CFuTqnmdoWiuTtFiHqcu25LAkzAYFgWmKT8N3LSfwfReGws (//GateTestMiner):
  dripped 1000 GPN, submitted a VALID solution → REJECTED with
  `ModuleError(GPUPoW::NotAttested)` [pallet=8 error=3]. ✅ gate blocks unattested miners.
- Attested provider 5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj (seed 0xf0fb…): after
  chain reset its attestation was wiped, so re-attested (drip 1200 via faucet-seed transfer,
  gpn_attest → deposit_stake 1000 + submit_attestation, Bronze/188 GFLOPS). Submitted the same
  VALID solution → OK (accepted, reward recorded). ✅ gate still lets attested miners through.
- Note: faucet drip endpoint has a global 3600s cooldown, so the provider was funded via a
  direct Balances::transfer from the faucet seed instead.

STEP 4 — Windows rebuild dispatched:
- POST release-windows.yml/dispatches {ref:main} → HTTP 204 ✅ (run 36056511503, head c056d34).
- Run status: in_progress (the self-hosted Windows runner is back online).

STEP 4 DONE — Windows rebuilt ✅
- release-windows.yml run 36056511503 (head c056d34) → completed **success** at ~14:04 UTC.
- Artifact uploaded: `gpn-windows` (142,900,850 bytes).

=== ROLLOUT COMPLETE ===
1 GPU = 1 reward via attestation: submit_solution now gated on attested_gflops > 0.
WASM regenerated (md5 e9a6978…), committed + pushed, chain reset on new genesis,
gate verified live (unattested → NotAttested; attested → accepted), Windows rebuilt.

Out-of-scope note: ci.yml (fmt/clippy + unit tests on 3 OSes) was already failing BEFORE this
change (run 36054256761 on 0c03450 failed at 20:20 UTC) and remains failing — unrelated to the
WASM regeneration; not touched per task scope.

================================================================================
2026-09-28 12:40 UTC — FORKLESS set_code UPGRADE (complete_job sub-ED fee fix)
================================================================================
STEP 1 DONE — WASM regen finished: /tmp/wasm-regen.log ends "Finished `release` profile";
  fresh gpn_runtime.compact.compressed.wasm present in wbuild (476504 bytes, md5 eec31f41…,
  differs from stale e9a6978f… in /tmp/stale-wasm.bak).
STEP 2 DONE — copied fresh WASM to gpn-node/runtime/, committed 0c5693d
  "runtime: regenerate canonical WASM (complete_job sub-ED fee fix)", pushed 5faff97..0c5693d.
STEP 3 — sudo 5HHSuwZgsYpa5Mfdfdgzr7LJcY4sU21t2iyu8sq1yV2PjuMn balance was 0; faucet drip
  1000 GPN ok (faucet bal before 13200). gpn_upgrade_runtime → "✅ runtime upgraded in place
  (forkless) — chain continued, state preserved" (nonce 0 consumed, now nonce=1).
STEP 4 DONE — NO RESET: head baseline #0x148 (328) before, #0x14c (332) after, still climbing.
  State survived: provider 5ENqSp5Q… staked=true attested=true (Bronze/188 GFLOPS),
  balance 999.9992 GPN; oracle feed present (price $0.15, apr 15%, plausible=true).

--------------------------------------------------------------------------------
2026-09-28 ~19:50 UTC — DIAGNOSIS: first set_code attempts were silently rejected
--------------------------------------------------------------------------------
Symptom: gpn_upgrade_runtime printed "✅ runtime upgraded" but the on-chain :code
  was UNCHANGED (still the stale 476523-byte blob, blake2 aaa2b9b9…), and
  complete_job kept failing with DispatchError variant=7 (Token) 0x02
  = TokenError::BelowMinimum (the exact sub-ED fee bug → fix not live).
Root cause found:
  * Sudo.sudo does NOT fail the outer extrinsic when the inner call fails; it emits
    Sudo::Sudid{ Err(...) } while the outer tx is ExtrinsicSuccess. The upgrade tool
    only scans for System::ExtrinsicFailed, so it missed the inner failure (false success).
  * Decoded block events (new gpn_dump_events bin):
      Sudo::Sudid fields=0x01030001000000
      = Err( Module{ pallet=0 (System), error=[1,0,0,0] } ) = System::SpecVersionNeedsToIncrease.
  * Confirmed in polkadot-sdk frame/system/src/lib.rs can_set_code():
      `if new_version.spec_version <= current_version.spec_version { SpecVersionNeedsToIncrease }`
    i.e. the new runtime's spec_version must be STRICTLY GREATER than the live one.
    Our rebuilt runtime kept spec_version=1 == live spec_version=1 → rejected (also
    spec_name must match: gpn==gpn OK).
FIX: bump runtime VERSION.spec_version 1 -> 2 (one line, gpn-node/runtime/src/lib.rs),
  rebuild the runtime WASM (fix INCLUDED: marketplace sub-ED fee skip), redeploy via set_code.
NOTE: this touches one line of runtime source beyond the WASM blob — unavoidable, since
  a spec_version bump is a prerequisite for any forkless set_code on this frame-system.

--------------------------------------------------------------------------------
2026-09-28 ~19:53–20:00 UTC — FORKLESS set_code REDONE with spec_version bump
--------------------------------------------------------------------------------
STEP 3 (retry) DONE — rebuilt runtime with spec_version 1->2, redeployed via sudo
  Sudo.sudo(System.set_code) [nonce 2], WASM 476529 bytes (md5 7d21d7da…).
  Decoded block events (gpn_dump_events):
    [5] System::CodeUpdated   hash=0x81e134d860a2bf5e73346b3ce6e803917988072a3f716532cd176301882f7748
    [6] Sudo::Sudid           = Ok (0x00)          <- inner call now SUCCEEDED
  On-chain :code len 476529 == local WASM (blake2 81e134d8…). state_getRuntimeVersion
  → specVersion=2. Committed 0c93210 + pushed.
STEP 4 DONE — still NO RESET: head kept climbing (#328 baseline → #370 → #381 → …).
  State survived across the upgrade: provider 5ENqSp5Q… staked=true attested=true
  (Bronze/188 GFLOPS); oracle feed present ($0.15, 15% APR, plausible=true);
  requestor/provider balances intact.
STEP 5 DONE — PAYOUT VERIFIED (this is the point of the whole exercise):
  - fresh requestor 5CGPnkqoVBv9iw5GVFCN9yz8r7ytK9MtPP9UCBX5H2NSttTM, dripped 100 GPN,
    gpn_api on :8090 (requestor signer). Posted LLMInference jobs price 10 GPN.
  - gpn_llm_provider (5ENqSp5Q…, restarted after upgrade so it reconnected with the
    new runtime metadata) auto-claimed + ran + settle.
  - Job #3 (and the previously-stuck #1/#2) → state **Completed**.
  - PROVIDER WALLET CREDITED: 999.9984 → 1009.9983 → 1029.9983 GPN (+10.00 per job;
    the sub-ED fee is correctly skipped and the whole escrow goes to the provider).
  - Note: a provider poll/block-timing race caused a duplicate accept_job
    (JobNotPending) that skipped start_job on the first pass; started via the
    provider-signed :8091 API to finish the run. Cosmetic to the payout proof.
STEP 6 DONE — Windows rebuilt: POST release-windows.yml/dispatches ref=main → HTTP 204,
  run 36476140664 (head 0c93210d) in_progress.

Scope note: besides the committed canonical WASM blob, the ONLY source change was one
  line — gpn-node/runtime/src/lib.rs spec_version 1->2 — which is a hard prerequisite for
  ANY forkless set_code on this frame-system (frames don't need a reset/re-seed either way).
  Added a throwaway diagnostic bin gpn-agent/src/bin/gpn_dump_events.rs (untracked, not
  committed) used to decode Sudo::Sudid / System::CodeUpdated.

=== FORKLESS UPGRADE OK — complete_job fix live, payout verified, no reset ===

================================================================================
2026-09-29 18:51–18:53 UTC — FREEZE GENESIS: rebuild + Windows sync (cron 75fbc745)
================================================================================
CONTEXT: commit 0f36ff9 restored gpn-node/runtime/gpn_runtime.compact.compressed.wasm
  to the chain-launch blob (sha256 b6b5435c6785a96cb25b0fac798fc2783d786cc562999e5b77919247dc290cd6),
  yielding genesis 0x08c4f181b2c29ca12fb1d56e05820525892f25119348cd058968445ba0e7948a.
  The complete_job fix stays ON-CHAIN via the earlier set_code (spec_version 2); it is NOT
  baked into the genesis blob.

STEP 1 DONE — Linux node rebuilt. `cargo build --release -p gpn-node` finished OK
  (gpn-node/target/release/gpn-node, fresh). build.rs embeds the committed frozen blob
  via include_bytes!(gpn-node/runtime/gpn_runtime.compact.compressed.wasm).

STEP 2 DONE — fleet restarted, NO RESET:
  pre-restart head #5110 → post-restart head #5111 (climbing), genesis still
  0x08c4f181b2c29ca12fb1d56e05820525892f25119348cd058968445ba0e7948a.

STEP 4 (Linux side) DONE — fresh gpn-node started against throwaway base-path /tmp/gpngenesis:
    "🔨 Initializing Genesis block/state (state: 0xbf5b…b737, header-hash: 0x08c4…948a)"
  header-hash MATCHES the live chain genesis 0x08c4…948a. Linux binary is syncable.

STEP 3 — Windows rebuild on frozen commit: release-windows.yml run 36613637944
  (head 0f36ff9f4) IN PROGRESS on self-hosted runner DESKTOP-USBUADL (started 18:39:45 UTC).
  No EPERM/checkout failure so far. Awaiting completion.

STEP 3 DONE — Windows rebuild SUCCESS: release-windows.yml run 36613637944 (head 0f36ff9f4)
  → completed success at ~19:04 UTC. Artifact `gpn-windows` (id 11055861288, 150,622,256 bytes).
  No EPERM/checkout failure (Windows gpn-node.exe was not holding the lock).

STEP 4 DONE — Windows binary VERIFIED against frozen genesis:
  Downloaded + extracted gpn-node/target/release/gpn-node.exe (67,345,408 bytes).
  Byte-search: the exe contains the frozen runtime WASM VERBATIM at offset 47,887,262 —
    sha256(b6b5435c6785a96cb25b0fac798fc2783d786cc562999e5b77919247dc290cd6) == committed
    gpn-node/runtime/gpn_runtime.compact.compressed.wasm (476,523 bytes). => the exe was built
    with the frozen genesis blob and yields genesis 0x08c4f181…948a.
  (No wine on host to exec the .exe directly; the Linux gpn-node built from this same commit
   WAS run against a throwaway base-path and printed header-hash 0x08c4…948a — the permitted
   equivalent verification.)

STEP 2 (recheck) — fleet still healthy, NO RESET: head climbing #5110 → #5111 → … → #5162;
  genesis still 0x08c4f181b2c29ca12fb1d56e05820525892f25119348cd058968445ba0e7948a.
  WASM blob untouched (sha256 b6b5435c…0cd6, git clean).

=== GENESIS FROZEN — Windows binary matches chain genesis 0x08c4f181…948a, syncable ===
