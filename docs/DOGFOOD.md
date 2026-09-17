# Dogfooding Cadence

Cadence was built by agents it manages. Every milestone below was
dispatched through Cadence itself — a PM pane (Devin TUI on a pty
endpoint) receiving `message send` specs, implementing, and reporting
through `message result`. The tool being built was the tool being used,
which surfaced a set of friction points that no amount of unit testing
would have. This file records what broke, what it cost, and which fix
landed.

## Pane-kill interruptions are expensive

**What happened.** During the knowledge task (`m-knowledge`), the PM's
pane was killed mid-implementation. The submitted message had been
pasted and accepted by the TUI, so the daemon could not prove the turn
never ran — the message fenced `unknown`, the agent fenced `attention`,
and the work sitting in the pane vanished with it. The spec had to be
re-dispatched as `m-knowledge-2` and the work restarted from the
committed state.

**What it taught.** A pty endpoint is *display* state, not *durable*
state — anything not yet reported is lost with the pane. The fixes that
landed: ambiguous post-submission outcomes fence `unknown` (never
silently replayed, never marked done), `agent resume` reopens the saved
native thread, and fenced agents carry a `next.resume` hint in their
launch summary so the operator's recovery path is one command, not a
search through docs.

**Rule that fell out:** report early, report through the protocol —
unreported pane work is a single `kill-session` away from gone.

## The ready gate is right; the UX was wrong

**What happened.** pty delivery requires a human to assert the terminal
is idle with an empty input — `agent ready <alias>` — before a paste is
accepted. In practice the dispatch loop was: look at the pane, type
`cadence agent ready w1`, type `cadence message send w1 --text ...`,
twice per send, every send. Operators forgot the claim, sends sat
`queued`, and the debugging loop ("why didn't it deliver?") kept ending
at the same gate.

**What landed.** `send --ready` / `ask --ready`: the flag *is* the
claim, fused into the send. It calls `agent_ready` first, silently no-ops
on endpoint kinds with no gate, and keeps ordering claim-then-send. The
gate itself was not relaxed — an explicit human assertion still precedes
every paste — only the ceremony collapsed into one keystroke.

## The bootstrap knowledge gap

**What happened.** Workers launched before briefings existed had no idea
they were cadence-managed: they didn't know their alias, where the
report verbs were, or that results routed to a PM. A retrofit
(`agent bootstrap <alias>`) and the universal briefing writer were added
after the fact: every launch now writes
`.cadence/<root>/BRIEFING-<alias>.md` (identity, protocol quickref,
roster snapshot) plus an idempotent `<!-- cadence:* -->` block in the
repo's `AGENTS.md`. `join` still enqueues the durable `bootstrap-<alias>`
kickoff; `--bootstrap` extends that to standalone launches;
`--no-bootstrap` opts out entirely.

**What it taught.** The protocol primer has to arrive *with* the agent,
not in the operator's head. An agent that can't discover `cadence self`
will never report; the whole coordination loop silently no-ops.

## The resume–attach race

**What happened.** Early `agent resume` returned `{"state":"starting"}`
immediately. An operator (or script) that followed it with `attach`
raced the endpoint opening: too early → "no live endpoint"; too late →
nothing. The same race existed for `devin -r` fenced-agent recovery.

**What landed.** `agent resume` now behaves like a launch: bounded ~30s
poll for the endpoint (clear timeout error naming `agent show`), then
attach by default under the shared rule — exec only on a TTY outside
tmux, print the command otherwise, `--detach` opts out. Group resume
(`cadence resume <group>`) reuses the same rule per member with a ~15s
bound and lands on the PM when done. Non-attachable kinds (managed
stdio, fake) return the receipt immediately — no endpoint will ever
exist to wait on.

## tmux copy was hostile

**What happened.** The Devin TUI captures the mouse: click-drag selects
inside the TUI, not the terminal, so operators couldn't copy output the
way they expected. Copying meant copy-mode gymnastics nobody
discovered unaided.

**What landed.** Owned panes get `mouse on`, `set-clipboard on` (OSC52
→ system clipboard on supporting terminals), `pane-border-status top`
with the session name, and a longer `status-left`. The README documents
the two escapes: **Shift+drag** for terminal-native select when the TUI
holds the mouse, and `Ctrl-b [` copy mode as the fallback.

## Orphan lifecycle needed explicit verbs

**What happened.** Stopped and fenced agents accumulated in the
registry with no way to distinguish live from dead in `agent list`, and
no deletion path — the only way out was SQL.

**What landed.** `agent list` rows carry `dead` (no live endpoint),
`group`, and `group_root`; `agent remove <alias>` deletes a dead agent
plus its history while refusing live endpoints; `agent gc
[--older-than]` sweeps dead rows on request — never on a timer. Group
lifecycle rounded it out: `cadence stop <group>` leaves members
registered and resumable, `cadence resume <group>` brings back the PM
first then its members, and a member whose pane bound a *different*
native session is reported `unrecoverable` with the explicit
remove-and-rejoin path (`agent remove` + `join -r`) — because resume on
a mismatched pane can never converge, and guessing would take over a
session that isn't ours.

## The ready claim needed a machine-checked form

**What happened.** `agent ready` is a human assertion — "I looked, the
pane is idle". A QA audit of a real dispatch found the failure mode:
an agent pastes `--ready` blind because inspecting first is a separate
`agent capture` call nobody is forced to make. Worse, a busy Devin pane
*accepts* pasted input into a staged queue, so a "sent" message can sit
unexecuted while the sender believes it is `running` — bytes were
delivered, the work never started.

**What landed.** `auto_ready=verified` (params, or `--auto-ready` on
launch, or `agent set <alias> auto_ready=verified` live) makes the
daemon earn the claim itself: before every paste it probes the pane —
prompt glyph present, input empty, no busy markers, no approval menu —
and only then mints a single-use claim. Approval menus get priority
over prompt shape because their `❭` option marker mimics the idle
prompt. `agent probe <alias>` exposes the same analyzer read-only.
After `Enter`, the paste must visibly render before `submitted` is
reported: a missed render requeues routed notifications with a bound,
but a task message goes `unknown` and fences the actor — a possibly-
executed task is never replayed. Every claim consumption writes a
`claim_used` audit event naming the claimer. The gate is still a gate;
it just no longer trusts a human to have looked.

## Some consumers are not agents

**What happened.** The QA-audit loop needed somewhere for verdicts to
land that was not another LLM session — a place a script could read.
The only delivery targets were live actors; there was no durable,
readable queue, so reviewers were simulated with workers or notes on
disk, and "did the verdict arrive" meant polling `agent show`.

**What landed.** `provider=inbox` / `endpoint_kind=inbox`: a
registered agent that is a pure mailbox — `inbox://<alias>` endpoint,
state `idle`, no actor, no briefing, no pane. Messages sent to it (or
routed via `reply_to`, including as a group root collecting worker
results) stay `queued` durably. `cadence inbox <alias>` drains them
over the daemon socket — `--wait`/`--follow` block on the daemon's
change signal rather than polling — and each consumed message
completes `via=inbox_read`. A drained message with `reply_to` routes
its result in the same transaction, so a consumer's answer can still
wake the waiting actor. Lifecycle verbs refuse the things a mailbox
cannot do (`resume`, `stop` are rejected; `remove` deletes the
mailbox outright).

## The general lesson

Every one of these was discovered by the system failing *in use*, not
in review. The pattern that kept repeating: the durable model was
already right (fencing, claims, transactional routing) — what was
missing was the operator's next command spelled out at the moment of
failure. The audit rule that came out of this pass: every error names
the next command.
