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
  # sign one artifact per platform from a build directory
  scripts/gen-release-manifest.py \\
      --version 0.2.0 --min-version 0.1.5 \\
      --asset linux/x86_64=dist/gpn_node \\
      --asset windows/x86_64=dist/gpn_node.exe \\
      --url-base https://gpncoin.ai/dl \\
      --key ~/.openclaw/credentials/gpn-release-key.txt \\
      --out website/releases.json

  # verify an existing manifest (what a node does)
  scripts/gen-release-manifest.py --verify website/releases.json

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
import sys
import tempfile
from pathlib import Path

CANONICAL_PREFIX = "gpn-release-v1"
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


def build_manifest(args) -> dict:
    assets = []
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
    return {
        "version": args.version,
        "min_version": args.min_version,
        "published_at": args.published_at
        or _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "assets": assets,
    }


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
    ap.add_argument("--version", help="release version, e.g. 0.2.0")
    ap.add_argument(
        "--min-version",
        default="",
        help="oldest version still allowed to mine (empty = no cutoff)",
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
        verify(Path(args.verify))
        return 0

    if not args.version:
        ap.error("--version is required when generating")

    key_path = Path(args.key or os.environ.get("GPN_RELEASE_KEY") or DEFAULT_KEY)
    if not key_path.is_file():
        raise SystemExit(
            f"private release key not found at {key_path}\n"
            "  It is NOT in this repo (by design). Ask release ops for it, or\n"
            "  generate a new one with scripts/gpn-release-key.sh and update\n"
            "  RELEASE_PUBKEY_HEX in gpn-agent/src/update.rs."
        )
    seed = read_private_key(key_path)
    man = sign(build_manifest(args), seed)

    if args.print_canonical:
        sys.stderr.write(canonical_text(man).decode())

    body = json.dumps(man, indent=2) + "\n"
    out = Path(args.out)
    write_atomic(out, body)
    print(f"wrote {out} ({man['version']}, {len(man['assets'])} asset(s))")
    print(f"  sha256 of {out.name}: {hashlib.sha256(body.encode()).hexdigest()}")

    # Self-check: never publish something the node would reject.
    verify(out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
