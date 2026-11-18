#!/usr/bin/env python3
"""Per-section tick tally for a release checklist. Prints one line per section."""
import re, sys
path = sys.argv[1] if len(sys.argv) > 1 else None
if not path:
    print("usage: checklist-status.py docs/releases/<version>.md"); raise SystemExit(1)
sec, box, tick = None, 0, 0
rows = []
for line in open(path):
    m = re.match(r"^## (.+)$", line)
    if m:
        if sec: rows.append((sec, tick, box))
        sec, box, tick = m.group(1).strip(), 0, 0
        continue
    if line.startswith("- [x] "): box += 1; tick += 1
    elif line.startswith("- [ ] "): box += 1
if sec: rows.append((sec, tick, box))
total_t = sum(r[1] for r in rows); total_b = sum(r[2] for r in rows)
for s, t, b in rows:
    if b == 0: continue
    mark = "DONE" if t == b else f"{t}/{b}"
    print(f"  {mark:<8} {s}")
print(f"  TOTAL    {total_t}/{total_b}")
