#!/usr/bin/env bash
# gpn-release-key.sh — create (or inspect) the ed25519 GPN release signing key.
#
# The key pair:
#   * PRIVATE half -> release operations only. Never in the repo, never in CI
#     logs. Default location: ~/.openclaw/credentials/gpn-release-key.txt
#     (mode 0600). Back it up offline; losing it means shipping a new node
#     build with a new embedded public key before anything else can be signed.
#   * PUBLIC half  -> compiled into every node
#     (gpn-agent/src/update.rs::RELEASE_PUBKEY_HEX).
#
# Usage:
#   scripts/gpn-release-key.sh            # create the key if absent, print the pubkey
#   scripts/gpn-release-key.sh --show     # print the public key + fingerprint
#   scripts/gpn-release-key.sh --force    # regenerate (BREAKS every deployed node)
#
# See docs/updates.md -> "Key custody".
set -euo pipefail

KEY="${GPN_RELEASE_KEY:-$HOME/.openclaw/credentials/gpn-release-key.txt}"
MODE="create"

case "${1:-}" in
  --show) MODE="show" ;;
  --force) MODE="force" ;;
  "") ;;
  -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
  *) echo "unknown option: $1" >&2; exit 2 ;;
esac

show_pub() {
  local pub
  pub=$(grep -o 'pubkey[^:]*: *[0-9a-f]\{64\}' "$KEY" | grep -o '[0-9a-f]\{64\}' | head -1 || true)
  if [ -z "$pub" ]; then
    pub=$(python3 - "$KEY" <<'PY'
import sys, nacl.signing, pathlib
raw = [l.strip() for l in pathlib.Path(sys.argv[1]).read_text().splitlines()
       if l.strip() and not l.strip().startswith('#')][-1]
seed = bytes.fromhex(raw.removeprefix('0x'))[:32]
print(nacl.signing.SigningKey(seed).verify_key.encode().hex())
PY
)
  fi
  echo "key file : $KEY"
  echo "public   : $pub"
  echo "fingerprint: $(printf '%s' "$pub" | sha256sum | cut -c1-16)"
  echo
  echo "Compiled into the node as RELEASE_PUBKEY_HEX in gpn-agent/src/update.rs:"
  echo "  pub const RELEASE_PUBKEY_HEX: &str = \"$pub\";"
}

if [ "$MODE" = "show" ]; then
  [ -f "$KEY" ] || { echo "no key at $KEY" >&2; exit 1; }
  show_pub
  exit 0
fi

if [ -f "$KEY" ] && [ "$MODE" != "force" ]; then
  echo "key already exists — refusing to overwrite (use --force to regenerate)"
  show_pub
  exit 0
fi

mkdir -p "$(dirname "$KEY")"
python3 - "$KEY" <<'PY'
import sys, nacl.signing, pathlib
sk = nacl.signing.SigningKey.generate()
pub = sk.verify_key.encode().hex()
p = pathlib.Path(sys.argv[1])
p.write_text(
    "# GPN release signing key (ed25519). PRIVATE — never commit, never paste\n"
    "# into an issue or a CI log. Custody: docs/updates.md.\n"
    f"# pubkey (compiled into every node): {pub}\n"
    f"{sk.encode().hex()}\n"
)
p.chmod(0o600)
print(pub)
PY
chmod 600 "$KEY"
echo "created $KEY (mode 0600)"
echo
show_pub
