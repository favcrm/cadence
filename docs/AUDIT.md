# `cadence audit` — reconstruct every merge from stored data

`cadence audit` walks the default branch's merge history and rebuilds,
for every merged PR, the record that made it legal: the reviewed head,
the verdict, the reviewer and merger identities, the risk class and
what triggered it, the operator approval for human-class merges, the
gate summary, and the post-merge outcome. It exists because review
evidence already lives in six places — merge commits, the
`qa-verdict` commit status, verdict and ops notes under
`/var/www/agent-notes/`, tracker folders, the daemon's own event and
verdict tables, and the daemon's operator approval records — and
nobody should have to re-derive a merge's provenance by hand.

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

Two operator verbs write the approval evidence the report reads (see
[Operator approval evidence](#operator-approval-evidence-the-human-class-gate)):

```bash
cadence audit approve --pr 84 --head 011d212bb5314e5c2dee959e7156732728a31018 \
    --source "chris in chat 2026-09-20T22:33Z"   # --repo defaults to the cwd's origin
cadence audit revoke merge-pr84-011d212bb531 --source "chris" --reason "head moved"
```

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
    approval operator-claimed · id merge-pr63-7896dd273503 · source "chris in chat" · head 7896dd273 · recorded 2026-09-20T10:50:02Z (before merge) · unverified until CAD-280
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
- **approval** — the operator approval bound to the landed head
  (below). Printed for human-class rows, and for any row an approval
  record binds to.

Merge rows come from `git log --first-parent`: squash merges parse
their `(#N)` suffix (anywhere in the subject — a trailing
`(rebased)` marker does not hide it) and true merges parse
`Merge pull request #N`. A commit whose subject carries no PR number
is still listed as a `?` row — a direct push to the default branch is
exactly what the audit should surface. The repository's root commit is
exempt from flagging.

## Flags

Each prints as `FLAG[…]` on the row and makes the command exit **1**.
A flag is reserved for a finding that discriminates — something that
is true of this merge and not of the fleet as a whole.

- **`no-passing-verdict`** — nothing proves a `pass` on the exact head
  that landed: no verdict note/table row bound to `headRefOid`, and
  no `qa-verdict: SUCCESS` status on it. A verdict note or status
  timestamped *after* the merge does not count — the question is what
  was true at merge time. This fires on stale verdicts (the PR was
  rebased after review), on absent verdicts, and on every non-PR
  commit that isn't the root — but never when a needed source did not
  answer.
- **`approval-missing`** — a human-class merge (the verdict note's
  `Risk: human`) with no operator approval in force at merge time for
  the exact landed head: no record, only a record for another head,
  or only a record written after the merge.
- **`approval-revoked`** — a human-class merge whose approval for the
  landed head was explicitly revoked before the merge.
- **`reviewer==merger`** — the `qa-verdict` status's `creator.login`
  equals `mergedBy.login` (case-insensitive), **in a run where the
  fleet's GitHub identities differ**. Self-merges are never legal. The
  note `From:` never feeds this flag — it is an agent alias, a
  different identity namespace. When every identity is one account,
  the match is structural — see the digest below.

## The digest: structural facts are reported once (CAD-207)

When every GitHub login the fleet names — every `mergedBy` of every
merged PR the run fetched, and every `qa-verdict` status creator it
holds — is **one account**, the
fleet pushes everything through a shared token, and
`reviewer==merger` holds on every row that has a status by
construction. A flag that fires on 100% of rows discriminates nothing
and buries the real findings, so in that case the match is:

- **not** a per-row flag — the row lists it under `structural` in
  JSON, and prints nothing extra in text;
- reported **once**, as a summary line:

  ```
  structural: 34 merge(s) share GitHub identity cc-syntax — the qa-verdict status and the merge were made by the same GitHub account on every row that has both — …; reported once, not flagged (docs/AUDIT.md)
  summary: 36 row(s), 2 flagged — no-passing-verdict, no-passing-verdict
  ```

- excluded from the exit code — a clean run against a shared-token
  fleet exits 0.

As soon as the run shows a second identity (a QA bot token posting
some statuses, say), a row whose status creator is its own merger
deviates from the fleet and keeps `FLAG[reviewer==merger]`. The
determination is made over the whole fetch, not the rendered window:
a narrow `--since`/`--class`/`--project`/`--limit` cannot turn a real
self-review into a structural one. Mergers cover every merged PR
`gh pr list` returned; status creators cover every status in hand —
all of them with `--merge-report`, only the rendered rows' on a live
run (fetching the rest would cost one API call per PR). The structural fact is
still a real limitation — attestation and merge cannot be told apart
under one token (see the trust model) — it is just not a per-merge
finding.

## Operator approval evidence (the human-class gate)

Class `human` merges wait for the operator
([`docs/roles/risk-classes.md`](roles/risk-classes.md)). The audit
can only verify that gate if the approval exists as durable evidence
bound to the exact head that landed — PR #84's approval lived only in
a queue message that was later cancelled, invisible to every
after-the-fact reconstruction (CAD-217).

**Records.** `cadence audit approve` writes one `approval_recorded`
event; `cadence audit revoke` writes one `approval_revoked` event.
Both go through the daemon (`approval_record` / `approval_revoke`
RPCs) onto a dedicated event stream, `audit:approvals` — not a valid
agent alias, so `agent rm` cannot delete it, and never pruned (the
`daemon` stream is trimmed to its newest rows; this one is not).

| field | meaning |
|---|---|
| `approval_id` | stable id for retries and revocation (default `<action>-pr<N>-<head[..12]>`; `-2`, `-3`, … once an earlier default was revoked) |
| `source` | who approved and where — a claim the record carries, never authority by itself; `user` and `daemon` are refused (they are delivery/stream identities, not operators) |
| `action` | what was approved (`merge`; the audit binds only `merge`) |
| `head_sha` | the full 40-hex head the approval names |
| `scope` | `{repo: "owner/name", pr: N}` |
| `recorded_via` | stamped by the daemon: how the writer was authorized (`operator-connection`) |
| time | the event time — when the operator recorded it |

A revocation carries `approval_id`, `source`, `reason` and its time,
and must name a recorded approval. An identical re-send of a live
approval dedupes (`duplicate: true`); the same id naming a different
head, source, action or scope — or a second, different revocation —
is refused. An id is never reused after a revoke: re-recording a
revoked `--id` is refused with a message naming the revoke, and with
no `--id` the daemon counts the default up (`<base>-2`, …), so
`approve → revoke → approve` records a fresh approval.

**Who may record.** Only an operator connection, by exactly the rule
`slot_reconcile` applies (CAD-230, CAD-276). The daemon derives the
caller from the socket's `SO_PEERCRED` pid and its `/proc` ancestry: a
caller that descends from a registered pane or an enrolled managed
endpoint is an agent and is refused, and so is one that is not
*provably* the operator (`peer::operator_proof`: the daemon's uid, a
fully readable ancestry with no pane, endpoint or daemon-launched hop
and no `CADENCE_ALIAS`, no pane's pty on its stdio, and a session
leader on its own ancestry). Identity-shaped request fields (`by`,
`operator`, `actor`, `alias`, `lane`, `pid`, `pane`, `recorded_via`)
are refused rather than read. Worker output, worker messages and queue
messages are never authority. Accepted residual, inherited from
CAD-276: a same-uid process that leaves every agent's ancestry without
orphaning its session (`setsid -f`) and scrubs its env and stdio still
passes. Operator-by-positive-proof as a design decision
(tracker issue **CAD-280**, "operator by positive proof") will replace this rule for
board writes, `slot_reconcile` and these verbs alike.

**Approvals are operator claims, not proof — until CAD-280.** Two
gaps mean a bound record cannot prove the operator approved:

1. The residual above is real and reproduced: from inside an agent's
   pane, `env -u CADENCE_ALIAS setsid -f … cadence audit approve …`
   passes the gate, and the record is stamped
   `recorded_via: operator-connection`. An agent could so "approve"
   its own human-class merge.
2. Any process running as the daemon's uid can write rows straight
   into `cadence.sqlite3`, bypassing the daemon entirely.

So the audit never reports a bound approval as verified: its state is
**`operator-claimed`**, the row carries `verified: false` and the note
`unverified until CAD-280` (text: `· unverified until CAD-280` on the
approval line), and the summary line states how many approvals are
operator-claimed. The gate still discriminates what it can — a
missing or revoked approval is a real finding — but a claimed one is
a claim.

**What is never an approval.** Queue and message state is context
only: a message whose body says `OPERATOR APPROVED #84 at <sha>` —
queued, delivered or CANCELLED — neither records nor revokes
anything, and the audit does not read messages at all. Cancelling a
delivery is not revoking an authorization. The records grant nothing
either: dispatch and merge never read them.

**Binding.** For each row the audit looks for records with action
`merge`, the row's PR number, the audit's own `owner/name` (gh's
repo, or the checkout's `origin` in fixture runs; unchecked only when
neither is a github.com remote), and `head_sha` **equal to the full
landed head** (`headRefOid`). An approval for an older head never
counts for a newer one — it is listed under `other_heads` and named
in the reason. States:

- **`operator-claimed`** — a record for the landed head was recorded
  before the merge and not revoked before it. A revocation after the
  merge is shown but does not change the state: the question is what
  held when the merge ran. Not flagged, and not verified (above).
- **`revoked`** — every pre-merge record for the landed head was
  revoked before the merge. Flags `approval-revoked` on human rows.
- **`missing`** — the approval stream answered and no record was in
  force at merge time. A record written after the merge (a backfill)
  is shown as the row's record with `before_merge: false`, but does
  not clear the gate. Flags `approval-missing` on human rows.
- **`unknown`** — the question could not be answered: no daemon
  store on this host (approval records live nowhere else), the store
  or stream unreadable, or no landed head to bind. Reported with its
  reason under `unknowns[]`, **never flagged** and never a non-zero
  exit on its own. Also used for rows with no risk class recorded.
- **`not-required`** — an `auto`/`notify` row with no record. A
  non-human row an approval binds to shows that record's state for
  context; only human rows flag.

"Before the merge" compares the daemon host's clock (the record's
event time) with GitHub's `mergedAt`: skew on the daemon host shifts
the window by that much. The audit assumes the host keeps NTP time.

Historical human-class merges from before the recorder existed have
no records, so they report `approval-missing` — that is accurate: at
merge time no durable approval existed.

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
(a shared bot token), the attestation and the merge share one
identity on every such merge. That is a fleet-wide fact, reported
once as the `structural:` summary line rather than flagged per row.

An approval record is as strong as the operator rule that admitted
its writer (above) and the store file it lives in: the daemon refuses
agent connections it can attribute, but a detached same-uid process
passes, and any same-uid process can write the SQLite file directly —
hence `operator-claimed`, never verified, until CAD-280. The record's
`source` is the operator's own statement of who approved and where.

What the audit **cannot** detect:

- Whether the GitHub login that posted `qa-verdict` is the same human
  as the agent alias in `From:` — the namespaces cannot be joined.
  The flag answers the narrower question "did the merge's GitHub
  actor also post its verdict status".
- Evidence that was never written: a verbal approval leaves no trace
  unless the operator records it with `cadence audit approve`.
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
      "structural": [],
      "approval": {
        "required": false, "state": "not-required", "reason": null,
        "record": null, "before_merge": null, "revocation": null,
        "other_heads": [], "verified": null, "note": null
      },
      "evidence_unavailable": null,
      "unknowns": [{"field": "smoke", "reason": "…"}]
    }
  ],
  "summary": {"rows": 18, "flagged": 0,
              "flags": [], "structural": [],
              "approvals": {"operator_claimed": 0, "verified": false,
                            "note": "unverified until CAD-280"},
              "by_class": {"auto": 1}}
}
```

`approval.record` (and each `other_heads[]` entry) is
`{id, source, action, head_sha, scope: {repo, pr}, recorded_via,
recorded_at}`; `approval.revocation` is `{source, reason, revoked_at,
before_merge}`; `approval.verified` is `false` whenever a record is
bound (`null` otherwise) and `approval.note` then reads
`unverified until CAD-280`; `summary.approvals` is
`{operator_claimed, verified: false, note}`; `approval.required` is `true` for human rows, `false`
for other classes and `null` when no class is recorded. A row's
`structural` lists matches the digest moved out of `flags`
(`["reviewer==merger"]`); `summary.structural` holds one entry per
fleet-level fact:
`{code: "shared-github-identity", match: "reviewer==merger",
identity, rows, explanation}`.

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
