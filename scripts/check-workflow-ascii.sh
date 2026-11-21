#!/usr/bin/env bash
# Reject non-ASCII bytes in .github/workflows/*.yml.
#
# Why this gate exists, twice over:
#   - run 44728980382: an em-dash inside a `run:` string became three latin1 chars
#     when Windows PowerShell 5.1 decoded the file as ANSI, and the step died with
#     "Unexpected token 'refusing'" after the build had already run.
#   - run 44924273272 (2026-11-21): an em-dash in a *comment* of ci.yml:348 tripped
#     the release job's own encoding guard nine seconds in, so the Windows release
#     artifact was never built. The guard was right; the byte was the bug.
#
# The Windows guard can only run on the Windows runner, i.e. at release time. This
# script is the same check on Linux, in the cheap lint job, so the byte is caught in
# minutes on the machine that produced it.
#
# Usage: scripts/check-workflow-ascii.sh    Exit 0 clean, 1 findings.
set -uo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root" || exit 2

if [ ! -d .github/workflows ]; then
  echo "workflow-ascii: no .github/workflows directory" >&2
  exit 0
fi

# \x09 tab, \x20-\x7e printable ASCII. Everything else is a finding.
if out=$(LC_ALL=C grep -rnP '[^\x09\x20-\x7e]' .github/workflows --include='*.yml' 2>/dev/null); then
  echo "workflow-ascii: non-ASCII bytes in workflow files" >&2
  printf '%s\n' "$out" >&2
  echo >&2
  echo "Windows PowerShell 5.1 decodes run: scripts as ANSI, so these mangle into" >&2
  echo "parse errors at release time. Replace them with plain ASCII." >&2
  exit 1
fi

echo "workflow-ascii: OK (all workflow files are pure ASCII)"
exit 0
