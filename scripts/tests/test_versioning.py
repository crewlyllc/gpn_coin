#!/usr/bin/env python3
"""Tests for the versioning tooling: `bump-version.py` and `gen-release-manifest.py`.

Stdlib `unittest` only (plus PyNaCl, which the manifest signer already needs).

    /usr/bin/python3 -m unittest discover -s scripts/tests -v
    /usr/bin/python3 scripts/tests/test_versioning.py
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
BUMP = REPO / "scripts" / "bump-version.py"
GEN = REPO / "scripts" / "gen-release-manifest.py"
PY = sys.executable or "/usr/bin/python3"


def load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


gen = load_module(GEN, "gen_release_manifest")


# ─────────────────────────────────────────────────────────────────────────────
# A throwaway repo that looks enough like GPN for the bump script
# ─────────────────────────────────────────────────────────────────────────────

AGENT_TOML = """[package]
name = "gpn-agent"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = "1"
"""

NODE_TOML = """[workspace]
members = ["node"]

[workspace.package]
version = "0.1.0"
edition = "2021"
"""

NODE_PKG_TOML = """[package]
name = "gpn-node"
version = "0.1.0"
edition = "2021"
"""

INHERIT_TOML = """[package]
name = "gpn-inherit"
version.workspace = true
edition = "2021"
"""


class FakeRepo:
    def __init__(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.write("gpn-agent/Cargo.toml", AGENT_TOML)
        self.write("gpn-node/Cargo.toml", NODE_TOML)
        self.write("gpn-node/node/Cargo.toml", NODE_PKG_TOML)
        self.write("gpn-node/pallets/x/Cargo.toml", NODE_PKG_TOML.replace("gpn-node", "gpn-x"))
        # Not a version declaration: must not be rewritten or counted.
        self.write("gpn-node/elsewhere/Cargo.toml", INHERIT_TOML)
        self.changelog = self.root / "CHANGELOG.md"

    def write(self, rel: str, body: str) -> Path:
        p = self.root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(body)
        return p

    def version_of(self, rel: str) -> str:
        for line in (self.root / rel).read_text().splitlines():
            if line.startswith("version = "):
                return line.split('"')[1]
        raise AssertionError(f"no version in {rel}")

    def run(self, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [PY, str(BUMP), "--root", str(self.root), *args],
            capture_output=True,
            text=True,
        )

    def cleanup(self) -> None:
        self._tmp.cleanup()


class BumpVersionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = FakeRepo()
        self.addCleanup(self.repo.cleanup)

    def assert_all_versions(self, expect: str) -> None:
        for rel in (
            "gpn-agent/Cargo.toml",
            "gpn-node/Cargo.toml",
            "gpn-node/node/Cargo.toml",
            "gpn-node/pallets/x/Cargo.toml",
        ):
            self.assertEqual(self.repo.version_of(rel), expect, rel)

    def test_check_reports_the_version(self) -> None:
        r = self.repo.run("--check")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("gpn version: 0.1.0", r.stdout)
        # inherited `version.workspace = true` crates are not sources of truth
        self.assertNotIn("elsewhere", r.stdout)

    def test_patch_minor_major(self) -> None:
        for level, expect in (("patch", "0.1.1"), ("minor", "0.2.0"), ("major", "1.0.0")):
            r = self.repo.run(level)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn(f"0.1.0 -> {expect}", r.stdout)
            self.assert_all_versions(expect)
            # reset for the next level so each is measured from 0.1.0
            self.assertEqual(self.repo.run("--set", "0.1.0", "--force").returncode, 0)

    def test_set_explicit(self) -> None:
        r = self.repo.run("--set", "0.2.0")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assert_all_versions("0.2.0")

    def test_invalid_versions_are_rejected(self) -> None:
        for bad in ("1.2", "1.2.3.4", "1.2.3-rc1", "1.2.3+build", "01.2.3", "one.two.three", ""):
            r = self.repo.run("--set", bad)
            self.assertNotEqual(r.returncode, 0, f"{bad!r} was accepted")
            self.assert_all_versions("0.1.0")  # nothing written

    def test_backwards_is_refused(self) -> None:
        self.assertEqual(self.repo.run("--set", "0.2.0").returncode, 0)
        r = self.repo.run("--set", "0.1.9")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("backwards", (r.stderr + r.stdout).lower())
        self.assert_all_versions("0.2.0")

    def test_backwards_allowed_with_the_flag(self) -> None:
        self.assertEqual(self.repo.run("--set", "0.2.0").returncode, 0)
        r = self.repo.run("--set", "0.1.5", "--force")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assert_all_versions("0.1.5")

    def test_level_and_set_are_mutually_exclusive(self) -> None:
        r = self.repo.run("minor", "--set", "0.3.0")
        self.assertNotEqual(r.returncode, 0)
        self.assert_all_versions("0.1.0")

    def test_no_argument_is_an_error(self) -> None:
        self.assertNotEqual(self.repo.run().returncode, 0)
        self.assert_all_versions("0.1.0")

    def test_idempotent(self) -> None:
        self.assertEqual(self.repo.run("minor").returncode, 0)
        first = self.repo.changelog.read_text()
        r = self.repo.run("--set", "0.2.0")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("already at 0.2.0", r.stdout)
        self.assertEqual(self.repo.changelog.read_text(), first, "changelog entry duplicated")

    def test_dry_run_changes_nothing(self) -> None:
        r = self.repo.run("minor", "--dry-run")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("no file written", r.stdout)
        self.assert_all_versions("0.1.0")
        self.assertFalse(self.repo.changelog.exists())

    def test_changelog_stub_is_created_and_prepended(self) -> None:
        self.assertEqual(self.repo.run("minor").returncode, 0)
        text = self.repo.changelog.read_text()
        self.assertIn("# Changelog", text)
        self.assertIn("## [0.2.0]", text)
        self.assertEqual(text.count("## [0.2.0]"), 1)
        self.assertEqual(self.repo.run("patch").returncode, 0)
        text = self.repo.changelog.read_text()
        self.assertLess(
            text.index("## [0.2.1]"), text.index("## [0.2.0]"), "newest entry must be first"
        )

    def test_drift_is_detected_and_never_silently_followed(self) -> None:
        self.repo.write("gpn-agent/Cargo.toml", AGENT_TOML.replace("0.1.0", "0.1.3"))
        r = self.repo.run("--check")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("DRIFT", r.stderr)

        # A level bump is ambiguous while the tree is drifted: it must refuse
        # rather than pick a base and silently move some crates.
        r = self.repo.run("minor")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("ambiguous", (r.stderr + r.stdout).lower())
        self.assertEqual(self.repo.version_of("gpn-node/node/Cargo.toml"), "0.1.0")
        self.assertEqual(self.repo.version_of("gpn-agent/Cargo.toml"), "0.1.3")

        # --set converges everything onto one version.
        self.assertEqual(self.repo.run("--set", "0.2.0").returncode, 0)
        self.assert_all_versions("0.2.0")
        self.assertEqual(self.repo.run("--check").returncode, 0)

    def test_inherited_version_is_not_written(self) -> None:
        self.assertEqual(self.repo.run("minor").returncode, 0)
        self.assertIn("version.workspace = true", (self.repo.root / "gpn-node/elsewhere/Cargo.toml").read_text())


class RealRepoVersionTests(unittest.TestCase):
    """The repo itself must be self-consistent after this change."""

    def test_repo_versions_agree(self) -> None:
        r = subprocess.run([PY, str(BUMP), "--check"], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr + r.stdout)

    def test_repo_version_matches_the_changelog(self) -> None:
        r = subprocess.run([PY, str(BUMP), "--check"], capture_output=True, text=True)
        last = r.stdout.strip().splitlines()[-1]
        version = last.split(": ", 1)[1].strip()
        self.assertRegex(version, r"^\d+\.\d+\.\d+$")
        changelog = (REPO / "CHANGELOG.md").read_text()
        self.assertIn(f"## [{version}]", changelog, "the current version needs a changelog entry")

    def test_build_stamp_prefix_matches_the_generator(self) -> None:
        build_rs = (REPO / "gpn-agent" / "build.rs").read_text()
        needle = gen.BUILD_ID_PREFIX.decode()
        self.assertIn(needle, build_rs)
        self.assertIn("GPN_BUILD_ID", build_rs)


# ─────────────────────────────────────────────────────────────────────────────
# Manifest generator
# ─────────────────────────────────────────────────────────────────────────────


def make_artifact(path: Path, version: str, sha: str = "d606264", spec: str = "41") -> Path:
    """A fake 'binary': random-ish bytes with the build stamp in the middle."""
    stamp = f"gpn-build-v1 version={version} sha={sha} spec={spec}".encode()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"\x7fELF" + b"\x00" * 64 + stamp + b"\x00" * 64 + b"\x90" * 32)
    return path


def manifest_args(**over) -> argparse.Namespace:
    base = dict(
        version="",
        min_version="",
        min_version_policy="same-minor",
        no_min_version=False,
        asset=[],
        url_base="https://gpncoin.ai/dl",
        published_at="2026-10-21T17:00:00Z",
        allow_empty_assets=False,
        allow_unmarked_artifact=False,
    )
    base.update(over)
    return argparse.Namespace(**base)


class ManifestGeneratorTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)

    def build(self, *assets, **over):
        specs = [f"{platform}={path}" for platform, path in assets]
        with redirect_stdout(io.StringIO()):  # the generator prints the stamps it read
            return gen.build_manifest(manifest_args(asset=specs, **over))

    def test_version_and_hashes_come_from_the_artifacts(self) -> None:
        linux = make_artifact(self.root / "gpn_node", "0.2.0", "abc1234")
        win = make_artifact(self.root / "gpn_node.exe", "0.2.0", "abc1234")
        man = self.build(("linux/x86_64", linux), ("windows/x86_64", win))

        self.assertEqual(man["version"], "0.2.0")
        self.assertEqual(man["min_version"], "0.2.0")  # same-minor policy default
        for asset, path in zip(man["assets"], (linux, win)):
            self.assertEqual(asset["sha256"], hashlib.sha256(path.read_bytes()).hexdigest())
            self.assertEqual(asset["size"], path.stat().st_size)
            self.assertTrue(asset["url"].endswith(path.name))
        self.assertEqual([a["os"] for a in man["assets"]], ["linux", "windows"])

    def test_hand_typed_version_that_disagrees_is_refused(self) -> None:
        linux = make_artifact(self.root / "gpn_node", "0.2.0")
        with self.assertRaises(SystemExit) as cm:
            self.build(("linux/x86_64", linux), version="0.1.0")
        self.assertIn("disagrees", str(cm.exception.code))

    def test_matching_assertion_is_accepted(self) -> None:
        linux = make_artifact(self.root / "gpn_node", "0.2.0")
        man = self.build(("linux/x86_64", linux), version="0.2.0")
        self.assertEqual(man["version"], "0.2.0")

    def test_artifacts_from_different_builds_are_refused(self) -> None:
        a = make_artifact(self.root / "a", "0.2.0")
        b = make_artifact(self.root / "b", "0.2.1")
        with self.assertRaises(SystemExit) as cm:
            self.build(("linux/x86_64", a), ("windows/x86_64", b))
        self.assertIn("different versions", str(cm.exception.code))

    def test_unstamped_artifact_needs_an_explicit_version(self) -> None:
        plain = self.root / "old-bin"
        plain.write_bytes(b"\x7fELF no stamp here")
        with self.assertRaises(SystemExit) as cm:
            self.build(("linux/x86_64", plain))
        self.assertIn("gpn-build-v1", str(cm.exception.code))

        man = self.build(("linux/x86_64", plain), version="0.1.0", allow_unmarked_artifact=True)
        self.assertEqual(man["version"], "0.1.0")

    def test_cutoff_only_manifest_needs_a_version(self) -> None:
        with self.assertRaises(SystemExit):
            self.build(allow_empty_assets=True)
        man = self.build(allow_empty_assets=True, version="0.2.0", min_version="0.1.5")
        self.assertEqual(man["assets"], [])
        self.assertEqual(man["min_version"], "0.1.5")

    def test_min_version_policy_explicit_and_empty(self) -> None:
        linux = make_artifact(self.root / "gpn_node", "0.4.7")
        self.assertEqual(self.build(("linux/x86_64", linux))["min_version"], "0.4.0")
        self.assertEqual(
            self.build(("linux/x86_64", linux), min_version="0.1.0")["min_version"], "0.1.0"
        )
        self.assertEqual(
            self.build(("linux/x86_64", linux), no_min_version=True)["min_version"], ""
        )
        with self.assertRaises(SystemExit):
            self.build(("linux/x86_64", linux), no_min_version=True, min_version="0.1.0")

    def test_missing_asset_is_an_error(self) -> None:
        with self.assertRaises(SystemExit):
            self.build(("linux/x86_64", self.root / "nope"))

    def test_sign_and_verify_roundtrip(self) -> None:
        import nacl.signing

        linux = make_artifact(self.root / "gpn_node", "0.2.0")
        man = gen.sign(self.build(("linux/x86_64", linux)), os.urandom(32))
        normalised = json.loads(json.dumps(man))
        # verify() requires the manifest and the artifact side by side
        dst = self.root / "site"
        dst.mkdir()
        (dst / "releases.json").write_text(json.dumps(man))
        (dst / linux.name).write_bytes(linux.read_bytes())
        with redirect_stdout(io.StringIO()):
            out = gen.verify(dst / "releases.json", expect_key=man["signing_key"])
        self.assertEqual(out["version"], "0.2.0")

        # Any change to a signed field must break it.
        for field, value in (("version", "0.2.1"), ("min_version", "0.2.0.1")):
            tampered = dict(normalised, **{field: value})
            (dst / "releases.json").write_text(json.dumps(tampered))
            with self.assertRaises(SystemExit), redirect_stderr(io.StringIO()):
                gen.verify(dst / "releases.json", expect_key=man["signing_key"])
        # A different trusted key is rejected before verifying.
        with self.assertRaises(SystemExit) as cm, redirect_stderr(io.StringIO()):
            gen.verify(dst / "releases.json", expect_key="00" * 32)
        self.assertIn("rejected", str(cm.exception.code))

    def test_canonical_text_is_stable_and_sorted(self) -> None:
        a = make_artifact(self.root / "a", "0.2.0")
        b = make_artifact(self.root / "b", "0.2.0")
        man = self.build(("windows/x86_64", b), ("linux/x86_64", a))
        text = gen.canonical_text(man).decode()
        lines = text.splitlines()
        self.assertEqual(lines[0], "gpn-release-v1")
        self.assertEqual(lines[1], "version=0.2.0")
        self.assertEqual(lines[2], "min_version=0.2.0")
        self.assertTrue(all(line.startswith("asset=") for line in lines[5:]))
        self.assertEqual(
            [l.split()[0] for l in lines[5:]],
            ["asset=linux/x86_64", "asset=windows/x86_64"],
        )
        self.assertTrue(text.endswith("\n"))
        # Order of the assets in the manifest dict must not matter.
        man2 = dict(man, assets=list(reversed(man["assets"])))
        self.assertEqual(gen.canonical_text(man2), gen.canonical_text(man))


class EmbeddedPubkeyTests(unittest.TestCase):
    def test_python_and_rust_agree_on_the_trust_root(self) -> None:
        update_rs = (REPO / "gpn-agent" / "src" / "update.rs").read_text()
        self.assertIn(gen.EMBEDDED_PUBKEY_HEX, update_rs)


if __name__ == "__main__":
    unittest.main(verbosity=2)
