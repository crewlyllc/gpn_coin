#!/usr/bin/env python3
"""task-done.py — close a GPN task row with evidence, from the command line.

Why: several tasks were fixed and never written back to `GPN-TASKS.md`
(#138 `Requires=`→`Wants=` on 2026-11-05, #87 box1 GFLOPS), so the queue driver
kept advertising them as owner-gated open calls and Jeremy was told there was
work waiting that had already shipped. A task is only done when the table says
so, with the evidence attached.

Usage:
    scripts/task-done.py 138 "Wants= applied to 6 units; verified both directions"
    scripts/task-done.py 138 --file evidence.txt
    scripts/task-done.py 138 --dry-run

The row's status cell is replaced (not appended to), keeping the historic
reason underneath so the record of why it was blocked survives.
"""
from __future__ import annotations

import argparse
import datetime as _dt
import pathlib
import sys

TASKS = pathlib.Path.home() / "shared_workspace/artifacts/project7/GPN-TASKS.md"


def find_row(lines: list[str], task_id: str) -> int:
    for i, line in enumerate(lines):
        if line.startswith(f"| {task_id} "):
            return i
    raise SystemExit(f"task #{task_id} not found (rows look like '| {task_id} | … |')")


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("task_id")
    ap.add_argument("evidence", nargs="?", default="")
    ap.add_argument("--file", help="read the evidence from a file")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args(argv[1:])

    evidence = args.evidence
    if args.file:
        evidence = pathlib.Path(args.file).read_text(encoding="utf-8").strip()
    if not evidence:
        raise SystemExit("refusing to close a task without evidence")

    lines = TASKS.read_text(encoding="utf-8").split("\n")
    row = find_row(lines, args.task_id)
    parts = lines[row].split("|")
    if len(parts) < 5:
        raise SystemExit(f"row {args.task_id} is not a table row")

    old_status = parts[3].strip()
    if old_status.startswith("**DONE"):
        print(f"#{args.task_id} is already DONE — nothing to do")
        return 0

    today = _dt.date.today().isoformat()
    historic = old_status
    for prefix in ("BLOCKED — reason:", "**BLOCKED — reason:", "TODO", "IN PROGRESS"):
        if historic.startswith(prefix):
            historic = historic[len(prefix):].strip()
            break
    parts[3] = (
        f" **DONE {today}** — {evidence} "
        + (f"Historic reason (superseded): {historic}" if historic else "")
    )
    lines[row] = "|".join(parts)

    if args.dry_run:
        print(lines[row][:400])
        return 0

    backup = TASKS.with_suffix(".md.bak-" + _dt.datetime.now().strftime("%Y%m%d-%H%M%S"))
    backup.write_text(TASKS.read_text(encoding="utf-8"), encoding="utf-8")
    TASKS.write_text("\n".join(lines), encoding="utf-8")
    print(f"#{args.task_id} marked DONE ({today}); backup {backup.name}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
