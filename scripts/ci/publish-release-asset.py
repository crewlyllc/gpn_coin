#!/usr/bin/env python3
"""Publish (create-or-update) a rolling GitHub Release asset.

Replaces the curl+jq publish step from release-unix.yml: this runner has no
`jq`, and a failed `jq` in a `set -e` pipe fails the step in confusing ways.

    GH_TOKEN=... GITHUB_REPOSITORY=owner/repo \
      python3 scripts/ci/publish-release-asset.py \
        --tag linux-latest --title "GPN linux (latest)" \
        --file /tmp/gpn-linux.tar.gz --name gpn-linux.tar.gz

Deletes an existing asset with the same name first (rolling release), then
uploads the new bytes. Prints size + sha256 of what it published.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import urllib.error
import urllib.request

API = "https://api.github.com"
UPLOAD = "https://uploads.github.com"


def _req(url: str, token: str, method: str = "GET", data: bytes | None = None,
         ctype: str = "application/json", accept: str = "application/vnd.github+json"):
    req = urllib.request.Request(url, method=method, data=data)
    req.add_header("Authorization", f"Bearer {token}")
    req.add_header("Accept", accept)
    req.add_header("User-Agent", "gpn-ci")
    if data is not None:
        req.add_header("Content-Type", ctype)
    with urllib.request.urlopen(req, timeout=300) as r:
        body = r.read()
    return json.loads(body) if body else {}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag", required=True)
    ap.add_argument("--title", default=None)
    ap.add_argument("--body", default=None)
    ap.add_argument("--file", required=True)
    ap.add_argument("--name", default=None)
    ap.add_argument("--target-commitish", default="main")
    args = ap.parse_args()

    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    repo = os.environ.get("GITHUB_REPOSITORY")
    if not token or not repo:
        print("ERROR: GH_TOKEN/GITHUB_TOKEN and GITHUB_REPOSITORY are required", file=sys.stderr)
        return 2
    name = args.name or os.path.basename(args.file)
    with open(args.file, "rb") as fh:
        blob = fh.read()
    sha = hashlib.sha256(blob).hexdigest()
    print(f"asset {name}: {len(blob)} bytes sha256={sha}")

    # 1. find or create the release for the rolling tag
    try:
        rel = _req(f"{API}/repos/{repo}/releases/tags/{args.tag}", token)
    except urllib.error.HTTPError as e:
        if e.code != 404:
            raise
        payload = {
            "tag_name": args.tag,
            "name": args.title or args.tag,
            "body": args.body or f"Rolling GPN build. commit {os.environ.get('GITHUB_SHA', '')}",
            "target_commitish": args.target_commitish,
            "draft": False,
            "prerelease": False,
        }
        rel = _req(f"{API}/repos/{repo}/releases", token, "POST",
                   json.dumps(payload).encode())
        print(f"created release {rel['id']} for {args.tag}")

    # 2. drop an existing asset with the same name (rolling release)
    assets = _req(f"{API}/repos/{repo}/releases/{rel['id']}/assets?per_page=100", token)
    for a in assets:
        if a.get("name") == name:
            _req(a["url"], token, "DELETE")
            print(f"deleted old asset {a['id']} ({name})")

    # 3. upload
    up = _req(f"{UPLOAD}/repos/{repo}/releases/{rel['id']}/assets?name={name}",
              token, "POST", blob, ctype="application/gzip")
    print(f"published {args.tag}/{name} -> {up.get('browser_download_url')} "
          f"({up.get('size')} bytes)")
    print(f"sha256 {sha}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
