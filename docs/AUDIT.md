# `cadence audit` — reconstruct every merge from stored data

`cadence audit` walks the default branch's merge history and rebuilds,
for every merged PR, the record that made it legal: the reviewed head,
the verdict, the reviewer and merger identities, the risk class and
what triggered it, the gate summary, and the post-merge outcome. It
exists because review evidence already lives in five places — merge
commits, the `qa-verdict` commit status, verdict and ops notes under
`/var/www/agent-notes/`, tracker folders, and the daemon's own event
and verdict tables — and nobody should have to re-derive a merge's
provenance by hand.

The command is **read-only**: it runs `git log`/`diff`/`patch-id`,
`gh pr list`/`gh api …/status`, file reads, and a
`SQLITE_OPEN_READ_ONLY` open on `cadence.sqlite3`. Nothing is written
at merge time and nothing is written by the audit itself — there is no
bookkeeping to drift.

```bash
cadence audit                          # every merge on the default branch
cadence audit --since 24h              # merges in the last day
cadence audit --class human            # only human-class merges
cadence audit --project cadence        # only merges whose issue lives in that project
cadence audit --json --limit 50        # machine form for digests/tiles
```

`--since` accepts `24h`, `7d`, `2w`, `YYYY-MM-DD` or a unix epoch.
`--limit 0` means no cap (default 200).

## Each row

```
#63 session start|end: one-verb session gate and teardown (CAD-92)
    merge 33a6a82 · merged_at 2026-09-20T10:53:00Z · merger cc-syntax
    reviewed_head 7896dd27 · landed_head 7896dd273 · contains_head yes (patch-id match)
    verdict pass · qa-verdict SUCCESS · reviewer qa-1
    class notify · trigger deletion and lifecycle paths; fourth review round
    gates suite=256/256 stress=none recorded flakes=none recorded
    auditor_check What an auditor should check after the merge: …
    residue CAD-147
    outcome tree_match=yes (patch-id match) smoke=unknown (no smoke record) daemon_restart=… revert=no
```

- **merge** — the squash commit on the default branch (from `git log`;
  cross-checked against `gh`'s `mergeCommit.oid`).
- **landed_head** — `headRefOid`: the head that actually landed.
- **reviewed_head** — the head the bound verdict note names. For a
  squash merge these agree; when they differ the verdict was on a head
  that never landed.
- **contains_head** — whether the merge carries the reviewed change:
  ancestry for true merges, `git patch-id` equality for squashes.
- **verdict** — `pass`/`fail` from the verdict note (or the store's
  verdicts table). `pass (post-merge)` when the note's own timestamp
  post-dates the merge.
- **qa-verdict** — the `qa-verdict` commit status on the *landed*
  head. The status does not record which agent posted it, so reviewer
  identity comes only from the note's `From:`.
- **class / trigger** — the verdict note's `Risk:` line.
- **gates** — suite result, stress runs and disclosed flakes as
  recorded in the verdict note (`none recorded` when the note is
  silent; `unknown` when no note binds at all).
- **auditor_check** — the reviewer's "what an auditor should check"
  line.
- **residue** — issue ids the verdict filed for follow-up.
- **outcome** — post-merge evidence: `tree_match` (computed), `smoke`
  and `daemon_restart` (ops-merge notes and daemon events), `revert`
  (a later `Revert` commit naming this merge).

A merge whose subject carries no `(#N)` is still listed as a `?` row —
a direct push to the default branch is exactly what the audit should
surface. The repository's root commit is exempt from flagging.

## The two flags

Both print as `FLAG[…]` on the row and make the command exit **1**.

- **`reviewer==merger`** — the identity that reviewed equals the
  identity that merged (verdict note `From:` vs `mergedBy.login`,
  case-insensitive). Self-merges are never legal.
- **`no-passing-verdict`** — nothing proves a `pass` on the exact head
  that landed: no verdict note/table row bound to `headRefOid`, and
  no `qa-verdict: SUCCESS` status on it. This fires on stale verdicts
  (the PR was rebased after review), on absent verdicts, and on every
  non-PR commit that isn't the root.

## `--json` shape (stable)

```json
{
  "schema": "cadence.audit/1",
  "repo": "…", "default_ref": "origin/main",
  "since": "24h", "since_epoch": 1789…,
  "filters": {"class": null, "project": null, "limit": null},
  "merges": [
    {
      "pr": 63, "title": "…",
      "merge_sha": "…", "merged_at": 1789…,
      "landed_head": "…", "reviewed_head": "…",
      "contains_head": "yes (patch-id match)",
      "qa_verdict_status": "SUCCESS",
      "verdict": "pass", "verdict_post_hoc": false,
      "reviewer": "qa-1", "merger": "cc-syntax",
      "class": "notify", "trigger": "…",
      "gates": ["…raw lines…"],
      "gate_summary": {"suite": "256/256", "stress": null, "flakes": null},
      "auditor_check": "…", "residue": ["CAD-147"],
      "project": "cadence", "issues": ["CAD-92"],
      "outcome": {"tree_match": "…", "smoke": "…",
                  "daemon_restart": "…", "revert": "…"},
      "flags": [],
      "unknowns": [{"field": "smoke", "reason": "…"}]
    }
  ],
  "summary": {"rows": 18, "flagged": 0,
              "flags": [], "by_class": {"auto": 1}}
}
```

Stability contract for CAD-87's digest and the Overview tile: field
names, nesting and the `schema` tag do not change; new fields may be
added. Every field that a source could not prove is `null` in JSON and
`unknown` in text, with the reason in `unknowns[]` — the audit never
guesses provenance.

## Hidden fixture flags (tests)

`--repo <path>` audits a checkout other than the cwd; `--notes-dir`
and `--merge-report <path>` substitute the notes directory and every
`gh` call (`{"prs": […], "statuses": {"<sha>": {…}}}`). A malformed
fixture is a hard error — a fixture can never fall back to a live
scan, and in fixture mode `gh` is never invoked.
