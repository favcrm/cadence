# 0002 — The steering contract: amendments that cannot replace the objective

- Status: **proposed**
- Date: 2026-09-20
- Author: `arch-1` (architect)
- Deciders: operator (whether cadence may refuse a worker's completion), PM (ticket order)
- Issues: CAD-157 (this), relates AOS-12, CAD-78 (task roles), CAD-138
- Code citations pinned to `33a6a82`. Line numbers move; re-locate by symbol.

## 1. Context

On 2026-09-20 an AgenticOS worker was dispatched to implement Hono / D1 /
Drizzle / Better Auth email OTP **plus tests**. A later clarification
changed one detail — reuse the V1 email provider. The worker produced
provider docs at `b4e5a2d`, opened PR4, reported a result about the
clarification, and went idle. The implementation was absent. Nobody
noticed until review, and the operator had to redispatch.

The report is careful, and this ADR keeps that care: *"This is observed
agent behavior, not proven controller root cause."* What follows is what
the controller demonstrably does, which is enough to design a contract
whether or not the model's behaviour was the proximate cause.

This matters more than ergonomics. With the operator auditing rather than
approving (ADR-0001, charter L3→L4), **a silently narrowed task is the
failure mode that escapes**: everything downstream looks healthy. The
message completed, the turn ended cleanly, the agent went idle, a PR
exists. Every signal cadence currently emits says success.

### 1.1 What the controller actually does

| Behaviour | Evidence |
|---|---|
| A send to a running agent is **never** injected; it is queued | `rpc_send` → `enqueue_tx` with no check on agent state (`daemon.rs:1194-1220`, `store.rs:1139-1189`); the actor loop is strictly serial (`daemon.rs:664-830`) |
| For managed Claude it becomes a **fresh turn on the same session**, full prior context intact | `run_turn` writes one user line to the long-lived child and blocks until the next `result` (`adapter/claude.rs:486-620`) |
| By then the kickoff has **already completed and already moved the task to `review`** | `Store::finish` routes the result and calls `task_on_completed`, binding `head_sha` (`store.rs:1350-1400`, `:3154-3161`) |
| Only `source='job_dispatch'` drives task state | `task_on_running`/`task_on_completed` early-return on any other source (`store.rs:3112-3122`, `:3154-3161`) |
| So a steering message is **invisible to the task axis** | `send --task` sets `messages.task_id` for indexing only (`store.rs:1119-1131`) |
| Managed Claude **cannot acknowledge without completing** | registry marks claude/managed `Reporting::TurnResult` (`adapter/registry.rs:192-193`); every pty row is `Explicit` (`:236-237`). Its one result text is simultaneously "understood" and "done" |
| pty *can* ack — the distinction exists, one endpoint short | `message_report kind:"ack"` → `mark_ack` keeps the message `running` (`daemon.rs:1741-1815`, `store.rs:1307-1332`) |
| Going idle with work outstanding is **unmodelled** | the drift check fires only when the kickoff is terminal **and not `completed`** (`daemon.rs:2107-2145`) |
| The stall watch cannot see it either | it measures silence of the running message only, and explicitly never interrupts or re-dispatches (`daemon.rs:2546-2549`) |

Read the last three rows together. **A kickoff that completes normally
always looks healthy.** An agent that finishes fast with a confident,
wrong result is invisible to every watchdog cadence has, because all of
them are tuned for silence, staleness or failure — and this failure is
none of those.

### 1.2 The acceptance criteria were thrown away first

`tasks.acceptance` exists (`store.rs:571-587`). It is a single free-text
string. It is interpolated into the kickoff prose as `" Acceptance: {}."`
(`store.rs:3283-3287`) and **it is the first thing truncated** when the
body exceeds the 4 000-character pty ceiling, replaced by
`"… (truncated — full criteria in the spec file)."` (`store.rs:3336-3350`).
Nothing ever reads it back.

Meanwhile the tracker writes a `## Acceptance` heading into every new
issue (`issue/write.rs:377`) and ships a checkbox counter,
`parse::checkbox_progress`, which counts `- [x]`/`- [ ]` anywhere in a
body (`issue/parse.rs:68-92`). Its only consumer is a board card
(`issue/board.rs:483`). It gates nothing; `issue/finish.rs` never calls it.

I measured the result across the whole tracker:

> **144 of 144 issues have an `## Acceptance` heading. All 144 are empty.**

So the slot for a checklist exists, the parser for it exists, the display
for it exists — and it has never been used once. That is why partial
completion was undetectable: **there was nothing to compare the result
against.** This also means CAD-157's "durable parent objective +
acceptance checklist" does not need a new system, which is what the
ticket asks for ("use existing jobs/verdicts where possible, do not
invent duplicate task systems").

### 1.3 One live injection path does exist, and it is unrecorded

`cadence send <w> --ready --force` claims a pty readiness slot on a busy
pane, pasting mid-turn — the code comment says the force exists "rather
than letting a paste land mid-turn" (`adapter/pty/mod.rs:991-1018`).
There is no `steer` event and no link to the in-flight message id.

So today steering is either invisible (queued, managed) or untraceable
(forced, pty). Neither is a contract.

## 2. What "done" looks like

- **D1 Objective survives.** After any amendment, the original objective
  and its outstanding criteria are still addressable by the agent and
  visible to the reviewer, as a distinct record from the amendment.
- **D2 Amendments are typed.** A message that changes scope must say so;
  a clarification cannot narrow scope as a side effect.
- **D3 Ack ≠ complete.** An agent can answer a question without that
  answer counting as "the task is done" — on every endpoint, not just pty.
- **D4 Partial completion is caught before review.** A result that leaves
  declared criteria unaddressed does not present as reviewable; it is
  flagged, with the specific outstanding criteria named.
- **D5 Idle-with-outstanding-work is detected**, and the continuation is
  bounded: no runaway retries, no broadened authority, and a user stop or
  cancel is always respected.

Non-goals: cadence verifying criteria itself (that is `cadence review`
and the reviewer); a second task system; anything that makes legitimate
mid-flight correction harder.

## 3. Options

### Option A — Do nothing

The reviewer catches it.

- D1–D5 ✗. It *did* catch it — at review, after a wasted turn, a
  misleading PR and an operator redispatch.
- The reason this is not merely slow: §1.1 shows every automated signal
  reported success. As the operator moves from approving to auditing,
  "the reviewer will notice" becomes the only defence, and it is applied
  after the work is done rather than before it is reported. Rejected, but
  it correctly sets the bar — the system survives this failure, so no fix
  may introduce a worse one (e.g. blocking legitimate steering).

### Option B — The smallest thing that could work: compose the objective into every steering message

When a message is sent to an agent with an open task, the delivered body
is composed: the amendment, then a restatement of the objective, then the
outstanding acceptance criteria. No schema change beyond populating
criteria; no new states.

- D1 ✓ D2 ✗ D3 ✗ D4 ✗ D5 ✗.
- Strong for its size, and it attacks the observed mechanism directly: a
  fresh turn carrying "here is the amendment **and** here is what you are
  still on the hook for" is much harder to read as a replacement than a
  bare "actually, use V1". It needs no agreement about who may refuse
  what.
- Weakness: it is a prompt-shaped fix for a protocol-shaped problem. It
  improves the odds; it does not make the failure detectable. If the
  agent still narrows, nothing notices — which is the actual complaint.
- **Adopted as phase 1**, on the explicit understanding that it is
  mitigation, not the contract.

### Option C — Typed amendments plus a partial-completion gate

Steering becomes a first-class, typed, recorded amendment to the task's
kickoff; acceptance criteria become a per-criterion checklist; a
completion that leaves criteria unaddressed does not reach `review`.

- D1–D5 ✓. Cost: a new message source, an amendment record, per-criterion
  state, and a change to the task transition — the trust boundary and the
  store (risk triggers 1, 2).

### Option D — Rejected: tell agents in their briefing not to drop the objective

Rejected on the same grounds ADR-0001 §3 Option E rejected it: this is
what we already do, and instructions degrade. CAD-75's own design note is
that every relay hop degrades instructions; the incident is that note
coming true. A refusal does not degrade.

### Option E — Rejected: queue all steering until the turn ends, forbid mid-turn entirely

Safe, and it destroys the legitimate case: the main value of steering is
correcting a turn *early*, before the agent spends an hour going the
wrong way. Forbidding it would push operators back to `--ready --force`
(§1.3), which is the untraceable path. A contract people route around is
not a contract.

### Option F — Rejected: cadence verifies acceptance criteria itself

Have cadence run each criterion and decide.

Rejected as overreach and as a duplicate system. `cadence review` is the
gate runner and deliberately "never posts a status, never merges, never
pushes" (`review.rs:1-9`); per-criterion verification belongs to the
reviewer. What cadence can do without judgement is compare a **declared**
list against an **explicitly claimed** disposition — arithmetic, not
evaluation. That distinction is what keeps Option C honest.

## 4. Decision

**Adopt Option C, in three phases; phase 1 is Option B.**

| Phase | Content | Satisfies | Risk class |
|---|---|---|---|
| 1 | Compose objective + outstanding criteria into every task-bound message; make `## Acceptance` real (see §5.1) and refuse to dispatch an issue with no criteria | D1 | `auto` for the tracker rule; `human` for kickoff composition (touches dispatch) |
| 2 | Typed amendments: `source='steer'`, an amendment record bound to the task, the verb and its refusals; ack-vs-complete on managed endpoints | D2, D3 | `human` (triggers 1, 2) |
| 3 | Per-criterion disposition on completion; task does not enter `review` with unaddressed criteria; bounded idle-with-outstanding-work detection | D4, D5 | `human` (triggers 1, 2) |

Phase 1 first because it is the only phase that helps **before** anyone
agrees on the harder questions in §8, and because §1.2 shows its
precondition — criteria that actually exist — is missing today and blocks
every later phase. Nothing in phases 2–3 can work while 144 of 144
acceptance sections are empty.

## 5. Design

### 5.1 The objective of record, and criteria that exist

The objective of record is **already** well defined and should not be
reinvented: the task's dispatch kickoff, `source='job_dispatch'`, pointed
at by `tasks.dispatch_message`, the only source that drives task state.
This ADR does not add an objective; it stops other messages from being
mistaken for one.

Criteria move from prose to a list:

- The issue's `## Acceptance` section holds `- [ ]` items. The parser
  already exists (`parse::checkbox_progress`); it gains a sibling that
  returns the item *text*, not just counts.
- `tasks.acceptance` keeps the prose for compatibility but gains a
  structured list; the list is what gates, the prose is what explains.
- **`cadence dispatch` refuses an issue whose acceptance list is empty.**
  This is the cheapest rule in the ADR and the one that would have made
  AOS-12 visible: you may not dispatch work whose done-ness is undefined.
  It is also consistent with my own briefing — every acceptance criterion
  is something `qa-1` can run.
- Criteria are **never truncated**. §1.2's truncation order is inverted:
  if a kickoff exceeds the ceiling, the prose narrative is truncated and
  the criteria list is kept, because the list is the part that is
  machine-compared later. Dropping the contract to fit the story is
  exactly backwards.

### 5.2 The steering contract

A steering message is an **amendment** to a task's objective. It is sent
with an explicit kind, recorded as its own row, and never rewrites
`tasks.dispatch_message`.

| Kind | May | Must not | Effect on criteria |
|---|---|---|---|
| `refine` | add detail, correct a fact, answer a question | change what "done" means | none — the list is untouched |
| `constrain` | add a restriction on *how* (use V1's provider, don't add deps) | remove an existing criterion | may **add** a criterion |
| `descope` | remove work, explicitly | remove anything silently | **must name** the criteria it drops; they are marked `dropped`, with the amendment id, never deleted |
| `abort` | stop the task | leave the task looking healthy | all outstanding criteria marked `abandoned`; task → `cancelled` |

The AOS-12 clarification is a `refine` — arguably a `constrain`. Neither
can drop a criterion. **The observed failure becomes unrepresentable**,
which is the property to aim for (ADR-0001 §1.4 makes the same argument
about forged message sources: better to make a class of error
unrepresentable than to check for it).

Scope may only ever *narrow* through `descope`, and narrowing is a
recorded, attributable act. Broadening is not a steering operation at all
— it is a new dispatch, so that authority is never widened by a message
(a requirement CAD-157 states explicitly).

Delivery composes the body: **amendment first, then the objective, then
the outstanding criteria.** Phase 1 ships this composition even before
the kinds exist.

### 5.3 Ack is not completion

§1.1 shows managed Claude structurally cannot acknowledge — its single
result text is both. Two changes:

1. **Generalise the ack channel.** `message_report kind:"ack"` already
   does the right thing (`mark_ack` keeps the message `running`). Its
   staleness check is hardcoded to pty tokens —
   `token.starts_with("pty-{gen}-")` (`daemon.rs:1757-1767`) — so a
   managed turn id (`claude-<gen>-<uuid>`) can never satisfy it. Generalise
   the prefix to the endpoint's own scheme and ack works everywhere.
2. **A turn on a `steer` message never completes the objective.** It
   completes the amendment. This is close to today's behaviour by
   accident (only `job_dispatch` drives task state) and should become
   deliberate and documented, because the accident is load-bearing.

The reviewer-visible consequence: the verdict view shows the kickoff, the
ordered amendments with their kinds and authors, and the per-criterion
disposition. A reviewer can see that a `refine` arrived and that two
criteria are still open — the exact picture missing on 2026-09-20.

### 5.4 Partial completion, caught before review

When a `job_dispatch` message completes, `task_on_completed` currently
moves the task to `review` and binds `head_sha` unconditionally
(`store.rs:3154-3161`). It gains one check:

- Every criterion must carry a disposition claimed by the worker:
  `met` / `not-met` / `blocked` / `dropped` (by amendment id).
- If any criterion has none, the task does **not** enter `review`. It
  enters `incomplete`, and a `job_event` routes to the PM naming the
  unaddressed criteria.

Cadence is not judging whether a criterion is truly met — Option F is
rejected. It is checking that the worker **said something** about each
one. That is the whole mechanism, and it is sufficient for AOS-12: the
worker would have had to explicitly claim "implementation: not-met",
which is a visible, auditable, reviewable act, instead of silence.

> The failure was silence. The fix is to make silence unrepresentable.

`incomplete` is deliberately not a failure state — a worker that ran out
of turn with three of five criteria met is in a normal, expected
condition, and saying so is the honest report.

### 5.5 Idle with outstanding work

The drift check at `daemon.rs:2107-2145` fires only when the kickoff is
terminal *and not* `completed` — §1.1's blind spot. Extend it: a task in
`dispatched`/`running`/`incomplete` whose kickoff **completed** and whose
criteria are unaddressed is a needs-attention row, surfaced in
`overview()` beside `fenced` and `stalled`.

The continuation must be bounded, since CAD-157 names runaway retries as
a failure mode:

- At most one automatic continuation per task per revision; after that it
  is the PM's call.
- The continuation message is a **restatement**, mechanically composed
  from the objective and the outstanding criteria — never new
  instructions, so authority cannot broaden.
- Never for a task that is `cancelled`, `blocked`, or whose last
  amendment was `abort`; a user stop always wins.
- Sent at a safe boundary — when the agent is idle, through the normal
  queue, never via the forced mid-turn paste of §1.3.

## 6. Consequences

**Good.** The objective stops being "the most recent message". Narrowing
becomes attributable. The reviewer sees amendments and outstanding
criteria instead of inferring them. `## Acceptance` finally earns its
place in 144 issues. Most of this reuses jobs/tasks/verdicts rather than
adding a parallel system, as the ticket required.

**Bad, accepted.** Dispatch gets stricter: no criteria, no dispatch. That
is friction on every kickoff, and it will be unpopular the first week. It
is also the only thing that makes the rest checkable, and the tracker has
144 pieces of evidence that the optional version does not get used.
Per-criterion disposition makes results longer and more structured, which
is a real cost for pty workers already near the 4 000-char ceiling.

**Ugly.** Phase 3 lets cadence decline to advance a task on the strength
of a worker's own bookkeeping. If workers learn to tick every box, the
gate measures compliance rather than completion — see §9.

## 7. Acceptance checks

Commands `qa-1` runs. An empty result must never read as a pass — assert
inputs are non-empty before asserting properties (CAD-138).

**Phase 1**

```bash
# criteria exist and are parsed as items, not just counted
cadence issue show CAD-XX --json | jq -e '.acceptance | length > 0'

# you may not dispatch work whose done-ness is undefined
cadence dispatch CAD-EMPTY --to dev-1 ; test $? -ne 0   # names the empty section

# the objective rides along with every task-bound message
cadence send dev-1 --task <task> --text "use the V1 email provider"
# delivered body contains the original objective AND the outstanding criteria
cadence message show <id> | grep -q 'Outstanding criteria'

# criteria survive truncation; narrative does not
cadence job task add <job> --accept "$(printf 'a%.0s' {1..5000})"
cadence job task show <task> | grep -qv 'full criteria in the spec file'
```

**Phase 2**

```bash
# a refine cannot drop a criterion
cadence steer <task> --refine --text "use V1 email" ; test $? -eq 0
cadence job task show <task> --json | jq -e '[.acceptance[].state] | index("dropped") | not'

# a descope must name what it drops
cadence steer <task> --descope --text "skip tests" ; test $? -ne 0      # refuses: no --drop
cadence steer <task> --descope --drop c3 --text "skip tests" ; test $? -eq 0
cadence job task show <task> --json | jq -e '.acceptance[] | select(.id=="c3") | .state=="dropped" and .by!=null'

# the kickoff is never rewritten by an amendment
before=$(cadence job task show <task> --json | jq -r .dispatch_message)
cadence steer <task> --refine --text "…" 
test "$before" = "$(cadence job task show <task> --json | jq -r .dispatch_message)"

# D3: ack works on a MANAGED endpoint (the generalised token check)
cadence message ack <id> --token "$(cadence self | jq -r '.running[0].turn_id')" --text "understood"
cadence message show <id> --json | jq -e '.state == "running"'   # acked, not completed
```

**Phase 3**

```bash
# D4: docs-only result cannot satisfy an implementation+tests task
#     -> task goes to `incomplete`, NOT `review`, naming what is unaddressed
cadence job task show <task> --json | jq -e '.state == "incomplete"'
cadence job task show <task> --json | jq -e '[.acceptance[] | select(.disposition==null)] | length > 0'

# a genuine blocker is expressible and is NOT treated as incomplete
cadence message result <id> --token <t> --text '...' --criterion c2=blocked:"D1 quota"
cadence job task show <task> --json | jq -e '.state == "review"'

# D5: bounded continuation, and stop wins
cadence job task show <task> --json | jq -e '.continuations <= 1'
cadence job task cancel <task> && cadence overview | grep -qv "continue <task>"
```

## 8. Open questions

1. **Operator decision:** may cadence refuse to advance a task to
   `review` on the strength of the worker's own disposition claims (§5.4)?
   This is the one place the design lets the controller override a
   worker's "done", and it should be an explicit decision, not a
   side-effect of an ADR.
2. **PM:** does `steer` become a new verb, or a flag on `send`
   (`send --task <t> --amend refine`)? A verb is clearer and greppable; a
   flag reuses the existing path and its idempotency. I lean verb.
3. Should phase 1's dispatch refusal apply to **all** issues or only
   job-backed dispatches? Refusing everything is simpler and stricter;
   refusing only job dispatches limits blast radius while the 144 empty
   sections get filled.
4. Sent to `rsch-1` on 2026-09-20 (reply pending at time of writing):
   prior art for amendment-vs-replacement of a running unit of work
   (Temporal signals vs updates, LangGraph `interrupt()`/`Command(resume)`),
   for automatic comparison of a reported result against a declared
   acceptance list, and for ack-vs-complete primitives in async queues.
   May refine §5.2's kind vocabulary and §5.4's mechanism; does not block
   phase 1.

## 9. How we would know this was wrong

- Workers learn to tick every criterion regardless → the gate measures
  compliance, not completion, and the disposition should require evidence
  (a SHA, a test name) rather than a word.
- `descope` becomes the common path → the criteria are being written too
  ambitiously at dispatch, and the problem is upstream in how work is
  specified.
- Dispatch refusals from empty acceptance sections get worked around with
  a boilerplate criterion (`- [ ] it works`) → the rule created ceremony
  instead of clarity, and should move to the reviewer.
- Nobody ever sends a typed amendment, only `refine` → the taxonomy is
  over-built and `refine` plus `abort` would have done.
- `incomplete` tasks pile up unattended → the state moved the problem
  rather than solving it, and it should raise an escalation rather than a
  row in a list.
