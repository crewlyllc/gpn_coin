# Release checklists — index

Completed checklists are moved to `archive/<version>.md` and listed here. The row is written when
the checklist's last box is ticked and the version is published.

| version | published | items | checklist |
|---|---|---|---|
| 0.3.79 | 2026-11-25 | 270 (AP refuses a loopback `channel_url` published by a remote provider — code+tests `cargo test --bin gpn_scheduler channel_url` 3 passed, live PNG half NOT observed: local provider's sdxl-turbo recorded unloadable ⇒ `HTTP 425`); 271 (update-guard refusal no longer claims a direction it cannot verify — live `gpn_update` refusal line on real signed manifest, `EXIT=3`, nothing installed); 272 (first chat after a restart answers `503 gpn_warming` + `Retry-After` instead of a 10 s timeout — live `POST :8092/v1/chat/completions` `200` in 3.384 s after the 0.3.79 swap); 256 (AWAITING-OWNER — venue decision, `docs/NEEDS-TRIAGE.md` item 256) | archive/0.3.79.md |
| 0.3.78 | 2026-11-25 | 263 (image/video jobs reach a pull-only provider over the published channel URL — live proof on the new binary: PNG 256x256 125,300 B); 256 (AWAITING-OWNER — venue decision + the structural two-version window) | archive/0.3.78.md |
| 0.3.77 | 2026-11-25 | 262 (deep-fork refusal held across a peer merely ahead — live proof box 3, PASS=True); 256 (AWAITING-OWNER — carried in §0e, GPU guest needed) | archive/0.3.77.md |
| 0.3.76 | 2026-11-24 | 262 (fix `b884bc54` shipped here; its live proof needed a binary containing it, so it was earned and recorded on 0.3.77) | archive/0.3.76.md |
| 0.3.75 | 2026-11-24 | 194 (both halves proven live on the published binaries); 256 (AWAITING-OWNER — carried in §0e, GPU guest needed) | archive/0.3.75.md |
| 0.3.74 | 2026-11-24 | 257, 259, 260, 261; 256 (AWAITING-OWNER — carried in §0e, GPU guest needed) | archive/0.3.74.md |
| 0.3.73 | 2026-11-24 | 255, 258; 256 (AWAITING-OWNER — steps 3-6 need a GPU guest, [TRIAGE] item 256) | archive/0.3.73.md |
| 0.3.72 | 2026-11-24 | 250, 251, 252, 253, 254 | archive/0.3.72.md |
| 0.3.71 | 2026-11-23 | 16 (per-release sweep; closed), 248, 249 | archive/0.3.71.md |
| 0.3.60 | 2026-11-19 | 205 (blocked: runtime), 206, 207 | archive/0.3.60.md |
| 0.3.61 | 2026-11-20 | 194 (code half; live venue AWAITING-OWNER), 202, 208, 209, 210, 211, 212 | archive/0.3.61.md |
| 0.3.62 | 2026-11-20 | 52 (mechanism; cadence residual -> row 217), 191, 196/205, 213, 214, 215/216 | archive/0.3.62.md |
| 0.3.63 | 2026-11-20 | 217 (restated acceptance + gate; the cadence fix itself rides row 218's release) | archive/0.3.63.md |
| 0.3.64 | 2026-11-20 | 217 (cadence acceptance met on a 0.3.64-authored window), 218 (sealer heartbeat; every long gap attributable) | archive/0.3.64.md |
| 0.3.65 | 2026-11-20 | 221, 222, 223, 224, 225, 226, 227, 228, 229 | archive/0.3.65.md |
| 0.3.52 | 2026-11-16 | 195, 198 | archive/0.3.52.md |
| 0.3.53 | 2026-11-17 | 199, 200, 207, 206 | archive/0.3.53.md |
| 0.3.54 | 2026-11-17 | 199, 200 | archive/0.3.54.md |
| 0.3.55 | 2026-11-17 | 199, 200 | archive/0.3.55.md |
| 0.3.56 | 2026-11-17 | 199, 200 | archive/0.3.56.md |
| 0.3.57 | 2026-11-17 | see file (no row ids) | archive/0.3.57.md |
| 0.3.58 | 2026-11-17 | 203, 204 | archive/0.3.58.md |
| 0.3.59 | 2026-11-18 | 161 | archive/0.3.59.md |
| 0.3.66 | 2026-11-22 | 230, 231 (source; enactment pending), 232 | archive/0.3.66.md |
| 0.3.67 | 2026-11-22 | 233, 234, 235, 236 | archive/0.3.67.md |
| 0.3.68 | 2026-11-22 | 237, 238 | archive/0.3.68.md |
| 0.3.69 | 2026-11-22 | 239 | archive/0.3.69.md |
| 0.3.70 | 2026-11-23 | 240, 241, 242, 243 (source in 0.3.70; spec-54 `set_code` enacted live 2026-11-23 02:2x PT), 244, 245, 246, 247 | archive/0.3.70.md |
