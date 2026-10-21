#!/usr/bin/env python3
"""Generate and sign the GPN release manifest (`releases.json`).

The manifest is what `gpn_node` fetches on start (and every
`GPN_UPDATE_CHECK_HOURS`) to answer two questions:

  1. is there a newer build for my OS/arch?  -> optional auto-update
  2. is my build still allowed to mine?      -> the `min_version` cutoff

It is signed with the **ed25519 release key**. Only the public half is compiled
into the node (`gpn-agent/src/update.rs::RELEASE_PUBKEY_HEX`); the private half
must never be in the repo. See `docs/updates.md` for key custody.

The signature is over a canonical text blob, NOT over the JSON, so the signer
and the verifier cannot disagree about JSON formatting:

    gpn-release-v1
    version=0.2.0
    min_version=0.1.5
    published_at=2026-10-21T17:00:00Z
    signing_key=<64 hex>
    asset=linux/x86_64 <url> <sha256> <size>
    ...                                   (assets sorted by os, arch, url)

Usage
-----
  # sign one artifact per platform from a build directory. The version is read
  # OUT of the artifacts (no hand-typed value to drift).
  scripts/gen-release-manifest.py \\
      --asset linux/x86_64=dist/gpn_node \\
      --asset windows/x86_64=dist/gpn_node.exe \\
      --min-version 0.1.0 \\
      --url-base https://gpncoin.ai/dl \\
      --key ~/.openclaw/credentials/gpn-release-key.txt \\
      --out website/releases.json

  # verify an existing manifest (what a node does)
  scripts/gen-release-manifest.py --verify website/releases.json

Where the values come from
--------------------------
* `version` — parsed out of the artifact itself. `gpn-agent/build.rs` stamps
  `gpn-build-v1 version=<ver> sha=<sha> spec=<n>` into the binary as a string
  literal, so the manifest describes what was **built**, not what someone typed
  — and it works for a Windows `.exe` that cannot be executed here. All assets
  must agree. `--version X.Y.Z` is accepted as an assertion: if it disagrees
  with the artifacts the script fails instead of publishing a lie.
* asset `sha256` / `size` — computed from the file, never given.
* `min_version` — `--min-version X.Y.Z`, or the policy default described below.

`min_version` policy (`--min-version-policy same-minor`, the default):

    min_version = <MAJOR>.<MINOR>.0   of the version being published

A PATCH release therefore keeps the whole minor line supported, while a MINOR
release cuts off everything older — which is only correct if the minor really is
a feature line that older peers interoperate with but should not run; pass
`--min-version` explicitly whenever that is not what you want (it usually is not
for the FIRST release that introduces a feature). `--no-min-version` publishes
an empty cutoff. The script prints the resulting effect loudly.

Never publish a manifest whose artifacts are not uploaded yet, and never sign one
made from a stale `dist/` — see the release checklist in `docs/updates.md`.

Environment
-----------
  GPN_RELEASE_KEY   path to the private key file (else --key, else the default
                    operator location). NEVER commit it.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import json
import os
import re
import sys
import tempfile
from pathlib import Path

CANONICAL_PREFIX = "gpn-release-v1"
#: Build stamp injected by `gpn-agent/build.rs` (`GPN_BUILD_ID`). The version and
#: git SHA that follow it are what this artifact was actually compiled from.
BUILD_ID_PREFIX = b"gpn-build-v1 version="
#: A real release version. `unknown` is the `unwrap_or` fallback that
#: `firstrun::build_id()` carries for builds without `build.rs`; it must never be
#: published as a release version.
_VERSION_RE = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")
#: One stamp, matched from its first byte. `sha` is constrained to hex (or the
#: `unknown` sentinel) so that a stamp which happens to be adjacent to another
#: string literal cannot borrow characters from it.
_STAMP_RE = re.compile(
    r"gpn-build-v1 version=(?P<version>\S+)\s+sha=(?P<sha>[0-9a-f]{7,40}|unknown)"
    r"(?:\s+spec=(?P<spec>\d+))?"
)
DEFAULT_KEY = Path.home() / ".openclaw/credentials/gpn-release-key.txt"
#: The public key compiled into the node. Kept in sync with
#: `gpn-agent/src/update.rs::RELEASE_PUBKEY_HEX`; `--verify` fails loudly if a
#: manifest names a different key.
EMBEDDED_PUBKEY_HEX = "78015fab541c9a6230d3fb302a89f1a9950d1b181652cdf0a65901b6f0278c5e"


# ─────────────────────────────────────────────────────────────────────────────
# Canonical form (must match gpn-agent/src/update.rs::ReleaseManifest)
# ─────────────────────────────────────────────────────────────────────────────


def canonical_text(man: dict) -> bytes:
    """The exact bytes that are signed and verified."""
    assets = sorted(
        man.get("assets", []), key=lambda a: (a["os"], a["arch"], a["url"])
    )
    lines = [
        CANONICAL_PREFIX,
        f"version={man['version']}",
        f"min_version={man.get('min_version', '')}",
        f"published_at={man.get('published_at', '')}",
        f"signing_key={man.get('signing_key', '').lower()}",
    ]
    for a in assets:
        lines.append(
            f"asset={a['os']}/{a['arch']} {a['url']} "
            f"{a['sha256'].lower()} {int(a.get('size', 0))}"
        )
    return ("\n".join(lines) + "\n").encode("utf-8")


# ─────────────────────────────────────────────────────────────────────────────
# Keys
# ─────────────────────────────────────────────────────────────────────────────


def read_private_key(path: Path) -> bytes:
    """Read the raw 32-byte ed25519 seed from our key file.

    Accepted shapes: a bare hex seed, or the generated file (comments starting
    with `#` plus the last non-empty line being the hex seed).
    """
    text = Path(path).read_text()
    hexline = ""
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        hexline = line
    if not hexline:
        raise SystemExit(f"{path}: no key material found")
    raw = bytes.fromhex(hexline.removeprefix("0x"))
    if len(raw) == 64:  # libsodium's seed||pubkey form
        raw = raw[:32]
    if len(raw) != 32:
        raise SystemExit(f"{path}: expected a 32-byte ed25519 seed, got {len(raw)}")
    return raw


def public_key_hex(sk) -> str:
    return bytes(sk.verify_key).hex()


# ─────────────────────────────────────────────────────────────────────────────
# Build / sign / verify
# ─────────────────────────────────────────────────────────────────────────────


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def read_build_ids(path: Path) -> list[dict]:
    """Every `gpn-build-v1 …` stamp in an artifact, in file order."""
    data = path.read_bytes()
    found = _scan_stamps(data, BUILD_ID_PREFIX, 1, "ascii")
    if not found:
        # A UTF-16 build (unlikely) interleaves NULs; try the easy case. Only as a
        # fallback: an ASCII stamp is what every toolchain we use produces.
        interleaved = b"\0".join(bytes([c]) for c in BUILD_ID_PREFIX)
        found = _scan_stamps(data, interleaved, 4, "utf-16-le")
    return found


def _scan_stamps(data: bytes, needle: bytes, width: int, encoding: str) -> list[dict]:
    """Parse every occurrence of `needle` as a build stamp (see read_build_ids).

    Fields are matched with a regex rather than by splitting the printable run to
    its end: two stamps can be **adjacent in `.rodata` with no separator** (the
    real one and `firstrun::build_id()`'s `unknown` fallback), so a run-based
    parse would swallow the next literal into the last field.
    """
    found: list[dict] = []
    start = 0
    while True:
        idx = data.find(needle, start)
        if idx < 0:
            break
        start = idx + 1
        tail = data[idx : idx + 256 * width].decode(encoding, "ignore")
        m = _STAMP_RE.match(tail)
        if m:
            fields = {k: v for k, v in m.groupdict().items() if v}
            fields["build_id"] = tail[: m.end()]
            found.append(fields)
    return found


def read_build_id(path: Path) -> dict:
    """Parse the `gpn-build-v1 version=… sha=… spec=…` stamp out of an artifact.

    Returns `{}` when the artifact carries no stamp (an old binary, a stripped
    build, or something that is not a gpn_node at all). Works for foreign-format
    binaries (a Windows `.exe`) because it only looks for ASCII in `.rodata`.

    **A current build embeds two stamps**: the real one, and the fallback
    `gpn-build-v1 version=unknown sha=unknown` that `firstrun::build_id()` needs
    when the crate is compiled without `build.rs`. A single `find()` can hit the
    fallback (it is a separate literal, and its position is up to the linker) and
    publish the release as `version=unknown` — the exact failure this script
    exists to prevent. So every occurrence is collected, and a stamp carrying a
    real `MAJOR.MINOR.PATCH` wins; a build that genuinely has only the fallback
    is treated as unstamped.
    """
    stamps = read_build_ids(path)
    if not stamps:
        return {}
    real = [s for s in stamps if _VERSION_RE.match(s.get("version", ""))]
    if real:
        return real[0]
    # Only the `unknown` fallback is present: report "no stamp" so the caller
    # either fails loudly or needs --allow-unmarked-artifact --version.
    return {}


def policy_min_version(version: str) -> str:
    """The default cutoff: `<MAJOR>.<MINOR>.0` of the version being published."""
    major, minor = version.split(".")[:2]
    return f"{major}.{minor}.0"


def resolve_min_version(args, version: str) -> str:
    if getattr(args, "no_min_version", False):
        if args.min_version:
            raise SystemExit("--no-min-version conflicts with --min-version")
        return ""
    if args.min_version:
        return args.min_version.strip()
    if getattr(args, "min_version_policy", "same-minor") != "same-minor":
        raise SystemExit(
            f"unknown --min-version-policy {args.min_version_policy!r} "
            "(supported: same-minor)"
        )
    return policy_min_version(version)


def build_manifest(args) -> dict:
    assets = []
    stamps = []
    for spec in args.asset:
        if "=" not in spec:
            raise SystemExit(f"--asset must be os/arch=path, got {spec!r}")
        platform, path = spec.split("=", 1)
        if "/" not in platform:
            raise SystemExit(f"--asset platform must be os/arch, got {platform!r}")
        osname, arch = platform.split("/", 1)
        p = Path(path)
        if not p.is_file():
            raise SystemExit(f"--asset {spec}: {p} does not exist")
        stamp = read_build_id(p)
        if stamp:
            stamps.append((platform.lower(), stamp))
        assets.append(
            {
                "os": osname.lower(),
                "arch": arch.lower(),
                "url": f"{args.url_base.rstrip('/')}/{p.name}",
                "sha256": sha256_file(p),
                "size": p.stat().st_size,
            }
        )
    if not assets and not getattr(args, "allow_empty_assets", False):
        raise SystemExit(
            "at least one --asset is required (or --allow-empty-assets to publish "
            "a cutoff-only manifest before any binary exists)"
        )

    version = resolve_version(args, assets, stamps)
    man = {
        "version": version,
        "min_version": resolve_min_version(args, version),
        "published_at": args.published_at
        or _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "assets": assets,
    }
    return man


def resolve_version(args, assets, stamps) -> str:
    """The version to publish: from the artifacts, cross-checked against --version."""
    declared = (getattr(args, "version", "") or "").strip()
    built = {s.get("version", "") for _, s in stamps}
    built.discard("")

    if not assets:
        # cutoff-only manifest: no artifact to read, so --version is the input.
        if not declared:
            raise SystemExit(
                "--version is required when publishing with no assets "
                "(there is no artifact to read it from)"
            )
        return declared

    if not stamps:
        if getattr(args, "allow_unmarked_artifact", False):
            if not declared:
                raise SystemExit(
                    "artifacts carry no gpn-build-v1 stamp; --version is required "
                    "with --allow-unmarked-artifact"
                )
            print(
                "WARNING: no artifact carries a gpn-build-v1 stamp; trusting "
                f"--version {declared} (--allow-unmarked-artifact)",
                file=sys.stderr,
            )
            return declared
        raise SystemExit(
            "none of the artifacts carries the `gpn-build-v1 version=…` stamp "
            "injected by gpn-agent/build.rs — refusing to guess the version.\n"
            "  Build with a current build.rs, or pass --allow-unmarked-artifact "
            "--version X.Y.Z for a legacy binary."
        )

    if len(built) > 1:
        detail = ", ".join(f"{p}={s.get('version')}" for p, s in stamps)
        raise SystemExit(
            f"artifacts were built from different versions ({detail}) — they are "
            "not one release; rebuild them from the same commit"
        )
    artifact_version = built.pop()
    if declared and declared != artifact_version:
        raise SystemExit(
            f"--version {declared} disagrees with the artifacts ({artifact_version}) — "
            "the manifest would describe a build nobody has. Drop --version (it is "
            "derived) or rebuild."
        )
    if len(stamps) != len(assets):
        missing = len(assets) - len(stamps)
        print(
            f"WARNING: {missing} asset(s) carry no build stamp; publishing "
            f"{artifact_version} read from the other(s)",
            file=sys.stderr,
        )
    for platform, stamp in stamps:
        print(f"  {platform}: version={stamp.get('version')} sha={stamp.get('sha', '?')}")
    return artifact_version


def sign(man: dict, seed: bytes) -> dict:
    import nacl.signing  # PyNaCl

    sk = nacl.signing.SigningKey(seed)
    man = dict(man)
    man["signing_key"] = public_key_hex(sk)
    sig = sk.sign(canonical_text(man)).signature
    man["signature"] = sig.hex()
    return man


def verify(path: Path, expect_key: str = EMBEDDED_PUBKEY_HEX) -> dict:
    import nacl.exceptions
    import nacl.signing

    man = json.loads(Path(path).read_text())
    sig_hex = (man.get("signature") or "").strip()
    key_hex = (man.get("signing_key") or "").strip().lower()
    if not sig_hex or not key_hex:
        raise SystemExit("manifest is unsigned")
    if expect_key and key_hex != expect_key.lower():
        raise SystemExit(
            f"manifest names key {key_hex}, node trusts {expect_key} — it would be rejected"
        )
    vk = nacl.signing.VerifyKey(bytes.fromhex(key_hex))
    try:
        vk.verify(canonical_text(man), bytes.fromhex(sig_hex))
    except nacl.exceptions.BadSignatureError:
        raise SystemExit("SIGNATURE DOES NOT VERIFY")
    # Belt and braces: the artifacts must exist and match if they are local.
    for a in man.get("assets", []):
        p = Path(Path(path).parent / Path(a["url"]).name)
        if p.is_file():
            actual = sha256_file(p)
            if actual != a["sha256"].lower():
                raise SystemExit(f"{p}: sha256 {actual} != manifest {a['sha256']}")
    print(
        f"OK: {man['version']} (min {man.get('min_version', '')}), "
        f"{len(man.get('assets', []))} asset(s), key {key_hex[:16]}…"
    )
    return man


def write_atomic(path: Path, body: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        "w", dir=path.parent, prefix=path.name + ".tmp-", delete=False
    ) as f:
        f.write(body)
        tmp = f.name
    os.replace(tmp, path)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--version",
        default="",
        help="assert the release version (default: read from the artifacts)",
    )
    ap.add_argument(
        "--min-version",
        default="",
        help="oldest version still allowed to mine (default: policy, see above)",
    )
    ap.add_argument(
        "--min-version-policy",
        default="same-minor",
        choices=["same-minor"],
        help="how min_version is derived when not given (default: same-minor)",
    )
    ap.add_argument(
        "--no-min-version",
        action="store_true",
        help="publish an empty min_version (no cutoff at all)",
    )
    ap.add_argument(
        "--allow-unmarked-artifact",
        action="store_true",
        help="accept an artifact with no gpn-build-v1 stamp (legacy build; "
        "requires --version)",
    )
    ap.add_argument(
        "--asset",
        action="append",
        default=[],
        metavar="OS/ARCH=PATH",
        help="artifact to publish; repeatable",
    )
    ap.add_argument(
        "--url-base",
        default="https://gpncoin.ai/dl",
        help="public base URL the artifacts are served from",
    )
    ap.add_argument("--published-at", default="", help="ISO-8601 UTC; default now")
    ap.add_argument("--key", default="", help=f"private key file (default {DEFAULT_KEY})")
    ap.add_argument("--out", default="website/releases.json")
    ap.add_argument(
        "--verify",
        metavar="MANIFEST",
        default="",
        help="verify an existing manifest and exit",
    )
    ap.add_argument(
        "--expect-key",
        default="",
        metavar="HEX",
        help="with --verify: trust this ed25519 public key instead of the one "
        "compiled into the node (key rotation / testing a throwaway key)",
    )
    ap.add_argument(
        "--allow-empty-assets",
        action="store_true",
        help="publish a manifest with no artifacts (e.g. to raise min_version "
        "before the new binaries are uploaded)",
    )
    ap.add_argument(
        "--print-canonical",
        action="store_true",
        help="print the signed canonical text (debugging)",
    )
    args = ap.parse_args(argv)

    if args.verify:
        verify(Path(args.verify), args.expect_key or EMBEDDED_PUBKEY_HEX)
        return 0

    key_path = Path(args.key or os.environ.get("GPN_RELEASE_KEY") or DEFAULT_KEY)
    if not key_path.is_file():
        raise SystemExit(
            f"private release key not found at {key_path}\n"
            "  It is NOT in this repo (by design). Ask release ops for it, or\n"
            "  generate a new one with scripts/gpn-release-key.sh and update\n"
            "  RELEASE_PUBKEY_HEX in gpn-agent/src/update.rs."
        )
    seed = read_private_key(key_path)
    unsigned = build_manifest(args)
    print(
        f"manifest: version={unsigned['version']} "
        f"min_version={unsigned['min_version'] or '(none)'}"
    )
    if unsigned["min_version"]:
        print(
            f"  cutoff: nodes older than {unsigned['min_version']} will SYNC but "
            "not mine/announce"
        )
    man = sign(unsigned, seed)

    if args.print_canonical:
        sys.stderr.write(canonical_text(man).decode())

    body = json.dumps(man, indent=2) + "\n"
    out = Path(args.out)
    write_atomic(out, body)
    print(f"wrote {out} ({man['version']}, {len(man['assets'])} asset(s))")
    print(f"  sha256 of {out.name}: {hashlib.sha256(body.encode()).hexdigest()}")

    # Self-check: never publish something the node would reject. (With
    # --expect-key this is the same check against a throwaway/rotated key.)
    verify(out, args.expect_key or EMBEDDED_PUBKEY_HEX)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
