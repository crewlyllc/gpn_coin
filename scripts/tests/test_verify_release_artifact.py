#!/usr/bin/env python3
"""Regression tests for scripts/verify-release-artifact.py (queue #193).

The failure this gate exists for: a release whose version string, sha256 and
signed manifest are all correct while the *code* is not in the binary, because
the build came from the wrong tree. A gate that cannot say "no" is worse than
no gate, so these tests are mostly negatives — each one asserts the specific
exit code the gate must produce.

No network and no real artifacts: bundles are built in a temp dir from
synthetic bytes, and the source-constant check runs against a throwaway git
repo, so nothing here downloads or scans 300 MB.
"""

from __future__ import annotations

import io
import os
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / "scripts" / "verify-release-artifact.py"

GOOD = (b"..fork-choice..gpn-forked..deep-fork..primary-authoritative.."
        b"gpncs/2..gpn-node/0.3.50..")
#: what a 0.3.50 build would contain if it had been cut from a tree without the
#: fork-choice work: the version stamp is right, every marker is absent.
WITHOUT = b"..gpn-node/0.3.50..some other feature.."


def make_tar(path: Path, payload: bytes, member: str = "gpn-node") -> Path:
    with tarfile.open(path, "w:gz") as tf:
        info = tarfile.TarInfo(member)
        info.size = len(payload)
        tf.addfile(info, io.BytesIO(payload))
    return path


def make_zip(path: Path, payload: bytes, member: str = "gpn-node.exe") -> Path:
    with zipfile.ZipFile(path, "w") as zf:
        zf.writestr(member, payload)
    return path


def run(*args: str, cwd: Path | None = None) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, str(SCRIPT), *args],
                          capture_output=True, text=True, cwd=str(cwd or REPO))


class VerifyReleaseArtifact(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="gpn-verify-test-"))

    def tearDown(self) -> None:
        subprocess.run(["rm", "-rf", str(self.tmp)])

    def bundle(self, name: str, payload: bytes, kind: str = "tar") -> Path:
        p = self.tmp / name
        return make_tar(p, payload) if kind == "tar" else make_zip(p, payload)

    # ── positives ───────────────────────────────────────────────────────────

    def test_a_bundle_that_carries_the_code_passes(self) -> None:
        b = self.bundle("gpn-linux.tar.gz", GOOD)
        r = run("--no-manifest", f"--bundle=linux={b}", "--version", "0.3.50")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("OK: 0.3.50 artifacts verified", r.stdout)
        self.assertIn("MAX_REORG_DEPTH = 288", r.stdout)

    def test_a_windows_zip_is_read_too(self) -> None:
        b = self.bundle("gpn-windows.zip", GOOD, kind="zip")
        r = run("--no-manifest", f"--bundle=windows={b}", "--version", "0.3.50")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("gpn-node.exe", r.stdout)

    # ── negatives: the gate must refuse ─────────────────────────────────────

    def test_wrong_tree_with_the_right_version_fails_on_the_markers(self) -> None:
        """The exact incident #193 exists for: right version, missing code."""
        b = self.bundle("gpn-linux.tar.gz", WITHOUT)
        r = run("--no-manifest", f"--bundle=linux={b}", "--version", "0.3.50")
        self.assertEqual(r.returncode, 4, r.stdout + r.stderr)
        self.assertIn("does not carry the code", r.stderr)
        for marker in ("fork-choice", "gpn-forked", "deep-fork",
                       "primary-authoritative", "gpncs/2"):
            self.assertIn(marker, r.stderr)

    def test_a_single_missing_marker_is_enough_to_fail(self) -> None:
        payload = GOOD.replace(b"deep-fork", b"something-else")
        b = self.bundle("gpn-linux.tar.gz", payload)
        r = run("--no-manifest", f"--bundle=linux={b}", "--version", "0.3.50")
        self.assertEqual(r.returncode, 4, r.stdout + r.stderr)
        self.assertIn("deep-fork", r.stderr)
        self.assertNotIn("'gpn-forked'", r.stderr)  # only the missing one is named

    def test_a_version_stamp_mismatch_is_its_own_failure(self) -> None:
        payload = GOOD.replace(b"gpn-node/0.3.50", b"gpn-node/0.3.49")
        b = self.bundle("gpn-linux.tar.gz", payload)
        r = run("--no-manifest", f"--bundle=linux={b}", "--version", "0.3.50")
        self.assertEqual(r.returncode, 5, r.stdout + r.stderr)
        self.assertIn("not the 0.3.50 build", r.stderr)

    def test_an_asset_that_is_not_the_signed_bytes_fails(self) -> None:
        b = self.bundle("gpn-linux.tar.gz", GOOD)
        manifest = self.tmp / "releases.json"
        manifest.write_text(
            '{"version":"0.3.50","min_version":"0.1.0",'
            '"published_at":"2026-11-16T00:00:00Z","assets":['
            '{"os":"linux","arch":"x86_64","url":"http://127.0.0.1:9/x.tar.gz",'
            f'"sha256":"{"0"*64}","size":{b.stat().st_size}}}],'
            '"signing_key":"' + "0" * 64 + '","signature":"' + "0" * 128 + '"}')
        r = run("--manifest", str(manifest), f"--bundle=linux={b}")
        # exit 2 (unsigned manifest) or 3 (digest) — both are refusals, never 0.
        self.assertIn(r.returncode, (2, 3), r.stdout + r.stderr)

    def test_a_bundle_without_the_node_binary_fails(self) -> None:
        b = self.bundle("gpn-linux.tar.gz", GOOD, kind="tar")
        make_tar(b, GOOD, member="README.txt")
        r = run("--no-manifest", f"--bundle=linux={b}", "--version", "0.3.50")
        self.assertEqual(r.returncode, 4, r.stdout + r.stderr)
        self.assertIn("contains no gpn-node", r.stderr)

    # ── the compile-time constant, verified where it lives ──────────────────

    def test_the_reorg_bound_is_read_from_the_release_revision(self) -> None:
        """`MAX_REORG_DEPTH` is a const, so it is checked in the source, not the image."""
        import importlib.util
        spec = importlib.util.spec_from_file_location("vra", SCRIPT)
        mod = importlib.util.module_from_spec(spec)
        assert spec.loader is not None
        spec.loader.exec_module(mod)
        module = "gpn-node/node/src/fork_choice.rs"
        # the real tree must carry the documented bound
        self.assertEqual(mod.source_constant("HEAD", module, "MAX_REORG_DEPTH"), 288)
        # an unreadable path/revision is "unknown" (skipped loudly), never a pass
        self.assertIsNone(mod.source_constant("HEAD", "nope.rs", "MAX_REORG_DEPTH"))
        self.assertIsNone(mod.source_constant("does-not-exist", module, "MAX_REORG_DEPTH"))

    def test_a_changed_bound_in_the_release_tree_is_refused(self) -> None:
        """A tree whose constant disagrees with the policy must not ship quietly."""
        import importlib.util
        spec = importlib.util.spec_from_file_location("vra2", SCRIPT)
        mod = importlib.util.module_from_spec(spec)
        assert spec.loader is not None
        spec.loader.exec_module(mod)
        # --skip-marker keeps this test about the constant alone; the injected
        # expectation is what the gate compares, so a wrong bound must exit 6.
        b = self.bundle("gpn-linux.tar.gz", GOOD)
        orig = mod.DEFAULT_CONSTS
        mod.DEFAULT_CONSTS = [("gpn-node/node/src/fork_choice.rs", "MAX_REORG_DEPTH", 999)]
        try:
            with self.assertRaises(SystemExit) as ctx:
                mod.main(["--no-manifest", f"--bundle=linux={b}", "--version", "0.3.50"])
            rc = ctx.exception.code
        finally:
            mod.DEFAULT_CONSTS = orig
        self.assertEqual(rc, 6)


if __name__ == "__main__":
    unittest.main(verbosity=2)
