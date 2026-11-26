#!/usr/bin/env bash
# Row 279: `network.json` shipped `spec_version: 54` while every released
# binary built the spec-55 runtime. `classify()` prints "OUTDATED — running
# spec 54 < canonical 54" as healthy, and nothing failed when the two files
# disagreed, so the floor silently lagged a whole spec behind the binary.
#
# The guard must fail on ANY divergence between the runtime's spec_version and
# the registered floor; equality is the only shipped state. Raising the floor
# is what declares older nodes incompatible, so it is a deliberate edit made
# together with the runtime bump, not a side effect.
set -uo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
script="$root/scripts/check-spec-version.sh"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
pass=0; fail=0
ok()   { pass=$((pass+1)); echo "  ok   $1"; }
bad()  { fail=$((fail+1)); echo "  FAIL $1"; }

# fake tree: <dir>/gpn-node/runtime/src/lib.rs + <dir>/network.json
mk() { # $1 dir, $2 src spec, $3 net spec
  mkdir -p "$1/gpn-node/runtime/src"
  printf 'construct_runtime!(\n  pub enum Runtime {\n    System: frame_system = 0,\n  },\n);\n\n#[sp_version::runtime_version]\npub const VERSION: RuntimeVersion = RuntimeVersion {\n    spec_name: create_runtime_str!("gpn"),\n    spec_version: %s,\n    impl_version: 1,\n};\n' "$2" > "$1/gpn-node/runtime/src/lib.rs"
  printf '{\n  "network": "gpn",\n  "spec_version": %s\n}\n' "$3" > "$1/network.json"
}

run() { GPN_REPO_ROOT="$1" bash "$script" 2>&1; }

# 1. equal ⇒ OK
d="$tmp/equal"; mk "$d" 55 55
out="$(run "$d")"; rc=$?
[ $rc -eq 0 ] && grep -q '^OK$' <<<"$out" && ok "equal (55/55) passes" || bad "equal (55/55) should pass (rc=$rc): $out"

# 2. the exact production defect: binary builds 55, registry says 54 ⇒ FAIL
d="$tmp/behind"; mk "$d" 55 54
out="$(run "$d")"; rc=$?
[ $rc -ne 0 ] && grep -q 'network.json spec_version (54)' <<<"$out" && ok "registry behind runtime (55/54) fails with the numbers" || bad "registry behind runtime must fail (rc=$rc): $out"

# 3. registry ahead of the runtime ⇒ FAIL (the floor would declare a node
#    built from this tree outdated)
d="$tmp/ahead"; mk "$d" 54 55
out="$(run "$d")"; rc=$?
[ $rc -ne 0 ] && ok "registry ahead of runtime (54/55) fails" || bad "registry ahead of runtime must fail (rc=$rc): $out"

# 4. arbitrarily far ahead ⇒ still hard FAIL (no "warning" lane)
d="$tmp/far"; mk "$d" 54 60
out="$(run "$d")"; rc=$?
[ $rc -ne 0 ] && ! grep -q '::warning::' <<<"$out" && ok "no warning lane: 54/60 is an error" || bad "54/60 must be an error, not a warning (rc=$rc): $out"

# 5. the real tree must be green right now
out="$(bash "$script" 2>&1)"; rc=$?
[ $rc -eq 0 ] && ok "real repo passes: $(tr '\n' ' ' <<<"$out")" || bad "real repo fails: $out"

# 6. row 279's premise: the shipped registry floor equals the runtime spec
src=$(grep -m1 -oE 'spec_version: [0-9]+' "$root/gpn-node/runtime/src/lib.rs" | grep -oE '[0-9]+')
net=$(python3 -c "import json;print(json.load(open('$root/network.json'))['spec_version'])")
[ "$src" = "$net" ] && ok "shipped network.json spec_version=$net == runtime spec $src" || bad "shipped network.json spec_version=$net != runtime spec $src"

echo "PASS=$pass FAIL=$fail"
[ "$fail" -eq 0 ]
