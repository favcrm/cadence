# Code areas and advisory path leases (CAD-378)

Parallel lanes rarely break on text conflicts: the merge queue catches
those. They break on **ownership**: a lane reworks rules another epic
owns, or two lanes build the same primitive at once. Cadence now warns
about both early. It never refuses work, because the operator prefers
pace (operator decision 2026-09-24: options D and A; hard leases (B) are
out; PR-level CI enforcement (C) comes later if drift continues).

## Areas: `PROJECT.md` frontmatter

```yaml
---
project: cadence
areas:
  caller-identity:
    paths: [src/peer.rs, src/daemon/caller_rule.rs]
    owner: CAD-411/pm-cc      # an epic id, a PM alias, or EPIC/pm-alias
    max_open_prs: 2           # optional; omit for no limit
  board-ui:
    paths: [ui/]
    owner: pm-ui
---
```

- `paths` are repo-relative, **file-level** globs. `*` and `?` match
  within one path segment. A `**` segment matches any depth. A trailing
  `/` (or a plain directory path) covers everything under it.
  Symbol-level areas such as `src/daemon.rs#slot_identity` are refused
  as a config error. That is a known limitation: an area that needs one
  function has to own its whole file. Matching is linear — a glob of
  stacked `*`s cannot stall a render — and a path is refused outright
  past 256 bytes or 16 wildcards.
- An unknown key, an empty `paths`, an absolute path or one containing
  `..`, a malformed owner, or `max_open_prs: 0` is an error that names
  the area. `issue lint` warns about it. `issue start`/`dispatch` report
  it as `leases.config_error`, the overview shows it as `areas_error`,
  and area checks are skipped until it is fixed. The work model's stage
  gates read the same file but ignore `areas:`, so a bad area never
  blocks a stage move.
- A ticket is on the **owner's side** of an area when it is the owning
  epic or one of the epic's tickets (a child, or a ticket of the epic's
  plan), or when the dispatching PM is the owner PM.

## Planned paths: `paths:` on an issue

```sh
cadence issue set CAD-500 paths=src/peer.rs,src/daemon/
```

The value is stored sorted and de-duplicated in the issue frontmatter,
and an empty value clears it. The same glob rules apply — and they are
applied again on load: a hand-planted `paths:` entry a write would have
refused is dropped, never matched.

## What warns (never refuses)

`issue start` and `cadence dispatch` compute a `leases` block from the
ticket's planned paths. They print each warning to stderr. When the lane
is new, the warnings are recorded on the issue as a comment of kind
`lease`. A dispatch also adds them to its dispatch comment.

| kind | when |
|---|---|
| `overlap` | a planned path overlaps an open lane's planned paths **or its actual changed files**; the warning names that lane's ticket, worker and PR |
| `owned` | a planned path is in an area owned by someone other than this ticket's side; the warning names the owner and the open lanes there |
| `capacity` | the area already holds `max_open_prs` open lanes; the warning names them |

An **open lane** is an issue that is not done or dropped and has an open
worktree ref. Its changed files come from
`git diff --name-only <merge-base> <rev>` — **committed
work only**. Both sides of the diff are objects, so the read never
hashes the working tree: a lane's `.gitattributes` filters, fsmonitor
command and hooks cannot run inside the board process, no matter what
its worktree config says. The price of that safety is honest:
uncommitted edits and untracked files are invisible to the board — it
warns on what a lane has committed. Lane probes run on a bounded pool
(four git readers at once), each bounded by a timeout.

What the probe reads — the worktree directory and the `<rev>` it diffs
— comes from the **daemon's dispatch record**, not the live tracker.
`dispatches.json` in the daemon's state dir holds one record per issue:
the worktree and branch the kickoff bound, the kickoff's message id,
and the `pm` — the connection-derived caller `dispatch_record` saw
(an agent records only a dispatch it really sent; `master_dispatch`
writes the record in-daemon with the master's alias). `cadence
dispatch` writes it after the kickoff send; a record failure is
reported on the dispatch result, never silently dropped.

A lane with no dispatch record is **unbound**: `pm` is empty, `head`
pins nothing, and its frontmatter worktree still feeds the advisory
probe — nothing else. Unbound lanes' `area_ack` rows are **never
suppressed**: unknown means needs-ack, not empty-and-silent. The same
rule covers a bound lane whose recorded dir or branch can no longer be
probed — the row raises and names the lane's planned paths (or says
the changed files are unknown outright) instead of passing on an
empty diff.

`claim.by`, frontmatter refs, `Actor:` commit trailers and tracker
comments bind **nothing** — every one of them is a file the lane can
write. `parent` and `plan_epic` — the epic side of "the owner's side"
— stay frontmatter and remain advisory: a lane that claims membership
of the owning epic is the same class of self-assertion the feature
tolerates.

## Needs-you: the owner's ack

When an open lane that has a PR (an open `pr` ref, the review loop's
record, or an open PR branch) changes files in an area owned by someone
else, the overview shows an `area_ack` row for the owner's PM. The row
escalates to the operator like any team row. It clears once the owner
acks:

```sh
cadence issue ack CAD-500 --area caller-identity [--note "agreed in thread"]
```

The daemon's `area_ack` binds the acker from the connection. Only the
proven operator or the agent whose alias is the area's owner PM may ack.
It refuses any other agent (the lane's own PM included), a detached
child of the owner, and identity-shaped request fields. The ack is
stored in the daemon's state dir (`area_acks.json`) together with an
`area_acked` event. It is deliberately **not** a tracker comment:
tracker files accept any author a writer names, so a comment could
forge the owner.

An ack pins the recorded branch's committed tip (`head`) and records
the files it covered — the record's worktree and branch, never a live
frontmatter ref the lane can re-point after the fact. The row stays
down only while that branch's head is still the pinned commit — a lane
that commits again re-raises it, whether or not the new commits touch
the area, so a fresh change gets a fresh look. Acks never expire by
age; only a new head renews the question. Acks written before pinning
existed carry no `head` and suppress nothing. An unbound lane cannot
be acked at all: nothing a daemon saw exists to pin, so `area_ack`
refuses it by name and its row stays up until the lane is dispatched
through the daemon or closed.

Warning text and lease comments are scrubbed of control and bidi
characters before they reach a terminal or a tracker file: refs,
aliases and planted frontmatter are all agent-writable.

Residuals: area ownership lives in `PROJECT.md`, which agents can edit
— deleting or renaming an area silences its rows, and a changed owner
shows in the tracker's git history but needs no operator approval;
advisory by design. `dispatch_record` trusts the connection, so a lane
bound at dispatch stays bound — but a same-uid process that writes the
state dir directly can rewrite `dispatches.json`/`area_acks.json`
(the CAD-276 residual every state-dir record shares), which is why the
feature warns and never refuses.

## Board

The Overview's project summary lists each project's open lanes on one
line each: worker, PR, areas and overlapping lanes. Overlapping lanes
come first and are highlighted.
