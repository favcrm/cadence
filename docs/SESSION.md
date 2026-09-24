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
cadence session start --all   # judge every project, not just the cwd repo's
cadence session ack <key> --reason "<why>" --expires 3d
                              # park a known item: it warns, not fails, until expiry
cadence session ack --list    # every acknowledgement, expired ones marked expired
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
restarts a running daemon, never removes anything.

**Scope.** The gate judges one project: the one whose declared repo
holds the cwd (the same remote-then-path match `issue` uses, minus
`CADENCE_PROJECT` — an ambient variable never narrows the gate), or
`--project <key>` (an unknown key is an error, same as `issue ls
--project`). Reconcile and inbox findings that belong to other
projects collapse into one `others` line — item count and worst
severity per project — which never raises the exit above `1`.
`--all` restores the fleet-wide gate. Run outside every known project
repo, the gate judges every project, as `--all`, and its `scope:` line
says so. A finding is attributed from its issue's project, its repo
checkout, or its agent (cwd under a declared checkout, else the one
project whose open issues it owns); a finding nothing attributes stays
in scope. Host checks are machine-wide and always in scope.

**Acknowledgements.** Every finding prints a key in brackets —
`[reconcile:<message-id>]`, `[fenced:<alias>]`, `[doing:<ISSUE>]`,
`[worktree:<path>]`, `[inbox:<alias>]`, `[host:<check>]`. `cadence
session ack <key> --reason <text> --expires <90m|12h|3d|YYYY-MM-DDTHH:MM:SSZ>`
records who, why and until when in `<state>/sessions/acks.json`; the
expiry may be at most 14 days out. While an ack is live the finding
downgrades from `fail` to `warn` and still prints, with `(acknowledged
until …: reason)`; after expiry it fails again and prints
`(acknowledgement expired …)`. Records are appended, never pruned:
`--list` shows expired ones as expired. An unreadable ack store adds
an `acks` warn row and applies no acknowledgement.

**Kill remedies.** Where a host check suggests signalling processes
(orphans, the biggest FIFO holders), the remedy is one line per pid
naming what it is, read from `/proc` at report time — `kill 456663  #
node  cwd=/…/.cadence/wt/x (deleted)  age=17h`. A pid that exited
before the report is omitted. `doctor --host` prints the same lines.

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
uid, not a symlink, and its provider must have no *live* in-flight
cadence turn (`submitting`/`running` in cadence's own store on an
alias whose actor is alive, and — for a pty turn awaiting its report —
still inside `report_timeout_secs`; a stale row never defers, CAD-250 —
a courtesy gate, since SQLite's locking is what actually protects the
data).
TRUNCATE cannot lose committed frames; a busy or failed attempt just
defers to the next tick. Successes record `wal_checkpointed` on the
`daemon` event stream (`cadence events daemon`) with before/after
bytes. Opt out with `[host] wal_checkpoint: false`; preview with
`wal_dry_run: true` (emits `wal_checkpoint_pending`, never writes) —
`doctor --host` also flags stores `over_checkpoint_limit`.
Dead agent registry rows are the other thing the daemon *can* sweep
itself, but only when you opt in: `[host] agent_gc_older_than_secs`
(unset = off; below 7 days is raised to 7 days with a warning) makes
the daemon run the `agent gc` rule at most hourly, skipping enabled
agents, live pty panes and any agent with a queued, running or
`unknown` message, and recording `agent_gc_removed` per row on the
`daemon` stream. It is **records only** — it frees no memory and no
disk, and a removed agent can no longer be resumed — so it is not a
remedy for memory or disk pressure. `cadence daemon status` shows the
effective setting under `agent_gc_timer`.
Idle agents *are* the memory remedy, and that one is on by default: an
agent with nothing queued, running, awaiting a report or `unknown` and
no delivery, report or turn activity for 60 minutes is stopped through
the normal `agent stop` path (its pane and MCP children go with it) and
records `agent_auto_stopped`. It stays resumable — `cadence agent
resume <alias>`, or `cadence resume <group>` — and `cadence status`
shows it as `stopped (auto, idle 72m)`. A message queued for it
resumes it automatically and is delivered (CAD-413). An agent stopped
by an operator or PM stays stopped. A failed auto-resume is not
retried; it raises an `auto_resume_failed` needs-me row naming the
waiting message. PMs/group roots, inboxes and
pty agents with an attached terminal are never stopped; opt one agent
out with `cadence agent set <alias> auto_stop=off`, or tune the bound
with `[host] auto_stop_idle_secs` (0 = off) and
`auto_stop_idle_secs_by_provider`. `cadence daemon status` shows the
bound and why each live agent was kept, under `agent_auto_stop`.
`pm.yaml [host]` also tunes `mem_warn_pct`/`mem_fail_pct` (15/5) and
`swap_warn_pct`/`swap_fail_pct` (20/5) — and note `Committed_AS` over
`CommitLimit` under heuristic overcommit is normal, not a failure.
A `[host]` table that cannot be applied (an unknown key or a bad value,
such as a quoted `"false"`) is never dropped silently: every threshold
falls back to its default, the WAL watch turns **off**, and `doctor
--host` shows a `config` warn naming the key. `project.yaml` refuses
unknown keys the same way.

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
lanes own right now. Shared files (`src/main.rs`, the shared harness in
`tests/common/`) survive if each lane touches only its own verbs and
appends its own tests to its own area binary under `tests/`.

Two limits matter more than the issue graph:

- **The integration suite is load sensitive.** Workers run focused
  groups only — the integration tests that call what they changed,
  found by grepping for the RPC, verb or helper — plus their new tests
  5× alone. CI runs the full suite on every push and the reviewer runs
  it once per PR; a worker runs it only when `docs/roles/dev.md` step 4
  names an exception (`src/daemon.rs`, `src/store.rs`, `src/adapter/`,
  `src/main.rs`, the shared test harness) or the kickoff asks. Three to
  four workers is comfortable on a 16-core host.
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
#   records both on the issue, moves it to doing, prints the commit trailer;
#   re-running it reuses that open lane (re-applying the cargo target) —
#   a different --name is refused while the lane is open
cadence send dev-a --text "read /var/www/agent-notes/<kickoff>.md — CAD-60: one line. Your worktree exists: .cadence/wt/cad-60-short-slug. PR to main, qa note to pm."
```

**Claim before you start (CAD-383).** `dispatch` and `issue start`
refuse a doing/review issue that another PM or lane holds, naming the
holder and the claim age. A dispatch records you (`--reply-to`) as the
claimant. When your lanes run outside cadence — Claude Code subagents,
Codex, a human — nothing records the claim for you, so do it first and
refresh it on long work:

```bash
cadence issue claim CAD-60 --note "claude subagent, branch fix/cad60-x"
cadence issue release CAD-60 --note "handed back"      # when you stop
cadence dispatch CAD-60 --to dev-b --note <kickoff> --take-over "pm-a's lane died 09:00; agreed in #ops"
```

Refused? Ask the holder first (`cadence issue show <ID>`, `cadence
status` shows claims and their age). `--take-over "<reason>"` is for a
stale or agreed hand-over; it is recorded on the issue. Backlog/ready
issues with an owner only warn.

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
from `cadence-review.toml` **as committed on the base head** (`git show
<base-sha>:cadence-review.toml` — never the PR's copy, never the
reviewer's working tree; a base without the file is refused), stresses
every new test matching the
"waits on daemon state" pattern `--stress` times in isolation, runs
the full suite once, and reruns every failing test alone on the gated
tree **and** on the base head before calling anything a regression.
The report (Markdown + `--json`) lands under
`<state>/reviews/` with per-step durations and tails, new-test stress
counts, the three-way failure compare, pairwise conflicts with other
open PRs, a schema-migration flag, and a `suggested_verdict` of
`pass|needs-hands-on|blocked` with reasons — also the process exit
code (0/1/2). It never posts a status, never merges, never pushes.
Every step command runs like GitHub CI, with no git identity (CAD-301,
CAD-307). A test that commits without `-c user.name=… -c user.email=…`
therefore fails the review, not just CI. Besides its own context
variables (`CADENCE_REVIEW_PR`, `_HEAD`, `_BASE`, `_MERGE_BASE`,
`_TREE`, `_ROOT`) and the suite-slot ones below, the review sets:

- `HOME`: a scratch directory, mode 0700 whatever the umask, removed
  when the review ends.
- `GIT_CONFIG_NOSYSTEM=1`.
- `user.useConfigOnly=true`, appended through `GIT_CONFIG_COUNT` /
  `GIT_CONFIG_KEY_<n>` / `GIT_CONFIG_VALUE_<n>`. The caller's own
  entries are kept, renumbered, except identity keys.
- `CARGO_HOME`, `RUSTUP_HOME` and `XDG_DATA_HOME`: the caller's real
  paths. When unset they derive from the real home (`HOME`, else the
  passwd entry, as cargo and rustup resolve it).

It unsets `EMAIL`, `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`,
`GIT_COMMITTER_NAME`, `GIT_COMMITTER_EMAIL`, `GIT_CONFIG_PARAMETERS`,
`GIT_CONFIG_GLOBAL`, `GIT_CONFIG_SYSTEM` and `XDG_CONFIG_HOME`.

Safety edges the tool owns: it refuses to reuse an existing
`.cadence/wt/review-<pr>` checkout (a `--keep` leftover or a
reviewer's own tree) and marks every worktree it creates so cleanup
can never remove a foreign one. A failure it cannot rerun — the test
file is not locatable, the base tree would not prepare, the run timed
out — is `unknown`, the comparison `inconclusive`, and the suggestion
`blocked`; it never launders "could not run" into "pre-existing".
A PR cannot weaken its own gates (risk class 7): one that changes
`cadence-review.toml` is still gated with the base head's copy, the
report records `config.changed_by_pr: true` (compared against the
merge-base, so a config change that landed on the base later is not
blamed on the PR), and the suggestion is never `pass` — at best
`needs-hands-on`, for operator review.

Two guards keep it from colliding with the fleet: one review at a time
per repo (a lock under `<state>/reviews/`), and the host-wide suite
slot. `CADENCE_SUITE_LOCK` names one path per host
(`$HOME/.local/state/cadence/suite.lock` — spell out `$HOME` in agent
env files, where `~` is not expanded); every full
`cargo test --all-targets` — a worker's, a reviewer's, `cadence
review`'s — takes that exclusive `flock` when its first test daemon
starts and holds it until the test process exits, so
ten-minute suites on one host take turns instead of starving each other
into load flakes. A filtered run (`cargo test --test agents_pty
claude_`) never queues. A waiting suite prints `suite slot … busy` and
gives up after `CADENCE_SUITE_LOCK_WAIT_SECS` (default 3600).
`cadence review` refuses its full run while the variable is unset
(`--no-suite-lock` overrides, `--no-full` skips the suite).

Since CAD-173 (#93 for review, #94 for CI, both merged 2026-09-21 with
operator approval) the full suite and the isolated reruns run under
nextest. `scripts/cadence-nextest` verifies the pinned `cargo-nextest
0.9.145` binary against `.config/cargo-nextest.sha256`, clears the
`NEXTEST_RETRIES`/`NEXTEST_PROFILE` environment overrides, passes CLI
`--retries 0`, uses `.config/nextest.toml` with `retries = 0`, and takes
the same `CADENCE_SUITE_LOCK` in an outer `flock` before launching direct
runs.
Install it once per host with `scripts/install-cadence-nextest`: both
checksums are verified and the binary lands in
`${XDG_DATA_HOME:-~/.local/share}/cadence/tools/cadence-nextest-0.9.145/`
(operator-approved location, CAD-273), never on PATH, Cargo home or the
repository. The wrapper finds it there without `CADENCE_NEXTTEST_BIN`,
which still overrides; CI installs into its own `runner.temp`. A missing or
untrusted runner shows in `cadence review` as "test runner refused to
start", not as a failed suite.
When `cadence review` owns the slot, `src/review.rs` clears the child's
lock path and sets an explicit held marker; the wrapper then runs without
a nested flock. `scripts/nextest-inventory` compares non-empty cargo and
nextest test-name manifests from the repository root before any runner
switch, even when the command is invoked from another directory.
`cadence-review.toml` points both the full and isolated review commands at
the same wrapper, and the review reads its profile-resolved JUnit report.
The review deletes the old report before each command; missing, malformed,
or zero-test evidence is unknown/blocking rather than a pass. A change to
that config is still human-class (risk class 7).

The CI follow-up uses the same reviewed wrapper for `--all-targets` after a
non-empty Cargo/nextest inventory comparison covering the library, binary
and every `tests/` target. Because nextest does not execute Rust
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
`target/debug/cadence sandbox up <name>` builds all three at once from
the PR binary — its own state dir, tracker and board port, with the
skill sync, tailnet and provider WAL checkpoints gated off, and agents
that keep the sandbox's tracker — and `sandbox reset <name>` removes it,
panes included. It refuses production's dirs and port 3010, so it never
stands in for a rollout; see the README's Sandbox section. Put
`CADENCE_SANDBOX_ROOT` under a short `/tmp/<x>.XXXX`: a root inside an
agent scratchpad can push `state/cadence.sock` past the 107-byte Unix
socket limit, which `up` refuses.

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

1. Once the post-merge CI run on `main` is green, install that exact
   build: `cadence upgrade --latest-main` (see "Installing the tested
   main build" below). Do not build a release locally. The tracker's
   pre-commit hook lints with the `cadence` on `PATH`, so upgrade before
   using a newly merged tracker feature on the live tracker.
2. Close the issue with the merge commit in a comment.
3. Clean up the lane: `cadence issue finish <ID> --remote` removes
   the worktree and the local+remote branches — and refuses while the
   worktree is in use (a live message recorded against it, an
   unreconciled `unknown` message bound to it or held by a registered
   agent whose cwd is on it, a process with cwd inside it, or any
   non-ignored file modified in the last 30 minutes — named with its
   age), while the tree is dirty, while the branch has not started
   (no commits beyond where it was cut — never "merged"), or while the
   branch is neither merged nor pushed, so it is safe by default. An
   issue with several open worktree refs needs `--worktree <path>`;
   naming a lane whose directory is already gone just closes its refs.
   A reconciled agent whose cwd is still this path, an owner busy in
   a DIFFERENT worktree, an unknown owner, or a queued message on an
   inbox or dead owner does not block.
   Deletion is
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
   preview of a clean finish). A lane with no commits yet is
   `skipped(not started)`, never finished as merged. A recorded
   worktree directory that is already missing, or a branch checked
   out at a different live path, is `skipped` on both the dry run and
   a real sweep — never `would-finish`, and the sweep does not close
   the ref or delete the branch. The reason is `reconcile: missing-worktree, branch-present`,
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
4. Restart the daemon — `upgrade` never does this on its own. Run the
   `restart_command` it printed (`cadence daemon restart --when-idle
   --ui`, plus `--as <identity>` outside a pane), or pass `--restart` to
   `upgrade`. `cadence daemon restart` stops cleanly and
   starts a new process on the same state. The rollout lease gates a
   build change and a schema crossing. A same-build `daemon stop`
   followed by `daemon start`, or a crash restart of the same build,
   stays lease-free, so the lease check on that same-build
   `daemon restart` is advisory. The daemon's own `shutdown` is not
   (CAD-384): it admits the proven operator — a shell outside every
   pane, `--as operator:<name>` / `CADENCE_ROLLOUT_AS` — or the agent
   that holds the live rollout lease **under an operator grant**, from
   its own pane. The operator grants the rollout owner once, from a
   shell outside every pane: `cadence rollout grant ops-1 [--until 7d]`
   (recorded as a `rollout_grant` event, listed by `rollout status`);
   `cadence rollout revoke ops-1` ends it. Without a live grant an
   agent's `rollout claim` is refused, and a lease it holds (a handoff,
   a grant since revoked or expired) does not let its pane stop the
   daemon. Any other agent's `daemon stop`/`restart` is refused — the
   restart names the refusal and records no `rollout_restart_proceeded`
   — so a pane-run rollout owner holds a grant and claims the lease
   first, even for a same-build restart. `--ui` restarts the board
   without the pane's `CADENCE_ALIAS`: the board is the operator's.
   An operator-shaped holder (`rollout claim --as operator:<name>`)
   must be provably the operator too, so a pane cannot hold the lease
   under an operator name. **This gate stops mistaken and misattributed
   stops — an agent restarting production it does not own, a detached
   child passing for the operator. It is not a security boundary:** an
   agent running as the same uid can write the state database or signal
   the daemon directly (CAD-280). The before/after table
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

### Installing the tested main build (`cadence upgrade`, CAD-334)

A build is installed from CI, never compiled on the host. On every push
to `main`, once `fmt`, `clippy`, `test`, `build` and `ui` have passed on
that sha (in that push run, or for a queued merge in the merge_group run
of the same sha, CAD-409; see "Post-merge CI on main"), CI's `release-artifact` job (ubuntu-24.04, `contents: read`
only) builds `cargo build --release --locked --features ui` after
`pnpm build` with the same floating `stable` toolchain the test jobs use
(the exact `rustc`/`cargo` versions go in the manifest), and checks that
`cadence --version` ends in `+<sha>`. A separate `release-attest` job,
the only one with `id-token: write`, runs no repository code: it
re-checks that build's sha256 and manifest, attests the binary, and
uploads the artifact `cadence-<sha>-x86_64-linux` (kept 90 days) with
three files:

- `cadence`, the binary;
- `cadence.sha256`, its `sha256sum` line;
- `manifest.json`: `source_sha`, `run_id`, `run_attempt`, `rustc`,
  `cargo`, `features`, `target`, `runner`, `checks`, `checks_event`
  (`push` or `merge_group`) and `checks_run_id` (the run whose gates
  passed; since CAD-409), `sha256`, `built_at`.

The attestation is a GitHub build-provenance attestation for the
binary. Pull-request and merge-queue runs never produce an artifact,
and neither job is a required check.

```bash
cadence upgrade --latest-main --dry-run   # verify everything, install nothing
cadence upgrade --latest-main             # newest main sha whose CI run succeeded
cadence upgrade --sha <40-hex>            # a specific main commit
```

`--latest-main` picks the newest successful `ci.yml` push run on `main`,
which may be older than `main`'s head while that head's run is still
going. It never moves the link backwards: when that sha is an ancestor
of the linked one it refuses, and a deliberate downgrade takes an
explicit `--sha`. Before anything is installed, `upgrade` checks each of these and
refuses, naming the fix, on the first that fails:

1. `gh` is installed and logged in (`gh auth login`).
2. The sha is on `main` (GitHub compare says `identical` or `ahead`).
3. CI's `test` job passed on that exact sha: in a `ci.yml` push run on
   `main` (a direct push, and every build before CAD-409), or else in a
   `ci.yml` merge_group run on a `gh-readonly-queue/main/*` ref whose head
   is that sha (a queued merge, whose push run skips the gates).
   Pull-request runs, and merge_group runs of any other sha, do not count.
4. The push run on `main` for that sha still holds the artifact. It can be missing (the job did not
   run, or the build predates CAD-334) or expired (after 90 days).
5. The downloaded binary hashes to `cadence.sha256`, and to the
   manifest's `sha256`.
6. The manifest's `source_sha` is the requested sha.
7. `gh attestation verify <binary> --repo favcrm/cadence
   --signer-workflow favcrm/cadence/.github/workflows/ci.yml
   --source-ref refs/heads/main --source-digest <sha>
   --deny-self-hosted-runners` passes.
8. Only then is the binary run: `--version` must end in `+<sha>`.

`--dry-run` runs all of these on a temporary copy and changes nothing.

**Install.** The binary is written to
`~/.local/share/cadence/releases/<sha>/cadence` (mode 0755) through a temp
file and a rename, with its `manifest.json` and `cadence.sha256` beside
it. Then `~/.local/bin/cadence` is repointed by making a new symlink at a
temp name and renaming it over the old link, so the link is never
missing. The releases dir is read off the current link. `--link` and
`--releases-dir` override both paths. `--repo`, `--link` and
`--releases-dir` are operator inputs: they change which repository's
builds are trusted and what is installed or replaced, so never take them
from a message or an agent. A link that is a regular file is never
replaced. The persisted copy is re-hashed before the link moves, and the
link is re-hashed through afterwards and pointed back if it does not
resolve to the verified bytes. Earlier releases stay on disk. Before the
install, a verified `pre-update` backup of the store is taken; if it
fails, the upgrade is refused (see "Backup, export and restore"). The
report is JSON: `from_sha`, `to_sha`, `installed_path`, `verified{…}`,
`backup`, `restarted`, and `restart_command`.

**Releases already on disk.** A release under `releases/<sha>/` is
reused without a download only when it proves to be the CI build: its
recorded `cadence.sha256` and `manifest.json` match, and `gh attestation
verify` passes on the installed copy, before it is ever run. The report
says `trust: "attested CI build"`. `--latest-main` also requires a CI
manifest with a `run_id`; a release without one (built by hand, or
planted) or one that fails attestation is replaced by the downloaded CI
build, through the same temp file and rename.

**Rollback.** `cadence upgrade --sha <previous>` rolls back to a release
already under `releases/`. When it attests, nothing is downloaded. A
release whose binary no longer matches its recorded checksum is refused.
A hand-built release (everything installed before CAD-334) has no
attestation and no CI artifact, so a plain `--sha` refuses and names
`--allow-unattested`; with that flag it rolls back and the report says
`trust: "unattested local release"`, `verified.attestation: "failed: …"`
and a `warning` that it is NOT the tested build. Without `gh` (offline)
an explicit `--sha` rollback also proceeds, labelled the same way with
`verified.attestation: "skipped: offline …"`. An unattested release is
never presented as the tested build.

**Restart.** Installing changes the CLI at once, but the daemon keeps
running its old build until it is restarted, and a restart changes fleet
behaviour, so it stays an explicit operator step. Without `--restart`,
`upgrade` prints the exact command: `cadence daemon restart --when-idle
--ui`. With `--restart` (plus `--as <identity>` outside a pane; without
an identity it refuses before installing), after a successful install
it runs that same command with the *new* binary. `daemon restart`
respawns its own executable, so a restart run from the old process would
start the old build again. The restart goes through the CAD-268 rollout
lease: claim it first with `cadence rollout claim --reason "<why>"
--target <sha> --as <identity>`. The report carries the restart's
before/after table and exit code.

Expected rollout effects after the restart:

- A Devin PM (managed) whose turn was in flight is fenced: its provider
  process died with the old daemon, and the turn goes `unknown` with the
  agent in `attention`. Once you have checked what it did, the operator
  reconciles it from a shell outside every pane with `cadence agent
  unfence <pm> --status interrupted` — an agent, the rollout owner's
  own pane included, is refused (CAD-374). That resumes
  it; add `--no-resume` to reconcile only. PTY turns that are re-adopted
  show `kept` in the table.
- Every endpoint open writes `ready`, so the idle auto-stop clock (CAD-96,
  default 3600 s) starts afresh. Idle agents begin to auto-stop about
  60 minutes after the restart, not at once. The stops are resumable.
- `cadence overview` and `doctor` stop reporting deploy drift once the
  daemon runs the new sha. While drift shows, their remedy is
  `cadence upgrade --latest-main`.

The artifact job first runs on the first push to `main` after CAD-334
merges. Builds of earlier commits have no artifact, and `upgrade` says so
rather than installing anything.

### Tagged releases and `install.sh` (CAD-311)

A new machine installs a tagged release instead of a main build. Pushing
a `v<version>` tag runs every `ci.yml` gate on the tagged commit, then:

- `release-gate` refuses a tag that does not equal `v` + Cargo.toml's
  version or whose commit is not on `main`;
- `release-build` (`contents: read`) builds `pnpm build` + `cargo build
  --release --locked --features ui` natively for `x86_64-linux`
  (ubuntu-22.04), `aarch64-linux` (ubuntu-22.04-arm) and `aarch64-macos`
  (macos-14), and checks `--version` is `cadence <version>+<sha>`;
- `release-publish` runs no repository code and waits in the `release`
  environment: it re-checks each tarball's sha256 and listing, attests
  the tarballs and `install.sh`, creates the GitHub Release as a draft
  with every asset, and publishes it last. Published assets never change:
  a re-run against a published release only passes when every asset is
  byte-identical.

A `v*` tag runs the `ci.yml` of the tagged commit, so the gate above is
only as strong as who can push tags and approve the job. Applied repo
settings: the "release tags" ruleset (id 23893821) makes creating,
updating or deleting `v*` tags admin-only; the `release` environment
requires reviewer cc-syntax and admits only `v*` tags; releases are
immutable once published. `cross-build` builds aarch64-linux and
aarch64-macos on every PR and merge-queue entry, so the platform cfg
gates are proven before a tag.

Assets per target: `cadence-<tag>-<target>.tar.gz` (the same
`cadence`, `cadence.sha256` and `manifest.json` that `upgrade` keeps,
manifest plus `version`) and `cadence-<tag>-<target>.tar.gz.sha256`, plus
`install.sh` (from `scripts/install.sh`).

```bash
# cut a release (admin): bump Cargo.toml's version on main first
git tag v0.2.0 origin/main && git push origin v0.2.0

# install (or reinstall, or roll back) on a machine
curl -fsSL https://github.com/favcrm/cadence/releases/latest/download/install.sh | sh
sh install.sh --version v0.2.0 --prefix /opt/cadence

# authenticity: built by this repo's ci.yml from that tag
gh attestation verify cadence-v0.2.0-x86_64-linux.tar.gz --repo favcrm/cadence \
  --source-ref refs/tags/v0.2.0 \
  --signer-workflow favcrm/cadence/.github/workflows/ci.yml
```

The `.sha256` files are served next to the tarballs, so `install.sh`'s
checksum checks catch corruption and truncation, not a forged release;
the attestation is the authenticity check.

`install.sh` installs into `<prefix>/releases/<tag>/` (default prefix
`${XDG_DATA_HOME:-~/.local/share}/cadence`, the releases dir `upgrade`
uses) and repoints `~/.local/bin/cadence` atomically. `upgrade` reads the
releases dir off a link into `<dir>/v<version>/`, so a later `cadence
upgrade --latest-main` lands next to the tagged release — on x86_64-linux
only: `upgrade` installs main builds for `upgrade::TARGET` alone, so
other targets update by rerunning `install.sh`. A paste-in prompt for
agents is in [INSTALL-AGENT.md](INSTALL-AGENT.md). The macOS binary's CLI
works; its daemon refuses to start until CAD-315.

### Post-merge CI on main

Policy (CAD-228): **every commit pushed to `main` is verified**, and its
push run is never cancelled. Pull requests keep cancel-stale behaviour.
Since CAD-409, a commit that landed through the merge queue is verified
by the queue's own run of that exact commit, and its push run only builds
the release. The `concurrency:` block in `.github/workflows/ci.yml` sets
the groups:

| Event | Group | Running run | Pending runs |
|-------|-------|-------------|--------------|
| `pull_request` | `ci-<PR number>` | cancelled by a newer head | at most one; a newer head replaces it (`queue: single`) |
| `push` to `main` | `ci-main-<sha>`, one per commit | never cancelled | none: a commit's group only ever holds its own run, so main push runs never wait for each other |
| `push` to `feat/**`, `v*` tag | `ci-<ref>` | never cancelled | up to 100 wait, oldest first (`queue: max`); only a 101st is cancelled |
| `merge_group` (merge queue) | `ci-<gh-readonly-queue ref>`, one per queue entry | never cancelled | `queue: max` |

**What a push run on `main` runs (CAD-409).** The merge queue merges an
entry by moving `main` to the exact commit its merge_group run tested
(GitHub: "the temporary branch `main/pr-2` will be merged in to the target
branch"; on record, #206 landed as 624f656, the head of merge_group run
35888699424). Re-running the gates on that commit re-tests identical
content, and while push runs were serial it held the release jobs about
1.5 h behind merges. So the push run starts with `queue-evidence`, which
asks the API for a merge_group run of `ci.yml` on a
`gh-readonly-queue/main/*` ref whose head is this sha and whose latest
attempt has `fmt`, `clippy`, `test`, `build` and `ui` all `success`:

| Push to `main` | `queue-evidence` | Gates (`fmt` `clippy` `test` `build` `ui`) | `release-artifact` → `release-attest` |
|---|---|---|---|
| queued merge, all five gates passed in its merge_group run | `tested=true`, names the run | skipped | run; `manifest.json` records `checks_event: merge_group` and `checks_run_id` |
| direct or admin-bypass push (no merge_group run), a queue run where a gate failed, or a failed lookup | `tested=false` | run here, as before CAD-409 | run only if all five passed here; `checks_event: push` |

The lookup never fails the run: an API error means `tested=false`, so the
gates run. `cadence upgrade` accepts the same evidence (the merge_group
run's `test` passed on that exact sha), keeps accepting a push run's own
`test` for direct pushes and older builds, and always installs the
attested artifact of the push run. The gates skip only when evidence
was found, so every artifact has one kind of evidence or the other.

**Merging through the queue (CAD-290).** When the merge queue is enabled on
`main`, do not `gh pr update-branch` and rerun by hand each time main moves.
Enqueue the PR once it is green and reviewed — `gh pr merge <n> --squash
--auto` (or "Merge when ready" in the UI) — and GitHub builds a
`gh-readonly-queue/main/*` ref with the PR on top of main and the PRs ahead
of it, runs the required checks (`test`, `fmt`, `clippy`, `build`, `ui`)
there, and merges only if they pass. A failing entry is removed and the
rest re-test without it. The queue's run is also `main`'s CI for that
commit (CAD-409, above). An admin bypass merge (`--admin`) skips the queue
and its combined test; the push run then finds no queue evidence and runs
every gate itself, so the release follows only after they pass. Reserve
it for emergencies.

History (CAD-228, when push runs were one serial group per ref).
Before that change, pushes used GitHub's default single pending slot, even though
`cancel-in-progress` was false. When three merges landed inside one run,
the second one's pending run was cancelled with zero jobs and that SHA was
never verified. Runs 35607746920 (a3e6f8f) and 35615227527 (4a9e3e8) are
the two cases on record. GitHub rejects `queue: max` together with
`cancel-in-progress: true`, so both keys are expressions on the event and
a single run never gets both.

Cost, measured from the 25 most recent completed main runs before the
change: median wall-clock `T` = 6.55 min (the `test` job is the long
pole), median 11.7 job-minutes per run (15 once each job is rounded up to
a whole minute). For `N` pushes that land while one run is going:

| N rapid pushes | Old policy: runs / job-min / latest verified after | New policy: runs / job-min / latest verified after | SHAs verified, old vs new |
|---|---|---|---|
| 1 | 1 / 11.7 / 6.6 min | 1 / 11.7 / 6.6 min | 1/1 vs 1/1 |
| 2 | 2 / 23.4 / 13.1 min | 2 / 23.4 / 13.1 min | 2/2 vs 2/2 |
| 3 | 2 / 23.4 / 13.1 min | 3 / 35.2 / 19.7 min | 2/3 vs 3/3 |
| 5 | 2 / 23.4 / 13.1 min | 5 / 58.6 / 32.8 min | 2/5 vs 5/5 |

The concurrency block landed with #94 (2026-09-21). In the 34 main pushes
from then to 2026-09-22, there were two bursts of three, and each one lost
a SHA. There were also a few bursts of two, which lost nothing. Under the
new policy those two bursts would have cost about 23 more job-minutes over
two days. The repo is public, so
standard hosted runners are not billed. The real cost is that in a burst,
the newest SHA waits one extra `T` for each extra push ahead of it. Runs
stayed serial (one per group), so the queue was bounded. Since CAD-409
main push runs have one group per commit and run in parallel: there is
at most one per pushed commit, so the merge rate bounds them, and a queued
merge's run only builds and attests the release.

Reading main CI: a green push run on a main SHA means its gates passed
there, or in the merge_group run that `queue-evidence` names in the run
summary. A `cancelled` run on a main SHA is **not** a test
failure, and it is **not** a pass either. That SHA is unverified until
its own run succeeds, or until a descendant on main succeeds, and even
then it is only "covered by" that descendant, never passed. Under this
policy a cancelled main run should only come from a manual cancel (on
`feat/**`, also from a queue past 100). Check it with
`gh run list --workflow ci.yml --branch main --json databaseId,headSha,status,conclusion`,
and look at the zero-job runs with `gh run view <id> --json jobs`.

## 8. When something goes wrong

| Symptom | Meaning | Do |
|---|---|---|
| Agent `attention`, message `unknown` | A delivery outcome could not be proved. A pty fence detaches the pane; it is not killed unless `agent stop` ran (stop kills a surviving pane). `agent capture` needs a live actor and fails while the fence holds. | Read `agent show` for that message's error and inspect side effects. A missed render does not prove the paste was not delivered. Reconciliation is an explicit operator decision, not automatic interrupted or completed. CLI `agent unfence` resumes by default — do not resume again after it. `--no-resume` reconciles without resuming. Bare RPC unfence does not resume. Do not dispatch a new revision until continuation is safe. |
| Queued message never delivers | The probe reads busy. | `cadence agent probe <a>` for the reason; `cadence agent capture <a>`; only then `agent ready --force` |
| Managed Claude `waiting_input` | A brokered approval is open. | `cadence agent requests <a>`, `cadence agent respond <a> --request <h> --decision accept|decline [--reason …]` |
| Worker "Running tools" for an hour | Usually waiting on CI or a long suite. | `agent capture`; interrupt only through the pane (`Escape Escape`), never by pasting |
| Tracker push failures | The remote moved. | `cadence issue sync` (`--dry-run` first; `--resolve ours|theirs` for a real conflict) |
| `dead: true` | The surface is gone and nobody stopped it. `resumable: true` says it can come back. | `cadence agent resume <a>` |
| Bash `git diff`/`git show` denied by a `PreToolUse` hook | `scripts/rtk-diff-guard.py` (wired in `.claude/settings.json`) denies every `git diff`/`git show` the rtk hook would rewrite — bare, `git -C`/`-c` spellings, `yadm`, `rtk git diff`, `rtk diff` — because the condensed output can print nothing for a real diff. On 2026-09-20 a filtered `git diff --numstat` came back empty and a net-deletion check read clean against a head that deleted 3018 lines (CAD-138). | Re-run as `rtk proxy git diff …` (the deny message names it) and use that form for any diff you base a decision on. `RTK_DISABLED=1 git diff …` (rtk keys on the variable's presence, not its value) or `\git diff …` also skips rtk and the guard. An empty filtered diff is unproven, not clean. |

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
runs `agent gc --older-than 1h` — under the caller rule (CAD-149):
the operator sweeps every dead agent, a PM only its own members, and
the dead agents this caller may not sweep are listed on the gc row as
a warning (`--dry-run` marks them from the daemon's `agent_gc_plan`),
never silently skipped — reports orphan test processes and
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
job to refuse. It prints the same `[host:<check>]` keys, ack notes
and named kill lines as `session start`; `session end` runs no
reconcile or inbox checks, so the cwd scope does not apply to it.
Only the run's own failures (a sweep RPC error, an unwritable
handoff) exit 2. Both verbs accept a hidden `--host-report
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

## 10. Backup, export and restore

The store (`<state>/cadence.sqlite3`) is the only durable controller
state that is not already in git. Three commands cover it (CAD-314):

```bash
cadence backup                          # verified copy + manifest into <state>/backups
cadence backup --reason nightly         # its own retention group, keeps 7 by default
cadence backup --dir /srv/cadence-bak --keep 14
cadence export --out ~/cadence-bundle   # portable bundle; credential patterns scanned
cadence restore <copy>.manifest.json    # or: cadence restore ~/cadence-bundle
cadence restore ~/cadence-bundle --repo ~/src/cadence --repo ~/src/app
```

**Backup.** The copy is taken with SQLite's online backup API from a
read-only connection. It is one snapshot, and a running daemon's writer
is never blocked, so there is no need to stop anything. The copy is
turned into a single file with no `-wal`, checked with `PRAGMA
integrity_check`, and hashed. A manifest named `<copy>.manifest.json`
is written next to it:

- `schema_version`, read from the copy itself;
- `sha256` and `bytes`;
- `integrity_check`;
- `versions`: the cadence build, the schema this binary migrates to, and SQLite;
- `repos`: each checkout the store points into, with its `origin` remote,
  userinfo stripped.

The pair is then read back from disk and verified before the command
reports success. The files are `0600`. A backup directory that cadence
creates is `0700`; an existing `--dir` keeps the mode it already has.

`--keep N` (default 7) prunes the oldest backups that share the same
`--reason` in that directory (CAD-396):

- The copy just written is never pruned; it counts as one of the N.
- Age comes from the UTC stamp in the file name
  (`cadence-<reason>-<stamp>-<id>`), then the manifest's mtime. Manifest
  content never decides age, so clock skew or a planted future-dated
  manifest cannot push out the fresh copy. The name is checked for its
  shape (`YYYYMMDDTHHMMSSZ` digits and an 8-hex id), not for valid date
  ranges. A backup written while the clock ran ahead carries that
  future stamp and outranks newer copies until real time catches up;
  the fresh copy still always survives.
- A copy is deleted only when it is the regular file
  `<manifest stem>.sqlite3` next to its manifest and its sha256 and size
  match that manifest. A symlink, an edited file, or a manifest naming
  some other file is left alone and listed under `prune_skipped`.
- Hand-made files, such as `cadence-live-<ts>.sqlite3`, are never
  touched, and a burst of `pre-restore` copies cannot push out the
  nightly ones.

`backup` re-checks after pruning that its own pair is still on disk and
fails otherwise. A pruning error is reported as such ("was written and
verified, but pruning … failed"), distinct from a failed backup.

**Nightly.** Nightly backups are a schedule, not a daemon feature. Add a
cron line:

```cron
17 3 * * * $HOME/.local/bin/cadence backup --reason nightly >> $HOME/.local/state/cadence/backups/nightly.log 2>&1
```

With the default `--keep 7`, this holds a week of copies. The backup
runs out of process, on its own read connection, so the daemon's
single-writer mutex is never involved. A daemon timer would add a new
failure mode to the production process, and it would need a config and
status surface like `agent_gc_timer`. That timer is a follow-up, not
part of this command.

**Rollout receipts.** `cadence rollout backup --path` refuses a file
inside the state dir. For a schema-crossing rollout, take the copy with
`--dir` outside the state dir after `rollout claim`, then record that
`.sqlite3` path.

**Export.** `export --out <new dir>` writes exactly two files:

- `cadence.sqlite3`
- `manifest.json`, whose `export` block lists what is in the bundle and what is not

Only the store goes in. It is changed in these ways:

- `agents.generation`, the generation every turn token is bound to, is set to NULL.
- `messages.turn_id`, the turn token itself, is set to NULL.
- Every turn token in the store is replaced with `[redacted]` in every
  text cell, before the columns above are nulled. A turn token is a value
  with the registry's token shape, `<prefix>-<generation>-<nonce>`: the
  prefix `pty` or `claude`, a generation of 12 or 32 lowercase hex, and a
  32-hex nonce. The value can sit in any table or column, under any JSON
  key or none, or inside escaped JSON. Event payloads outlive their
  messages, so this covers tokens whose message row is gone. Only that
  shape counts: prose under a `"turn_id"` key, such as
  `{"turn_id": "workspace"}`, is left alone.
- A generation is 12 or 32 lowercase hex. It is redacted in every text
  cell where it appears, alone or inside other text, when the store
  names it in one of three places: inside a turn token,
  in `agents.generation`, or as the value under a JSON `"generation"`
  key at any escape depth. The last covers an old endpoint's `ready` or
  `pane_root` event after every token and agent row naming it is gone.
  Hex under any other key (`owner_generation`, a message id, a commit
  SHA) is not collected as a generation. It stays, except where it
  contains a generation collected in one of those three places.
- The export then re-reads every text cell, decoding JSON `\uXXXX`
  escapes. It refuses if any token-shaped value, redacted generation or
  generation under a `"generation"` key is still there. That includes a
  token or generation spelled with escapes, which cannot be redacted in
  place. The result reports `redacted: {tokens, cells}`.
- `agents.pid` is set to NULL.
- The file is `VACUUM`ed, so deleted rows left in freed pages do not travel.

Everything else stays out by construction:

- `ui.json`, `slots.json`, logs, the lock and the socket;
- `secret-allowlist.toml` and `intake-relay.yaml`;
- `.env` files;
- `private/`, `sessions/`, `briefings/`, `reviews/`, `agents/`, `roles/` and `backups/`;
- provider sign-in state, which lives in the providers' own dirs and is never read;
- the tracker, which is a git repo with its own remote.

No board session secret exists yet (CAD-313). Because only the store is
exported, a token file in the state dir stays out. A secret column added
to the store must join `SCRUB_COLUMNS` in `src/backup/mod.rs`.

Every text cell of every table then goes through the CAD-109 secret
scan (`cadence secret scan`'s rules). A single blocking finding refuses
the export and removes the directory. The refusal names each
`table.column`, rowid, rule and fingerprint, never the value. A false
positive is for the operator to allowlist by rule and fingerprint in
`<state>/secret-allowlist.toml`. There is no bypass flag. Warn-level
findings pass and are counted under `scan.warnings`: the scan catches
credential *patterns*, it does not prove the store holds none.

A stale turn token quoted in prose (a message body or event payload,
rule `cadence-argv-secret`) blocks an export even though its generation
is gone. The token is dead once the agent's generation changes; after
checking that, allowlist it by fingerprint:

```toml
[[allow]]
rule = "cadence-argv-secret"
fingerprint = "<fingerprint from the refusal>"
reason = "expired turn token quoted in history"
```

A bundle is not signed. The manifest sha256 detects corruption, not
tampering: restore only bundles you made or received over a channel you
trust.

**Restore.** `restore` takes a backup manifest or a bundle directory and
writes `--state-dir` (default: the usual state dir). It refuses in each
of these cases:

- **A daemon holds the state dir.** A running daemon holds
  `cadence.lock`. Restore takes that lock itself for the whole run, so
  no daemon can start mid-restore.
- **The manifest's schema is newer than this binary.**
- **The copy does not match its manifest**: a different sha256 or size,
  a failed integrity check, or a different recorded schema.
- **The state dir already has a store**, unless `--force` is passed.
  `--force` first takes a verified `pre-restore` backup into
  `<state>/backups`. It then renames the old store and its `-wal`/`-shm`
  aside, links the verified copy in without overwriting (`hard_link`),
  and removes the aside files only after that succeeded; on failure the
  old files are renamed back. If that rollback itself fails, the error
  says `ROLLBACK FAILED` and names where the previous store now is.
  `cadence.lock` is opened with `O_NOFOLLOW`, so a symlinked lock is
  refused.
- **An earlier forced restore was interrupted**: a
  `cadence.sqlite3*.replaced-*` file in the state dir may hold the
  previous store. Every restore refuses until it is moved back or away.
  So does the daemon: `daemon start` and `daemon run` refuse before they
  open or create a store. `daemon restart` refuses before it shuts
  anything down, so the running daemon keeps serving, and checks again
  right before shutdown after a `--when-idle` wait. The error names each
  leftover and gives the `mv` commands that recover. If
  `cadence.sqlite3` is missing, it gives the commands that put the
  previous store back. If `cadence.sqlite3` exists too, the restore may
  have finished, or a store may have been created after the
  interruption. The error then gives both recoveries.

Repo paths are rewritten by remote. The restore matches each recorded
repo to the `--repo` checkout whose `origin` is the same remote.
`https://…/o/r.git`, `git@host:o/r` and `ssh://git@host:22/o/r` all
count as the same remote. The rewrite covers `agents.cwd`, `jobs.repo`,
`jobs.spec_path`, `tasks.worktree` and `tasks.spec_path` by prefix.

A recorded path that is still a checkout of the same remote is kept.
Anything else is listed under `unmapped` and left as written. Paths
inside JSON params and event payloads are history and are not rewritten.

A restored store with an older schema is migrated by the next daemon
start only under a rollout lease with a backup receipt. The JSON output
says so in `note`.

**Self-update.** `cadence upgrade` takes a backup first. The backup
runs after every verification and immediately before the release is
installed and the link moves. It goes through `backup::before_self_update`,
the same as `cadence backup --reason pre-update`, and writes a verified
copy into `<state>/backups`, which keeps 7 `pre-update` copies. A failed
backup refuses the upgrade, and nothing is installed. Before installing,
`upgrade` also checks that the reported copy and manifest still exist
and still verify; a missing or changed pair refuses the upgrade. `--dry-run` and an
upgrade that is already current take no backup. The copy is reported
under `backup`, and a state dir with no store yet reports `skipped`.
This copy is inside the state dir, so it is not a rollout receipt for a
schema crossing (see above).

Never point `restore` at the production state dir to try it out. Rehearse
in a `mktemp -d` state dir with a read-only copy of the store:
`sqlite3 -readonly "file:<live>?mode=ro" ".backup <tmp>/cadence.sqlite3"`.
