# Running a cadence session

This is the working guide for the person or agent in the PM seat: how to
start a session, run several workers side by side, review what they
produce, and keep the controller healthy. It is written from a real day
of use (seventeen pull requests through four workers) and records what
worked and what bit. Reference detail lives in
[PROTOCOL.md](PROTOCOL.md), [JOBS.md](JOBS.md), [BOARD.md](BOARD.md) and
[DOGFOOD.md](DOGFOOD.md); this file is the path through them.

## 1. The roles

| Role | What it is | How it talks |
|---|---|---|
| **Operator** | The human. Merges, approves, decides scope. | Terminal, board |
| **PM** | Scopes work, dispatches, reviews, records verdicts. A human or an agent session. | An **inbox** endpoint: `cadence inbox <alias> --follow` |
| **Worker** | Implements one issue at a time in its own worktree. | A provider endpoint: Devin or Claude terminal (pty), managed Claude, Codex |

A PM that is an outside session (a Claude Code or Codex session you are
already talking to) needs no pane. Register an inbox alias for it and
every worker result routes there:

```bash
cadence daemon start
cadence agent register pm --provider inbox     # the PM seat
cadence inbox pm --follow                      # blocks; one JSON object per routed result
```

## 2. Start of session checklist

```bash
cadence daemon start          # or: cadence doctor, if anything looks off
cadence ui start              # board at http://cadence.localhost:18000 behind the dev gateway
cadence ui tailscale start    # optional: phone/laptop access at https://<dns>:9450 — tailnet-only, loopback bind unchanged
cadence issue doctor          # tracker: hooks ours, lint clean, ahead/behind origin
cadence issue sync            # pull other hosts' tracker writes before you plan
cadence agent list --all      # who exists; `resumable: true` agents can come back
cadence resume <pm>           # bring the group's workers back on their saved sessions
```

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

Review every PR in your own detached checkout, never in the worker's
worktree (shared build directories and in-place rebases falsify test
results):

```bash
git fetch origin
git worktree add --detach .cadence/wt/review-NN origin/<branch>
cd .cadence/wt/review-NN/ui && ln -sfn ../../../../ui/node_modules node_modules && pnpm build && cd ..
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --lib --bins --test board
cargo test --test integration            # about 8 minutes; once
```

Then do one thing the tests do not: drive the feature by hand on a
scratch daemon (`CADENCE_STATE_DIR=/tmp/short-path`), a temp repo or a
temp tracker. Most real findings came from this step.

Rules that were learned the hard way:

- **Compare under equal conditions before blaming a PR.** A failure in a
  full parallel run means nothing until the same test has been run
  alone on the PR head *and* on main.
- **Stress new tests that wait on daemon state**, five to ten times in
  isolation. One green full run missed a real race.
- **When main moved under the PR, gate the merge result**: check out
  `origin/main`, `git merge` the PR branch, run the gates there. If the
  author rebases meanwhile, `git diff <gated tree> <new head>` being
  empty lets you re-issue the verdict for the new head.
- **A schema migration gets a rehearsal**: `sqlite3 <live db> ".backup
  copy.db"`, disable every agent in the copy (`update agents set
  enabled=0`), open it with the PR binary.
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
3. Remove the worker's worktree **only when its pane probes idle**
   (`cadence agent probe <alias>`). A worker that is still watching CI
   keeps running commands in that directory.
4. Restart the daemon when every pane is idle. `cadence daemon stop`,
   wait for the process to exit, `cadence daemon start`, then
   `cadence ui stop && cadence ui start`. Panes survive a restart and
   are re-adopted with the same pid; verify it.
5. Queue the worker's next message **after** the restart. A message
   queued before it starts a new turn the moment the pane idles and
   closes the restart window.

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

- Every open PR has a verdict or a follow-up kickoff; no loop ends on a
  worker's self-report.
- Issues reflect reality: `done` with a merge commit, or `doing` with an
  owner.
- `git worktree list` shows only the main tree and active lanes.
- `cadence stop <pm>` parks the group; sessions stay resumable.
- Write down what changed in how you work, not only what shipped.
