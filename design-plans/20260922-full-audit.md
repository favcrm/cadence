# Cadence full audit — use cases, scope, UX, code, tracker

Date: 2026-09-22. Source: `9aa8144` (main). Running daemon/UI build: `58afcc0` (3 commits behind).
Read-only: no tracker, agent, message or UI mutations. Builds on
[20260921-project-workspace-audit.md](20260921-project-workspace-audit.md).

## 1. Verdict

Cadence's core contracts are sound. Durable messages, verdicts pinned to a SHA, fail-closed
pane gating, git-native tracker, and CI discipline (pinned actions, nextest `retries=0`) all
hold up. The weak half is the **operating surface**:

- The system produces many signals and nothing retires them. Running turns never end, inboxes
  have no consumer, and stopped agents never leave the list.
- Operator views therefore become noise, and operators learn to ignore them. That defeats the
  charter's goal of detecting problems before a human reads panes.

The backlog has the same shape: 141 open tickets captured over two days, with nearly no
acceptance criteria and many owners who cannot act.

**Main advice: stop adding detectors for a sprint. Make the existing signals end, dedupe, and
stay scoped to a project. Then prune the backlog to something one PM can plan.**

## 2. Use cases and scope

| Persona | Primary job | Current happy path | Gap |
|---|---|---|---|
| Operator (human) | Decide substantial things, unblock | `session start` → `overview` → act | The gate always exits no-go (67 reconcile items, 17 of them from other repos). Overview lists symptoms (48 rows, 9 kinds). The UI's "Needs your decision" group is empty while merges and fences wait. |
| PM agent | Plan, dispatch, route review | `join` → `dispatch`/`job new+task add+job dispatch`/`issue start` | Three kickoff paths. Tickets have no acceptance criteria, so PMs dispatch titles. |
| Worker | Implement, report | briefing → work → `message result` | Unreported turns stay `running` forever. |
| QA | Verdict on exact SHA | `review <PR>` → `job verdict` | `cadence-review.toml` is read from the PR tree, so a PR can weaken its own gates. |
| Ops | Merge, restart, clean up | `gh pr merge … --admin`, `daemon restart`, manual gc | The generated merge command bypasses branch protection. GC is manual. |

Scope assessment:

- The charter lists L3 as current. Evidence in `tests/cad225_acceptance.sh` and CAD-225 (0/6
  checks) says L3 is **not yet proven unattended**.
- The roadmap has 7 epics in flight at once. Unparented work spans L4 (CAD-224/139) and L5
  (CAD-111/112), plus AgenticOS/Pi feasibility (CAD-231) and Devin Cloud (CAD-243).
- Recommendation: freeze L4/L5 and new provider work until CAD-225's acceptance passes. That
  means one goal carried to a reviewed merge with restart and provider-failure recovery.

## 3. Findings (ranked)

### Critical — signals that never retire

1. **`running` is unbounded.** The live store holds 128 running messages. aos-pm (devin/pty)
   has 91, each with a distinct turn_id, dated 2026-09-20 to 09-21. aos-ui-author has 14.
   - The daemon claims one actor serializes turns (`src/daemon.rs:9-12`), but the store
     allows many turns per alias (`src/store.rs:452-457`).
   - Fix: add a `delivered_awaiting_report` state. Allow at most one `running` per pty actor.
     After a timeout, move the message to `unknown` and send one notice.
2. **Queues with no consumer.** 868 messages are queued: fable-cc 644, sportslog-plans-pm 110,
   aos-pm 42.
   - fable-cc also owns 9 tickets, so in practice nobody owns them.
   - Fix: warn at send/route time when the target inbox has no consumer and more than N
     messages unread. Add retention, or require a named owner per inbox.
3. **Agent sprawl.** 67 agents are registered: 25 stopped and 5 fenced/dead, all shown every
   day.
   - GC is "never automatic". CAD-96/98/199/246 are all still in the backlog.
   - Fix: `status` hides stopped agents by default. Ship the record-only timed gc (CAD-199,
     merged with CAD-98) and idle auto-stop (CAD-96).

### High — safety and trust

4. **The generated merge command uses `--admin`** (`src/overview.rs:1276`, also in
   `docs/roles/ops.md:18` and `docs/BOARD.md:748`). *Corrected during execution:*
   branch protection on `main` requires 1 approving GitHub review, and Cadence's verdict is a
   commit status, so `--admin` is the designed path. The row is emitted only for a passing
   verdict with green checks, and it is pinned with `--match-head-commit`.
   - Residual risk: `--admin` also skips the strict up-to-date rule, so the only guarantee
     that the verdict covered the merge result is `cadence review` checking the merge tree.
   - Fix stays CAD-123/124: an owned `cadence land` that checks this itself. No code change
     was made.
5. **The board writes as `operator` for any local process** (`src/ui.rs:1169-1196`). The
   daemon, by contrast, derives the caller from SO_PEERCRED plus `/proc` ancestry
   (`src/daemon.rs:2150-2175`).
   - Any agent can `curl` the loopback UI to change issue state as the operator. That breaks
     separation of duties.
   - Fix: route UI writes through the daemon's caller derivation.
6. **Review gates come from the PR's own tree** (`src/review.rs:198`, `855`).
   - Fix: load `cadence-review.toml` from the base head, and flag PRs that change it.
7. **The store is a single `Mutex<Connection>` with 85 `lock().unwrap()` calls**
   (`src/store.rs:450`).
   - One panic poisons the lock and brings down the whole daemon. `busy_timeout` is not set.
   - Fix: a `with_conn` helper that recovers from poisoning, plus `busy_timeout`.
8. **Fail-open and success-shaped failures:**
   - CAD-218: `send` returns `queued` for multi-line text the pty will always reject.
     **Fixed in this pass** (refused at `agent_send`).
   - CAD-175: a format rejection fences the agent. **Already fixed** before this audit: it
     is a `pre_write` failure and the agent stays idle (pinned by
     `pty_send_rejects_control_chars`).
   - CAD-170/189: unknown or bad config keys are silently ignored.
   - CAD-149: a worker can change a peer's trust params.
   - All contradict charter principle 3 (fail closed), and all are small. Raise them to P1
     bugs.

### High — operator UX

9. **The UI hides operator decisions.** `ui/src/uxCopy.ts:29-45` maps `merge`, `fenced` and
   `approval_menu` to "team", so "Needs your decision" is empty. *Corrected during
   execution:* this is partly deliberate. Risk classes let ops-1 merge class `auto` on its own,
   and #121 (2026-09-22) chose to keep `approval_menu` as team.
   - The real gap: `fenced` rows go unresolved for 5 hours or more, and `unfence` is an
     operator verb. A team-grouped row with no live team owner is effectively the operator's.
   - Fix: group rows by whether a live owner can act, not by kind alone. This needs your
     product decision, so it is filed as a ticket rather than changed.
10. **Overview rows have no subject or cause key.** Five agents each appear as both `stalled`
    and `silent_end`, and `overview` has no `--project`/`--group`.
    - Fix: add `subject`/`cause` fields, merge rows per subject, and add scope flags. Share
      the model between the CLI and the UI.
11. **The session gate cries wolf.**
    - It always exits 2, reports daemon uptime as `29834943m` (an epoch bug), and suggests
      `kill <pids>` without naming the processes.
    - Fix: scope it to the cwd project, allow a known backlog to be acknowledged with an
      expiry, fix the uptime, and name each process.
12. **UI loading looks like failure.**
    - Endpoint times: `/api/overview` 12.6s, `/api/issues` 7.2s (287KB).
    - While `data===null`, the page shows "overview unavailable", and the Board shows
      "0 issues, empty".
    - Every SSE event refetches all six resources, with no dedupe (App.tsx:190-196).
    - Fix: separate loading from failed, keep the last good data, refetch only the named
      resource, and coalesce requests.
13. **A project's own agents are missing from its views.** Agents?project=cadence shows 0,
    "No agents registered", while three cadence workers are busy.
    - Binding uses only tasks × jobs. Use the dispatch/owner binding the CLI already derives.
14. **Counts disagree across screens.** Sidebar 235, Board 226, CLI 232. Overview says doing
    10; the Board says Doing 5.
    - Fix: one derived count, with labelled exclusions.
15. **The memory loop is stuck.** All 11 cadence memories are `proposed` and "review
    blocked: proposer has no authenticated native identity". No accepted lessons exist, so
    L5 has no input.
16. **The CLI surface is heavy.**
    - 32 top-level commands; `issue` has 24 subcommands and `agent` 17.
    - Help entries run up to 930 characters, and the README quick start is about 295 lines.
    - Overlapping pairs: send/message send, attach/agent attach, resume/agent resume,
      stop/agent stop, and dispatch/job dispatch/issue start.
    - Output conventions differ: `agent list` prints JSON only, while `status` prints a
      table.
    - An unknown `--project` exits 0 as if the project were empty.
    - Fix: one-line help entries, and a `cadence help workflows` page per role. Short verbs
      become the human path; long forms are marked as plumbing. Tables on a TTY, JSON when
      piped. An unknown project is an error.

### Medium — structure

17. **God modules.**

    | File | Lines |
    |---|---|
    | `src/doctor/host.rs` | 9,049 (also holds the security redaction) |
    | `src/main.rs` | 6,697 |
    | `src/store.rs` | 6,689 |
    | `src/daemon.rs` | 5,795 |
    | `tests/integration.rs` | 25,443 (356 tests, 116 sleeps) |
    | `ui/.../Drawer.tsx` | 1,130 |

    - Fix: mechanical splits. Split `doctor/host.rs` by probe and move `redact` into its own
      module. Split `main.rs` into `cli/<verb>.rs` and `tests/` by area. This directly speeds
      up CAD-173.
18. **The store knows the pty token format** (`pty-{gen}-` at `store.rs:1266,1405` and
    `daemon.rs:2347`, CAD-162).
    - Fix: store the minted token on the message row and compare for equality.
19. **Liveness rules differ by adapter.** Codex has a fixed 600s wall clock
    (`adapter/codex.rs:26`); Claude uses a 900s activity-based idle.
    - Fix: put limits in the registry capability data and apply them with one shared
      watchdog. That also closes CAD-227.
20. **Migrations are an ad-hoc ALTER list.** `agents.quota` is added twice (lines 1068 and
    1094).
    - Fix: an ordered `MIGRATIONS` table, plus a CI check that version numbers are unique.
21. **Docs are accurate but too heavy** (PROTOCOL.md 88K, BOARD.md 48K). ARCHITECTURE.md is
    already outdated (it describes host.rs in one line).
    - Fix: a module table generated in CI, and PROTOCOL.md limited to invariants.

## 4. Tracker audit (project `cadence`)

Snapshot: 232 issues, 141 open (127 backlog, 6 doing, 5 ready).

- By priority: P0 2, **P1 40 (28%)**.
- 105 open issues have no owner, 66 have no parent, and 63 have no component.
- 49 are tagged `proposed`.
- **About 95% have an empty `## Acceptance`.** In 142 of them the body is just the title.
- 121 of the open issues were filed on 2026-09-19 and 09-20.

**Close (shipped):**
- CAD-132 (WAL watch, #74)
- CAD-156 (flock fix, #88)

**Retitle to what's left (partly shipped):**
- CAD-111 (retro, #89)
- CAD-114 (Codex quota, #103)
- CAD-213 (local relay, #82)
- CAD-226 (#116)
- CAD-224 (#114)
- Confirm CAD-134 against SESSION.md:184,298. It is still valid.

**Merge duplicates:**
- CAD-199 → CAD-98
- CAD-97 → CAD-200
- Put CAD-188's phases under CAD-91
- CAD-151 + CAD-155 + CAD-178 → one ticket
- Collapse CAD-78 and CAD-115…128 into 4 phase tickets
- Fold CAD-125 and CAD-127 into CAD-122
- Fold CAD-121 into CAD-120
- Fold CAD-209, 210 and 211 into one ticket
- Fold CAD-218 and CAD-175 into one fix

**Re-parent:**
- CAD-157 and CAD-142 become epics over their ADR phase tickets
- CAD-179…186 → CAD-173
- Move CAD-243 out from under CAD-91, which is the wrong parent

**Owners who cannot act:**
- root-pm (owns P0 CAD-225), board-dev, luna-watchdog (owns P0 CAD-173),
  luna-queue-efficiency and claude-b are not registered agents.
- cookie-cesium is fenced/dead and owns 5 tickets.
- fable-cc is an unread inbox and owns 9 tickets.

**Priority:**
- Cut P1 to about 10.
- CAD-173: either staff it as P0 or make it P2.
- Raise the fail-open bugs (218, 175, 170, 189, 203, 149).
- Lower CAD-231, CAD-38 (80% done: close it after moving the rest), CAD-187 and CAD-236.
- CAD-75 (P2) parents P0 CAD-225. Align them.

**Stale in progress:**
- CAD-58: ready since 09-20, with no refs. Run it or drop it.
- CAD-90: no comments.
- CAD-129: unblocked, needs an owner.

**Hygiene gates:**
- Enforce CAD-159: refuse to dispatch an issue whose acceptance is empty.
- Backfill acceptance on every P0/P1/ready issue.
- Set `kind` on every open issue; it is empty on all of them today.

## 5. Recommended plan (in order)

| # | Change | Size | Closes / unblocks |
|---|---|---|---|
| 1 | Drop `--admin` from the generated merge command. Map merge/fenced/approval_menu to "decision" in the UI. | XS | Safety; UI misses operator actions |
| 2 | Tracker prune: close, merge and retitle per §4. Reassign dead owners. Cut P1 to about 10. Backfill acceptance on ready and P1 issues. | S, PM-only | Planning signal |
| 3 | Message lifecycle: `delivered_awaiting_report`, at most one running per actor, timeout to unknown. The token lives on the row. | M | Findings 1 and 18, CAD-162; fixes the stalled/silent_end noise |
| 4 | Inbox consumers: warn on send, retention or owner. Record-only timed gc. Hide stopped agents by default. | S–M | Findings 2 and 3, CAD-96/98/199 |
| 5 | Overview model: subject/cause keys, dedupe, `--project`/`--group`. Session gate scoped with acknowledgement plus expiry. Uptime fix. | M | Findings 10 and 11, CAD-82 |
| 6 | Trust: UI writes through daemon caller derivation. Review config read from the base head. `with_conn` plus `busy_timeout`. | M | Findings 5, 6 and 7 |
| 7 | Fail-closed bug batch: 218/175, 170/189, 149, 203. | S each | Charter principle 3 |
| 8 | UI data layer: loading vs failed, targeted refetch, one count source, project binding from dispatch/owner. | M | Findings 12, 13 and 14 |
| 9 | Mechanical splits: host.rs, main.rs, tests/integration.rs. Sleeps → deadline polls. | M, parallelisable | CAD-173/184 |
| 10 | CLI polish: one-line help, a workflows page, output/error convention, unknown project is an error, "did you mean". | S–M | Finding 16 |
| 11 | Unblock memory review: define native-identity proof for proposers, or an operator curation path. | S design | Finding 15, L5 |

Then prove CAD-225 end-to-end before reopening L4/L5 and new providers.

## 6. Notes

- Cadence memory `browser-agents-cannot-reach-loop` is stated too broadly.
  `AGENT_BROWSER_FORCE_LOCAL=1` reaches loopback; only the remote backend cannot.
- The overview drift line prints PR numbers twice (`(#119) (#119)`).
- UI build `58afcc0` is behind main by #118, #119 and #121.
- UI screenshots from this audit are in the session scratchpad and were not committed.

## 7. Execution log (2026-09-22)

**Code changes in this pass** (uncommitted on `main`):

| Change | Files | Evidence |
|---|---|---|
| `session start` daemon uptime read `started_at` (f64) with `as_i64()`, which returns None, so uptime counted from 1970 (`up 29834943m`). It now reads f64, and a missing value reports "uptime unknown". | `src/session.rs` | unit `session::tests::uptime_reads_fractional_started_at` |
| Overview drift printed each PR twice (`(#119) (#119)`): `pr` is parsed from the subject's own tail. The extra copy is removed in the CLI and the UI. | `src/main.rs`, `ui/src/components/Overview.tsx` | manual (no existing test covers the CLI print) |
| `issue ls --project <unknown>` exited 0 with "no issues — `issue new` creates one". It is now an error that lists the known keys, as SESSION.md already claimed. The session hint also pointed at `cadence project ls`, which doesn't exist; it now says `cadence issue project ls`. | `src/issue/cli.rs`, `src/session.rs` | board `issue_ls_unknown_project_is_an_error` |
| CAD-218: `agent_send` to a pty agent refuses a body with control characters at send, instead of answering `queued` and failing at delivery. There is now one shared predicate, `pty::has_control_chars`, for the adapter, send and dispatch. | `src/daemon.rs`, `src/adapter/pty/mod.rs`, `src/issue/dispatch.rs`, `docs/PROTOCOL.md` | integration `pty_send_rejects_control_chars` (contract updated deliberately; its no-fence assertions are kept) |

Checks were run directly, **not build-slot admitted**. `cadence build-slot` refuses a caller
outside a registered pane (CAD-230). Results:

- `cargo fmt --check`: pass
- `cargo clippy --all-targets --all-features -D warnings`: pass
- `vite build`: pass
- UI `tsc --noEmit`: pass
- UI `test:url-state`: pass
- the focused tests above: pass
- `cargo test --lib --bins --test board`: pass (426/26/91), re-run on exact commit `35ba5cb` after a concurrent `git pull` brought in #122

The full integration suite was not run locally; CI runs it.

**Not changed, after verification:** `--admin` in the merge command (finding 4) and the UI
need grouping (finding 9). See the corrections above.

**Tracker writes were blocked by the permission classifier** (external-system writes). Run
these yourself after review. Each one was checked against source and git:

```bash
A=audit-20260922
# shipped
cadence issue comment CAD-132 --author $A -m "Shipped in #74 (5c0c4c7): 60s WAL watch, PASSIVE then TRUNCATE above wal_max_bytes, deferred while the provider has a running turn. Residual: busy_providers defers devin indefinitely while aos-pm holds 91 never-completing running messages - see message-lifecycle ticket."
cadence issue comment CAD-156 --author $A -m "Fix merged in #88 (b79a4c6, Issue: CAD-156). Reopen with a sighting on a tree containing b79a4c6."
cadence issue set CAD-132 CAD-156 status=done
# CAD-218 fixed by this pass; CAD-175's no-fence half was already true (pre_write)
cadence issue comment CAD-175 --author $A -m "No-fence half already holds: pty rejection is pre_write, agent stays idle (pty_send_rejects_control_chars). Send-time refusal lands with the CAD-218 fix."
# duplicates
cadence issue link CAD-98 duplicate_of CAD-199
cadence issue link CAD-155 duplicate_of CAD-151
cadence issue link CAD-178 duplicate_of CAD-151
cadence issue comment CAD-151 --author $A -m "Single risk-label sweep for ADRs 0001-0003; folds CAD-155 (incl. its stale registry.rs:721-723 citation) and CAD-178."
cadence issue link CAD-97 relates CAD-200
cadence issue set CAD-98 CAD-155 CAD-178 status=dropped
# fail-open bugs up; speculative work down
cadence issue set CAD-170 CAD-189 CAD-203 CAD-149 priority=P1
cadence issue set CAD-231 CAD-187 CAD-236 priority=P2
# CAD-173 children
for i in 179 180 181 182 183 184 185 186; do cadence issue link CAD-$i parent CAD-173; done
```

After the CAD-218 fix merges, run `cadence issue set CAD-218 status=done`.

**New tickets to file** (`cadence issue new "<title>" --project cadence`, then
`issue acceptance`):

1. *Bound `running`: at most one running turn per pty actor, a delivered-awaiting-report
   state, and a timeout to `unknown` with one notice.*
   - Acceptance: aos-pm-style accumulation (91 running) is impossible in a test. The
     overview shows one stalled row per agent. WAL watch `busy_providers` no longer defers
     forever. Parent: CAD-91.
2. *Inbox with no consumer: warn at send/route, retention or owner requirement.*
   - Acceptance: sending to an inbox with more than N unread and no consumer warns. The
     `status` footer names the stale inboxes. Parent: CAD-226.
3. *Overview rows carry `subject` and `cause`, dedupe per subject, and `overview
   --project/--group`.*
   - Acceptance: `stalled` plus `silent_end` for one agent render as one row. The CLI and
     the UI share the grouping. Parent: CAD-82.
4. *Group needs-me rows by whether a live owner can act, not by kind* (for example, a
   fenced agent with no live PM goes to decision). Needs your product decision; relates to
   CAD-137.
5. *Board writes derive the caller like the daemon does* (refuse pane descendants, or route
   through the daemon socket). Security.
6. *`cadence review` loads `cadence-review.toml` from the base head and flags PRs that
   change it.* Security.
7. *Store `with_conn` recovers a poisoned lock and records an event, plus `busy_timeout`.*
8. *Session gate: scope to the cwd project, acknowledge a known backlog with an expiry,
   and name the process for each suggested `kill`.*
9. *UI data layer: loading vs failed, keep the last good data, per-resource SSE refetch with
   coalescing, one count source.*
10. *Mechanical splits: `doctor/host.rs` by probe with `redact` in its own module,
    `main.rs` into `cli/<verb>.rs`, and `tests/integration.rs` by area.* Parent: CAD-173.
11. *Unblock memory review: define native-identity proof for proposers, or an operator
    curation path* (all 11 cadence memories are stuck `proposed`).

**Left for you to decide:** reassigning tickets owned by root-pm, board-dev,
luna-watchdog, luna-queue-efficiency, claude-b (these may be external sessions),
cookie-cesium (fenced) and fable-cc (an inbox with 644 unread messages). Also whether CAD-173
stays P0 without a live owner.

**Tracker commands applied 2026-09-22** at your request. The tracker lints clean and is pushed to
`favcrm/pm` (0 ahead, 0 behind). One deviation: `parent CAD-173` was refused because
CAD-173 already sits under CAD-91 and the tracker allows only two levels, so CAD-179…186
now carry `relates CAD-173` instead. CAD-218 is still open until `35ba5cb` merges.

**New tickets filed 2026-09-22:** these are the 11 tickets above, in order, as CAD-250…CAD-260. P1: CAD-250, 254, 255. P3: CAD-259. The rest are P2. Each has a
component, a root parent (CAD-91, 82, 75 or 65), an evidence comment and an acceptance
checklist, and no owner. CAD-251 relates to CAD-226, CAD-253 to CAD-137, and CAD-259 to CAD-173,
because those three are already sub-issues.

## 8. Backlog execution pass (2026-09-22, second goal)

**Shipped as PRs** (each ticket's requirement was checked against current source first):

| PR | Tickets | Theme | Risk class |
|---|---|---|---|
| [#125](https://github.com/favcrm/cadence/pull/125) | CAD-218 (plus CAD-175's remaining half), CAD-170, CAD-189, CAD-166, plus the uptime, drift and unknown-project fixes | Fail closed: refuse bad input and config instead of dropping it | auto: nothing touches merge, credentials or schema, and the WAL change only ever writes *less* |
| [#126](https://github.com/favcrm/cadence/pull/126) | CAD-227 | Codex turn liveness is activity-based, the same model as Claude | auto: liveness only, no permission change |

Tickets are in `review` with PR refs. Close them when their PR merges.

**What checking the requirements found:**
- CAD-175's "fences the receiver" half was already fixed.
- CAD-166 only affects hand-recorded refs today; it becomes common once CAD-165 adoption ships.
- CAD-132's WAL watch can be starved by stuck `running` rows (CAD-250).
- No live or historical `project.yaml` uses an unknown key, so CAD-170 breaks nothing.

**Recommended next bundles.** One PR per theme *and* risk class. Human-class trust changes stay
out of auto-class bundles. Don't bundle into files that an in-flight lane owns.

1. **Trust boundary** (human class, operator review): CAD-255 (review config from the base head)
   and CAD-254 (board caller derivation), then CAD-149 once the operator decides the policy.
   CAD-254 and CAD-149 need the same "who is calling" primitive, so build it once. CAD-109
   (pre-publish secret scan) fits here too.
2. **Message lifecycle**: CAD-250 (bound `running`), then CAD-162 (store the token on the row,
   which CAD-243 Devin Cloud also needs).
   - **Wait for CAD-245 (a2a-dev, in flight in `daemon.rs`) to merge first.**
   - Coordinate with fable-cc's CAD-158 and CAD-212 (queue supersession), which touch the same
     code.
3. **Admission for external workers: CAD-230.** This session hit it directly: `cadence
   build-slot` refuses any caller outside a pane, so no outside agent can run an admitted
   gate. It is a prerequisite for CAD-129 and for honest evidence on the CAD-173 work.
4. **Acceptance gates** (CAD-159, then 160 and 157): about 95% of open tickets have an empty
   Acceptance section. Enforcing "refuse dispatch without acceptance" now would block almost
   everything. Ship it as **warn**, backfill ready and P1 tickets, then switch to refuse.
5. **Test wall-clock (CAD-173, P0)**: start with CAD-259 (split `tests/integration.rs`) and
   CAD-184 (sleeps → deadline polls). Either staff CAD-173 as P0 or downgrade it; its owner
   luna-watchdog is not a registered agent.

**Deprioritize or rescope:**
- CAD-203, 192, 194 and 208 (memory retrieval gates): no accepted memory exists (CAD-260),
  so these filters have nothing to act on yet. Unblock CAD-260 first.
- CAD-129 (test service): large, and it overlaps CAD-230 and CAD-173. Rescope after CAD-230.
- CAD-213 and CAD-114 are partly shipped; CAD-38 is about 80% done. Retitle or close them.
- Epics (CAD-82, CAD-91) carry P1, which inflates the P1 count; epics should roll up, not
  rank.
- CAD-231 and CAD-224 are strategy work, not engineering tickets.

**Cleanup ownership:** `.worktrees/fail-closed-bundle` and `.worktrees/codex-turn-idle` stay
until #125 and #126 merge, since review rounds happen there. After merge, run `git worktree
remove` on both and delete the branches.
