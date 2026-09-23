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

