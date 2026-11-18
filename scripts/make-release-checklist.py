#!/usr/bin/env python3
"""Generate a release checklist from GPN-LIST.md + the generic checklist.

Usage:  make-release-checklist.py 0.3.58

Reads every IN-PROGRESS item from GPN-LIST.md, copies docs/UPDATE-CHECKLIST.md,
inserts one tick-box line per item into a new "Items in this release" section
placed just before the push step, and writes docs/releases/<version>.md.

Idempotent: re-running for the same version preserves any boxes already ticked.
"""
import os, re, sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LIST = os.path.join(REPO, "GPN-LIST.md")
TEMPLATE = os.path.join(REPO, "docs", "UPDATE-CHECKLIST.md")
OUTDIR = os.path.join(REPO, "docs", "releases")


def items():
    """Return [(id, title, acceptance)] for every IN-PROGRESS row."""
    out = []
    for line in open(LIST):
        m = re.match(r"\|\s*(\d+)\s*\|\s*IN-PROGRESS\s*\|\s*(.+?)\s*\|\s*(.+?)\s*\|", line)
        if m:
            out.append((m.group(1), m.group(2), m.group(3)))
    return out


def blocked():
    out = []
    for line in open(LIST):
        m = re.match(r"\|\s*(\d+)\s*\|\s*BLOCKED\s*\|\s*(.+?)\s*\|\s*(.+?)\s*\|", line)
        if m:
            out.append((m.group(1), m.group(2), m.group(3)))
    return out


def main():
    if len(sys.argv) < 2:
        print("usage: make-release-checklist.py <version>"); return 1
    version = sys.argv[1]
    os.makedirs(OUTDIR, exist_ok=True)
    out_path = os.path.join(OUTDIR, version + ".md")

    body = open(TEMPLATE).read()

    section = ["\n## Items in this release (from GPN-LIST.md)\n"]
    for iid, title, accept in items():
        section.append(f"- [ ] **{iid} — {title}** — done when: {accept}")
    for iid, title, reason in blocked():
        # blocked items are pre-ticked: they must not hold the release hostage
        section.append(f"- [x] **{iid} — {title}** — blocked, not in this release: {reason}")
    if not items() and not blocked():
        section.append("- (no IN-PROGRESS items — nothing bundled)")

    anchor = "## 4. Push (once)"
    if anchor in body:
        body = body.replace(anchor, "\n".join(section) + "\n\n" + anchor, 1)
    else:
        body += "\n".join(section)

    header = f"# Release {version} — checklist\n\nGenerated from GPN-LIST.md by make-release-checklist.py.\n"
    new = header + body

    # preserve existing ticks from a previous generation of the same file
    if os.path.exists(out_path):
        old = open(out_path).read()
        for line in old.splitlines():
            m = re.match(r"- \[x\] (\*\*\d+ —[^\n]*)", line)
            if m and line.strip() not in new:
                new = new.replace("- [ ] " + m.group(1), "- [x] " + m.group(1), 1)

    open(out_path, "w").write(new)
    print(f"wrote {out_path}  ({len(items())} in-progress, {len(blocked())} blocked)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
