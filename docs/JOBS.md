# Cadence M3 — job/task lifecycle

Status: implemented (M3a). Messages stay the delivery axis; jobs and
tasks are a work axis layered on top. Nothing here weakens the existing
invariants — single-writer `BEGIN IMMEDIATE`, `unknown` fences, explicit
ready claims, transactional result+outbox, deterministic routing ids.

This document is the accepted design plus the seven amendments (A1–A7)
from the M3a kickoff, which follow from PRs 14–17 (durable inboxes,
operator reconcile for unknowns, the managed `claude` provider, the
issue board).

---

## 1. Data model (schema v4)

```sql
CREATE TABLE jobs(
    id TEXT PRIMARY KEY,              -- identifier charset, e.g. "m3-design"
    title TEXT,
    spec_path TEXT NOT NULL,          -- absolute path to the spec/brief
    spec_sha256 TEXT,                 -- content hash at creation (drift detection)
    pm_alias TEXT NOT NULL,           -- owning PM agent (the group root)
    issue_id TEXT,                    -- board issue <PREFIX>-<n> (A1), may be NULL
    repo TEXT,                        -- repo root the job's worktrees live under
    base_ref TEXT,                    -- base branch/SHA QA is relative to
    state TEXT NOT NULL,              -- open|done|failed|cancelled
    max_revisions INTEGER NOT NULL DEFAULT 2,
    error TEXT,
    created REAL NOT NULL, updated REAL NOT NULL);

CREATE TABLE tasks(
    id TEXT PRIMARY KEY,              -- globally unique, identifier charset
    job_id TEXT NOT NULL REFERENCES jobs(id),
    title TEXT,
    role TEXT NOT NULL DEFAULT 'implementer',  -- implementer (reviewer/merger are M3b)
    assignee TEXT,                    -- worker alias; NULL until dispatched
    spec_path TEXT,                   -- task-level spec (falls back to job's)
    acceptance TEXT,                  -- inline acceptance criteria
    worktree TEXT,                    -- .cadence/wt/<name> — the scope claim
    branch TEXT,                      -- cadence/<name>
    base_sha TEXT,                    -- revision the work starts from
    head_sha TEXT,                    -- the worker's reported revision
    state TEXT NOT NULL,              -- see §3
    revision INTEGER NOT NULL DEFAULT 0,     -- current attempt number
    dispatch_message TEXT,            -- message id of the live kickoff
    error TEXT,
    created REAL NOT NULL, updated REAL NOT NULL);
CREATE INDEX tasks_job ON tasks(job_id, state);

CREATE TABLE verdicts(
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL REFERENCES tasks(id),
    revision INTEGER NOT NULL,        -- the attempt this judges
    sha TEXT NOT NULL,                -- MUST equal tasks.head_sha for that revision
    verdict TEXT NOT NULL,            -- pass|revise|blocked
    reviewer TEXT NOT NULL,           -- agent alias or 'operator'
    evidence TEXT,                    -- JSON: commands run, outputs, paths
    message TEXT,                     -- message id that carried the QA report, if any
    verify TEXT,                      -- JSON: worktree checks {checked, skipped} (v5, CAD-51)
    created REAL NOT NULL);
CREATE INDEX verdicts_task ON verdicts(task_id, revision);
```

Attachment columns on existing tables:

```sql
ALTER TABLE messages ADD COLUMN task_id TEXT;   -- NULL = unattached delivery
ALTER TABLE events   ADD COLUMN job_id TEXT;
ALTER TABLE events   ADD COLUMN task_id TEXT;
CREATE INDEX msg_task   ON messages(task_id);
CREATE INDEX events_job ON events(job_id, seq);
```

- A message attaches to **one task** (the kickoff it carries, or a
  task-scoped follow-up via `send --task`). `messages.task_id →
  tasks.job_id` gives the job view; no `messages.job_id`.
- Events keep their per-alias row and additionally carry
  `job_id`/`task_id` when caused by a job operation. `job_events` is one
  indexed query — no new stream, no fan-out writes.
- Old messages migrate with `task_id NULL` — plain deliveries forever.

### Migration path (A7)

`schema_version → 4`, one transaction, the v2/v3 convergence pattern:
the actual current version is read from `schema_version`, column
existence is checked before each `ALTER`, `CREATE TABLE/INDEX IF NOT
EXISTS` covers the new objects, and the version bump commits last — an
interrupted or half-applied upgrade converges on reopen instead of
wedging. No data migration is needed.

### What does NOT change

- `messages` keeps its exact state machine and idempotency rule.
- `agents` keeps `params.upstream` as the group wire. No `params.job` —
  job membership derives from `tasks.assignee` + `jobs.pm_alias`.
- No `attempts` table. The kickoff message row is the attempt's durable
  record; `tasks.revision` numbers attempts; `verdicts` keys QA to them.

Deferred to M3b: `artifacts` table, reviewer/merger task roles,
`--workers-from`, stall detection, board changes.

GitHub bridge for `job verdict` (built, CAD-51): when the task is
worktree-scoped the CLI verifies the judged sha against the worktree
before the RPC (§3), and after the verdict commits it posts the
`qa-verdict` commit status on the PR head — the same call
`scripts/qa-verdict.sh` makes, which stays the manual path
([DOGFOOD.md](DOGFOOD.md#reviewer-verdict-gate)).

---

## 2. Job-scoped groups

- `jobs.pm_alias` is the group root — the job IS the group's current
  unit of coordinated work. **The PM may be an `inbox` agent (A5)** —
  `job new --pm <inbox-alias>` works; notifications are durable queue
  entries a consumer drains with `cadence inbox`.
- `tasks.assignee` must satisfy `assignee == pm_alias` (PM self-task) or
  `assignee.params.upstream == pm_alias`. A task can never be dispatched
  outside the job's group — the same wire routes results back to the PM.
- `job new` is **bookkeeping, not spawning**: registered PM + readable
  spec file → `open` job row + a default `<job>-t1` draft task. Agent
  lifecycle stays with `join`/`devin`/`codex`/`claude`.
- Idempotency: `--job <id>` is the client key; same id + same
  (pm, spec, sha, issue) → `duplicate:true`; different content →
  `rejected`.

### Board issue linkage (A1)

`jobs.issue_id` is nullable and validated only against the issue grammar
(`<PREFIX>-<n>`, e.g. `CAD-31`) — never against `~/pm` or the board
filesystem. One leaf issue maps to one non-terminal job. The issue shows
in `job show`/`job list`, rides in the routed `worker_result` payload,
and — when set — the kickoff body tells the worker to put the header
line `Issue: <ID>` in any agent-note it writes.

`cadence issue start <ID> --job --pm <alias> --spec <file>` is the
board-side entry point (CAD-43): it mints the issue's
`.cadence/wt/<id>-<slug>` worktree + `cadence/<id>-<slug>` branch in
the project repo, records both refs on the issue, and then opens the
job through `job_new` — `--issue <ID>`, `--repo`, `--base-ref <base
sha>` on the job, and `task_worktree`/`task_branch`/`task_base_sha`
/`task_assignee` scoping the default `<job>-t1` task, so the job has
exactly one task already bound to the worktree. The daemon, the PM
alias and any `--assignee` (PM or group member) are probed before
anything is created, so a daemon-down, unknown-PM or bad-assignee run
leaves nothing behind.

---

## 3. Lifecycle

```
delivery axis (messages, unchanged):
  queued → submitting → running → completed | failed | interrupted | unknown | cancelled

work axis (tasks):
  draft → dispatched → running → review ─┬─(verdict pass, sha==head)→ verified → done
            ↑              ↑             ├─(verdict revise, revision<max)→ revising →(re-dispatch)→ dispatched
            │              │             ├─(verdict revise, revision==max)→ blocked
            │              │             └─(verdict blocked)→ blocked
            │              └─ kickoff completes (sha via --sha or `SHA:` trailer)
            └─ kickoff enqueued
  blocked|verified|failed →(job task reopen)→ draft      any non-terminal → cancelled
  dispatched|running →(kickoff unknown/interrupted/failed)→ stays put, flagged (§5)
```

`verified → done` is the acceptance edge (`job accept [--merged-sha]`);
"merged" is evidence on the accept, never a resting state.

### Transition authority

| Transition | Actor | Mechanism |
|---|---|---|
| draft/revising → dispatched | PM (or operator) | `job dispatch` — one tx: task state + enqueue kickoff + events |
| dispatched → running | **daemon** | kickoff message observed `running` |
| running → review | **daemon** | kickoff `completed`; `head_sha` = `result.sha` else last `SHA:` line else NULL |
| review → verified/revising/blocked | reviewer or operator | `job verdict --sha` — one tx |
| blocked/verified/failed → draft | operator | `job task reopen` — re-scope, `revision` resets to 0 |
| → failed | PM/operator | `job task fail --reason` |
| → cancelled | PM/operator | `job task cancel` / `job cancel` |

The daemon edges fire inside `mark_running`/`finish`/`reconcile` **only
when `messages.source = 'job_dispatch'`** — `--task` follow-ups and
`job_event` notifications attach `task_id` for indexing but never drive
task state.

### Binding QA to an exact revision

1. **Reported SHA is protocol data.** `message_report` accepts `sha`;
   `cadence message result <id> --token <t> --text '…' --sha "$(git
   rev-parse HEAD)"`. Managed endpoints (codex/claude/fake) never call
   `message result` — their kickoff instead asks for a trailer line
   `SHA: <40-hex>` and the daemon takes the **last** such line of the
   result text as the SHA (A3). Completion with no SHA still lands in
   `review` with `head_sha NULL`; `cadence job task sha <task> <sha>`
   records it as an event, and `job verdict` on a NULL head is rejected
   naming that fix. A SHA is never invented or inferred.
2. **The verdict must name the SHA and match.** `job verdict --sha <s>`
   is rejected unless `sha == tasks.head_sha`, `state == 'review'`, and
   the optional `--revision` equals the current one. Verdicts are
   append-only, keyed `(task_id, revision)` — a stale revision can never
   overwrite a judged attempt.
3. **Reviewer independence (A4).** Inside a cadence pane the reviewer IS
   `CADENCE_ALIAS` — `--reviewer` is rejected there and `operator` cannot
   be claimed there. Outside a pane `--reviewer <alias|operator>` is
   required. `reviewer == assignee` is always rejected. The verdict event
   records the claimed reviewer and whether a pane alias was present —
   identity is self-asserted on a same-host socket.

Max-revision enforcement: `revise` while `revision >=
jobs.max_revisions` records the verdict but transitions to `blocked` —
the PM is notified once per verdict, and the loop cannot continue
without an operator `reopen`.

4. **Worktree verification (CAD-51).** When the task carries
   `worktree` + `branch` (every task `cadence issue start --job`
   mints), `job verdict` first checks — client-side, in the job's
   `repo`, each with a short timeout — that `--sha` resolves to a
   commit, equals the tip of `branch`, has `base_sha` as an ancestor,
   that `git status --porcelain` in the worktree is empty, and that
   `origin/<branch>` equals the sha (the commit is pushed). A failed
   check rejects before anything is written, naming the check and both
   values; checks that cannot apply (absent worktree dir, no `origin`,
   no `base_sha`) land in `verify.skipped`. The result is stored on the
   verdict row and echoed on `verdict_recorded`; `--no-verify-worktree`
   opts out and is recorded the same way.
5. **The qa-verdict bridge.** After the verdict commits, when the
   job's `repo` has a GitHub `origin` and an open PR exists for the
   task's branch (`gh pr list --head <branch>`, or `--pr <n>` to name
   it), the CLI posts a `qa-verdict` commit status on the judged sha —
   `success` for `--pass`, `failure` for `--revise`/`--blocked`,
   description `<verdict> — <task> r<revision>` capped at 140 chars,
   no `target_url`. The same call `scripts/qa-verdict.sh` makes.
   Posting never decides the verdict: missing `gh`, no PR, an API
   error, or a PR head that differs from `--sha` is reported
   `status: {posted: false, reason}` while the committed verdict
   stands — a status is never posted to a sha the reviewer did not
   name. `--no-status` skips the bridge.

---

## 4. CLI surface

```
cadence job new --pm <pm> --spec <file> [--job <id>] [--title t]
                [--issue CAD-31] [--repo <path>] [--base-ref <ref>]
                [--max-revisions 2] [--stall-secs <n>]
                [--task-title t]
                [--task-worktree <name>] [--task-branch <b>]
                [--task-base-sha <sha>] [--task-assignee <alias>]
cadence job list [--state s] [--all]
cadence job show <job>                 # job + tasks + verdicts + live kickoffs + drift flags
cadence job events <job> [--after n] [--wait s] [--follow]

cadence job task add <job> [--task <id>] [--title t] [--spec f]
                [--accept "..."] [--assignee w] [--worktree path]
                [--branch b] [--base-sha s]
cadence job task show <task>           # row + attached messages + verdicts
cadence job task sha <task> <sha>      # repair a NULL head_sha (event)
cadence job task fail <task> --reason "..."
cadence job task reopen <task>         # blocked|verified|failed → draft
cadence job task cancel <task>
cadence job dispatch <task> [--to <worker>] [--ready] [--message <id>]
cadence job verdict <task> --sha <sha> (--pass|--revise|--blocked)
                [--reviewer <alias>] [--evidence <file>]
                [--message <note-id>] [--revision <n>]
                [--no-verify-worktree] [--no-status] [--pr <n>]
cadence job accept <task> [--merged-sha <sha>]
cadence job cancel <job>               # all non-terminal tasks
cadence job close <job>                # legal only when every task is done
```

Additive changes to existing verbs:

- `send`/`ask` gain `--task <id>` — attach an ad-hoc message to a task.
- `message result` gains `--sha <sha>`.
- `agent list` rows gain `"task"`: the alias's current non-terminal
  assignment (derived, never stored).
- `cadence self` gains `"task": <id>` on each running message.
- `message reconcile` gains `--sha` — an operator-stated SHA on a
  `completed` reconcile binds exactly like a worker `--sha`.

`job dispatch` is sugar over `agent_send` plus bookkeeping: `--ready`
fuses the operator claim exactly like `send --ready`; without it the
kickoff waits on the existing ready gate. Dispatch to a dead-but-
registered worker is legal (`queued_behind_dead: true` warns) — it
delivers on resume; a fence (`attention`) is the only hard block.

### Kickoff schema (dispatch body)

Single line, ≤4000 chars, control-char free:

```
Cadence task <task-id> (job <job>, revision <n>): implement per spec at
<spec-path>. Scope: worktree <path>, branch <branch>, base <sha>.
Acceptance: <criteria>. Report when done:
`cadence message result <dispatch-id> --token <turn_id> --text '<summary>' --sha "$(git rev-parse HEAD)"`.
Do not report a SHA you have not committed. Correlation: <32-hex>.
```

`<criteria>` is the task's acceptance string, control characters
turned to spaces. When `issue start --job`/`dispatch --job` set it, it
is the issue's items as `1) [ ] "first"; 2) [x] "second"` — numbered,
each text a JSON string literal, so a `; ` or `[x]` inside an item
stays inside its quotes (CAD-300).

Explicit envelopes end with ` Correlation: <32-hex sha256 of the
dispatch id>.` — the pty render probe slices the body's tail, and the
derived suffix keeps that tail unique per dispatch id (bounded even for
`--message` overrides). Truncated bodies keep the same ending after the
report contract. Managed envelopes take no screen probe and carry no
correlation.

Managed endpoints get the trailer form instead:

```
 Report when done: end your final answer with a one-line summary
 followed by a last line `SHA: <40-hex>` naming the commit you
 produced — the daemon reads that line as the reported revision.
```

When `issue_id` is set the body adds: `This job tracks issue <ID> — if
you write an agent-note, put the header line `Issue: <ID>` in it.`

Dispatch message id: deterministic
`uuid5("cadence-dispatch:<task>:r<n>:a<k>")` where `k` counts prior
kickoffs — a re-dispatch while the kickoff is live returns the live id
(`duplicate:true`), a new revision mints fresh, and a reopened task
(revision reset to 0) can never collide with its earlier kickoffs.
`--message <id>` overrides for operators who prefer readable ids.

Result routing: kickoff `reply_to` = `jobs.pm_alias` explicitly. The
routed `worker_result` gains `"task"`/`"sha"` fields so the PM's
notification is self-describing.

### Turn tokens per endpoint (CAD-162)

`--token` is the running message's `turn_id` (`cadence self` prints it).
A report is accepted only when the token equals the recorded `turn_id`
AND is current for the agent's live endpoint generation under that
endpoint's own scheme — one predicate,
`adapter::registry::turn_token_current`, judges `message_report` and the
hot-restart adoption checks alike:

| Endpoint | Token minted today | Checkable? |
|---|---|---|
| any `pty` (claude, devin, cursor) | `pty-<generation>-<uuid>`, generation minted per pane open | yes |
| `claude` `managed` | `claude-<generation>-<uuid>`, generation minted per process open | yes — `ack` only; the turn result completes the message |
| `codex` `managed` / `managed-ws` | the provider's own turn id | no — every report refused |
| `devin` `cloud` | the message id (generation is the Devin session id) | no — every report refused |
| `fake`, `inbox` | counter / none | no — every report refused |

A token from an earlier generation, or one minted by a different
endpoint kind, is never current. An endpoint with no checkable scheme
fails closed; it completes through its adapter's turn result instead.

---

## 5. Failure semantics (A2)

Everything reuses the fence/unknown rules; the job layer adds visibility
and a disciplined retry path, never a blind replay.

- **Mid-job fence.** Kickoff → `unknown`, agent → `attention`: the task
  stays `dispatched`/`running`, flagged. `job show` says to inspect that
  kickoff and its side effects before reconciling. A missed render
  observation does not prove the delivery did not happen. Reconciliation
  is an explicit operator decision, not an automatic interrupted or
  completed result. CLI `agent unfence` resumes by default; do not follow
  it with a second resume. `--no-resume` reconciles without resuming.
  `job dispatch` is not the immediate recovery: it is a new paste and
  starts another revision only after reconciliation and a decision that
  continuation is safe. Once the kickoff is terminal for any reason
  other than normal completion (`interrupted`, `failed`, or a reconcile
  to either) dispatch is legal from `dispatched`/`running` and starts a
  new revision. An ordinary interrupted or failed kickoff still names
  `job dispatch` as that next revision. An operator reconcile to
  `completed` behaves like a normal completion, including the SHA rules
  above.
- **Retry idempotency.** Dispatch of the same revision with a live
  kickoff returns `duplicate` and never re-pastes. A new revision always
  mints a fresh id; the queue's envelope-conflict rule still rejects id
  reuse with different content.
- **Orphaned worker.** `job show` reports `assignee_dead`; `job dispatch
  --to <other>` reassigns (revision bumps; prior verdicts stay bound to
  the old revision+SHA). Reassigning under a *live* kickoff is refused —
  the pane may already hold the paste.
- **Cancel.** `job cancel` → job `cancelled`; every non-terminal task →
  `cancelled`; a kickoff still `queued`/`submitting` is cancelled in the
  same transaction. A `running` kickoff completes or fences on its own
  — its late result records but cannot transition the cancelled task.
  **Agents are never stopped by a job.**
- **Daemon restart.** `recover()` fences in-flight kickoffs `unknown`;
  tasks are untouched — no startup task reconciliation. `job show`
  computes drift lazily.
- **Job notifications are routed deliveries (A6).** PM notifications use
  `source='job_event'` — a member of `Message::is_routed()` alongside
  `worker_result`/`worker_notice`. They get the same fire-and-forget
  completion on pty, bounded render-miss retry, and park-instead-of-
  fence behaviour — a job notification can never fence or kill a PM
  pane. They are never agent turn results.
- **Stalled kickoffs notify the PM.** When a running kickoff goes
  silent past the resolved budget (`jobs.stall_secs` → the assignee's
  `stall_secs` param → daemon default 1800s; `0` disables), the daemon
  emits one `turn_stalled` event per silence episode — job/task-scoped,
  so `job events` pages it — and routes one `job_event` notice to the
  PM naming the task, the silence, and the read-only inspection
  commands. `turn_resumed` closes the episode to the same recipient
  and re-arms. The kickoff stays `running`; the stall watch never
  interrupts, fences, or replays the turn — it is a visibility edge,
  not a lifecycle one. While it lasts, `job show`/`task show` flag the
  task row with `stalled` + `silent_secs`. Non-kickoff deliveries
  (`send --task` follow-ups) notify their `reply_to` instead.

---

## 6. RPC surface

`job_new`, `job_list`, `job_show`, `job_events`, `task_new`,
`task_show`, `task_dispatch`, `task_verdict`, `task_accept`,
`task_sha`, `task_fail`, `task_reopen`, `task_cancel`, `job_cancel`,
`job_close`. Plus `message_report.sha`, `agent_send.task`, and the
`task` binding in `agent_list`/`self` output.

`job_new` also accepts optional task-scope params — `task_worktree`,
`task_branch`, `task_base_sha`, `task_assignee`, `task_acceptance`
(CAD-159: `issue start --job` passes the issue's acceptance items as
the numbered, JSON-quoted listing `1) [ ] "a"; 2) [x] "b"` that plain
`dispatch` inlines — see BOARD.md, CAD-300) —
applied to the default `<job>-t1` it mints (assignee is validated
against the job's group, same rule as `task_new`). Absent params keep
the original behaviour: an unscoped draft `t1`.

`task_verdict` accepts `verify` — the CLI's worktree-verification
result `{checked, skipped}` (§3), stored verbatim on the verdict row
and echoed on `verdict_recorded`. All network/git work stays
client-side; the daemon only persists what the CLI proved.

Capabilities: `job_lifecycle`, `revision_bound_verdicts`.

Job-scoped event kinds: `job_created`, `task_created`,
`task_dispatched`, `task_running`, `task_reported`, `verdict_recorded`,
`task_revising`, `task_blocked`, `task_reopened`, `task_failed`,
`task_cancelled`, `task_sha_recorded`, `task_done`, `job_closed`,
`job_cancelled`.
