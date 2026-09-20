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

The command is **read-only**: it runs `git log --first-parent` /
`diff`/`patch-id`, `gh pr list` plus `gh api …/statuses` (and
`…/status` for the check-run fallback), file reads, and a
`SQLITE_OPEN_READ_ONLY` open on `cadence.sqlite3`. Nothing is written
at merge time and nothing is written by the audit itself — there is no
bookkeeping to drift. Every subprocess is time-bounded; the per-head
status calls run only for rows that survive `--since`/`--class`/
`--project`, so `--limit` bounds the GitHub fan-out.

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
    verdict pass · qa-verdict SUCCESS · reviewer qa-1 · reviewer@gh qa-bot
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
  head: its state, the GitHub `creator.login` that posted it
  (`reviewer@gh` in text), and whether it post-dates the merge
  (`(post-merge)`). The note's `From:` stays the display reviewer —
  the status records the *GitHub* identity, the note records the
  *agent* identity.
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

Merge rows come from `git log --first-parent`: squash merges parse
their `(#N)` suffix (anywhere in the subject — a trailing
`(rebased)` marker does not hide it) and true merges parse
`Merge pull request #N`. A commit whose subject carries no PR number
is still listed as a `?` row — a direct push to the default branch is
exactly what the audit should surface. The repository's root commit is
exempt from flagging.

## The two flags

Both print as `FLAG[…]` on the row and make the command exit **1**.

- **`reviewer==merger`** — the `qa-verdict` status's `creator.login`
  equals `mergedBy.login` (case-insensitive). Self-merges are never
  legal. The note `From:` never feeds this flag — it is an agent
  alias, a different identity namespace.
- **`no-passing-verdict`** — nothing proves a `pass` on the exact head
  that landed: no verdict note/table row bound to `headRefOid`, and
  no `qa-verdict: SUCCESS` status on it. A verdict note or status
  timestamped *after* the merge does not count — the question is what
  was true at merge time. This fires on stale verdicts (the PR was
  rebased after review), on absent verdicts, and on every non-PR
  commit that isn't the root — but never when a needed source did not
  answer.

## Trust model

The audit compares identities only inside one namespace:

- **`mergedBy.login`** and a status's **`creator.login`** are GitHub
  identities — authoritative for "who clicked merge" and "who posted
  the status", and the only pair `reviewer==merger` compares.
- A verdict note's **`From:`** is an agent alias. It renders as
  `reviewer`, but an alias string-matching a GitHub login is a
  coincidence of naming, not proof of self-review.

One caveat on identity strength: notes can be written by any agent —
a `From:` alias and even a `pass` verdict are forgeable, so a bound
note satisfies `no-passing-verdict` but its alias never feeds
`reviewer==merger`. A commit status is only as strong as its token:
if QA posts `qa-verdict` and merges through the *same* GitHub account
(a shared bot token), `reviewer==merger` fires on every such merge —
correctly, and that is the finding: the attestation and the merge
share one identity.

What the audit **cannot** detect:

- Whether the GitHub login that posted `qa-verdict` is the same human
  as the agent alias in `From:` — the namespaces cannot be joined.
  The flag answers the narrower question "did the merge's GitHub
  actor also post its verdict status".
- Evidence that was never written: a verbal approval leaves no trace
  in any of the five sources.
- What a source would have said when it did not answer — gh down,
  notes dir unreadable, store unopenable, or the reviewed head absent
  from the clone renders as `evidence unavailable`: the row is
  unknown, unflagged, and the command still exits 0.

`no-passing-verdict` therefore means "every reachable source answered,
and none proves a pass bound to the landed head" — never "a source
was down".

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
      "qa_verdict_status": "SUCCESS", "qa_verdict_creator": "qa-bot",
      "status_post_hoc": false,
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
      "evidence_unavailable": null,
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
