# CAD-1377 acceptance contract — interface the test pins

`tests/cad1377_reconcile_acceptance.rs` (author lane) drives the REAL
`cadence` binary. The implementation landed this exact surface —
confirmed by a real run of the check; the guard expectations did
not move.

## CLI

```
cadence issue reconcile --issue <ID> [--project <KEY>] [--dry-run] [--limit N] [--json]
```

- `--issue <ID>` selects exactly one ticket: the sweep classifies it
  and its worktree refs only; every other `doing`/`review` candidate
  is out of scope (no status rows, no finish rows, no probes of its
  recorded lanes).
- Unknown id or an id outside `--project` refuses non-zero BEFORE any
  git/gh probe. (The test does not yet exercise this; add if cheap.)
- `--limit N` bounds candidates examined within scope — with
  `--issue` it must never starve the selected ticket behind an
  earlier-sorting unrelated candidate (proven: unrelated `C77-1`
  sorts first, selected `C77-2` still closes under `--limit 1`).
  Bare `--limit 1` (no `--issue`) bounds the cleanup pass too: only
  the actually-examined candidate's lanes face the sweep, the
  unexamined one is reported under `deferred`/`cleanup_deferred`
  before any probe of its lane or branch, `partial` is `true`, and
  dry run changes nothing on disk.
- Selected ticket already `done` + live process in its lane:
  `finish::sweep_row`'s existing guard refuses/defers cleanup; the
  worktree and branch survive (status `done` is not delete proof).

## JSON report (per-row fields the test reads)

- `partial` — `true` when the run left candidates unexamined or a
  cleanup deferred/refused; a bounded or guarded run that reports
  complete is the false-green this check exists to catch.
- `deferred` / `cleanup_deferred` — issue ids the bound excluded
  from classification / cleanup.

- `rows[].issue` — status-classification rows carry the issue id
  (existing shape).
- `finish.rows[].issue` — worktree-sweep rows carry the issue id
  (existing `finish::sweep` shape under a `finish` key; if the
  targeted report nests it elsewhere, update `rows_for`).
- No fixed new key names are asserted beyond those two arrays — the
  implementer keeps freedom on `scope`/`examined`/`deferred` naming
  as long as the rows stay findable by `issue`.

## Non-goals confirmed

CLI-only selection; no new RPC/HTTP/auth route; no 20 s negative-proof
ceremony (guard deferral is proven by immediate file/branch survival,
not by elapsed time).

## Elevated CI fixture context

The check needs the kernel's real proc visibility: on the hosted
ubuntu-latest runner an unprivileged read of `2/ns/pid` fails EACCES
and the unchanged completeness guard correctly refuses. On Linux with
`GITHUB_ACTIONS=true` only, the outer test re-execs its own binary
through `/usr/bin/sudo -n -- /usr/bin/env -i` with a fixed env
(ISOLATED + ELEVATED markers, system PATH, the non-secret
CADENCE_SUITE_LOCK path). The root child enters the existing isolated
branch, allocates all private HOME/PM/state fixture dirs itself and
cleans them via TempDir; the run prints euid + `self/ns/pid` +
`2/ns/pid` link targets as proof evidence. Missing/denied sudo fails
the check — there is no unprivileged fallback, no `sudo -E`, no
shell, no credential or CADENCE identity forwarding, and the suite
itself never runs elevated. Non-CI runs never elevate. This is a
private-test fixture context only; no guard, daemon rule or
production path changes, and CI success is confirmed only by an
actual hosted run.
