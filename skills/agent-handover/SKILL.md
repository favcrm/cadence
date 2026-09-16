---
name: agent-handover
description: >
  Write a self-contained markdown brief to /var/www/agent-notes/ for another
  agent on this machine — a kickoff "start work" prompt or a QA closure note
  for review, chained in a kickoff → QA → kickoff loop under one session id.
  Use when dispatching a bounded unit of work to a fresh agent, handing
  finished work to a QA/reviewer agent, or passing context between agents on
  the same instance (Devin, Codex, Orca worktree). Triggers on agent
  handover, handover note, kickoff prompt, start-work brief, closure for QA,
  QA handoff note, handover loop, write a prompt for another agent.
---

# agent-handover

Write a one-file markdown brief that another agent can act on with **zero**
access to this conversation. Notes are machine-local files, not repo
artifacts — persistent; only closed chains idle for more than 30 days are auto-pruned.

| | |
|---|---|
| Directory | `/var/www/agent-notes/` — **not web-published**; inspect via SSH |
| Filename | `YYYYMMDD-HHMMSS-<session>-<slug>-<type>.md` (UTC) |
| Session | 5-char hex — mint once per loop: `openssl rand -hex 3 \| head -c 5` |
| Slug | lowercase `[a-z0-9-]{1,48}`, no leading/trailing `-` |
| Type | `kickoff` · `qa` · `verdict` (terminal record — closes a loop) |
| Publish | `scripts/note-publish.sh <sess> <slug> <type> <file|->` — **never write directly** (lock + index) |
| Index | `index.html` — regenerate after every write: `scripts/notes-index.sh` |
| Prune | closed chains idle >30d — daily cron runs `scripts/notes-prune.sh` |
| Mailbox | `/tmp/agent-mail/<mailbox>/inbox/` — per agent; `claimed/` while processing, `read/` after explicit ack |
| Mailbox id | `^[a-z0-9][a-z0-9-]{0,31}$` |
| Lifetime | notes: closed-chain retention · mail: ephemeral /tmp — **same host only** |

Example: `/var/www/agent-notes/20260915-151429-f3a2c-recall-paint-kickoff.md`
One loop's chain: `ls /var/www/agent-notes/*-f3a2c-*`
Open the index: `file:///var/www/agent-notes/index.html` (or port-forward)

## Pick the doc type

- **kickoff** — the receiver has never seen this work. It needs the goal,
  scope, and everything required to start.
- **qa** (qa-closure) — the work claims to be done. The receiver needs to
  verify it, not redo it.
- **verdict** — the reviewer's terminal record (pass / blocked / exhausted).
  This is what makes a loop observably *closed*; chains whose latest note is not a verdict remain open.

## The loop

Notes chain into a harness loop under one session id:

1. **kickoff** — written by whoever scopes the work; mints the session id.
2. Worker implements → on completion **auto-writes a `qa` note** under the
   same session id and returns it to the sender's `From:` mailbox.
3. Reviewer verifies → **pass**: writes a `verdict` note, loop closed.
   **Gaps**: writes a **new `kickoff`** under the same session id (quoting
   the failing items) → back to step 2.

The loop terminates only when its latest note is a stored `verdict` — never on a worker's
self-report. A `qa` note answers a `kickoff`; a follow-up `kickoff`
answers a `qa`; a `verdict` answers the last `qa`.

## Messaging

Notes reach their receiver through per-agent mailboxes. `From:` on the
incoming note is the return address — replies keep the loop `Session:`,
write their own `From:`, and post back to that mailbox.

```bash
# deliver: atomic, unique, FIFO. Prints msgid=<id> path=<dest>
scripts/mail-post.sh <to> <type> [--from <me>] [--reply-to <msgid>] <file|->

# claim-and-print, oldest first; prints claim-token=<tok> per message
scripts/mail-poll.sh <me> [--timeout 600] [--reply-to <id>] [--match S] [--lease 300] [--peek]

# ack after processing — token must match the claim generation
scripts/mail-ack.sh <me> <claimed-file> <tok>     # or: --token <tok>
# extend a lease on long work
scripts/mail-renew.sh <me> <claimed-file> <tok>
```

- **Filename**: `<ts%N>-<from>-<type>-<msgid>[-re-<msgid>].md` — name sort
  is FIFO; `-re-<msgid>` (a strict suffix field) correlates a reply.
- **Delivery is at-least-once.** A claim is one serialized transition under
  `<box>/.lock`: `mv` to `claimed/<name>-claim-<tok>.md` + fresh mtime =
  lease start. Exactly one poller wins; claimed files await `mail-ack.sh`;
  claims idle past `--lease` (default 300s) requeue on the next claiming
  poll. Duplicates are possible — consumers dedupe by msgid.
- **Acks are generation-checked.** A stale token (from an expired claim)
  cannot ack a newer claim — `mail-ack` rejects it and preserves the
  current owner. `--token <tok>` acks every claim made under that token.
- **Consultation**: post `consult`, keep its `msgid`, then
  `mail-poll <me> --reply-to <msgid>` — only a message carrying the
  `-re-<msgid>.md` suffix satisfies it. `--match` is generic inspection,
  not correlation.
- **`--peek` is read-only** — no claims, no requeue, no state change.
- **Delegation**: post the `kickoff` to the worker's inbox. The worker must
  be a live session in listen mode (poll → act → ack → poll again), or
  something must step it — see below.
- **Identifiers are validated, never sanitized** — `worker_a` ≠ `workera`;
  invalid ids are rejected, not mangled.

### Waking the receiver

A note or mailbox entry is an artifact, not a wake endpoint. Register the
receiver's actual provider, native session, endpoint and return route.
Never substitute a fresh managed session for an existing terminal silently.
The loop's five-hex `Session:` is not a provider session ID.

Prefer a verified provider-native route: Codex queue/app-server or Claude's
inbox when available. A managed ACP connection reaches its owned session;
`session/load` does not prove attachment to an independently open native TUI.
For a registered managed terminal, verify pane/process/session ownership and
use the controller's literal-message sender. Do not paste into an arbitrary
pane, overwrite an operator draft, or treat a permission prompt as agent input.
If no supported route exists, return the note path for a human handoff.

Keep these evidence levels separate:
- Published/queued: the artifact or transport accepted the message.
- Submitted: a native endpoint or terminal received input bytes.
- Acknowledged: the addressed agent explicitly answered with correlation.
- Verified: a reviewer checked the exact revision and acceptance criteria.

A terminal echo is not an agent reply. Ambiguous submission must not trigger
blind retries. Peer messages do not grant new user authorization.
A waiting process/socket can wake an idle session without an LLM polling loop.
Use bounded mailbox polling only for a deliberately cooperative workflow;
claim, process, acknowledge and preserve a cursor/idempotency key.

Cadence is under development. Discover installed commands/capabilities before
using it; do not turn proposed commands into executable handoff instructions.
Every kickoff states a working return route. File mail alone is insufficient
when the PM requires an automatic new turn in its original conversation.

## Write it well

- **Resolve facts first.** Inspect the repo/worktree before writing. If the
  work turns out to be already done, report the evidence and create nothing.
- **Behavioral, not procedural.** Describe what "done" looks like and the
  contracts involved; let the receiver choose its own implementation.
- **Durable references.** Key changes to stable paths, symbols, interfaces,
  and commands — never line numbers, which go stale.
- **Decision-complete.** No placeholders and no open choices that require
  the old chat. Resolve assumptions from repo evidence or record them
  explicitly.

## `kickoff` skeleton

```markdown
# Kickoff: <title>
> Handover from <agent/session> — <date>. Read fully before starting.
> Session: `<loop-hex>` — keep this id on every note in this loop.
> From: `<originator-mailbox>` — reply notes go back to this mailbox.

## Goal
<one paragraph: what "done" looks like>

## Context
- Repo / worktree: <abs path>
- Key context: <stable paths / symbols / interfaces, one-line why each>
- Specs / plans / tickets: <paths or URLs — do not inline them>
- Native receiver / endpoint: <registered provider session and delivery route>
- Return route: <working command or endpoint; do not include secrets>
- Revision / ownership: <base revision and worker-owned worktree or files>

## Scope
- In: <bullets>
- Out: <explicit non-goals>

## Conventions & commands
- Install: <cmd>  Build: <cmd>  Test: <cmd>  Typecheck: <cmd>
- <rules that bite: e.g. pnpm not npm, run from sub-app dir>

## Acceptance criteria
- [ ] <independently verifiable condition>

## Working rules
- Be pragmatic — implement only the logic this task needs; no speculative
  abstractions, no gold-plating.
- Respect the user's existing scope and authorization. Use a UI preview
  when it helps review the requested change; this template does not add
  an approval gate the user did not request.

## Suggested skills
- <skills the receiver should invoke>

## Report back
End your final message with exactly these fields:
`status` (completed | blocked) · `summary` · `changed files` ·
`verification` (each command + outcome) · `residual risks` · `blockers`

## On completion
When `status: completed`, write a `qa` note under session `<loop-hex>`
with `From: <your-mailbox>` (per the agent-handover skill) and return it
to `<originator-mailbox>`.
```

## `qa-closure` skeleton

```markdown
# QA closure: <title>
> Ready for review — <agent/session>, <date>.
> Session: `<loop-hex>` — keep this id on every note in this loop.
> From: `<worker-mailbox>` — follow-up kickoffs go back to this mailbox.
> Answers: <path of the kickoff this closes>

## What changed
- Branch / commits / PR: <refs>
- Diff range: <base..head>
- <2–5 bullets of substance, not a file list>

## Verify
1. <setup: env vars, seed data, test accounts>
2. <steps to exercise the change>
3. <expected result>

## Evidence
- <test output, screenshots, preview URLs, logs — by path/URL>

## Quality gate
- [ ] `code-simplifier` review pass on the diff — done | pending (Known gaps)

## Known gaps / risks
- <what was NOT tested, deferred edge cases, follow-ups>

## Checklist for the reviewer
- [ ] <acceptance items to confirm>

## If gaps found
Do not fix them yourself — write a new `kickoff` under session `<loop-hex>`
with `From: <your-mailbox>`, quoting the failing checklist items, and
return it to `<worker-mailbox>`.
```

## `verdict` skeleton

```markdown
# Verdict: <title> — <pass | blocked | exhausted>
> Review closed — <agent/session>, <date>.
> Session: `<loop-hex>`
> From: `<reviewer-mailbox>`
> Answers: <path of the qa note this verdicts>

## Verdict
<pass|blocked|exhausted> — <one line>

## Findings
- <resolved findings, or outstanding blockers>

## Revision reviewed
- <commit range / source hashes / artifact set>
```

## Rules for agents

1. **Self-contained.** The receiver has no conversation history. Point to
   plans, diffs, PRs, and specs by path/URL — never paste their contents in.
2. **No secrets** — no keys, tokens, or credentials in the note.
3. **Same-host only, not published.** `/var/www/agent-notes` is local
   inspect-only (no nginx exposure). It does not reach other machines or
   cloud sessions — for those, put the brief in a ticket comment or paste it
   into the receiving prompt instead.
4. **Quality gate on `qa-closure`.** Before writing the note, run a
   `code-simplifier` review pass over the changed files and fold in what it
   finds — "no verified findings" is a valid result, record it as such. If
   it was skipped, mark the gate **pending** in Known gaps so the QA agent
   runs it first — no final report until the gate passes.
5. **Print the absolute path** after writing — the note is meant to be
   pasted into the receiver's prompt (`read /var/www/agent-notes/....md`)
   or posted to their mailbox via `scripts/mail-post.sh`.
6. **One session id per loop, one mailbox per agent.** `Session:` names the
   loop chain (minted on the first `kickoff`, reused forever after);
   `From:` names the sender's mailbox (each agent's own 5-hex or name).
   Every reply keeps the loop `Session:` but writes its own `From:` — that
   header is the return address the next note in the chain answers to.
   Never mint a new session id for a follow-up — a fresh id = a fresh loop.
7. **New file every write.** Fresh timestamp + slug each note — never
   overwrite a note a receiver may already be reading.
8. **Preserve the assigned review role.** In an independent worker/reviewer
   loop, return findings as a new `kickoff`. If the user explicitly asks the
   reviewer to fix them, follow that scope and arrange verification of the fix.
9. **Publish notes via `scripts/note-publish.sh`** — it takes the notes
   lock, enforces the filename contract, and regenerates `index.html`.
   Direct file writes bypass the lock and race the pruner; don't do them.
   A loop ends with a `verdict` note. A later kickoff or QA reopens it;
   inactivity alone is not terminal.

## Cleanup

```bash
ls /var/www/agent-notes                   # all notes, newest last by name
ls /var/www/agent-notes/*-<hex>-*         # one loop's full chain
cat /var/www/agent-notes/index.html       # session summary table
ls /tmp/agent-mail/<box>/{inbox,claimed,read}   # pending / in-flight / done
rm /var/www/agent-notes/<file>.md         # drop one note
rm -rf /tmp/agent-mail/<box>              # drop a whole mailbox
```

Chains ending with a `verdict` note and >30d idle are pruned by daily cron
(`scripts/notes-prune.sh`, log at `.prune.log`). Open or ambiguously ordered chains
are never auto-pruned — remove them manually. Mail dies with the instance.

## Use this vs.

- `handoff` — compact *this* conversation so the same task can continue.
- `orca-cli` — actually dispatch work to an Orca worktree/terminal
  (transport; carry the note written here).
- `engineering-ticket-evidence` — QA evidence destined for Linear/Jira
  comments rather than a local file.
- `create-implementation-plan` — durable plan that lives in the repo.
