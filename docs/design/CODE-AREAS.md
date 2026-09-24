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
  function has to own its whole file.
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
and an empty value clears it. The same glob rules apply.

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
`git diff --name-only <merge-base of HEAD and the default branch>` in
its worktree (committed or not). They are read locally, never from `gh`.

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

Residuals: area ownership lives in `PROJECT.md`, which agents can edit,
so a changed owner shows in the tracker's git history but needs no
operator approval. A same-uid process that writes the state dir directly
is the CAD-276 residual that every state-dir record shares.

## Board

The Overview's project summary lists each project's open lanes on one
line each: worker, PR, areas and overlapping lanes. Overlapping lanes
come first and are highlighted.
