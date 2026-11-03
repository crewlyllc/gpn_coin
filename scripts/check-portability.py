#!/usr/bin/env python3
"""Fail on platform-specific code that is not inside a #[cfg(...)] gate.

Why this exists (2026-11-03): the Windows release build broke on
`use std::os::unix::fs::PermissionsExt;` in `gpn_update`. It compiles perfectly on
Linux; it only surfaced when the Windows job ran, which is at *release* time. The
same class bit us earlier with a non-ASCII PowerShell script and with a
`Copy-Item` that copied a file onto itself.

Cross-compiling in CI is not available on this host (no mingw, no MSVC), so this
is a fast static check that catches the recurring class: an unguarded
unix-only or windows-only API in code that must build for both.

Usage:  scripts/check-portability.py [paths…]      (default: tracked .rs files)
Exit:   0 clean, 1 findings
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

# Imports/paths that only exist on one platform.
PATTERNS = [
    re.compile(r"\bstd::os::unix\b"),
    re.compile(r"\bstd::os::windows\b"),
    re.compile(r"\buse\s+std::os::(unix|windows)\b"),
    re.compile(r"\bstd::os::fd\b"),  # unix-only alias since 1.66
]

# A gate on the same or a previous line within this window counts as guarded.
LOOKBACK = 12
GATE = re.compile(r"#\[cfg\((?P<body>[^\]]*(unix|windows)[^\]]*)\)\]")


def is_gate(body: str, platform: str) -> bool:
    """True if this cfg body *guards for* `platform` (i.e. may contain it)."""
    body = body.replace(" ", "")
    # cfg(unix), cfg(windows), cfg(any(unix, …)), cfg(target_os = "…")
    if f"not({platform})" in body:
        return False
    return platform in body


def scan(path: Path) -> list[tuple[int, str, str]]:
    out: list[tuple[int, str, str]] = []
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return out
    gates: list[tuple[int, str]] = []  # (lineno, cfg body)
    for i, line in enumerate(lines, start=1):
        m = GATE.search(line)
        if m:
            gates.append((i, m.group("body")))
        for pat in PATTERNS:
            if not pat.search(line):
                continue
            platform = "unix" if "unix" in line or "fd" in line else "windows"
            guarded = False
            for gl, body in gates:
                if i - gl > LOOKBACK:
                    continue
                if is_gate(body, platform) or is_gate(body, "windows" if platform == "unix" else "unix"):
                    guarded = True
                if is_gate(body, platform):
                    break
            if not guarded:
                out.append((i, line.strip(), platform))
            break
    return out


def main(argv: list[str]) -> int:
    root = Path(__file__).resolve().parent.parent
    if len(argv) > 1:
        files = [Path(a) for a in argv[1:]]
    else:
        tracked = subprocess.run(
            ["git", "-C", str(root), "ls-files", "*.rs"],
            capture_output=True, text=True, check=True,
        ).stdout.split()
        files = [root / f for f in tracked]

    findings = []
    for f in files:
        if "/target/" in str(f) or f.name.endswith(".generated.rs"):
            continue
        for lineno, text, platform in scan(f):
            findings.append((f, lineno, text, platform))

    if not findings:
        print("portability: OK (no unguarded platform-specific code)")
        return 0
    print("portability: platform-specific code outside a #[cfg(...)] gate:\n", file=sys.stderr)
    for f, lineno, text, platform in findings:
        print(f"  {f}:{lineno}: [{platform}] {text}", file=sys.stderr)
    print(
        "\nFix: wrap it in #[cfg(unix)] (or #[cfg(windows)]) and provide the other "
        "platform's version, or use a cfg!() branch.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
