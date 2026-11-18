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

    section = []
    for iid, title, accept in items():
        # keep the box one readable line: the title up to the acceptance clause,
        # the acceptance indented beneath it, evidence appended under that.
        short = title.split(" — done when")[0].split(".**")[0]
        short = short.replace("**", "").strip()
        short = (short[:110] + "…") if len(short) > 110 else short
        section.append(f"- [ ] **{iid} — {short}**")
        section.append(f"      - done when: {accept[:200]}")
        section.append("      - notes:")
    # BLOCKED rows are deliberately NOT added (owner decision 2026-11-18): a release
    # checklist contains only what must be done to ship this bundle. Blocked work lives on
    # GPN-LIST.md with its reason; including it here inflated the tick count with lines
    # that had nothing to do with the release.
    if not items():
        section.append("- (no IN-PROGRESS items — nothing bundled)")

    # Insert where the template says to: replace the marker comment under
    # "## 3b. Items in this release". Fall back to the old anchor, then the end.
    marker_re = re.compile(
        r"<!-- make-release-checklist\.py replaces this comment.*?-->", re.S)
    block = "\n".join(section).strip("\n")
    if marker_re.search(body):
        body = marker_re.sub(block, body, count=1)
    elif "## 3b. Items in this release" in body:
        body = body.replace("## 3b. Items in this release", "## 3b. Items in this release\n\n" + block, 1)
    elif "## 4. Push (once)" in body:
        body = body.replace("## 4. Push (once)", block + "\n\n## 4. Push (once)", 1)
    else:
        body += "\n" + block

    header = f"# Release {version} — checklist\n\nGenerated from GPN-LIST.md by make-release-checklist.py.\n"
    new = header + body

    # Preserve ticks from a previous generation of the same file.
    # Match on a PREFIX, not the whole line: a ticked box usually has evidence
    # appended, so an exact-match compare loses every tick on regeneration.
    # (That bug wiped 0.3.60's progress on 2026-11-18.)
    if os.path.exists(out_path):
        old = open(out_path).read()
        ticked = {}
        for line in old.splitlines():
            if line.startswith("- [x] "):
                body = line[6:]
                ticked[body[:70]] = line
        if ticked:
            out_lines = []
            for line in new.splitlines():
                if line.startswith("- [ ] "):
                    body = line[6:]
                    key = body[:70]
                    if key in ticked:
                        out_lines.append(ticked[key])
                        continue
                out_lines.append(line)
            new = "\n".join(out_lines) + "\n"

    open(out_path, "w").write(new)
    print(f"wrote {out_path}  ({len(items())} in-progress, {len(blocked())} blocked)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
