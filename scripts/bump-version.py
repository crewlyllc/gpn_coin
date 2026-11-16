#!/usr/bin/env python3
"""Bump the GPN version across every crate, consistently, and start a changelog entry.

The version is carried in **many** `Cargo.toml` files (the agent, the chain
workspace, every pallet, the runtime, the shield crate), and in `network.json`
(`latest_version`, which is compiled into the binary). A release is only
coherent if they all agree, and a hand-edited bump always misses one. This
script is the one place that knows the whole set.

    scripts/bump-version.py patch                # 0.2.0 -> 0.2.1
    scripts/bump-version.py minor                # 0.2.1 -> 0.3.0
    scripts/bump-version.py major                # 0.3.0 -> 1.0.0
    scripts/bump-version.py --set 0.2.0          # explicit (refuses to go backwards)
    scripts/bump-version.py --check              # verify the tree is in lockstep
    scripts/bump-version.py minor --dry-run      # show, change nothing

The **reference** version is `gpn-agent/Cargo.toml` — that crate builds
`gpn_node`, the binary the signed release manifest and the updater talk about.

What it does **not** do — on purpose:

* it does **not** touch `spec_version` (`gpn-node/runtime/src/lib.rs`). A crate
  version is a *release* label; `spec_version` is a *consensus* marker. Only a
  runtime change that alters behaviour may raise it, and that is a separate,
  deliberate edit (`docs/updates.md`).
* it does not tag, build, sign or publish anything. See the release checklist in
  `docs/updates.md`.

Exit codes: 0 success (including "already at that version"), 1 error, 2 usage.

Stdlib only; idempotent (bumping to the version already in the tree is a no-op
that changes no file and adds no changelog entry).
"""

from __future__ import annotations

import argparse
import datetime as _dt
import re
import sys
from pathlib import Path

#: The crate whose version *is* the release version.
REFERENCE = "gpn-agent/Cargo.toml"

#: Directories never searched for `Cargo.toml`.
SKIP_DIRS = {".git", "target", "node_modules", "__pycache__", ".cargo"}

#: `MAJOR.MINOR.PATCH`, numeric only. No `v` prefix, no pre-release / build
#: metadata: `update::is_newer` compares numerically, and a `-rc1` suffix would
#: silently sort *below* the release it precedes.
SEMVER = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")

#: The shipped network registry. Its `latest_version` is *derived*: it must be
#: the same string this tree builds, because the registry is `include_str!`d
#: into the binary and `network_registry::classify` prints it as "registry
#: latest" to every operator. It sat at 0.3.11 for 33 releases (found
#: 2026-11-13) precisely because nothing tied it to the crate versions.
REGISTRY = "network.json"
LATEST_VERSION_LINE = re.compile(r'("latest_version"\s*:\s*)"([^"]*)"')

#: Cargo lockfiles also carry the release label for every first-party crate.
#: They are *derived* (cargo rewrites them on the next build), which is exactly
#: why a stale one hides: the worktree goes dirty instead of failing, and only
#: a clean checkout with `--locked` reports it. It has been missed twice by
#: hand — `5ee8632` synced 0.3.43 -> 0.3.44 after the fact, and 0.3.49 shipped
#: with gpn-agent/Cargo.lock still saying 0.3.48 — so the audit covers them.
LOCKFILE = "Cargo.lock"
LOCK_NAME_LINE = re.compile(r'^name\s*=\s*"([^"]*)"\s*$')
LOCK_VERSION_LINE = re.compile(r'^version\s*=\s*"([^"]*)"\s*$')
PKG_NAME_LINE = re.compile(r'^\s*name\s*=\s*"([^"]*)"\s*$')

CHANGELOG = "CHANGELOG.md"
CHANGELOG_HEADER = """# Changelog

All notable changes to GPN are recorded here. The version in this file is the
one `gpn_node --version` prints, the one the signed release manifest publishes,
and the one the updater compares (`docs/updates.md`).

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions are [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with
the GPN-specific meaning of each level documented in `docs/updates.md`.
"""


# ─────────────────────────────────────────────────────────────────────────────
# Version arithmetic
# ─────────────────────────────────────────────────────────────────────────────


def parse(version: str) -> tuple[int, int, int]:
    m = SEMVER.match((version or "").strip())
    if not m:
        raise SystemExit(
            f"invalid version {version!r}: expected MAJOR.MINOR.PATCH "
            "(digits only, no leading zeros, no 'v', no -rc/+meta suffix)"
        )
    return tuple(int(g) for g in m.groups())  # type: ignore[return-value]


def render(t: tuple[int, int, int]) -> str:
    return ".".join(str(n) for n in t)


def bump(current: tuple[int, int, int], level: str) -> tuple[int, int, int]:
    major, minor, patch = current
    if level == "major":
        return (major + 1, 0, 0)
    if level == "minor":
        return (major, minor + 1, 0)
    if level == "patch":
        return (major, minor, patch + 1)
    raise SystemExit(f"unknown level {level!r}")


# ─────────────────────────────────────────────────────────────────────────────
# Cargo.toml discovery / rewriting
# ─────────────────────────────────────────────────────────────────────────────


class Manifest:
    """One `Cargo.toml`, with the line number of the version it carries."""

    __slots__ = ("path", "table", "version", "lineno")

    def __init__(self, path: Path, table: str, version: str, lineno: int):
        self.path = path
        self.table = table
        self.version = version
        self.lineno = lineno  # 0-based index into the file's lines

    def __repr__(self) -> str:  # pragma: no cover - debugging aid
        return f"<{self.path} [{self.table}] {self.version}>"


VERSION_LINE = re.compile(r'^\s*version\s*=\s*"([^"]*)"\s*(?:#.*)?$')
TABLE_LINE = re.compile(r"^\s*\[([^\]]+)\]\s*(?:#.*)?$")


def scan_manifest(path: Path, root: Path) -> Manifest | None:
    """Find the release version line in one `Cargo.toml`.

    Only `[package]` and `[workspace.package]` count. `version.workspace = true`
    (or `version = { workspace = true }`) is *inherited*, so it is not a source
    of truth and is skipped — the workspace root is.
    """
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as e:
        raise SystemExit(f"{path}: {e}")

    table = ""
    for i, line in enumerate(lines):
        m = TABLE_LINE.match(line)
        if m:
            table = m.group(1).strip()
            continue
        if table not in ("package", "workspace.package"):
            continue
        if re.match(r"^\s*version\.workspace\s*=\s*true", line):
            return None
        if re.match(r"^\s*version\s*=\s*\{", line):
            return None
        m = VERSION_LINE.match(line)
        if m:
            return Manifest(path.relative_to(root), table, m.group(1), i)
    return None


def discover(root: Path) -> list[Manifest]:
    found: list[Manifest] = []
    for p in sorted(root.rglob("Cargo.toml")):
        if any(part in SKIP_DIRS for part in p.relative_to(root).parts):
            continue
        m = scan_manifest(p, root)
        if m:
            found.append(m)
    return found


def reference(manifests: list[Manifest]) -> Manifest:
    for m in manifests:
        if str(m.path) == REFERENCE:
            return m
    raise SystemExit(f"{REFERENCE} not found — is --repo the repo root?")


class LockEntry:
    """One first-party crate's version inside a `Cargo.lock`."""

    __slots__ = ("path", "name", "version", "lineno")

    def __init__(self, path: Path, name: str, version: str, lineno: int):
        self.path = path
        self.name = name
        self.version = version
        self.lineno = lineno  # 0-based index into the file's lines

    def __repr__(self) -> str:  # pragma: no cover - debugging aid
        return f"<{self.path}:{self.name} {self.version}>"


def package_names(root: Path) -> set[str]:
    """Every crate this tree defines (so a same-named crates.io crate is left alone)."""
    names: set[str] = set()
    for p in sorted(root.rglob("Cargo.toml")):
        if any(part in SKIP_DIRS for part in p.relative_to(root).parts):
            continue
        table = ""
        for line in p.read_text(encoding="utf-8").splitlines():
            m = TABLE_LINE.match(line)
            if m:
                table = m.group(1).strip()
                continue
            if table != "package":
                continue
            m = PKG_NAME_LINE.match(line)
            if m:
                names.add(m.group(1))
                break
    return names


def lock_entries(root: Path) -> list[LockEntry]:
    """First-party crate versions recorded in every `Cargo.lock` in the tree."""
    ours = package_names(root)
    entries: list[LockEntry] = []
    for p in sorted(root.rglob(LOCKFILE)):
        rel = p.relative_to(root)
        if any(part in SKIP_DIRS for part in rel.parts):
            continue
        name = None
        for i, line in enumerate(p.read_text(encoding="utf-8").splitlines()):
            if line.strip() == "[[package]]":
                name = None
                continue
            if name is None:
                m = LOCK_NAME_LINE.match(line)
                if m:
                    name = m.group(1) if m.group(1) in ours else None
                continue
            m = LOCK_VERSION_LINE.match(line)
            if m:
                entries.append(LockEntry(rel, name, m.group(1), i))
                name = None
    return entries


def write_lock_version(root: Path, e: LockEntry, new: str) -> None:
    """Rewrite one lockfile version line, preserving every other byte."""
    path = root / e.path
    lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
    old_line = lines[e.lineno]
    eol = "\n" if old_line.endswith("\n") else ""
    body = old_line.rstrip("\r\n")
    lines[e.lineno] = LOCK_VERSION_LINE.sub(lambda _: f'version = "{new}"', body) + eol
    path.write_text("".join(lines), encoding="utf-8")


def registry_version(root: Path) -> str | None:
    """`latest_version` from the shipped registry (None when there is no file).

    A registry that exists but has no `latest_version` is a schema bug, not a
    "nothing to check": fail loudly rather than skip the audit.
    """
    path = root / REGISTRY
    if not path.exists():
        return None
    m = LATEST_VERSION_LINE.search(path.read_text(encoding="utf-8"))
    if not m:
        raise SystemExit(f"{path}: no \"latest_version\" field to audit")
    return m.group(2)


def write_registry_version(root: Path, new: str) -> bool:
    """Set `latest_version` in the registry, preserving every other byte."""
    path = root / REGISTRY
    if not path.exists():
        return False
    text = path.read_text(encoding="utf-8")
    new_text = LATEST_VERSION_LINE.sub(
        lambda m: f'{m.group(1)}"{new}"', text, count=1
    )
    if new_text == text:
        return False
    path.write_text(new_text, encoding="utf-8")
    return True


def write_version(root: Path, m: Manifest, new: str) -> None:
    """Rewrite one version line, preserving the rest of the file byte-for-byte."""
    path = root / m.path
    lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
    old_line = lines[m.lineno]
    eol = "\n" if old_line.endswith("\n") else ""
    body = old_line.rstrip("\r\n")
    new_body = VERSION_LINE.sub(lambda _: f'version = "{new}"', body)
    if new_body == body:  # pragma: no cover - scan_manifest guarantees a match
        raise SystemExit(f"{path}: could not rewrite line {m.lineno + 1}")
    lines[m.lineno] = new_body + eol
    path.write_text("".join(lines), encoding="utf-8")


# ─────────────────────────────────────────────────────────────────────────────
# Changelog
# ─────────────────────────────────────────────────────────────────────────────


def changelog_entry(version: str, old: str, level: str, today: str) -> str:
    how = (
        f"`scripts/bump-version.py {level}`"
        if level
        else "`scripts/bump-version.py --set`"
    )
    return (
        f"## [{version}] - {today}\n"
        f"\n"
        f"Bumped from {old} ({how}). Describe the release before publishing it.\n"
        f"\n"
        f"### Added\n"
        f"- _TODO_\n"
        f"\n"
        f"### Changed\n"
        f"- _TODO_\n"
        f"\n"
        f"### Fixed\n"
        f"- _TODO_\n"
    )


def update_changelog(root: Path, version: str, old: str, level: str) -> bool:
    """Insert a stub entry for `version`. Returns True if the file changed.

    Newest first (a changelog is read top-down), and idempotent: an existing
    `## [version]` heading is left alone, so a re-run after a failed commit does
    not stack duplicates.
    """
    path = root / CHANGELOG
    entry = changelog_entry(version, old, level, _dt.date.today().isoformat())

    if not path.exists():
        path.write_text(CHANGELOG_HEADER + "\n" + entry, encoding="utf-8")
        return True

    text = path.read_text(encoding="utf-8")
    if re.search(rf"^##\s*\[?{re.escape(version)}\]?\s", text, re.MULTILINE):
        return False

    m = re.search(r"^##\s", text, re.MULTILINE)
    if m:
        text = text[: m.start()] + entry + "\n" + text[m.start() :]
    else:
        text = text.rstrip("\n") + "\n\n" + entry
    path.write_text(text, encoding="utf-8")
    return True


# ─────────────────────────────────────────────────────────────────────────────
# CLI
# ─────────────────────────────────────────────────────────────────────────────


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        description="Bump the GPN version in every Cargo.toml (+ a CHANGELOG stub).",
        epilog=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    ap.add_argument(
        "level", nargs="?", choices=["major", "minor", "patch"], help="bump level"
    )
    ap.add_argument("--set", metavar="X.Y.Z", help="set an explicit version")
    ap.add_argument(
        "--check",
        action="store_true",
        help="verify every manifest carries the same version and exit (no changes)",
    )
    ap.add_argument("--dry-run", action="store_true", help="print the plan, change nothing")
    ap.add_argument(
        "--allow-downgrade",
        "--force",
        dest="allow_downgrade",
        action="store_true",
        help="allow --set to move backwards (abandoning a release); never needed "
        "for a normal bump",
    )
    ap.add_argument("--no-changelog", action="store_true", help="do not touch CHANGELOG.md")
    ap.add_argument(
        "--repo",
        "--root",
        dest="repo",
        default=".",
        help="repo root (default: cwd)",
    )
    args = ap.parse_args(argv)

    root = Path(args.repo).resolve()
    manifests = discover(root)
    ref = reference(manifests)
    current = parse(ref.version)
    drifted = [m for m in manifests if m.version != ref.version]

    # ── --check: lockstep audit (CI, and the release checklist) ──
    if args.check:
        for m in manifests:
            mark = "ok  " if m.version == ref.version else "DRIFT"
            print(f"  {mark} {m.version:<10} {m.path} [{m.table}]")
        reg = registry_version(root)
        reg_drift = reg is not None and reg != ref.version
        if reg is not None:
            mark = "ok  " if not reg_drift else "DRIFT"
            print(f"  {mark} {reg:<10} {REGISTRY} [latest_version]")
        locks = lock_entries(root)
        lock_drift = [e for e in locks if e.version != ref.version]
        for e in locks:
            mark = "ok  " if e.version == ref.version else "DRIFT"
            print(f"  {mark} {e.version:<10} {e.path}:{e.lineno + 1} [{e.name}]")
        if drifted or reg_drift or lock_drift:
            print(
                f"\nDRIFT: the tree is not in lockstep with {ref.path} "
                f"({ref.version}): {len(drifted)} of {len(manifests)} manifest(s) "
                f"disagree, {len(lock_drift)} lockfile entr(ies) disagree.",
                file=sys.stderr,
            )
            if reg_drift:
                print(
                    f"  {REGISTRY} latest_version is {reg!r}, this tree builds "
                    f"{ref.version!r} — the registry is shipped inside the binary, "
                    "so it reports a version nobody can install.",
                    file=sys.stderr,
                )
            if lock_drift:
                print(
                    "  "
                    + ", ".join(f"{e.path}:{e.name}={e.version}" for e in lock_drift)
                    + f" — a lockfile still records an older label, so a clean "
                    f"checkout built with `--locked` fails and any other build "
                    f"silently rewrites the file ({ref.version} expected).",
                    file=sys.stderr,
                )
            print(
                f"Fix with: scripts/bump-version.py --set {ref.version}", file=sys.stderr
            )
        else:
            print(
                f"lockstep: {len(manifests)} manifest(s) and {len(locks)} lockfile "
                "entr(ies) agree"
            )
        # Always last, and always parseable: `gpn version: <x.y.z>`.
        print(f"gpn version: {ref.version}")
        return 1 if (drifted or reg_drift or lock_drift) else 0

    if bool(args.level) == bool(args.set):
        ap.error("give exactly one of {major,minor,patch} or --set X.Y.Z")

    # ── work out the target ──
    if args.set is not None:
        target = parse(args.set)
        if target < current and not args.allow_downgrade:
            ap.error(
                f"refusing to go backwards: {render(current)} -> {render(target)} "
                "(pass --allow-downgrade if that is really what you want)"
            )
        level = ""
        # --set is also the repair tool: it converges a drifted tree, so every
        # manifest is rewritten, not just the ones already at the reference.
        selected = manifests
    else:
        level = args.level
        if drifted:
            ap.error(
                "ambiguous: the manifests disagree "
                f"({', '.join(f'{m.path}={m.version}' for m in drifted[:3])}"
                f"{'…' if len(drifted) > 3 else ''}); a level bump needs one base "
                f"version. Converge first: scripts/bump-version.py --set {ref.version}"
            )
        target = bump(current, level)
        selected = manifests

    new = render(target)
    stale_locks = [e for e in lock_entries(root) if e.version != new]
    if new == ref.version and not stale_locks:
        print(f"already at {new} — nothing to do")
        return 0
    if new == ref.version:
        # Repair path: the manifests already agree, only a derived lockfile lags.
        print(f"{new} — manifests agree, syncing {len(stale_locks)} lockfile entr(ies)")
        for e in stale_locks:
            print(f"  {e.path}:{e.name}  {e.version} -> {new}")
        if args.dry_run:
            print("\n--dry-run: no file written")
            return 0
        for e in stale_locks:
            write_lock_version(root, e, new)
        print(f"updated {len(stale_locks)} lockfile entr(ies) -> {new}")
        return 0

    old = ref.version
    print(f"{old} -> {new}" + (f"  ({level})" if level else "  (--set)"))
    for m in selected:
        print(f"  {m.path} [{m.table}]  {m.version} -> {new}")
    reg_now = registry_version(root)
    if reg_now is not None and reg_now != new:
        print(f"  {REGISTRY} [latest_version]  {reg_now} -> {new}")
    for e in stale_locks:
        print(f"  {e.path}:{e.name}  {e.version} -> {new}")

    if args.dry_run:
        print(f"\n--dry-run: no file written (would set {len(selected)} manifest(s) to {new})")
        return 0

    for m in selected:
        write_version(root, m, new)
    print(f"updated {len(selected)} manifest(s) -> {new}")
    if write_registry_version(root, new):
        print(f"updated {REGISTRY} latest_version -> {new}")
    relocked = stale_locks
    for e in relocked:
        write_lock_version(root, e, new)
    if relocked:
        print(
            f"updated {len(relocked)} lockfile entr(ies) -> {new} "
            f"({', '.join(f'{e.path}:{e.name}' for e in relocked)})"
        )

    if not args.no_changelog:
        if update_changelog(root, new, old, level):
            print(f"added a {CHANGELOG} entry for {new} (fill in the TODO sections)")
        else:
            print(f"{CHANGELOG} already has an entry for {new} — left alone")

    print(
        "\nnext: edit CHANGELOG.md, then build + test, then follow the release\n"
        "checklist in docs/updates.md (the manifest is generated from the artifacts)."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
