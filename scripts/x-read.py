#!/usr/bin/env python3
"""x-read.py — read a single X/Twitter post for free (no API key, no account).

Uses the public fxtwitter API (api.fxtwitter.com), which serves tweet content by ID/URL.
It is READ-ONLY and cannot notify you of new posts — see marketing/x-watchlist.md for why.

Usage:
  x-read.py <url|id|user/status/id>
  x-read.py https://x.com/ollama/status/1234567890
  x-read.py ollama/status/1234567890
  x-read.py 1234567890 --json

Notes:
  * fxtwitter may be rate-limited or blocked from some networks; exits 3 with the HTTP status.
  * Only the tweet's own data is returned (text, media, metrics, author) — no timeline.
"""
import json
import re
import sys
import urllib.error
import urllib.request

UA = ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/124.0 Safari/537.36")
API = "https://api.fxtwitter.com"


def parse_target(arg: str):
    """Return (user, tweet_id) from a URL, a user/status/id path, or a bare id."""
    arg = arg.strip()
    m = re.search(r"(?:x|twitter)\.com/([^/]+)/status(?:es)?/(\d+)", arg)
    if m:
        return m.group(1), m.group(2)
    m = re.match(r"^([^/\s]+)/status(?:es)?/(\d+)$", arg)
    if m:
        return m.group(1), m.group(2)
    if arg.isdigit():
        return None, arg
    return None, None


def fetch(path: str):
    req = urllib.request.Request(API + path, headers={"User-Agent": UA, "Accept": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            return json.load(r)
    except urllib.error.HTTPError as e:
        print(f"fxtwitter HTTP {e.code} for {path}", file=sys.stderr)
        raise SystemExit(3)
    except urllib.error.URLError as e:
        print(f"fxtwitter unreachable: {e.reason}", file=sys.stderr)
        raise SystemExit(3)
    except json.JSONDecodeError:
        print(f"fxtwitter returned non-JSON for {path} (bad id, deleted tweet, or blocked)",
              file=sys.stderr)
        raise SystemExit(4)


def main(argv):
    if len(argv) < 2 or argv[1] in ("-h", "--help"):
        print(__doc__)
        return 2
    as_json = "--json" in argv
    args = [a for a in argv[1:] if not a.startswith("--")]
    user, tweet_id = parse_target(args[0])
    if not tweet_id:
        print(f"could not parse a tweet id from: {args[0]}", file=sys.stderr)
        return 2

    path = f"/{user}/status/{tweet_id}" if user else f"/i/status/{tweet_id}"
    data = fetch(path)
    tw = data.get("tweet") or {}
    if not tw:
        print("no tweet in response (deleted, protected, or bad id)", file=sys.stderr)
        return 4

    if as_json:
        print(json.dumps(tw, indent=2, ensure_ascii=False))
        return 0

    author = tw.get("author") or {}
    print(f"@{author.get('screen_name','?')}  ({author.get('name','?')})")
    print(f"posted: {tw.get('created_at','?')}   id: {tw.get('id','?')}")
    print(f"metrics: {tw.get('likes',0)} likes · {tw.get('reposts',0)} reposts · "
          f"{tw.get('replies',0)} replies · {tw.get('views',0)} views")
    print()
    print(tw.get("text") or "(no text)")
    media = (tw.get("media") or {}).get("all") or []
    if media:
        print("\nmedia:")
        for m in media:
            print(f"  {m.get('type','?')}: {m.get('url','')}")
    quote = tw.get("quote")
    if quote:
        qa = (quote.get("author") or {}).get("screen_name", "?")
        print(f"\nquoting @{qa}: {(quote.get('text') or '')[:300]}")
    print(f"\n{tw.get('url','')}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
