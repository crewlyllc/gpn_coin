#!/usr/bin/env bash
# check-no-secrets.sh — thin wrapper around check_no_secrets.py (see its docstring).
# Red-team finding #5: a private seed was committed to a public repo; CI now rejects it.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$here/check_no_secrets.py" "$@"
