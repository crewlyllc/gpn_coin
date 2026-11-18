# The procedure — how work gets done here

Written 2026-11-17, replacing every previous process document (rungs, plan docs, diagnosis queue).
If this file and any other description disagree, this file wins.

## 1. One list, four states

`projectmanager07/GPN-LIST.md` is the only queue.

| state | meaning | who sets it |
|---|---|---|
| `NEW` | added, not yet in a version | whatever adds work (me, owner, a bug report) |
| `IN-PROGRESS` | bundled into a version being built and released | the bundling step |
| `COMPLETE` | shipped and **verified** — see §4 | the closing step, never by hand |
| `BLOCKED` | cannot be completed now; reason + what would unblock it recorded | the item's owner |

No fifth state. No "done except the release". No "probably fine".

`GPN-TASKS.md` is the archive (183 rows, migrated); the list is generated from it by
`scripts/migrate-task-list.py` so nothing is ever lost.

## 2. The loop — doing, not checking

Each run takes **one step**, in this order:

**A. There are `IN-PROGRESS` items** → work the release checklist.

1. Find the checklist: `docs/releases/<version>.md`. If missing, generate it with
   `scripts/make-release-checklist.py <version>`.
2. Take the **first unticked box** and *do the work it names*. Not "verify the work exists" —
   if the box says "the node survives closing the window", close the window and make it survive.
3. Tick it only with evidence inline: version string, log line, commit sha, command output.
4. If it cannot be done: tick it as `blocked — <reason>` and set the list item `BLOCKED`
   if the blocker is permanent. A blocked box never holds the release hostage.
5. Stop after one box.

**B. Every box is ticked** → close the version.

`COMPLETE` requires all four, in this order, observed in this run:
   - the version is published (`releases.json` serves it, polled twice)
   - the artifact was scanned for the strings the change added
   - a node updated **itself** and the running process reports that version
   - the behaviour the item promised was observed, with a timestamp

Then mark every bundled item `COMPLETE` with the version and date. Not before.

**C. Nothing is `IN-PROGRESS`** → open a new version.

   - bundle **every** `NEW` item → mark them all `IN-PROGRESS`
   - one bump, one changelog section covering all of them, one push
   - generate the checklist for that version
   - stop; the next run starts at A

## 3. Rules that make it work

- **One version in flight.** While a checklist is open: no new bundling, no unrelated commits.
  `~/.gpn/release-in-flight` holds the lock; `release-lock.sh take` also arms the repo's
  pre-commit hook (idempotent, same as `scripts/install-git-hooks.sh`), so the guard is on by
  default rather than on a machine where someone remembered to install it.
- **Evidence or it did not happen.** A tick without a version string, log line or commit sha is
  not a tick.
- **Test first.** A fix starts with a test that fails; if it cannot be tested, it says so to the
  owner before being called done.
- **Diagnose in order:** logs first, then the version history around the failing area. Never guess
  between them.
- **Never delete a row.** `COMPLETE` and `BLOCKED` are the record.
- **One writer.** Only the release loop commits while a release is open.

## 4. What "COMPLETE" is not

Not: "the code is written". Not: "CI is green". Not: "a subagent reported success". Not: "the box
says it updated". Each of those is a step, and every one of them has been true while the feature
was absent, wrong, or not deployed. `COMPLETE` means a user could receive it today.

## 5. Flaws in this procedure, stated plainly

1. **A 20-minute run is a small unit of work.** Anything needing a build plus a deploy plus an
   observation spans several runs. Mitigation: the checklist holds the state between runs, so a
   half-done box is visible rather than lost. Risk: a run that starts a 40-minute Windows build
   and stops at the timeout leaves nothing ticked and looks idle.
2. **The loop can tick its own homework.** It both does the work and judges it. Mitigation:
   every tick needs pasted evidence, and §4 lists the shapes of self-deception. Residual risk:
   the evidence could be misread — which happened twice this week already.
3. **`COMPLETE` rows are inherited, not re-verified — accepted by the owner (2026-11-17).**
   168 rows arrived as `COMPLETE` from the archive and are taken as the record. A current
   decision that depends on one of them still re-checks it at that moment; there is no bulk
   re-audit.
4. **Blocked items can hide real work.** Ticking a box as "blocked" keeps the release moving and
   is how a genuine defect becomes invisible. Mitigation: blocked items stay on the list with a
   reason and are reviewed when the blocker is expected to clear.
5. **One writer is a single point of failure.** If the loop stalls — as it did for 22 hours on a
   rung — everything stalls. Mitigation: the loop has no priority ladder to get stuck behind;
   if it is running, it is doing the first unticked box.
6. **Bundling everything `NEW` can create an unshippable version.** Ten unrelated changes in one
   release means ten chances for a red build. **If the batch cannot be made shippable, escalate to
   Jeremy with the blocking item and what would fix it** — do not silently defer items out of the
   batch, and do not ship a partial version to avoid the conversation.

## 6. The failure this replaced

Every previous process was a description: rungs that outranked the list, plan documents nothing
read, a checklist with no executor, and a rule ("publish first") that depended on a job noticing.
The result was 168 real completions and a fleet that spent a week split into islands while the
documents said everything was fine. The difference here is not better prose — it is that each run
must *produce* something and each produced thing must carry proof.
