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


def canonical_text(man: dict, include_layout: bool = True) -> bytes:
    """The exact bytes that are signed and verified.

    Must match `gpn-agent/src/update.rs::ReleaseManifest::canonical_text`
    (`include_layout=True`) and `canonical_text_legacy` (`False`).
    ` layout=` and `file=` lines are **additive**: with them absent the output is
    byte-identical to the pre-bundle format, so old manifests keep verifying.

    `include_layout=False` reproduces the rendering a **pre-bundle node**
    computes. Publishing that is the only way an already-deployed fleet can
    verify the manifest at all — see `--sign-layout`.
    """
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
        line = (
            f"asset={a['os']}/{a['arch']} {a['url']} "
            f"{a['sha256'].lower()} {int(a.get('size', 0))}"
        )
        layout = (a.get("layout") or "").strip().lower()
        if include_layout and layout:
            line += f" layout={layout}"
        lines.append(line)
        if include_layout:
            files = a.get("files") or {}
            for name in sorted(files):
                lines.append(f"file={a['os']}/{a['arch']} {name} {files[name].lower()}")
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


# ─────────────────────────────────────────────────────────────────────────────
# Bundles: a release is an archive of ~60 binaries, not one executable
# ─────────────────────────────────────────────────────────────────────────────

BUNDLE_LAYOUT = "bundle"
_GH_RELEASE_RE = re.compile(
    r"^https?://github\.com/(?P<owner>[^/]+)/(?P<repo>[^/]+)/releases/download/"
    r"(?P<tag>[^/]+)/(?P<asset>[^/?#]+)$"
)
_BUNDLE_EXTS = (".tar.gz", ".tgz", ".tar", ".zip")


def looks_like_bundle(target: str) -> bool:
    from urllib.parse import urlparse

    path = urlparse(target).path.lower() if "://" in target else target.lower()
    return path.endswith(_BUNDLE_EXTS)


def _github_token() -> str:
    tok = os.environ.get("GITHUB_TOKEN") or os.environ.get("GPN_GITHUB_TOKEN") or ""
    if tok:
        return tok.strip()
    for p in (
        Path.home() / ".openclaw/credentials/github-gpn-pat.txt",
        Path.home() / ".config/gpn/github-token",
    ):
        if p.is_file():
            return p.read_text().strip()
    return ""


def github_asset_digest(url: str) -> tuple[str, int]:
    """Read `(sha256, size)` for a GitHub release asset from the API.

    Never guess a digest: the API's `digest` field is what GitHub computed over
    the bytes it serves, which is exactly what a node will hash.
    """
    import urllib.request

    m = _GH_RELEASE_RE.match(url)
    if not m:
        raise SystemExit(
            f"cannot determine the digest of {url}: it is not a GitHub release "
            "download URL. Pass --bundle-digest os/arch=SHA256:SIZE."
        )
    api = (
        f"https://api.github.com/repos/{m['owner']}/{m['repo']}"
        f"/releases/tags/{m['tag']}"
    )
    req = urllib.request.Request(api)
    req.add_header("Accept", "application/vnd.github+json")
    req.add_header("User-Agent", "gpn-release-manifest/1")
    tok = _github_token()
    if tok:
        req.add_header("Authorization", f"Bearer {tok}")
    with urllib.request.urlopen(req, timeout=30) as r:
        rel = json.loads(r.read())
    for a in rel.get("assets", []):
        if a.get("name") != m["asset"]:
            continue
        digest = a.get("digest") or ""
        if not digest.startswith("sha256:"):
            raise SystemExit(
                f"GitHub reports no sha256 digest for {m['asset']} — pass "
                "--bundle-digest os/arch=SHA256:SIZE"
            )
        print(
            f"  {m['asset']}: sha256={digest[7:]} size={a['size']} (from the GitHub API)",
            file=sys.stderr,
        )
        return digest[7:].lower(), int(a["size"])
    raise SystemExit(f"{m['asset']} is not an asset of release {m['tag']}")


def read_bundle_stamp(path: Path) -> dict:
    """Best-effort build stamp of a **local** bundle archive.

    Streams the archive (gzip/tar or zip) and looks for the same
    `gpn-build-v1 version=…` literal `build.rs` injects. Used only to
    cross-check `--version`; a remote bundle cannot be introspected, so there
    `--version` is the input.
    """
    name = path.name.lower()

    def scan(data: bytes) -> dict:
        text = data.decode("ascii", "ignore")
        for m in _STAMP_RE.finditer(text):
            if _VERSION_RE.match(m.group("version")):
                return {
                    "version": m.group("version"),
                    "sha": m.group("sha"),
                    "build_id": m.group(0),
                }
        return {}

    import gzip as _gz

    if name.endswith((".tar.gz", ".tgz", ".tar")):
        opener = _gz.open if name.endswith((".tar.gz", ".tgz")) else open
        with opener(path, "rb") as f:
            while True:
                chunk = f.read(1 << 20)
                if not chunk:
                    break
                hit = scan(chunk)
                if hit:
                    return hit
    elif name.endswith(".zip"):
        import zipfile

        with zipfile.ZipFile(path) as z:
            for info in z.infolist():
                base = Path(info.filename).name
                if base not in ("gpn_node", "gpn_node.exe", "gpn-node", "gpn-node.exe"):
                    continue
                hit = scan(z.read(info))
                if hit:
                    return hit
    return {}


def dir_file_hashes(root: Path) -> dict[str, str]:
    """Per-file sha256 of an unpacked bundle, keyed by relative posix path."""
    out: dict[str, str] = {}
    for p in sorted(root.rglob("*")):
        if p.is_file():
            out[p.relative_to(root).as_posix()] = sha256_file(p)
    return out


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

    # ── bundles: the normal shape of a GPN release ──────────────────────────
    for spec in getattr(args, "bundle", []) or []:
        if "=" not in spec:
            raise SystemExit(f"--bundle must be os/arch=URL_OR_PATH, got {spec!r}")
        platform, target = spec.split("=", 1)
        if "/" not in platform:
            raise SystemExit(f"--bundle platform must be os/arch, got {platform!r}")
        osname, arch = platform.split("/", 1)
        platform = platform.lower()

        local = None if "://" in target else Path(target)
        if local is not None:
            if not local.is_file():
                raise SystemExit(f"--bundle {spec}: {local} does not exist")
            url = f"{args.url_base.rstrip('/')}/{local.name}"
        else:
            if not looks_like_bundle(target):
                raise SystemExit(
                    f"--bundle {spec}: {target} is not a .tar.gz/.tar/.zip archive"
                )
            url = target

        override = _digest_override(args.bundle_digest, platform)
        if override:
            sha, size = override
        elif local is not None:
            sha, size = sha256_file(local), local.stat().st_size
        else:
            sha, size = github_asset_digest(url)

        files: dict[str, str] = {}
        files_dir = _files_dir(args.bundle_files, platform)
        if files_dir:
            root = Path(files_dir)
            if not root.is_dir():
                raise SystemExit(f"--bundle-files {platform}: {root} is not a directory")
            files = dir_file_hashes(root)
            if not files:
                raise SystemExit(f"--bundle-files {platform}: {root} is empty")

        if local is not None:
            stamp = read_bundle_stamp(local)
            if stamp:
                stamps.append((platform, stamp))
                print(f"  {platform}: bundle version={stamp.get('version')} sha={stamp.get('sha', '?')}")

        assets.append(
            {
                "os": osname.lower(),
                "arch": arch.lower(),
                "url": url,
                "sha256": sha.lower(),
                "size": int(size),
                "layout": BUNDLE_LAYOUT,
                **({"files": files} if files else {}),
            }
        )

    if not assets and not getattr(args, "allow_empty_assets", False):
        raise SystemExit(
            "at least one --asset or --bundle is required (or --allow-empty-assets "
            "to publish a cutoff-only manifest before any binary exists)"
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


def _digest_override(specs, platform: str):
    for spec in specs or []:
        if "=" not in spec:
            raise SystemExit(f"--bundle-digest must be os/arch=SHA256:SIZE, got {spec!r}")
        plat, val = spec.split("=", 1)
        if plat.lower() != platform.lower():
            continue
        if ":" not in val:
            raise SystemExit(f"--bundle-digest {spec}: expected SHA256:SIZE")
        sha, size = val.split(":", 1)
        sha = sha.strip().lower()
        if not re.fullmatch(r"[0-9a-f]{64}", sha):
            raise SystemExit(f"--bundle-digest {spec}: not a 64-hex sha256")
        return sha, int(size.strip())
    return None


def _files_dir(specs, platform: str):
    for spec in specs or []:
        if "=" not in spec:
            raise SystemExit(f"--bundle-files must be os/arch=DIR, got {spec!r}")
        plat, val = spec.split("=", 1)
        if plat.lower() == platform.lower():
            return val
    return ""


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
        if getattr(args, "has_remote_bundle", False):
            if not declared:
                raise SystemExit(
                    "a remote bundle cannot be introspected for its build stamp; "
                    "pass --version X.Y.Z (an assertion — the node still proves "
                    "the version from the extracted stamps before switching)"
                )
            return declared
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


def sign(man: dict, seed: bytes, include_layout: bool = True) -> dict:
    import nacl.signing  # PyNaCl

    sk = nacl.signing.SigningKey(seed)
    man = dict(man)
    man["signing_key"] = public_key_hex(sk)
    sig = sk.sign(canonical_text(man, include_layout)).signature
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
    accepted = False
    for include_layout in (True, False):
        try:
            vk.verify(canonical_text(man, include_layout), bytes.fromhex(sig_hex))
            accepted = True
            break
        except nacl.exceptions.BadSignatureError:
            continue
    if not accepted:
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
    for a in man.get("assets", []):
        layout = (a.get("layout") or "single").lower()
        nfiles = len(a.get("files") or {})
        print(
            f"  {a['os']}/{a['arch']}: layout={layout} size={a.get('size')} "
            f"sha256={a['sha256'][:16]}…"
            + (f" files={nfiles}" if nfiles else "")
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
        "--bundle",
        action="append",
        default=[],
        metavar="OS/ARCH=URL|PATH",
        help="release BUNDLE (tar.gz/zip) to publish; the digest is computed "
        "for a local path and read from the GitHub API for a release URL; repeatable",
    )
    ap.add_argument(
        "--bundle-digest",
        action="append",
        default=[],
        metavar="OS/ARCH=SHA256:SIZE",
        help="declare a bundle's digest explicitly (non-GitHub URLs)",
    )
    ap.add_argument(
        "--bundle-files",
        action="append",
        default=[],
        metavar="OS/ARCH=DIR",
        help="sign per-file sha256 for an unpacked bundle (optional; pins "
        "individual files in addition to the build stamps)",
    )
    ap.add_argument(
        "--url-base",
        default="https://gpncoin.ai/dl",
        help="public base URL the artifacts are served from",
    )
    ap.add_argument(
        "--sign-layout",
        action="store_true",
        help="sign the `layout` marker and per-file hashes (extended canonical "
        "rendering). Leave OFF while the fleet still runs pre-bundle nodes "
        "(<= 0.2.0): they compute the legacy text and would reject the manifest. "
        "Safe from 0.3.0 on. A bundle's layout is decidable from its signed URL "
        "either way, so OFF loses no protection against a downgrade.",
    )
    ap.add_argument(
        "--published-at", default="", help="ISO-8601 UTC; default now")
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
    args.has_remote_bundle = any(
        "://" in spec.split("=", 1)[-1] for spec in (getattr(args, "bundle", []) or [])
    )

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
    if not args.sign_layout and any(a.get("files") for a in unsigned["assets"]):
        raise SystemExit(
            "per-file hashes (--bundle-files) must be signed; add --sign-layout "
            "(only once every deployed node is >= 0.3.0)"
        )
    man = sign(unsigned, seed, include_layout=args.sign_layout)
    if not args.sign_layout and any(a.get("layout") for a in unsigned["assets"]):
        print(
            "  note: `layout` is published as JSON but NOT signed (legacy rendering) "
            "so pre-bundle nodes can still verify this manifest; the signed URL "
            "decides the layout.",
            file=sys.stderr,
        )

    if args.print_canonical:
        sys.stderr.write(canonical_text(man, args.sign_layout).decode())

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
