#!/usr/bin/env python3
"""check_no_secrets.py — fail if a TRACKED file looks like it contains key material.

Red-team finding #5 (2026-10-08): `wallet-run-ai-test.txt` had committed a live
private seed and it was pushed to the public remote. Redaction alone is not
enough if the mistake can recur, so this guard runs in CI on every PR/push.

Scope: tracked files only (`git ls-files`), decoded as UTF-8 text (binary files
skipped). `//Alice`-style dev URIs are public dev keys and are intentionally not
flagged (no secret label, not 64-hex).

Exit 0 = clean, 1 = a possible secret was found.
"""
from __future__ import annotations

import re
import subprocess
import sys

# Literals known to have leaked and that must NEVER reappear in the tree.
# (Append new burned values here.) Stored split in two halves so this guard file
# itself never contains the contiguous secret (otherwise it would flag itself).
BLOCKLIST = [
    "e871c97ce671bd29efe234d077980d3" + "ee12333ce3c3eb3af71f52d210045018a",
]

# A secret label, then (optionally) the material.
_LABEL = r"(?:secret[\s_-]?seed|seed[\s_-]?phrase|mnemonic|private[\s_-]?key|privkey|secret)"
_HEX = re.compile(_LABEL + r"[^0-9A-Za-z]{0,6}(?:0x)?[0-9a-fA-F]{64}", re.IGNORECASE)
_MNEMONIC = re.compile(
    r"(?:secret[\s_-]?seed|seed[\s_-]?phrase|mnemonic|private[\s_-]?key)"
    r"[^0-9A-Za-z]{0,6}(?:[a-z]{3,}[ \t]+){11,}[a-z]{3,}",
    re.IGNORECASE,
)

SKIP_SUFFIXES = (".png", ".jpg", ".jpeg", ".gif", ".wasm", ".bin", ".gz", ".zip", ".pdf", ".ico")


def tracked_files() -> list[str]:
    out = subprocess.run(["git", "ls-files"], capture_output=True, text=True, check=True)
    return [f for f in out.stdout.splitlines() if f]


def main() -> int:
    fail = False
    for path in tracked_files():
        if path.lower().endswith(SKIP_SUFFIXES):
            continue
        try:
            with open(path, "r", encoding="utf-8") as fh:
                text = fh.read()
        except (OSError, UnicodeDecodeError):
            continue  # binary or unreadable as text -> skip

        for lineno, line in enumerate(text.splitlines(), 1):
            if "REDACTED" in line:
                continue
            if _HEX.search(line):
                print(f"::error file={path},line={lineno}::possible committed hex secret (labelled seed/private key)")
                fail = True
            if _MNEMONIC.search(line):
                print(f"::error file={path},line={lineno}::possible committed mnemonic")
                fail = True
            for bad in BLOCKLIST:
                if bad in line:
                    print(f"::error file={path},line={lineno}::known-leaked secret literal present — remove + rotate")
                    fail = True

    if fail:
        print("secret-scan: FAIL — tracked key material found (see errors above).")
        return 1
    print("secret-scan: OK (no labelled secrets in tracked files).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
