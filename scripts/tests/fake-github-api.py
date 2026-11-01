#!/usr/bin/env python3
"""A tiny, stateful fake of the two GitHub release APIs `release-cut.sh` uses.

Only what the script touches: release-by-tag, asset rename/delete, asset
upload, asset download, plus a stand-in for `https://gpncoin.ai/releases.json`.

State lives in a JSON file so the test harness can inspect it:

    {"source": {tag: {name, file, digest_override?}},
     "mirror": {tag: {name, file}},
     "site_path": "/abs/path/to/repo/website/releases.json",
     "fail_upload": false}

Every mutation is appended to `<state>/actions.log` as `VERB tag name`, which is
how the tests assert "no upload happened" / "the old asset was deleted".

    python3 fake-github-api.py <state-dir> <port-file>
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

STATE = Path(sys.argv[1])
LOCK = threading.Lock()


def load() -> dict:
    return json.loads((STATE / "state.json").read_text())


def save(d: dict) -> None:
    tmp = STATE / "state.json.tmp"
    tmp.write_text(json.dumps(d, indent=2))
    os.replace(tmp, STATE / "state.json")


def log(line: str) -> None:
    with open(STATE / "actions.log", "a") as f:
        f.write(line + "\n")


def sha(p: str) -> str:
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def asset_id(repo: str, tag: str, name: str) -> int:
    return int(hashlib.sha256(f"{repo}/{tag}/{name}".encode()).hexdigest()[:8], 16)


def asset_json(base: str, repo: str, tag: str, a: dict) -> dict:
    digest = a.get("digest_override") or "sha256:" + sha(a["file"])
    return {
        "id": asset_id(repo, tag, a["name"]),
        "name": a["name"],
        "size": Path(a["file"]).stat().st_size,
        "digest": digest,
        "browser_download_url": f"{base}/download/{repo}/{tag}/{a['name']}",
        "url": f"{base}/repos/{repo}/releases/assets/{asset_id(repo, tag, a['name'])}",
    }


def side_for(repo: str) -> str:
    return "source" if repo.startswith("src/") else "mirror"


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):  # keep the harness output readable
        pass

    # -- plumbing ---------------------------------------------------------
    def send(self, code: int, body: bytes = b"", ctype="application/json"):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body:
            self.wfile.write(body)

    def js(self, code: int, obj) -> None:
        self.send(code, json.dumps(obj).encode())

    def base(self) -> str:
        return f"http://{self.headers.get('Host', '127.0.0.1')}"

    def body(self) -> bytes:
        n = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(n) if n else b""

    # -- routes -----------------------------------------------------------
    def do_GET(self):
        u = urlparse(self.path)
        path = u.path

        if path in ("/site/releases.json", "/releases.json"):
            d = load()
            p = Path(d["site_path"])
            if not p.is_file():
                return self.send(404, b"not found", "text/plain")
            return self.send(200, p.read_bytes(), "application/json")

        m = re.fullmatch(r"/repos/([^/]+/[^/]+)/releases/tags/([^/]+)", path)
        if m:
            repo, tag = m[1], m[2]
            d = load()
            side = d[side_for(repo)].get(tag)
            if side is None:
                return self.js(404, {"message": "Not Found"})
            base = self.base()
            return self.js(
                200,
                {
                    "id": 900000 + asset_id(repo, tag, "release"),
                    "tag_name": tag,
                    "assets": [asset_json(base, repo, tag, a) for a in side],
                },
            )

        m = re.fullmatch(r"/repos/([^/]+/[^/]+)/releases/assets/(\d+)", path)
        if m:
            repo, aid = m[1], int(m[2])
            d = load()
            for tag, assets in d[side_for(repo)].items():
                for a in assets:
                    if asset_id(repo, tag, a["name"]) == aid:
                        return self.send(200, Path(a["file"]).read_bytes(),
                                         "application/octet-stream")
            return self.js(404, {"message": "Not Found"})

        m = re.fullmatch(r"/download/([^/]+/[^/]+)/([^/]+)/([^/]+)", path)
        if m:
            repo, tag, name = m[1], m[2], m[3]
            d = load()
            for a in d[side_for(repo)].get(tag, []):
                if a["name"] == name:
                    return self.send(200, Path(a["file"]).read_bytes(),
                                     "application/octet-stream")
            return self.js(404, {"message": "Not Found"})

        return self.js(404, {"message": "Not Found"})

    def do_DELETE(self):
        u = urlparse(self.path)
        m = re.fullmatch(r"/repos/([^/]+/[^/]+)/releases/assets/(\d+)", u.path)
        if not m:
            return self.js(404, {"message": "Not Found"})
        repo, aid = m[1], int(m[2])
        with LOCK:
            d = load()
            for tag, assets in d[side_for(repo)].items():
                for i, a in enumerate(assets):
                    if asset_id(repo, tag, a["name"]) == aid:
                        assets.pop(i)
                        save(d)
                        log(f"DELETE {tag} {a['name']}")
                        return self.send(204, b"")
        return self.js(404, {"message": "Not Found"})

    def do_PATCH(self):
        u = urlparse(self.path)
        m = re.fullmatch(r"/repos/([^/]+/[^/]+)/releases/assets/(\d+)", u.path)
        if not m:
            return self.js(404, {"message": "Not Found"})
        repo, aid = m[1], int(m[2])
        try:
            patch = json.loads(self.body() or b"{}")
        except json.JSONDecodeError:
            return self.js(400, {"message": "bad json"})
        with LOCK:
            d = load()
            for tag, assets in d[side_for(repo)].items():
                for a in assets:
                    if asset_id(repo, tag, a["name"]) == aid:
                        old = a["name"]
                        a["name"] = patch.get("name", old)
                        save(d)
                        log(f"PATCH {tag} {old}->{a['name']}")
                        return self.js(200, {"name": a["name"], "id": aid})
        return self.js(404, {"message": "Not Found"})

    def do_POST(self):
        u = urlparse(self.path)
        m = re.fullmatch(r"/repos/([^/]+/[^/]+)/releases/(\d+)/assets", u.path)
        if not m:
            return self.js(404, {"message": "Not Found"})
        repo = m[1]
        tag = (parse_qs(u.query).get("name") or [""])[0]
        # `name` here is the asset filename; the tag is recovered from the
        # release id we handed out (asset_id over "release")
        data = self.body()
        with LOCK:
            d = load()
            if d.get("fail_upload"):
                log(f"FAILUPLOAD {repo} {tag}")
                return self.js(500, {"message": "upload exploded (injected)"})
            for t, assets in d[side_for(repo)].items():
                if 900000 + asset_id(repo, t, "release") == int(m[2]):
                    tag = t
                    break
            else:
                return self.js(404, {"message": "no such release"})
            name = (parse_qs(u.query).get("name") or [""])[0]
            dest = STATE / f"uploaded-{repo.replace('/', '_')}-{tag}-{name}"
            dest.write_bytes(data)
            assets = d[side_for(repo)].setdefault(tag, [])
            assets.append({"name": name, "file": str(dest)})
            save(d)
            log(f"UPLOAD {tag} {name}")
        return self.js(201, {"name": name})


def main() -> None:
    srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
    Path(sys.argv[2]).write_text(str(srv.server_address[1]))
    srv.serve_forever()


if __name__ == "__main__":
    main()
