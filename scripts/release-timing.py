#!/usr/bin/env python3
"""release-timing.py — how long each checklist step actually took.

WHY
    Estimates in a status message are guesses; the checklist is a record. This reads the
    git history of `docs/releases/<version>.md` and reconstructs, per box, the moment it was
    created and the moment it went from `- [ ]` to `- [x]`. Every duration is therefore
    evidence (a commit + a timestamp), not an opinion.

WHAT IT REPORTS
    * per box: when it became done, how long after the checklist existed, and the gap since
      the previous box was ticked (the "step cost")
    * per section: boxes, ticked, wall-clock span, median step cost
    * whole release: start → last tick, and the biggest single step (the thing to attack next)

USAGE
    scripts/release-timing.py 0.3.61                 # table
    scripts/release-timing.py 0.3.61 --json out.json # machine-readable
    scripts/release-timing.py 0.3.60 --archive       # reads docs/releases/archive/<v>.md

    Nothing is written unless --json is given, so this is safe to run at any time.
"""
import argparse
import json
import re
import subprocess
import sys
from datetime import datetime, timezone

BOX = re.compile(r"^(- \[[ xX]\])\s*(.*)$")
SECTION = re.compile(r"^## (.+)$")


def git(*args, **kw):
    return subprocess.run(["git", *args], capture_output=True, text=True, **kw)


def normalise(text: str) -> str:
    """Stable identity for a box across commits: ignore evidence suffixes and trailing ticks."""
    t = text.strip()
    t = re.split(r"\s+—\s+", t)[0]          # drop " — 105 passed ..." evidence
    t = t.replace("**", "")
    return t[:64]


def commits(paths):
    """(timestamp, sha) for every commit that touched any of the paths, oldest first.

    No `--follow`: git only follows renames from one end, and combining it with
    `--reverse` silently returned a single commit (2026-11-20, 0.3.64 reported
    "no ticked boxes found in history" while `git log` plainly listed 9 commits).
    These checklists are never renamed, so plain `git log` is both correct and enough;
    default order is newest-first, so reverse in Python instead of via git."""
    log = git("log", "--format=%H %cI", "--", *paths)
    out = []
    for line in log.stdout.splitlines():
        sha, _, when = line.partition(" ")
        try:
            out.append((datetime.fromisoformat(when.strip()), sha))
        except ValueError:
            continue
    out.sort(key=lambda pair: pair[0])
    return out


def snapshot(paths, sha: str):
    """The file's full box state at one commit — diffs are useless here, because unchanged
    boxes appear only as context and a generator rewrite restates everything at once.

    `paths` is a list because filing moves the checklist from docs/releases/<v>.md to
    docs/releases/archive/<v>.md: a release read after filing must see BOTH histories, or
    every tick that happened before the move is invisible (2026-11-22: 0.3.67 reported a
    single 0 s step because only the rename commit matched). The first path that exists at
    that commit is read."""
    blob = None
    for path in paths:
        blob = git("show", f"{sha}:{path}")
        if blob.returncode == 0:
            break
    if blob is None or blob.returncode != 0:
        return {}, {}
    boxes, sections = {}, {}
    cur = "(preamble)"
    for line in blob.stdout.split("\n"):
        sm = SECTION.match(line)
        if sm:
            cur = sm.group(1).strip()
            continue
        m = BOX.match(line)
        if not m:
            continue
        mark, label = m.group(1), m.group(2)
        key = normalise(label)
        if key:
            boxes[key] = "x" in mark.lower()
            sections[key] = cur
    return boxes, sections


def analyse(paths):
    first_seen, ticked_at, section_of = {}, {}, {}
    for ts, sha in commits(paths):
        boxes, sections = snapshot(paths, sha)
        for key, done in boxes.items():
            first_seen.setdefault(key, ts)
            section_of.setdefault(key, sections.get(key, "?"))
            if done and key not in ticked_at:
                ticked_at[key] = (ts, sha)
    return first_seen, ticked_at, section_of


def fmt_delta(seconds: float) -> str:
    if seconds < 0:
        return "?"
    m, s = divmod(int(seconds), 60)
    h, m = divmod(m, 60)
    if h:
        return f"{h}h{m:02d}m"
    if m:
        return f"{m}m{s:02d}s"
    return f"{s}s"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("version")
    ap.add_argument("--archive", action="store_true")
    ap.add_argument("--json", metavar="OUT")
    args = ap.parse_args()

    base = "docs/releases/archive" if args.archive else "docs/releases"
    path = f"{base}/{args.version}.md"
    # The file may have been filed already: read both the working location and the archive,
    # so a release is analysable before and after the move.
    paths = [path]
    alt = f"docs/releases/{args.version}.md" if args.archive else f"docs/releases/archive/{args.version}.md"
    paths.append(alt)
    if not any(__import__("os").path.exists(p) for p in paths):
        sys.exit(f"no such checklist: {path}")

    first_seen, ticked_at, section_of = analyse(paths)
    if not ticked_at:
        sys.exit(f"{path}: no ticked boxes found in history")

    start = min(first_seen.values())
    ordered = sorted(ticked_at.items(), key=lambda kv: kv[1][0])

    print(f"release {args.version} — {path}")
    print(f"checklist first appears: {start.astimezone().strftime('%Y-%m-%d %H:%M %Z')}")
    print(f"boxes ticked: {len(ordered)} of {len(first_seen)} seen\n")

    print(f"{'#':>3}  {'ticked at':16}  {'since start':>11}  {'step cost':>9}  section / box")
    prev = start
    rows = []
    for i, (key, (ts, sha)) in enumerate(ordered, 1):
        since = (ts - start).total_seconds()
        step = (ts - prev).total_seconds()
        prev = ts
        sec = section_of.get(key, "?")
        short = re.sub(r"\s+", " ", key)[:62]
        print(f"{i:>3}  {ts.astimezone().strftime('%H:%M:%S'):16}  {fmt_delta(since):>11}  {fmt_delta(step):>9}  {sec[:18]:18}  {short}")
        rows.append({"box": key, "section": sec, "ticked_at": ts.isoformat(),
                     "since_start_s": int(since), "step_s": int(step), "sha": sha})

    last = ordered[-1][1][0]
    total = (last - start).total_seconds()
    print(f"\ntotal: {fmt_delta(total)} from checklist creation to the last tick")

    by_section = {}
    for r in rows:
        d = by_section.setdefault(r["section"], {"n": 0, "span_s": 0, "steps": []})
        d["n"] += 1
        d["steps"].append(r["step_s"])
        d["span_s"] = max(d["span_s"], r["since_start_s"])
    print("\nper section:")
    for sec, d in sorted(by_section.items(), key=lambda kv: -kv[1]["span_s"]):
        steps = sorted(d["steps"])
        med = steps[len(steps) // 2]
        print(f"  {sec[:42]:42} {d['n']:>2} boxes  span {fmt_delta(d['span_s']):>7}  median step {fmt_delta(med):>7}")

    slowest = max(rows, key=lambda r: r["step_s"])
    print(f"\nslowest single step: {fmt_delta(slowest['step_s'])} — [{slowest['section']}] {slowest['box'][:70]}")

    if args.json:
        with open(args.json, "w") as fh:
            json.dump({"version": args.version, "start": start.isoformat(),
                       "total_s": int(total), "rows": rows}, fh, indent=2)
        print(f"\nwrote {args.json}")


if __name__ == "__main__":
    main()
