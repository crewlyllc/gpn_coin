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
