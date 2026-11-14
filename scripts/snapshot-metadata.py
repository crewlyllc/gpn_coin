#!/usr/bin/env python3
"""Sign (and check) the public weekly snapshot metadata — `snapshot.json`.

`snapshot-weekly.sh` publishes a ~460 MiB chain snapshot. A fresh installer must
know, *before* it downloads or imports a byte, which sha256 and size to expect.
The GitHub release **body** is human-readable only: anyone who can serve a page
can edit text, so it is not a trust anchor.

`snapshot.json` is that anchor, signed with the **release ed25519 key** (the same
key whose public half is compiled into `gpn_node` for `releases.json`). The
signature is over a canonical text blob, not over JSON, so the signer here and
the verifier in `gpn_snapshot verify` cannot disagree about formatting:

    gpn-snapshot-v1
    url=<archive URL>
    sha256=<hex>
    size=<bytes>
    block=<height>
    built=<UTC stamp>
    signing_key=<64 hex>

Usage
-----
  # sign (release ops; needs the private key + PyNaCl)
  scripts/snapshot-metadata.py sign \\
      --url https://github.com/crewlyllc/gpn_coin/releases/download/snapshot-latest/gpn-testnet-snapshot.tar.gz \\
      --sha256 <hex> --size <bytes> --block <height> --built 20261028-185002 \\
      --out snapshot.json

  # check a document the way an installer will (delegates to gpn_snapshot verify)
  scripts/snapshot-metadata.py verify --meta snapshot.json [--snapshot snap.tar.gz]
"""

from __future__ import annotations

import argparse
import json
import math
import pathlib
import sys

CANONICAL_PREFIX_V1 = "gpn-snapshot-v1"
CANONICAL_PREFIX_V2 = "gpn-snapshot-v2"
DEFAULT_KEY = pathlib.Path.home() / ".openclaw/credentials/gpn-release-key.txt"
DEFAULT_BIN = "gpn_snapshot"


def read_seed(path: pathlib.Path) -> bytes:
    """The raw 32-byte ed25519 seed from our key file (same shape as the
    release-manifest signer: bare hex, `#` comments, optional 0x, or seed||pub)."""
    hexline = ""
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        hexline = line
    if not hexline:
        raise SystemExit(f"{path}: no key material found")
    raw = bytes.fromhex(hexline.removeprefix("0x"))
    if len(raw) == 64:  # libsodium seed||pubkey form
        raw = raw[:32]
    if len(raw) != 32:
        raise SystemExit(f"{path}: expected a 32-byte ed25519 seed, got {len(raw)}")
    return raw


def signing_key(path: pathlib.Path):
    try:
        import nacl.signing  # PyNaCl
    except ImportError:  # pragma: no cover - operator machine
        raise SystemExit(
            "PyNaCl is required to sign (pip install pynacl); "
            "verification uses gpn_snapshot verify, not this script"
        )
    return nacl.signing.SigningKey(read_seed(path))


def canonical_text(meta: dict) -> bytes:
    """Must match `SnapshotMeta::canonical_text` in gpn_snapshot.rs.

    v2 adds the checkpoint (genesis + the block hash at `block`) and — when the
    signer supplied one — the checkpoint history: one `checkpoints=` line,
    oldest first, `block:hash` pairs comma-separated with no spaces. The line is
    emitted ONLY when the list is non-empty, so a pre-history v2 document keeps
    hashing to its original layout.
    """
    block = meta.get("block")
    fmt = str(meta.get("format") or CANONICAL_PREFIX_V1).strip()
    body = (
        f"url={str(meta['url']).strip()}\n"
        f"sha256={str(meta['sha256']).strip().lower()}\n"
        f"size={int(meta['size'])}\n"
        f"block={'' if block is None else int(block)}\n"
    )
    if fmt == CANONICAL_PREFIX_V2:
        body += (
            f"genesis={str(meta.get('genesis', '')).strip().lower()}\n"
            f"block_hash={str(meta.get('block_hash', '')).strip().lower()}\n"
        )
        anchors = meta.get("checkpoints") or []
        if anchors:
            body += "checkpoints=" + ",".join(
                f"{int(c['block'])}:{str(c['hash']).strip().lower()}"
                for c in anchors
            ) + "\n"
    body += (
        f"built={str(meta.get('built', '')).strip()}\n"
        f"signing_key={str(meta['signing_key']).strip().lower()}\n"
    )
    return (f"{fmt}\n{body}").encode()


def parse_anchors(specs: list[str]) -> list[dict]:
    """`--checkpoint BLOCK:0xHASH` (repeatable) -> ascending, deduped list."""
    seen: dict[int, str] = {}
    for spec in specs or []:
        blk, sep, h = spec.partition(":")
        if not sep:
            raise SystemExit(f"--checkpoint: want BLOCK:0xHASH, got {spec!r}")
        try:
            b = int(blk)
        except ValueError:
            raise SystemExit(f"--checkpoint: block is not a number: {blk!r}")
        h = h.strip().lower()
        if not h.startswith("0x"):
            h = "0x" + h
        if len(h) != 66 or any(c not in "0123456789abcdef" for c in h[2:]):
            raise SystemExit(f"--checkpoint: hash at {b} is not 0x + 64 hex: {h!r}")
        if b in seen and seen[b] != h:
            raise SystemExit(f"--checkpoint: two different hashes for block {b}")
        seen[b] = h
    return [{"block": b, "hash": seen[b]} for b in sorted(seen)]


def cmd_sign(a) -> int:
    sha = a.sha256.strip().lower()
    if len(sha) != 64 or any(c not in "0123456789abcdef" for c in sha):
        raise SystemExit(f"--sha256: not a 64-hex digest: {a.sha256!r}")
    if a.size <= 0:
        raise SystemExit("--size: must be > 0")
    sk = signing_key(pathlib.Path(a.key))
    genesis = (getattr(a, "genesis", "") or "").strip().lower()
    block_hash = (getattr(a, "block_hash", "") or "").strip().lower()
    # v2 only when the checkpoint is actually supplied: an unsigned/absent
    # checkpoint must not silently produce a document that claims to have one.
    fmt = CANONICAL_PREFIX_V2 if (genesis and block_hash) else CANONICAL_PREFIX_V1
    anchors = parse_anchors(getattr(a, "checkpoint", None))
    if anchors:
        if fmt != CANONICAL_PREFIX_V2 or a.block is None:
            raise SystemExit(
                "--checkpoint history needs --genesis, --block-hash and --block"
            )
        own = [c for c in anchors if c["block"] == int(a.block)]
        if not own or own[0]["hash"] != block_hash:
            raise SystemExit(
                "--checkpoint history must contain --block/--block-hash "
                f"({int(a.block)}:{block_hash}); anchors above it are allowed"
            )
    meta = {
        "format": fmt,
        "url": a.url,
        "sha256": sha,
        "size": int(a.size),
        "block": None if a.block is None else int(a.block),
        "built": a.built or "",
        "signing_key": bytes(sk.verify_key).hex(),
    }
    if fmt == CANONICAL_PREFIX_V2:
        meta["genesis"] = genesis
        meta["block_hash"] = block_hash
    if anchors:
        meta["checkpoints"] = anchors
    import nacl.signing  # noqa: F401  (already imported in signing_key)

    meta["signature"] = sk.sign(canonical_text(meta)).signature.hex()
    out = pathlib.Path(a.out)
    out.write_text(json.dumps(meta, indent=2, sort_keys=True) + "\n")
    print(f"[snapshot-meta] signed {out}")
    print(f"[snapshot-meta] url={meta['url']}")
    print(f"[snapshot-meta] block={meta['block']} size={meta['size']} sha256={sha}")
    print(f"[snapshot-meta] key={meta['signing_key']}")
    return 0


def cmd_verify(a) -> int:
    import subprocess

    cmd = [a.bin, "verify", "--meta", a.meta]
    if a.snapshot:
        cmd += ["--snapshot", a.snapshot]
    r = subprocess.run(cmd)
    return r.returncode


def human(n: int) -> str:
    if n <= 0:
        return "0 B"
    units = ["B", "KiB", "MiB", "GiB", "TiB"]
    i = min(int(math.log(n, 1024)), len(units) - 1)
    return f"{n / 1024 ** i:.1f} {units[i]}"


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = p.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("sign", help="sign snapshot.json with the release key")
    s.add_argument("--url", required=True)
    s.add_argument("--sha256", required=True)
    s.add_argument("--size", required=True, type=int)
    s.add_argument("--block", type=int)
    s.add_argument("--genesis", default="", help="v2 checkpoint: chain genesis hash")
    s.add_argument("--block-hash", dest="block_hash", default="",
                   help="v2 checkpoint: block hash AT --block")
    s.add_argument("--checkpoint", action="append", default=[],
                   metavar="BLOCK:0xHASH",
                   help="v2 checkpoint HISTORY anchor (repeatable). A node is judged at the "
                        "newest anchor at or below its height; anchors may sit above "
                        "--block (they describe the branch, not the archive), but --block "
                        "itself must be present")
    s.add_argument("--built", default="")
    s.add_argument("--key", default=str(DEFAULT_KEY))
    s.add_argument("--out", required=True)
    s.set_defaults(func=cmd_sign)

    v = sub.add_parser("verify", help="verify snapshot.json via gpn_snapshot verify")
    v.add_argument("--meta", required=True)
    v.add_argument("--snapshot")
    v.add_argument("--bin", default=DEFAULT_BIN)
    v.set_defaults(func=cmd_verify)

    a = p.parse_args(argv)
    return a.func(a)


if __name__ == "__main__":
    sys.exit(main())
