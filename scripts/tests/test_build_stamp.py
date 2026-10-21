#!/usr/bin/env python3
"""Regression tests for reading the `gpn-build-v1` stamp out of an artifact.

The failure these guard against is subtle and expensive: a release manifest that
says `version=unknown`, so `is_newer()` is never true and the `min_version`
cutoff can never fire. That is exactly what a naive `data.find(prefix)` produces
once `firstrun::build_id()`'s `unwrap_or` fallback literal is linked into the
same binary as the real stamp.

    /usr/bin/python3 -m unittest discover -s scripts/tests -v
    /usr/bin/python3 scripts/tests/test_build_stamp.py
"""

from __future__ import annotations

import argparse
import importlib.util
import re
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
GEN = REPO / "scripts" / "gen-release-manifest.py"


def load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


gen = load_module(GEN, "gen_release_manifest_stamp")


def stamp(version: str, sha: str = "d606264", spec: str = "41") -> bytes:
    return f"gpn-build-v1 version={version} sha={sha} spec={spec}".encode()


FALLBACK = stamp("unknown", "unknown")


def artifact(path: Path, *parts: bytes) -> Path:
    """A fake ELF whose `.rodata` contains the given stamps, in that order."""
    path.parent.mkdir(parents=True, exist_ok=True)
    body = b"\x7fELF" + b"\x00" * 64
    for p in parts:
        body += p + b"\x00" * 64
    path.write_bytes(body + b"\x90" * 32)
    return path


class BuildStampTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)

    def test_real_stamp_wins_when_the_fallback_comes_first(self) -> None:
        p = artifact(self.root / "bin", FALLBACK, stamp("0.2.0"))
        self.assertEqual(gen.read_build_id(p).get("version"), "0.2.0")

    def test_real_stamp_wins_when_the_fallback_comes_last(self) -> None:
        p = artifact(self.root / "bin", stamp("0.2.0", "abc1234"), FALLBACK)
        got = gen.read_build_id(p)
        self.assertEqual(got.get("version"), "0.2.0")
        self.assertEqual(got.get("sha"), "abc1234")

    def test_only_the_fallback_is_treated_as_unstamped(self) -> None:
        p = artifact(self.root / "bin", FALLBACK)
        self.assertEqual(gen.read_build_id(p), {}, "`unknown` is not a version")

    def test_no_stamp_at_all(self) -> None:
        p = self.root / "plain"
        p.write_bytes(b"\x7fELF nothing to see")
        self.assertEqual(gen.read_build_id(p), {})
        self.assertEqual(gen.read_build_ids(p), [])

    def test_all_occurrences_are_returned_in_order(self) -> None:
        p = artifact(self.root / "bin", FALLBACK, stamp("0.2.0"), stamp("0.3.0"))
        versions = [s.get("version") for s in gen.read_build_ids(p)]
        self.assertEqual(versions, ["unknown", "0.2.0", "0.3.0"])

    def test_a_fallback_only_artifact_cannot_silently_become_the_release(self) -> None:
        p = artifact(self.root / "bin", FALLBACK)
        args = argparse.Namespace(
            version="",
            min_version="",
            min_version_policy="same-minor",
            no_min_version=False,
            asset=[f"linux/x86_64={p}"],
            url_base="https://gpncoin.ai/dl",
            published_at="2026-10-21T17:00:00Z",
            allow_empty_assets=False,
            allow_unmarked_artifact=False,
        )
        with self.assertRaises(SystemExit) as cm:
            gen.build_manifest(args)
        self.assertIn("gpn-build-v1", str(cm.exception.code))

    def test_the_real_built_binary_reports_the_crate_version(self) -> None:
        """End-to-end on `target/debug/gpn_node`, when it has been built.

        Skipped on a clean checkout (that is a `cargo build` away, and this test
        must not require a build to be useful).
        """
        bin_path = REPO / "gpn-agent" / "target" / "debug" / "gpn_node"
        cargo = REPO / "gpn-agent" / "Cargo.toml"
        if not bin_path.is_file():
            self.skipTest(f"{bin_path} not built")
        m = re.search(r'^version\s*=\s*"([^"]+)"', cargo.read_text(), re.MULTILINE)
        assert m, "no version in gpn-agent/Cargo.toml"
        stamp_fields = gen.read_build_id(bin_path)
        self.assertEqual(
            stamp_fields.get("version"),
            m.group(1),
            f"binary stamp {stamp_fields!r} != Cargo.toml {m.group(1)}",
        )
        self.assertRegex(stamp_fields.get("sha", ""), r"^([0-9a-f]{7,40}|unknown)$")


if __name__ == "__main__":
    unittest.main(verbosity=2)
