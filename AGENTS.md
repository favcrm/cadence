<!-- cadence:begin -->
## Cadence-managed agents

This repo may be worked on by cadence-managed agents. If
`CADENCE_ALIAS` is set in your environment: run `cadence self` for
your identity and running turn token, read your briefing at
`.cadence/<group>/BRIEFING-<alias>.md`, report with
`cadence message result <msg-id> --token <turn_id> --text ...`,
and discover peers with `cadence agent list`.
<!-- cadence:end -->

## Delivery workflow

Main moves every few minutes and many sessions work in parallel. Follow
this workflow exactly.

### Before you start, and before every push
- Run `git fetch origin`. Then run
  `gh pr list -R favcrm/cadence --state all --search "<ISSUE-ID>"` as a
  separate command and read its output. If another PR covers the issue,
  stop and report it. Search by topic as well, because duplicates often
  carry a different id.
- Work in the lane worktree that `cadence issue start <ID>` creates,
  never on main.
- One PR per feature (CAD-1099, CAD-1107), behind a flag if it must land
  incomplete, not a chain of small slices: each extra PR repeats CI, review,
  approval and queue. Size a PR by what it delivers (the ticket's acceptance
  end to end), never by a line cap. Sub-tickets of one feature ship in that
  one PR under the parent ticket's id in the title; the body lists them.
  Split only per repository, or to keep a small trigger 1/3 part on the
  two-review path so the rest needs one. Unfinished parts go into the open
  PR before review; REVISE fixes go into the same PR; notes on a PASS go to
  a follow-up PR (see "Notes never block"). Don't stack a PR on an unmerged
  branch: squash merges break the stack. Before asking for review,
  `git fetch origin && git rebase origin/main` so the reviewed head is
  current; after approval, rebase only if the PR conflicts.

### Design note in the PR (CAD-957, revised by CAD-1099)
When a change touches a daemon-enforced rule (gate, lease, lock, fence,
operator-only action, exactly-once) or a `docs/roles/risk-classes.md`
trigger 1-3, or trigger 7 when it changes a gate or who approves what, the PR
description carries a short design note: the approach, the rules it enforces
and the cases it refuses. There is no separate pre-code contract review
round; the code review checks the note and the code together. The operator
may still ask for a design to be agreed on the ticket before code.

### Review: what a merge needs (interim, CAD-815)
Until the delivery loop enforces a per-project policy (CAD-814), every
PR needs each of the following as a PASS on the exact head you enqueue:
- **One independent review by default (CAD-1099, CAD-1104).** A PR needs ONE
  independent review covering standards and spec, filed as
  `# Verdict: <ID> Review (standards+spec) — pass|revise`, when every
  changed path qualifies under `docs/roles/one-review-paths.toml`. That list
  covers code, docs, CI/workflows, scripts, rules, dependencies and rollout.
  It EXCLUDES only trust-boundary/identity and secrets (risk-classes
  triggers 1 and 3: the auth/identity modules, the identity-heavy source
  trees listed in that file, and UI, since the board runs with the
  operator's session). A PR touching an excluded path needs **Standards**
  and **Spec/security** reviews by two different independent reviewers.
  For a `human`-class PR (see below) the operator's approval is the second
  check, so one reviewer plus the operator is enough outside triggers 1
  and 3. No reviewer may be the author. Prefer a reviewer whose model
  vendor differs from the author's.
- **Notes never block (CAD-1104).** REVISE only for a correctness,
  security or ticket-requirement gap. Everything else (wording, docs,
  comments, style, extra tests) is a note: the reviewer PASSes with notes,
  and the author fixes them in a follow-up PR, not in the reviewed PR,
  because a new head voids every verdict. A gate or daemon-enforced rule
  change without its acceptance check ("Gates and security work") is a
  REVISE, not a note.
- **The path list is a floor.** A single reviewer who sees auth, identity,
  credential, signature, secret or confinement logic (triggers 1 and 3) in
  a one-review PR returns REVISE asking for a second (Spec/security)
  reviewer, and states the trigger in `Risk:`. When unsure, two. Gate,
  CI and rules logic stays one review: the operator's approval (enforced
  from `docs/roles/risk-paths.toml` by `scripts/enqueue-reviewed`) is its
  second check. The PR
  then follows the two-review path: the same reviewer refiles as the
  Standards review on the head, and a different reviewer files Spec/security. A single
  reviewer of a PR that changes or deletes a check states that no gate check
  and no isolation or fail-closed default was weakened, naming what was
  checked.
  - **Reviewer-count scaling (CAD-814):** for a two-review PR, the default is two distinct
    reviewers. When the project runs a solo-operator lane — the author is
    the only registered reviewer-capable identity on the project — a
    single independent registered-identity reviewer (not the author) may
    satisfy both axes only if the operator approves that head through the
    operator-connection-bound `cadence audit approve --pr <n> --head <full
    sha> --action merge` event on `audit:approvals` (the same channel every
    human-class approval uses). A ticket comment or note is not approval
    evidence. The operator approval substitutes for the second reviewer,
    not for independence: the single reviewer still must not be the author.
    Solo-operator scaling is declared per project in the `delivery:`
    policy, not per PR; it does not apply to any `human` trigger other
    than reviewer count, and it never lowers the Browser QA,
    qa-verdict-status, or risk-class gates.
  - **The list is mechanical (CAD-957/1099):** `docs/roles/one-review-paths.toml`
    (match rules in its header; exclude wins) is the only list, and
    `scripts/enqueue-reviewed` reads it at the PR's base. It needs no
    operator approval for the count. `human` triggers are unchanged: a
    one-review PR that hits a `human` trigger still needs the operator.
- **Browser QA** at desktop and narrow widths when the PR changes
  `ui/**`.
- **Operator approval** when any `human` trigger in
  `docs/roles/risk-classes.md` applies. That includes, but is not
  limited to, every change to `.github/**`, `scripts/**` (except the
  allowlist in `docs/roles/risk-classes.md`), `Cargo.toml`,
  `Cargo.lock`, `ui/package.json`, `cadence-review.toml`,
  `src/review.rs`, `docs/roles/**`, `docs/CHARTER.md`
  and this file. Most gates on a PR run from the PR's
  own workflow files, so review is the only control on a change to them.
  The operator decides. An agent the operator designates may prepare
  the decision and relay it, but never records it, and the PR's author
  does neither. Before enqueue, the operator records the decision from
  an operator connection (not an agent pane or endpoint):
  `cadence audit approve --pr <n> --head <full-sha> --source "<who decided, where>"`.
  A note or ticket comment is not approval evidence (`docs/AUDIT.md`).
  **Approve once per ticket (CAD-1106):** instead of a per-head approval the
  operator may run `cadence audit approve --issue <ID> --action scope` once;
  `scripts/enqueue-reviewed` then accepts it for every in-scope PR of that
  ticket when: the PR title names the ticket, its branch is an open lane
  branch recorded on it, the approval is operator-connection, unrevoked and
  its digest equals the sha256 of the ticket body as it reads now, every
  head-pinned verdict's `Risk:` line is `auto` or `human (<numbers>)` without
  trigger 1 or 3, and each counted verdict carries `Scope: in-scope <ID>`.
  The ticket body declares its triggers (`Risk: human (4, 7)`; missing or
  unparseable means no scope approval) and the approval covers only those:
  the triggers of the changed paths (risk-paths `[schema]`/`[trigger4]`/
  `[trigger6]`/`[trigger7]`) and of every verdict must be declared, and a
  human-class path in no trigger list needs the per-head approval. A diff
  touching a risk-paths trigger 1/3 path or a two-review path always needs
  the per-head approval, and so does a ticket not in ready, doing or review (backlog, done, dropped). A
  verdict with more than one `Risk:` line, or a ticket body with more than
  one line containing `Risk:` (Markdown markers stripped; fenced code and
  HTML comments count), gives no scope approval. Editing the ticket body voids the scope
  approval (the digest changes); `cadence audit revoke <id>` withdraws it.
- **Delegated approval** (CAD-918) when the reviewers class the PR
  `delegated` (`docs/roles/risk-classes.md`): the agent designated for
  the project runs, from its own pane,
  `cadence audit approve --pr <n> --head <full-sha> --delegated --source "<notes>"`.
  The daemon records it as `delegated:<alias>` only when every safeguard
  holds, including an allowlist of the paths a PR may touch
  (`docs/roles/risk-paths.toml`). Triggers 1, 3, 4, 6 and 7 are never delegated. Until the running daemon
  is built with CAD-918, delegated-class PRs still need the operator.

Record the evidence so that every merge can be audited:
- The PR title carries the issue id (`CAD-123: …`). A PR without a
  ticket does not merge.
- Each review files one verdict note in the notes dir (`notes_dir` in
  `pm.yaml`, default `/var/www/agent-notes`). Name it
  `YYYYMMDD-HHMMSS-<slug>-verdict.md` (UTC), and use this shape so
  `cadence audit` and the ticket view both read it:

  ```markdown
  # Verdict: CAD-123 Standards review — pass
  > Issue: CAD-123
  > From: <reviewer>

  ## Verdict
  pass — PR #456, head <full 40-hex sha>

  Risk: auto

  ## Gates
  - <each gate run and its result; what was read>

  ## Findings
  - <blocking / should-fix / nits>
  ```

  For a `human`-class PR, write `Risk: human (<trigger numbers>) — <reason>`;
  for a `delegated` one, `Risk: delegated (<triggers>)`.
  The `>` header lines must follow the title directly. A bare `Issue:`
  line does not link the note to the ticket. The ticket view takes the
  result from the title, and the audit takes it from the first line
  under `## Verdict`. The two must say the same thing: a note titled
  `— pass` whose section says `revise` shows as passed on the ticket.
  Link the note from the ticket.
- Before you enqueue, post one ticket comment for the head you enqueue.
  It lists every required verdict note, the green CI run and, for a
  `human`-class PR, the approval id.
- A new head voids every verdict on the old head.

Bot reviews (Devin Review, CodeRabbit and similar) are advisory:
- Before a PASS, the reviewer reads the bot's findings on that head. In
  the verdict, the reviewer either blocks on or refutes each severe bug
  and each critical security finding.
- Never make a bot's check a required check. Never let a bot push to a
  PR branch (auto-fix, or applying edits from a bot's chat).

### Merging: use the merge queue, never `--admin`
- A PR merges only after every review that the section above requires
  has passed, pinned to the same head SHA.
- To enqueue, run `scripts/enqueue-reviewed <n> --head <full-reviewed-sha>`
  (add `--dry-run` to check without enqueueing). It refuses unless the PR
  is open at that head with auto-merge off, every required check is
  present and green, the required verdict notes are pinned to the head, a
  ticket comment lists them and, for a `human` diff, an operator approval
  is recorded for the head. It never records an approval. The step it
  wraps is
  `gh pr merge <n> -R favcrm/cadence --auto --squash --match-head-commit <reviewed-sha>`.
  The queue re-tests the PR on the newest main plus every entry ahead of
  it, so you don't need to rebase first. If the head moved after the
  review, the enqueue is refused.
- If the queue ejects a PR (`failed_checks`), read the failed run. The
  cause is often a semantic clash with a PR that just landed, for example
  a new test that uses a name this PR reserves. Fix it on the branch,
  show a `git range-diff` against the reviewed head (only the rebase plus
  the fix), and enqueue it again.
- Auto-merge survives a force-push. `--match-head-commit` is checked only
  when auto-merge is enabled, so a later push goes into the queue
  unreviewed. Before sending a PR with auto-merge on back for changes (a
  rebase, a fix, another review round), run
  `gh pr merge <n> -R favcrm/cadence --disable-auto`. Re-enable it, pinned
  to the new head, only after that head passes review.
- Use `--admin` only in a declared emergency, never as routine.

### Behavior-first verification and delivery outcomes (CAD-1071)

Done means the ticket's expected result works, not a test count. A green
suite is regression evidence, not product acceptance.

- Read the acceptance items from the ticket and build to them. Don't derive
  the expected result from the code you just wrote.
- The evidence is required CI green plus the independent review of the code
  against the ticket. Do not add routine unit tests, snapshot or fixture
  tests, stress runs, sandbox demos or evidence ledgers unless the ticket asks
  for one. For a bug fix, show the failing case once, then fixed.
- Self-written tests confirm the author's own assumptions: a fake that mirrors
  the client proves nothing about the real server. When a change talks to
  another repo's API, check the request and response against that repo's
  current schema.
- Never claim a check passed that you did not run. Keep implemented, reviewed,
  merged, installed and operationally verified distinct; do not touch
  production to obtain acceptance evidence.

#### Legacy tests

Do not port, restore or rebuild deleted or legacy tests. Add a check only when
the ticket's outcome cannot be shown without one, or under "Gates and security
work" below. Removing an existing check that guards merged code needs the
operator's explicit decision, recorded on the ticket.

#### Emergency clean-slate transition

When the operator wants to retire the legacy suite without waiting for a full
replacement, an explicit operator-approved ticket may declare a time-bounded
transition and its merge gate. The declaration must name the deleted test scope,
the reduced checks allowed to merge, residual controls, duration and how the
normal gates return. An agent or PR may not grant that exception to itself.

- During the window, an approved PR may remove the named legacy tests and merge
  through only the approved reduced gate. The exception does not delete
  production effects, authorization or enforcement code; it changes which
  retained tests and required checks may be suspended for the named scope. A
  documentation-only/non-logic diff may use the declared reduced gate instead
  of the heavy compile/test pipeline where that gate has been explicitly
  approved and wired; this section by itself does not alter required CI.
- Changes merged under the transition must not produce release or production
  artifacts, and cleanup cannot depend on unverified state. A check that still
  protects a residual control remains mandatory; unknown or uncontrollable
  effects stay outside the window.
- Every reduced-gate PR still identifies the intended behavior and residual
  unprotected contracts, records executed checks and limitations, and receives
  the reviews/approval its own diff requires. A temporary gate cannot waive
  the risk classification itself.
- At expiry the declared restore criteria apply: the replacement behavior
  checks or the restored mandatory controls must be present before ordinary
  delivery resumes. The exception is evidence for a controlled transition, not
  proof that the removed coverage was safe or equivalent.

### Checks: trust exit codes, not filtered text
- A shell hook routes commands through `rtk`, which can print "clean" or
  nothing at all when a check really failed. It has hidden a
  `cargo fmt` failure and non-empty git diffs. Run checks as
  `rtk proxy <cmd>` and judge them by exit code:
  `rtk proxy cargo fmt --all -- --check`,
  `rtk proxy cargo clippy --all-targets -- -D warnings`,
  `rtk proxy gh pr checks <n>`, `rtk proxy git diff …`.
- Never skip, ignore or weaken a test or check to get green.

### Local builds and tests
- Before every push run `scripts/pre-push` (fmt, live doctor split check,
  clippy, UI typecheck and active script contracts for what you changed).
  Use `scripts/pre-push --tests` before requesting review of Rust changes:
  during CAD-1073 it always runs the safety floor, even when its source is
  unchanged. It is the regression floor, not proof of the ticket's outcome (see "Behavior-first verification").
  Integration split manifests are retired; do not run `split-map-sync`
  until a reviewed inventory restoration establishes its inputs.
- sccache is the host-wide rustc wrapper (set in `~/.cargo/config.toml`).
  Don't override `RUSTC_WRAPPER`. If a build fails in a strange way, rerun
  it once with `RUSTC_WRAPPER=` to rule sccache out.
- Many lanes share this host. Use `CARGO_BUILD_JOBS=4` and
  `--test-threads 2`, and run only filtered test groups; CI runs the full
  suite.
- Tests isolate HOME, XDG and TMPDIR under a short `/tmp/<lane>` root,
  because unix socket paths are limited to 107 bytes. They set
  `CADENCE_SUITE_LOCK` and pass `RUSTUP_HOME` and `CARGO_HOME` through so
  the toolchain resolves.
- If a test fails with a timeout or "daemon not reachable" while the host
  is loaded, rerun that test alone before treating the failure as real.

### Pi agents and models (CAD-559)
- Workers and reviewers run on `pi` with **Devin** (`devin/*`) or the
  configured **OpenCode Go subscription** (`opencode-go/*`) — never
  OpenRouter. Honor an explicitly requested provider and model; do not
  silently substitute another provider. If the requested model or its
  authentication is unavailable, stop and report it. This provider choice
  does not waive independent review, head-pinned verdicts, Browser QA,
  operator approval or production-safety rules.
- **OpenCode Go subscription:** the current Muse 1.3 id is
  `opencode-go/muse-spark-1.3-contributor`; for example:
  `cadence join <pm> pi --model opencode-go/muse-spark-1.3-contributor --effort xhigh`.
  Confirm the exact id with `pi --list-models opencode-go`, then verify it
  is permitted by the operator-owned `[pi].models` role allowlist and
  available in the provider configuration the worker launcher loads.
  Catalog visibility is not proof of authentication or a successful launch.
  Report launch failures without substituting another provider. Use the
  existing subscription; never read, print or copy its credentials into a
  prompt, note or command. Subscription limits and usage apply; do not
  describe OpenCode Go as free.
- **Cursor through Pi remains blocked (CAD-603):** `cursor/*` model ids
  may appear in Pi's catalog, but `src/pi_policy.rs::require_safe_transport`
  rejects their unsafe argv prompt transport (process-argument exposure
  and E2BIG). A subscription, model allowlist entry or this documentation
  does not override that refusal. Do not remove or bypass the guard to
  dispatch. Enabling Pi + Cursor requires a separately reviewed safe
  transport implementation and its independent refusal acceptance check.
  The native `cursor` adapter is a different route, not a silent substitute
  for a Pi request; use it only when the operator explicitly requests it.
- **Devin default when no provider is requested:** cost tiers per
  `devin models list` (the source of truth for Devin):
  `swe-2-{high,medium,max}` are **Free**; `deepseek-v4-1-flash-*` is
  low cost ($0.22/1M in, $0.66/1M out), not free. The default worker
  model is `devin/swe-2-high` with `--effort max` (the SWE-2 Max
  variant):
  `cadence join <pm> pi --model devin/swe-2-high --effort max`.
  `devin/deepseek-v4-1-flash-high` + `--effort max` is the low-cost
  alternative. Switch a live agent:
  `cadence agent set <alias> --next-launch model=<m> effort=max`
  then `agent stop` + `agent resume`.
- `openrouter/*` models are **paid** — never route a worker or reviewer
  through openrouter. (The production master's pinned
  `openrouter/z-ai/glm-5.3-flash` is the one accepted exception; the
  operator owns that pin in `pm.yaml`.)
- The devin provider comes from the pinned `pi-devin@<version>` package
  in `pm.yaml` `[pi].providers`, loaded with `-e` (CAD-559). A daemon
  built before CAD-559 cannot launch devin models: its worker argv is
  `--no-extensions` with no `-e`, so pi exits with
  `Model "devin/…" not found` and the agent fences with
  "Pi process disconnected". Check the running build before dispatching:
  `readlink ~/.local/bin/cadence`.
- Verify by hand as the operator:
  `pi --list-models -e ~/.pi/agent/npm/node_modules/pi-devin/extensions/index.ts | grep devin`
  and
  `pi -p "Reply with exactly: ok" --no-extensions -e <that path> --model devin/deepseek-v4-1-flash-high`.
- If deepseek answers `Your Windsurf version is out of date` while
  `devin/swe-2-high` works and the Devin CLI itself runs deepseek fine,
  the backend is gating the extension's `ide` field, not its version:
  `pi-devin` ≤ 0.2.1 sends `ide="devin-desktop"` for every request, and
  Cognition version-gates that ide for non-Local models. The local fix
  (applied on this host, 2026-09-26) patches
  `~/.pi/agent/npm/node_modules/pi-devin/src/stream.ts` so the chat
  metadata sends `ide="windsurf"` unless the model uid starts with
  `gpt-5-6-` (Devin Local-only models still need `devin-desktop`).
  A reinstall of the pinned package reverts it — re-apply or fix
  upstream.

### Production safety
- A production daemon runs on this host. Never touch
  `~/.local/state/cadence`, `~/pm` (apart from `cadence issue …`
  commands), port 3010, the installed `~/.local/bin/cadence`, or the
  tailnet.
- Never restart the daemon. The rollout has a single owner; see
  `cadence rollout status` and CAD-236.
- Boards and daemons you start use a temp state dir and a port in
  3110–3199.
- Never signal or kill a process you did not start.

### Explicitly delegated development staging

The production safety rules above protect production. The operator may explicitly
assign an agent to manage a named, isolated development or demo instance without
requiring a new permission request for every refresh. Record the delegation in
the issue before acting: state and PM directories, board port, any exact tailnet
mapping, process owner, permitted operations and rollback artifact.

- State and PM directories must be isolated from production; local ports stay
  in 3110–3199. A shared staging instance has one refresh owner at a time.
- Within that recorded scope, agents may build, update app bundles, and start,
  stop or refresh the staging board and daemon through supported commands.
  Operator-created staging processes require explicit ownership handoff first;
  permission to inspect staging alone does not authorize stopping them.
- Reuse the recorded tailnet mapping. Creating, removing or changing a staging
  mapping requires explicit delegation for that exact port and target; never
  alter unrelated mappings, the tailnet configuration or operator-user settings.
- Before a refresh, verify instance identity, process and port ownership,
  artifact hashes and current mappings. Retain data backups and a pinned rollback
  build; validate actual served build and application behavior afterward.
- This exception changes operational scope, not caller identity. Honor native
  authorization and rollout guards. If a supported command requires an operator
  connection, implement and independently review a bounded staging delegation
  path, or use an actual operator connection. Never impersonate an operator,
  clear identity variables, bypass a guard, or record operator approval as an agent.
- Production paths, port3010, the installed binary, live customer runtimes and
  production rollout remain outside this exception. Standard source reviews,
  tests and audit requirements still apply unless the operator explicitly grants
  a separate emergency delivery exception.

### Shared scratch space
- The session scratchpad is shared between agents. Write only inside your
  own `<scratchpad>/<lane>/` subdirectory, and delete only your own files,
  by name. Never run `rm <scratchpad>/*`.

### Gates and security work
- A rule the daemon or an HTTP route enforces (auth, operator-only,
  exactly-once, a gate), whether new or changed, or a change that deletes
  data, ships with ONE acceptance check proving the bad case is refused (for
  example an agent caller, a forged field or a replay). It is written from
  the ticket by someone other than the implementer (the reviewer or the
  ticket author), and the implementer may not edit or weaken it. The
  reviewer confirms it exercises the real guard. No other ceremony.
- A board or HTTP path must be at least as strict as the daemon RPC it
  relays, so run the same operator proof on the HTTP peer.
- Restrict actors with allowlists, not denylists.

### Reporting
- Every lane ends with a six-field reflection (expected, evidence, cause,
  correction, lesson, next) posted to the ticket.
- Copy SHAs from `git` or `gh` output; never type them by hand.
