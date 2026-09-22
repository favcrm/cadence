# Running a cadence session

This is the working guide for the person or agent in the PM seat: how to
start a session, run several workers side by side, review what they
produce, and keep the controller healthy. It is written from a real day
of use (seventeen pull requests through four workers) and records what
worked and what bit. Reference detail lives in
[PROTOCOL.md](PROTOCOL.md), [JOBS.md](JOBS.md), [BOARD.md](BOARD.md) and
[DOGFOOD.md](DOGFOOD.md); this file is the path through them.

## 1. The roles

The full operating model (roles, responsibilities, routing, artifacts,
guardrails, risk classes) is `docs/TEAM.md`; each role's standing
instructions are in `docs/roles/`. In short: the **operator** sets goals
and approves class `human` changes; the **PM** plans and dispatches;
**developers** implement; **qa-1** reviews; **ops-1** runs the merge queue
and post-merge operations; **rsch-1** and **arch-1** research and design
ahead of the work.

A PM that is an outside session (a Claude Code or Codex session you are
already talking to) needs no pane. Register an inbox alias for it and
every worker result routes there:

```bash
cadence daemon start
cadence agent register pm --provider inbox     # the PM seat
cadence inbox pm --follow                      # blocks; one JSON object per routed result
```

## 2. Start of session

```bash
export CADENCE_SUITE_LOCK="$HOME/.local/state/cadence/suite.lock"   # one path per host, in every shell and agent env
cadence session start         # the gate below as one verb — exit 0 go / 1 warnings / 2 no-go
cadence session start --fix   # same, plus the reversible fixes only:
                              #  daemon start, ui start, ui tailscale start
```

`session start` runs, in order: `host` (`doctor --host`), `binary`
(the running build's commit vs the repo's default ref), `daemon`
(reachable, and its build matches the binary's), `board` (the detached
UI, plus the persisted tailscale mapping when there is one),
`reconcile` (unknown messages, fenced agents, `doing` issues with no
live owner, open PRs on `cadence/` branches with no local worktree,
`.cadence/wt/*` dirs with neither an open PR nor an open issue) and
`inbox` (unread mailbox queues). Every line prints `ok`/`warn`/`fail`
with a one-line remedy; `--fix` only ever starts things, never
restarts a running daemon, never removes anything. `--project <key>`
scopes the tracker reads and repo scans to one project — an unknown
key is an error, same as `issue ls --project`.

Then bring the workers back:

```bash
cadence issue sync            # pull other hosts' tracker writes before you plan
cadence agent list --all      # who exists; `resumable: true` agents can come back
cadence resume <pm>           # bring the group's workers back on their saved sessions
cadence overview              # one screen: what needs a human, exact commands, deploy drift
```

What `session start` covers, for reference — the same checks the
checklist used to run by hand:

```bash
cadence daemon start          # or: cadence doctor, if anything looks off
cadence doctor --host         # host watchdog: disk free, provider WALs, pipe
                              #  pressure, memory (MemAvailable, swap free,
                              #  Committed_AS vs CommitLimit with overcommit
                              #  mode — strict-mode overshoot fails; heuristic
                              #  overshoot is informational), a process-group
                              #  census (counts, RSS, oldest idle), an
                              #  owned-session-tree census (per-tree PSS+swap,
                              #  owner, reclaim confidence — warns only when
                              #  the session-registry store is unreadable),
                              #  orphaned processes, leaked temp dirs,
                              #  legacy task cargo targets (`cad<digits>-*target*`
                              #  plus recorded `cargo_target` / `CARGO_TARGET_DIR`
                              #  — age, bytes, ownership, cwd/exe, cargo lock;
                              #  a name match is not ownership, no cwd reference
                              #  is not proof the dir is idle, and the remedy is
                              #  not a deletion command),
                              #  stale worktrees (shared cargo cache counted
                              #  once) — read-only, exit 0/1/2
cadence doctor --host --reclaim-plan
                              # same checks plus what could be freed — stale
                              #  worktrees, per-lane target dirs, the shared
                              #  cache — sizes + shell-quoted commands, never
                              #  deletes; exit is still the worst check level.
                              #  The shared-cache command empties the shared
                              #  subdirs but keeps the dirs themselves (every
                              #  lane symlinks into them); a retired shared
                              #  dir (e.g. examples) gets its own contents-
                              #  clearing row; a shared cache whose cargo
                              #  lock is held emits no command and counts
                              #  nothing — that lock flag is a scan-time
                              #  snapshot, recheck before running the plan;
                              #  live-lane target bytes are listed
                              #  separately under "freed with their lanes",
                              #  outside the reclaimable total
cadence ui start              # board at http://cadence.localhost:18000 behind the dev gateway
cadence ui tailscale start    # optional: phone/laptop access at https://<dns>:9450 — tailnet-only, loopback bind unchanged
cadence issue doctor          # tracker: hooks ours, lint clean, ahead/behind origin
```

`doctor --host` never kills or deletes; each warn/fail carries a
`remedy` — the exact command an operator would run. A non-zero exit is
a finding, not an error: fix or dismiss before dispatching workers
onto a host whose disk, pipes or orphans are already degrading lanes.

Provider WALs are the one thing the daemon maintains itself: every
minute it checkpoints any known provider store (devin `sessions.db`,
codex `*.sqlite`, claude projects) whose `-wal` exceeds
`[host] wal_max_bytes` (default 1 GiB) — PASSIVE then TRUNCATE. The
safety gates, honestly scoped: the WAL must be **quiet** (unwritten
for ~60s — this is what excludes writers cadence cannot see, like an
interactive `claude`/`devin` in a terminal), owned by the daemon's
uid, not a symlink, and its provider must have no in-flight *cadence*
turn (`submitting`/`running` in cadence's own store — a courtesy
gate, since SQLite's locking is what actually protects the data).
TRUNCATE cannot lose committed frames; a busy or failed attempt just
defers to the next tick. Successes record `wal_checkpointed` on the
`daemon` event stream (`cadence events daemon`) with before/after
bytes. Opt out with `[host] wal_checkpoint: false`; preview with
`wal_dry_run: true` (emits `wal_checkpoint_pending`, never writes) —
`doctor --host` also flags stores `over_checkpoint_limit`.
`pm.yaml [host]` also tunes `mem_warn_pct`/`mem_fail_pct` (15/5) and
`swap_warn_pct`/`swap_fail_pct` (20/5) — and note `Committed_AS` over
`CommitLimit` under heuristic overcommit is normal, not a failure.

Slice the backlog before you plan rather than scrolling it.
`cadence issue ls --open` is everything still live; narrow it with
`--tag`, `--owner`, `--component`, `--priority` or `--status` (repeat
`--tag` to require several, `--status` to allow several), and
`cadence issue epic ls` shows each epic's progress, blocked count and
owners, with `issue epic show <ID>` or `issue ls --epic <ID> --open`
for what is left inside one. Tag as you triage — `cadence issue tag
CAD-70 CAD-71 add flaky` and `cadence issue set CAD-70 CAD-71
status=ready` edit several issues in one commit — and send a filtered
board as a link: the filter bar keeps its state in the URL.

Join workers into the PM's group so results route back without any
`--reply-to`:

```bash
cadence join pm devin  --alias dev-a --bypass --auto-ready
cadence join pm claude --alias ci-a  --broker-approvals     # headless; approvals come to you
```

- `--bypass` (Devin) skips the trust prompt and the first approval menu.
  Without it a fresh pane stalls on a menu nobody is watching.
- `--auto-ready` opts a terminal worker into daemon-verified delivery:
  the daemon pastes a queued message only when its own screen probe
  reads the pane idle. Use it for every terminal worker. Set it later
  with `cadence agent set <alias> auto_ready=verified`.
- `--broker-approvals` (managed Claude) routes permission prompts to
  `cadence agent requests <alias>` / `cadence agent respond`. Use it
  instead of `--bypass` when you want a say over risky tool calls.

## 3. Lanes: what can run in parallel

Issues rarely depend on each other. **Files do.** Give each worker a
lane defined by the files it owns, run lanes in parallel, and keep work
inside a lane sequential, because the second issue would only rebase
onto the first.

Lanes that held up in this repository:

| Lane | Owns |
|---|---|
| Adapter and daemon | `src/adapter/`, actor loop and lifecycle RPCs in `src/daemon.rs` |
| Tracker | `src/issue/`, `tests/board.rs`, issue routes in `src/ui.rs`, the drawer |
| Messaging and store | message state in `src/store.rs`, the `message` verbs |
| CI and scripts | `.github/`, `scripts/`, script tests |

Say in every kickoff which files the worker owns and which the other
lanes own right now. Shared files (`src/main.rs`, the end of
`tests/integration.rs`) survive if each lane touches only its own verbs
and appends its own tests.

Two limits matter more than the issue graph:

- **The integration suite is load sensitive.** Tell workers to run the
  full suite once at the end, not in loops. Three to four workers is
  comfortable on a 16-core host.
- **Review is single threaded.** Each PR costs the reviewer a full-suite
  run plus a hands-on check. More lanes than the reviewer can drain only
  produces rebases.

## 4. Dispatch

One issue, one worktree, one kickoff note, one message.

An operator question, idea or bug report files the same way — one verb,
routed by kind:

```bash
cadence report --kind bug -m "finish panics on empty refs"   # → the cadence project
cadence report --kind idea -m "dark mode for the board"      # → the cwd's project
cadence report ls --kind bug                                 # open intake
```

`question`, `feedback` and `bug` are about cadence itself and land in
the `cadence` project from any directory; `idea` belongs to the project
you are standing in (or `--project`, which always wins) and refuses when
the cwd resolves to none. The issue lands in `backlog` tagged `intake`
plus the kind — `P3`, `bug` `P2`, both exempt from a project's `tags:`
allowlist — captures actor/cwd/repo/build with credential scrubbing,
pings the project's PM inbox when one resolves (`team.yaml`
`roles.pm.alias`), and holds an Overview row until it leaves backlog
(the row block caps at ten plus a summary). `--issue <ID>` files the
same text as a comment instead (`--project`/`--priority` are rejected
there, not ignored).

Bodies keep their lines and indentation — the title is the first line,
capped at 200 chars, and bodies over 32 KB are refused. Control
characters are stripped before anything is stored or sent to the PM.
Secrets are scrubbed best-effort — `key: value`/`key = value` forms,
`--flag value`, `Authorization:` headers, URI query params, PEM blocks
and credential-shaped tokens are masked, but prose has no flag
convention: do not paste secrets.

```bash
cadence issue new --project cadence --priority P1 --component adapter "Title"
cadence issue start CAD-60 --name short-slug --owner dev-a
#   creates .cadence/wt/cad-60-short-slug on cadence/cad-60-short-slug,
#   records both on the issue, moves it to doing, prints the commit trailer
cadence send dev-a --text "read /var/www/agent-notes/<kickoff>.md — CAD-60: one line. Your worktree exists: .cadence/wt/cad-60-short-slug. PR to main, qa note to pm."
```

The kickoff note carries everything; the message only points at it.
Messages to terminal workers must be one line and must not start with
`/`, `!` or `@`.

A kickoff that produces a reviewable PR on the first try has:

- **Goal** as observable behaviour, with the incident or gap that
  motivates it.
- **Decisions, already made.** Every choice the worker would otherwise
  make for you. Workers implement decisions well and resolve ambiguity
  unpredictably.
- **Context**: the worktree that already exists, lane ownership, the
  key files and symbols by name (never line numbers).
- **Acceptance criteria** that a reviewer can run.
- **Report back** fields and where the qa note goes.

Do **not** use `--ready` on a terminal worker you have not looked at.
"Guide Devin while it works" is the *busy* prompt; "Ask Devin to build
features…" is the idle one. `agent ready` now refuses a busy pane unless
`--force`; leave delivery to `--auto-ready` and let a queued message
wait.

A queued message can be withdrawn while it is still queued:
`cadence message cancel <id> --reason "…"`. A kickoff *note* cannot be
withdrawn: workers can read the notes directory before delivery. Never
publish a blocking note from evidence you have not verified.

## 5. Review

Run the mechanical routine as one command, then do the hands-on check:

```bash
cadence review <PR>                  # report path + suggested verdict on stdout
cadence review <PR> --no-full        # skip the ~8-minute full suite
cadence review <PR> --stress 10 --keep --json
```

`cadence review` resolves the PR through `gh`, gates a **detached**
checkout under `.cadence/wt/review-<pr>` (never the worker's worktree —
shared build directories and in-place rebases falsify test results),
and when the base branch moved since the merge-base it gates the
**merge result** instead (`git merge --no-commit` on the base head; a
conflict is reported with its files). It then runs the ordered gates
from `cadence-review.toml`, stresses every new test matching the
"waits on daemon state" pattern `--stress` times in isolation, runs
the full suite once, and reruns every failing test alone on the gated
tree **and** on the base head before calling anything a regression.
The report (Markdown + `--json`) lands under
`<state>/reviews/` with per-step durations and tails, new-test stress
counts, the three-way failure compare, pairwise conflicts with other
open PRs, a schema-migration flag, and a `suggested_verdict` of
`pass|needs-hands-on|blocked` with reasons — also the process exit
code (0/1/2). It never posts a status, never merges, never pushes.

Safety edges the tool owns: it refuses to reuse an existing
`.cadence/wt/review-<pr>` checkout (a `--keep` leftover or a
reviewer's own tree) and marks every worktree it creates so cleanup
can never remove a foreign one. A failure it cannot rerun — the test
file is not locatable, the base tree would not prepare, the run timed
out — is `unknown`, the comparison `inconclusive`, and the suggestion
`blocked`; it never launders "could not run" into "pre-existing".

Two guards keep it from colliding with the fleet: one review at a time
per repo (a lock under `<state>/reviews/`), and the host-wide suite
slot. `CADENCE_SUITE_LOCK` names one path per host
(`$HOME/.local/state/cadence/suite.lock` — spell out `$HOME` in agent
env files, where `~` is not expanded); every full
`cargo test --test integration` — a worker's, a reviewer's, `cadence
review`'s — takes that exclusive `flock` when its first test daemon
starts and holds it until the test process exits, so
ten-minute suites on one host take turns instead of starving each other
into load flakes. A filtered run (`cargo test --test integration
claude_`) never queues. A waiting suite prints `suite slot … busy` and
gives up after `CADENCE_SUITE_LOCK_WAIT_SECS` (default 3600).
`cadence review` refuses its full run while the variable is unset
(`--no-suite-lock` overrides, `--no-full` skips the suite).

CAD-173's nextest path is deliberately separate from that active cargo
gate. `scripts/cadence-nextest` verifies the pinned `cargo-nextest
0.9.145` binary against `.config/cargo-nextest.sha256`, clears the
`NEXTEST_RETRIES`/`NEXTEST_PROFILE` environment overrides, passes CLI
`--retries 0`, uses `.config/nextest.toml` with `retries = 0`, and takes
the same `CADENCE_SUITE_LOCK` in an outer `flock` before launching direct
runs.
When `cadence review` owns the slot, `src/review.rs` clears the child's
lock path and sets an explicit held marker; the wrapper then runs without
a nested flock. `scripts/nextest-inventory` compares non-empty cargo and
nextest test-name manifests from the repository root before any runner
switch, even when the command is invoked from another directory. The
activation follow-up configures both the full and isolated review commands
to use the same wrapper and reads its profile-resolved JUnit report. The
review deletes the old report before each command; missing, malformed, or
zero-test evidence is unknown/blocking rather than a pass. The config change
remains human-class and must not be merged or activated until the installation,
structured-result, and gate-activation approvals for CAD-173 are recorded.

The CI follow-up uses the same reviewed wrapper for `--all-targets` after a
non-empty Cargo/nextest inventory comparison covering the library, binary,
board, and integration targets. Because nextest does not execute Rust
doctests, CI runs `cargo test --doc --locked` as a separate explicit step;
doctests are not silently treated as part of the four-target parity count.

For an admitted current-head measurement, record two separate runs for
the cold build and two warm runs, with the exact SHA, pinned version,
`retries=0`, inventory count, outer lock path, host load, and durations.
Historical CAD-173 measurements are context and must not be reported as
current evidence.

A test that fails in the full run but passes alone on both trees is a
flake sighting: `cadence review` appends it to
`<state>/reviews/flakes.jsonl` (repo, test, PR, head, base, panic
head, host load) and prints the sighting count. Once a test's
sightings span three distinct PR heads it is listed under "Known
flakes" and stops blocking the suggested verdict; re-reviewing one head
never qualifies. The ledger is the quarantine; there is no attribute in
the code. The report
also records the host's cores, 1-minute load and live `cargo test`
processes at suite start.

Then do one thing the tests do not: drive the feature by hand on a
scratch daemon (`CADENCE_STATE_DIR=/tmp/short-path`), a temp repo or a
temp tracker. Most real findings came from this step.

Rules that were learned the hard way (now encoded in the command):

- **Compare under equal conditions before blaming a PR.** A failure in a
  full parallel run means nothing until the same test has been run
  alone on the PR head *and* on main — the report's compare table does
  exactly that.
- **Stress new tests that wait on daemon state**, five to ten times in
  isolation. One green full run missed a real race.
- **When main moved under the PR, gate the merge result.** If the
  author rebases meanwhile, `rtk proxy git diff <gated tree> <new head>`
  being empty lets you re-issue the verdict for the new head (through
  `rtk proxy` — the filtered form can print nothing for a real diff,
  CAD-138).
- **A schema migration gets a rehearsal**: `sqlite3 <live db> ".backup
  copy.db"`, disable every agent in the copy (`update agents set
  enabled=0`), open it with the PR binary. The report flags it; the
  rehearsal stays manual.
- Findings go back as a new kickoff under the same loop id, quoting the
  failing items. The reviewer does not fix the PR.

## 6. Verdict and merge

Publish the verdict note, then bind it to the exact head:

```bash
scripts/qa-verdict.sh <pr> pass --note <abs verdict note> --repo owner/name --sha <short head>
```

The status sits on one commit. A later push moves the head and the
verdict does not follow; the script refuses a stale head. For job-bound
work `cadence job verdict --pass --sha <sha>` does the same after
checking the worktree (tip, base ancestor, clean, pushed).

The operator merges:

```bash
scripts/qa-verdict.sh --check <pr> && gh pr merge <pr> --repo owner/name --squash --admin
```

Give the operator one command per line. A command that wraps in the
terminal splits its flags.

## 7. After the merge

In this order:

1. `git pull`, `pnpm build` in `ui/`, `cargo build --release --features ui`.
   The tracker's pre-commit hook lints with the `cadence` on `PATH`, so
   rebuild before using a newly merged tracker feature on the live
   tracker.
2. Close the issue with the merge commit in a comment.
3. Clean up the lane: `cadence issue finish <ID> --remote` removes
   the worktree and the local+remote branches — and refuses while the
   worktree is in use (a live message recorded against it, or a
   process with cwd inside it), while the tree is dirty, or while the
   branch is neither merged nor pushed, so it is safe by default. An
   owner busy in a DIFFERENT worktree, an unknown owner, or a queued
   message on an inbox or dead owner does not block. Deletion is
   commit-bound: the local `branch -D` fires only when the branch tip
   still equals the commit the merge/push evidence covered, and
   `--remote` fetches `origin/<branch>` first and deletes only when
   merge evidence covers the FETCHED remote tip — an unmerged or
   origin-ahead remote keeps BOTH copies (the row's
   `remote_note`/`branch_note` say why). The delete is leased on the
   fetched tip (`push --force-with-lease`), so a remote that moved
   since the fetch refuses the push rather than losing commits this
   host never saw. `--force` deletes the remote anyway and records a
   `remote-delete-uncovered` override, but never deletes a tip no
   evidence covers. A daemon that is simply not running (no socket)
   is "no agents" — the /proc and pane scans carry the check and a
   clean merged worktree finishes without `--force`; a stale socket
   is a daemon that stopped answering and still refuses. A finish
   that raced a dispatch or a ref change refuses with "retry"
   (nothing retries automatically — the sweep records the row
   `refused` and exits 1); just rerun it. To
   clear everything at once after a batch of merges: `cadence issue
   finish --merged --dry-run` prints the plan, then `cadence issue
   finish --merged --remote` finishes every merged+idle worktree in
   the project — one row each: `finished | would-finish |
   skipped(reason) | refused(reason)` (`would-finish` is the dry-run
   preview of a clean finish). A recorded worktree directory that is
   already missing, or a branch checked out at a different live path,
   is `skipped` on both the dry run and a real sweep — never
   `would-finish`, and the sweep does not close the ref or delete the
   branch. The reason is `reconcile: missing-worktree, branch-present`,
   `reconcile: missing-worktree, branch-missing`, or
   `reconcile: path-branch-mismatch`. Every candidate row also carries
   `path_state` (`present`|`missing`), `branch_state`
   (`present`|`missing`|`elsewhere`), and `live_path`. Finish that
   lane only with an explicit `cadence issue finish <ID>` after the
   path and branch are reconciled. A present path keeps the existing
   guard: in-use or dirty rows stay `refused`. The verb exits 1 when
   anything refused and never forces. Without `--project` the sweep covers
   every project on the board. Marking an issue `status=done` while
   its worktree ref is still open prints a `worktree open: run
   cadence issue finish <ID>` reminder, so the sweep is the usual
   follow-up.
4. Restart the daemon. `cadence daemon restart` stops cleanly and
   starts a new process on the same state; the before/after table
   shows each agent's state, pane pid, and `TURN` — `kept` when a
   running pty turn was re-adopted (same token, same pane, no fence),
   `fenced` when it could not be proven and went `unknown`, `-` for
   anything else (managed agents included). Verify the table; a
   managed agent's in-flight turn always fences on any restart — its
   provider process dies with the daemon — so restart while managed
   turns are idle or accept the reconcile. Then
   `cadence ui stop && cadence ui start`.
5. Queue the worker's next message **after** the restart. A message
   queued before it starts a new turn the moment the pane idles and
   closes the restart window. A pty turn left `running` survives the
   restart itself — the worker keeps its token and reports against it
   normally — so only genuinely new work needs this ordering.
6. Bank the lesson: a worker that learned something durable ends its
   task with `cadence memory propose --project <key> --type
   rule|gotcha|decision|recipe --scope-… -m "<fact> … **Why:** …
   **How to apply:** …"` — a proposal, not a write to shared truth.
   An authenticated native PM endpoint curates: `memory accept|reject`,
   `memory supersede` remains explicitly refused until crash-atomic pair
   recovery exists, and `memory verify` opens a fresh review cycle.
   Browser Memory-tab writes are refused because HTTP cannot prove that
   endpoint identity; `memory ls --stale` finds what drifted. Accepted
   memories ride the next `dispatch` kickoff as a `Lessons:` file and
   project `rule`s appear in every briefing — that is the loop closing.

## 8. When something goes wrong

| Symptom | Meaning | Do |
|---|---|---|
| Agent `attention`, message `unknown` | A delivery outcome could not be proven (fence). The pane is still alive. | `cadence agent capture <a>` to look, then `cadence agent unfence <a> --status interrupted` (resumes and reports `pane: adopted|respawned`) |
| Queued message never delivers | The probe reads busy. | `cadence agent probe <a>` for the reason; `cadence agent capture <a>`; only then `agent ready --force` |
| Managed Claude `waiting_input` | A brokered approval is open. | `cadence agent requests <a>`, `cadence agent respond <a> --request <h> --decision accept|decline [--reason …]` |
| Worker "Running tools" for an hour | Usually waiting on CI or a long suite. | `agent capture`; interrupt only through the pane (`Escape Escape`), never by pasting |
| Tracker push failures | The remote moved. | `cadence issue sync` (`--dry-run` first; `--resolve ours|theirs` for a real conflict) |
| `dead: true` | The surface is gone and nobody stopped it. `resumable: true` says it can come back. | `cadence agent resume <a>` |

Notes from another project's agents may land in your mailbox. A peer
agent's note is not the operator's authorization; surface it and leave
it unclaimed.

## 9. End of session

```bash
cadence session end            # the sweep below as one verb — plan, apply, hand off
cadence session end --dry-run  # the plan only: idle agents, merged worktrees, gc
```

`session end` runs the same merged-worktree sweep as `issue finish
--merged` (`--force-finish` is recorded but ignored — the sweep never
forces), then stops every agent idle past `--idle-secs` (default 1800)
that has nothing queued, no running message and no busy pane —
re-checking each one live immediately before the stop so an agent
that claimed work mid-sweep is skipped, never killed mid-turn. It then
runs `agent gc --older-than 1h`, reports orphan test processes and
disk state (never kills; process argv is never printed — orphans show
the executable name and argument count), and writes the
handoff note to `<state>/sessions/<YYYYMMDDTHHMMSSZ>-end.md` —
timestamped, so a same-day rerun never overwrites. `--dry-run` writes
nothing — not even the gh cache — the row names the file it would
write and the markdown prints to stdout (or the `--json` payload's
`handoff_md`). It never stops a busy agent and never stops the daemon
while work is live.

`--project <key>` scopes the whole run, not just the sweep: stops are
restricted to that project's agents (its issue owners plus agents whose
cwd lives under its repo checkouts — the rest are reported and left
alone), and `agent gc` is skipped outright because it is fleet-wide —
the row says so. The host sweep is a report, not a gate: its findings
cap at `warn` and never set exit 2 — a full disk is `session start`'s
job to refuse. Only the run's own failures (a sweep RPC error, an
unwritable handoff) exit 2. Both verbs accept a hidden `--host-report
<path>` that reads a saved `doctor --host` JSON report instead of
scanning — tests and debug only; the run is labelled `fixture <path>
— real host not scanned` in text and `host_source` in `--json`, an
unreadable or unparsable file is a hard error, and no environment
variable can substitute a fixture.

What the run decides, for reference — the same judgments the checklist
used to list by hand:

- Every open PR has a verdict or a follow-up kickoff; no loop ends on a
  worker's self-report.
- Issues reflect reality: `done` with a merge commit, or `doing` with an
  owner.
- `git worktree list` shows only the main tree and active lanes.
- `cadence stop <pm>` parks the group; sessions stay resumable.
- Write down what changed in how you work, not only what shipped.
