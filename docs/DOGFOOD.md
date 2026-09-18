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
prompt, and busy/menu markers are matched only in the bottom status
region — the transcript above can legitimately print the same strings
(including this repository's own source quoting them) without the pane
being busy. The region is anchored at the last *non-blank* row:
`capture-pane` pads the capture to pane height, and on a fresh session
with a tall pane the literal bottom rows are all blank — the first cut
of region-scoping anchored at the last row and read a thinking pane as
idle (caught in review, reproduced live: 89 captured rows, 67 trailing
blanks). The `Guide Devin while it works` input watermark is itself a
busy signal independent of the region maths. `agent probe <alias>`
exposes the same analyzer read-only.
Every claim consumption writes a `claim_used` audit event naming the
claimer. The gate is still a gate; it just no longer trusts a human to
have looked.

**What the first review round added.** The post-paste check is
*differential*: the screen is captured before the paste and the
body's normalized tail slice must occur *more often* afterwards —
every routed `worker_result` opens with the same sentence, so a plain
`contains` would pass a swallowed re-delivery on the strength of the
earlier one still on screen. And rendered ≠ submitted: the input line
must be empty again after `Enter`, because a paste can stage into the
draft while the keystroke is swallowed; a held draft is `NotRendered`,
left untouched for a human. On a render miss a routed notification is
requeued (bounded) then *parked* — `failed` with `via=pty_render_miss`
and a `delivery_parked` event — while a task message still goes
`unknown` and fences the actor: a possibly-executed task is never
replayed, but a notification must never kill the recipient's pane.
`agent set` narrowed to an allowlist (`auto_ready` only) after review
found the merge-into-params shape could silently rewrite `upstream`
result routing and `session` bindings on a live agent.

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

## A fence had no exit that kept history

**What happened.** A provider outcome that cannot be proven lands a
message in `unknown` and fences the agent — correct, and never replayed.
But there was no way out that preserved the record: the only recovery
was `agent remove`, which deletes the agent's message and event history.
Worse, a daemon restart *retried* fenced agents — `recover` marked them
`offline`, the relaunch loop re-armed them into `attention`, and every
recovery hint said only "resume", which the fence itself rejects.

**What landed.** `message reconcile <id> --status interrupted|completed|
failed [--note]` — an operator statement, no turn token (the token is
stale by definition), refused for any other state with the state named.
One transaction records `{status, via:"operator_reconcile", note}`,
emits `reconciled` with the caller, routes `reply_to` for
completed/failed (deterministic delivery id — exactly once); the last
unknown on an agent lifts the fence to
`stopped`, never auto-started. `agent unfence <alias>` is the bulk form
and resumes unless `--no-resume`. Daemon start now skips fenced agents
before any spawn (`relaunch_skipped` event) and keeps launching healthy
ones; `resume --all` lists them under `fenced` with the hint instead of
attempting them. Every fenced surface — launch `next`, resume
rejection, `devin -r`, `agent show` error — names unfence first, and
session-mismatch hints now say each retried resume mints a new provider
session.

**What the second review round fixed.** Three gaps. First, `recover`
still rewrote `attention` rows to `offline` before the serve loop read
them — the startup skip was dead code for every non-unknown fence; now
recovery preserves the fence and its error verbatim, clearing only the
dead runtime fields. Second, an `unknown` finish routed nothing at all,
so a fenced worker's PM was never told — now the fence routes one
`worker_notice` ("outcome unknown, worker fenced, reconcile pending")
under a `cadence-notice:` id disjoint from the `cadence-result:` slot,
and `reconcile --status interrupted` routes a closure notice the same
way; notices are plainly not results and carry no `reply_to`. Third, a
reconciled agent stayed `enabled`, so the next restart relaunched it —
the fence lift now lands `stopped` with `enabled=0`, identical to an
operator stop.

## Managed claude: a wire that isn't JSON-RPC

**What happened.** The third provider landed: `claude` on the `managed`
endpoint kind — one long-lived headless `claude -p --input-format
stream-json` process per agent, driven by newline-delimited typed
events instead of JSON-RPC. The phase-B0 observation pass mattered
more than the code: `system/init` does not arrive at spawn but with
the first turn, so session-id verification lives inside `run_turn`
rather than `open`; a `result` with `permission_denials` is still
`success`/`is_error:false` — denials are a policy fact, not a failure;
and `--verbose` is load-bearing (without it stream-json output refuses
to start). A real interactive session also produced a surprise: the
`!` shell escape executes immediately without a permission check, so
"watch the approval menu" is not a reliable denial probe.

**What it taught.** The fail-closed rule transferred cleanly: death
before a `result` is `unknown` + fence, `agent unfence` + `resume`
relaunches with `--resume <session>`, and a `system/init` session-id
mismatch means another process owns the session — `attention`, not a
retry. The briefing gained a provider-conditional line because managed
claude has no token flow: the turn's `result` text IS the report.

**Rule that fell out:** observe the real provider first — half the
design (lazy init, denial semantics, EOF-as-shutdown) came from
fixtures, not docs.

**Review round.** Independent review caught what the happy path hid:
a fixed 600s turn deadline would have fenced every healthy hour-long
PM turn — liveness is now activity-based (`turn_idle_secs` bounds
silence, `turn_max_secs` caps absolutely) and the fence records the
provider's own reason, so reconcile knows *why*. Environment scrubbing
moved from a name list to a prefix rule — a list keeps missing new
leak vars (`CLAUDE_CODE_SUBAGENT_MODEL`, `CLAUDE_EFFORT`, `CLAUDE_PID`)
— with an explicit keep-list for operator-set config. `send` on a
provably-dead transport is a provider error, not `unknown` — nothing
was written, so nothing is uncertain. And `assistant` events now emit
compact `tool_use` lifecycle events (name only) so `events --follow`
shows a long turn working.

## Splitting the pty adapter surfaced a literal-paste hazard

**What happened.** CAD-32 pulled `DevinPtyAdapter` apart into a
generic owned-tmux-pane adapter plus a per-TUI profile — launch argv,
session-ownership proof, screen signatures, wording, deadlines — so a
second provider TUI is a new profile module, not a copy of a
thousand-line file. The one deliberate behaviour change came straight
from the claude terminal investigation: a leading `!` there runs a
shell command immediately, outside permission checks — and cadence
pastes message bodies verbatim. Typed into a scratch Devin pane (never
submitted, so it cost no turns), the observed specials were `/` →
command menu, `!` → bash mode, `@` → file picker; `#` stayed a literal
draft char.

**What it taught.** A verbatim paste of a command-looking body is an
injection path, so the guard lives *before* the gate: a leading
forbidden prefix is `PreWrite` — provably no bytes reached the pane,
no claim consumed, no fence — and the list is the profile's, because
what is dangerous is a TUI fact, not a transport fact. A `tui-stub`
test-double profile (different glyph, markers, argv, prefix list) now
drives the whole fake-tmux harness, which is what actually proves the
adapter reads every one of those facts from the profile.

**Rule that fell out:** when you paste into someone else's UI, the
first character is part of the protocol — observe it, then forbid it.

## A managed Devin pane stalls on its own approval menu

**What happened.** A Devin worker launched by cadence sat at its first
tool-approval menu for four hours — the TUI was waiting on a keypress
nobody was there to make. The pane is owned and watched by the daemon,
but approval prompts are answered in the terminal, so an unattended
launch deadlocks the very autonomy it was launched for. Devin's own
`--permission-mode` flag fixes it, but cadence launched the TUI with
no way to pass one.

**What landed.** `cadence devin --permission-mode <mode>` and
`--bypass` (the `dangerous` shorthand, conflicting with an explicit
mode exactly like the claude verb's), plus the same flags on
`cadence join <pm> devin`. The choice stores as
`params.permission_mode` — `auto`, `accept-edits`, `smart` or
`dangerous`, validated at `agent_register` with the four values named
in the rejection — and the pane profile replays it as
`--permission-mode <mode>` in the launch argv on every open, fresh and
`-r` resume alike. It is launch-time only: `agent set` refuses to patch
it, because a live edit would diverge the stored mode from the running
pane's actual policy. Verified live (CAD-18): a scratch
`cadence devin --bypass --detach` pane ran `ls` with no approval menu;
Devin's separate directory-trust prompt still appears — it is a
workspace check, not a tool approval.

**Rule that fell out:** automation that owns the launch must also own
the approval policy — a permission mode left at "ask a human" turns
the manager into the bottleneck.

## The general lesson

Every one of these was discovered by the system failing *in use*, not
in review. The pattern that kept repeating: the durable model was
already right (fencing, claims, transactional routing) — what was
missing was the operator's next command spelled out at the moment of
failure. The audit rule that came out of this pass: every error names
the next command.
