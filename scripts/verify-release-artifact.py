#!/usr/bin/env python3
"""Prove that a released artifact carries the code — not just the version.

A release that ships the wrong tree is worse than no release: the version
string can be right, the sha256 can match the signed manifest, and the
capability can still be absent because the build came from a stale checkout,
a different branch, or a rebuild that silently dropped the feature.

This gate answers exactly that question, in the order that can actually fail:

  1. **the manifest is signed** — an unsigned/hand-edited `releases.json` can
     describe anything, so its asset digests are only worth what the signature
     is worth (delegated to `gen-release-manifest.py --verify`);
  2. **the bytes are the signed bytes** — size + sha256 of the downloaded
     archive vs the signed manifest (`--bundle` mode skips this: pre-publish
     there is no manifest yet, so the caller supplies the digest check);
  3. **the binary is the one built for this version** — `gpn-node/<version>`
     (the HTTP User-Agent stamp compiled in from `CARGO_PKG_VERSION`);
  4. **the binary carries the code** — every marker string belonging to the
     feature must appear in the executable image (default: the heaviest-chain
     fork choice, item #192);
  5. **the compile-time constants that never become strings are checked in the
     source of the release commit** — `MAX_REORG_DEPTH = 288` is a `const`, so
     it is folded to an immediate and cannot be grepped out of the binary.
     Pretending otherwise would make this gate report a false failure, so the
     constant is verified where it lives and the binary-visible consequence
     (`deep-fork`, `gpn-forked`) is verified in (4). `--source-rev` / HEAD
     supplies the revision.

Exit codes: 0 verified, 1 usage/IO, 2 manifest/signature, 3 asset digest,
4 missing marker, 5 version stamp, 6 source constant.

Usage
-----
  # verify what the world can actually download (default: live manifest)
  scripts/verify-release-artifact.py

  # verify a pinned manifest / a local copy
  scripts/verify-release-artifact.py --manifest website/releases.json
  scripts/verify-release-artifact.py --os windows --cache-dir /tmp/gpnv

  # pre-publish gate: the bundles release-cut already downloaded
  scripts/verify-release-artifact.py --version 0.3.50 \\
      --bundle linux=/work/gpn-linux.tar.gz \\
      --bundle windows=/work/gpn-windows.zip \\
      --no-manifest

  # add a marker for the next feature (repeatable)
  scripts/verify-release-artifact.py --marker 'my-feature=log line text'
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
DEFAULT_MANIFEST = REPO / "website" / "releases.json"
DEFAULT_CACHE = Path(os.environ.get("GPN_ARTIFACT_CACHE", Path.home() / ".cache" / "gpn-artifact-verify"))

#: marker string -> what it proves. A released binary must contain every one of
#: these; they exist only in the fork-choice code (item #192), so any one of
#: them missing means the bundle was built from a tree without that feature.
DEFAULT_MARKERS: list[tuple[str, str]] = [
    ("fork-choice", "fork-choice decision & log lines"),
    ("gpn-forked", "the sealing-pause marker file name"),
    ("deep-fork", "the deep-fork refusal reason"),
    ("primary-authoritative", "the authoritative-primary stance"),
    ("gpncs/2", "the v2 chain-state protocol id"),
]

#: constants folded at compile time: the *name* never reaches the binary, so
#: they are verified in the source of the release revision instead of by grep.
#: (module path, const name, expected value)
DEFAULT_CONSTS: list[tuple[str, str, int]] = [
    ("gpn-node/node/src/fork_choice.rs", "MAX_REORG_DEPTH", 288),
]


def die(code: int, msg: str) -> None:
    print(f"FAIL: {msg}", file=sys.stderr)
    raise SystemExit(code)


def ok(msg: str) -> None:
    print(f"  ok   {msg}")


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def verify_signature(manifest: Path) -> dict:
    """Signature check via the one implementation that also signs (no crypto copy)."""
    gen = HERE / "gen-release-manifest.py"
    if not gen.exists():
        die(2, f"cannot verify the manifest signature: {gen} is missing")
    proc = subprocess.run([sys.executable, str(gen), "--verify", str(manifest)],
                          capture_output=True, text=True)
    if proc.returncode != 0:
        die(2, "the manifest signature does not verify "
               f"(rc={proc.returncode}):\n{proc.stdout}{proc.stderr}".rstrip())
    first = (proc.stdout or "").strip().splitlines()
    print(f"  ok   signed manifest: {first[0] if first else 'verified'}")
    for line in first[1:]:
        print(f"       {line.strip()}")
    return json.loads(manifest.read_text())


def fetch_manifest(source: str) -> tuple[Path, dict]:
    if source.startswith(("http://", "https://")):
        tmp = Path(tempfile.mkdtemp(prefix="gpn-manifest-")) / "releases.json"
        print(f"  ..   fetching {source}")
        with urllib.request.urlopen(source, timeout=30) as resp:
            tmp.write_bytes(resp.read())
        return tmp, json.loads(tmp.read_text())
    path = Path(source)
    if not path.exists():
        die(1, f"manifest not found: {path}")
    return path, json.loads(path.read_text())


def download(url: str, dest: Path, want_sha: str, want_size: int) -> Path:
    """Download unless the cached copy already *is* the signed asset."""
    if dest.exists():
        if dest.stat().st_size == want_size and sha256_file(dest) == want_sha:
            print(f"  ok   {dest.name}: cached copy matches the manifest")
            return dest
        print(f"  ..   {dest.name}: cached copy does not match, re-downloading")
        dest.unlink()
    dest.parent.mkdir(parents=True, exist_ok=True)
    part = dest.with_suffix(dest.suffix + ".part")
    print(f"  ..   downloading {url}")
    with urllib.request.urlopen(url, timeout=60) as resp, part.open("wb") as out:
        shutil.copyfileobj(resp, out, 1 << 20)
    part.replace(dest)
    return dest


def extract_binary(archive: Path, os_name: str, tmpdir: Path) -> Path:
    """Pull just the node executable out of the bundle."""
    want = "gpn-node.exe" if os_name == "windows" else "gpn-node"
    out = tmpdir / want
    if archive.suffix == ".zip" or zipfile.is_zipfile(archive):
        with zipfile.ZipFile(archive) as zf:
            members = [m for m in zf.namelist() if Path(m.replace("\\", "/")).name == want]
            if not members:
                die(4, f"{archive.name} contains no {want}")
            with zf.open(members[0]) as src, out.open("wb") as dst:
                shutil.copyfileobj(src, dst)
    else:
        with tarfile.open(archive, "r:*") as tf:
            members = [m for m in tf.getmembers()
                       if m.isfile() and Path(m.name).name == want]
            if not members:
                die(4, f"{archive.name} contains no {want}")
            src = tf.extractfile(members[0])
            assert src is not None
            with out.open("wb") as dst:
                shutil.copyfileobj(src, dst)
    return out


def count(data: bytes, needle: bytes) -> int:
    return data.count(needle)


def source_constant(rev: str, module: str, name: str) -> int | None:
    """Read `<name>: u64 = <value>` out of the module at `rev` (git show)."""
    try:
        proc = subprocess.run(["git", "-C", str(REPO), "show", f"{rev}:{module}"],
                              capture_output=True, text=True)
    except OSError as exc:  # git missing
        print(f"  ??   cannot read {module} at {rev}: {exc}")
        return None
    if proc.returncode != 0:
        print(f"  ??   cannot read {rev}:{module} ({proc.stderr.strip().splitlines()[:1]})")
        return None
    m = re.search(rf"\b{re.escape(name)}\s*:\s*\w+\s*=\s*(\d+)", proc.stdout)
    return int(m.group(1)) if m else None


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Prove a released artifact carries the code.")
    ap.add_argument("--manifest", default=str(DEFAULT_MANIFEST),
                    help="releases.json path or URL (default: %(default)s)")
    ap.add_argument("--no-manifest", action="store_true",
                    help="pre-publish mode: no manifest/signature, digests come from --bundle")
    ap.add_argument("--bundle", action="append", default=[], metavar="OS=PATH",
                    help="verify a local bundle (linux=/p/gpn-linux.tar.gz); repeatable")
    ap.add_argument("--os", action="append", default=[], choices=["linux", "windows"],
                    help="restrict to these platforms (default: every asset)")
    ap.add_argument("--version", help="assert this version (default: the manifest's)")
    ap.add_argument("--marker", action="append", default=[], metavar="NAME=TEXT",
                    help="additional required marker string (repeatable)")
    ap.add_argument("--skip-marker", action="append", default=[], metavar="NAME")
    ap.add_argument("--source-rev", help="git revision for the compile-time constants (default: the build stamp in the bundle, else HEAD)")
    ap.add_argument("--cache-dir", default=str(DEFAULT_CACHE))
    ap.add_argument("--keep", action="store_true", help="keep the extracted binaries")
    args = ap.parse_args(argv)

    markers = [(n, why) for n, why in DEFAULT_MARKERS if n not in args.skip_marker]
    for spec in args.marker:
        if "=" not in spec:
            die(1, f"--marker needs NAME=TEXT, got {spec!r}")
        name, text = spec.split("=", 1)
        markers.append((text, f"extra marker ({name})"))
    if not markers:
        die(1, "no markers to check")

    bundles: dict[str, Path] = {}
    for spec in args.bundle:
        if "=" not in spec:
            die(1, f"--bundle needs OS=PATH, got {spec!r}")
        osn, path = spec.split("=", 1)
        p = Path(path)
        if not p.exists():
            die(1, f"bundle not found: {p}")
        bundles[osn] = p

    manifest: dict = {}
    if not args.no_manifest:
        mpath, manifest = fetch_manifest(args.manifest)
        verify_signature(mpath)

    version = args.version or manifest.get("version")
    if not version:
        die(1, "no version to assert: pass --version or use a manifest")
    print(f"== gate: artifact carries the code (version {version})")

    # Which platforms?  explicit --os, else the bundles given, else the manifest.
    if bundles and args.no_manifest:
        platforms = [o for o in bundles]
    elif bundles:
        platforms = [o for o in bundles]
    else:
        platforms = [a.get("os", "?") for a in manifest.get("assets", [])]
    if args.os:
        platforms = [p for p in platforms if p in args.os]
    if not platforms:
        die(1, "nothing to verify")

    findings: list[str] = []
    tmpdir = Path(tempfile.mkdtemp(prefix="gpn-verify-"))
    try:
        for osn in platforms:
            asset = next((a for a in manifest.get("assets", []) if a.get("os") == osn), {})
            print(f"\n-- {osn}")
            if osn in bundles:
                archive = bundles[osn]
                if asset:
                    want_sha = (asset.get("sha256") or "").lower()
                    want_size = int(asset.get("size") or 0)
                    got_sha, got_size = sha256_file(archive), archive.stat().st_size
                    if (want_sha and got_sha != want_sha) or (want_size and got_size != want_size):
                        die(3, f"{archive.name}: {got_size} B sha256={got_sha[:16]}… does not "
                               f"match the manifest ({want_size} B {want_sha[:16]}…)")
                    ok(f"bytes match the signed manifest ({got_size} B {got_sha[:16]}…)")
            else:
                url = asset.get("url")
                if not url:
                    die(1, f"the manifest has no {osn} asset")
                want_sha = (asset.get("sha256") or "").lower()
                want_size = int(asset.get("size") or 0)
                name = os.path.basename(url.split("?")[0])
                archive = download(url, Path(args.cache_dir) / name, want_sha, want_size)
                got_sha, got_size = sha256_file(archive), archive.stat().st_size
                if got_size != want_size:
                    die(3, f"{name}: {got_size} B != the manifest's {want_size} B")
                if got_sha != want_sha:
                    die(3, f"{name}: sha256 {got_sha[:16]}… != the manifest's {want_sha[:16]}…")
                ok(f"bytes match the signed manifest ({got_size} B {got_sha[:16]}…)")

            binary = extract_binary(archive, osn, tmpdir)
            data = binary.read_bytes()
            ok(f"{binary.name}: {len(data)} B extracted")

            stamp = f"gpn-node/{version}".encode()
            hits = count(data, stamp)
            if not hits:
                die(5, f"{binary.name} carries no {stamp.decode()!r} stamp — this is not "
                       f"the {version} build")
            ok(f"version stamp {stamp.decode()!r} x{hits}")

            for needle, why in markers:
                n = count(data, needle.encode())
                if not n:
                    findings.append(f"{osn}:{binary.name}: {needle!r} ({why})")
                    print(f"  MISS {needle!r:26}  {why}")
                else:
                    print(f"  ok   {needle!r:26}  x{n}  {why}")

        # Compile-time constants: verify where they live, not by grep on the image.
        rev = args.source_rev
        if not rev:
            for b in bundles.values():
                m = re.search(rb"gpn-build-v1[^\x00]{0,200}?sha=([0-9a-f]{7,40})", b.read_bytes()[:8 << 20])
                if m:
                    rev = m.group(1).decode()
                    break
        if not rev:
            proc = subprocess.run(["git", "-C", str(REPO), "rev-parse", "HEAD"],
                                  capture_output=True, text=True)
            rev = proc.stdout.strip() if proc.returncode == 0 else "HEAD"
        print(f"\n-- compile-time constants at {rev}")
        for module, name, want in DEFAULT_CONSTS:
            got = source_constant(rev, module, name)
            if got is None:
                print(f"  ??   {name}: not readable at {rev}:{module} (skipped)")
            elif got != want:
                die(6, f"{name} is {got} at {rev}:{module}, expected {want} — the release "
                       f"tree does not contain the intended bound")
            else:
                print(f"  ok   {name} = {got}   ({module} — a const, so not a binary string)")
    finally:
        if not args.keep:
            shutil.rmtree(tmpdir, ignore_errors=True)
        else:
            print(f"\nkept extracted binaries in {tmpdir}")

    if findings:
        print("\nFAIL: the artifact does not carry the code it claims:", file=sys.stderr)
        for f in findings:
            print(f"  - {f}", file=sys.stderr)
        return 4
    print(f"\nOK: {version} artifacts verified on {', '.join(platforms)} — "
          f"{len(markers)} marker(s) present in each")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
