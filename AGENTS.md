<!-- cadence:begin -->
## Cadence-managed agents

This repo may be worked on by cadence-managed agents. If
`CADENCE_ALIAS` is set in your environment: run `cadence self` for
your identity and running turn token, read your briefing at the
path `cadence agent show` prints, report with
`cadence message result <msg-id> --token <turn_id> --text ...`,
and discover peers with `cadence agent list`.
<!-- cadence:end -->

## Where the rules live

This file holds the principles and the commands, not the policy. Where
prose and an authoritative source disagree, the source wins.

- **When a rule is enforced mechanically, name the enforcement, not the
  rule.** A copied rule is a rule that drifts.
- **What a PR needs is computed, not described.** The shared evaluator
  `src/issue/delivery_requirements.rs` turns the resolved delivery policy
  and the PR's diff into the requirements; read them with
  `cadence delivery requirements --issue <ID> --pr <url> --head <sha>`.
  The evaluator reads the path lists at the PR's base, so a PR can never
  classify itself under rules it edited.
- The merge gate is mechanical: `scripts/enqueue-reviewed <pr> --head <sha>`
  (`--dry-run` enqueues nothing) refuses with one reason per line for
  everything still missing. Trust it over any prose, this file included.
- Risk classes, triggers, who approves what: `docs/roles/risk-classes.md`.
- Path lists: `docs/roles/one-review-paths.toml` (review count),
  `docs/roles/risk-paths.toml` (triggers; the daemon compiles it in).
- The delivery policy: the default lean policy needs no activation, and an
  absent `delivery:` section is that default — unless a custom section was
  approved before, in which case that policy stays in force and the
  delivery counts as unapproved. A project declares its own in
  `<pm>/<key>/PROJECT.md` and the operator approves it with
  `cadence issue project approve-work <key>` (`docs/BOARD.md`). A policy
  that is not the default is not lean: a malformed section, a custom
  section that is not operator-approved, or a custom section removed after
  approval falls back to `strict`.
- Elsewhere: audit evidence `docs/AUDIT.md`, charter and non-goals
  `docs/CHARTER.md`, dev cycle and the clean-slate transition
  `docs/DEV-CYCLE.md`, Pi provider mechanics `docs/BOARD.md`, rollout order
  and rollback `docs/guides/rollout-checklist.md`.

## Principles

### Scope: essentials first

Deliver the ticket's acceptance items end to end with the simplest change
that meets them, then stop. Refactors, new abstractions, adjacent fixes and
nice-to-haves go into a follow-up ticket, not this PR. One ticket per
feature, its acceptance covering the feature end to end; split only for
another repository or an outcome the operator can use on its own, never a
ticket per step. When two approaches both meet the ticket, pick the smaller
one and say why in the PR. Before review, simplify your own diff: drop dead
code, duplication and abstractions the change does not need, in the lines
you changed only. If expansion is necessary to meet acceptance, explain why
and ask first; otherwise defer it. Stop once acceptance and required
verification are complete. Scope never waives a check, a review, an
approval or a production-safety rule.

### Evidence: behaviour first, not test counts

Done means the ticket's expected result works. Read the acceptance items
from the ticket and build to them; do not derive the expected result from
the code you just wrote.

- The evidence is what the computed class requires. `routine` needs the
  required base CI on the exact head and genuine ticket-outcome evidence
  — no mandatory review, and no fabricated PASS to stand in for one.
  `consequential` and `strict` add one or two independent reviews of
  the code against the ticket, as the class states. Do not add routine
  unit tests, snapshot or fixture tests, stress runs, sandbox demos or
  evidence ledgers unless the ticket asks for one. For a bug fix, show
  the failing case once, then fixed.
- A self-written test proves the author's own assumptions. When a change
  talks to another repo's API, check the request and response against that
  repo's current schema.
- Never claim a check passed that you did not run. Keep implemented,
  reviewed, merged, installed and operationally verified distinct, and do
  not touch production to obtain acceptance evidence.
- Do not port, restore or rebuild deleted or legacy tests. Add a check only
  when the ticket's outcome cannot be shown without one, or under "Gates
  and security work" below. Removing an existing check that guards merged
  code needs the operator's explicit decision, recorded on the ticket.
- Only an explicit operator-approved ticket opens an emergency clean-slate
  transition.

### Review: what a merge needs

- **Two axes, and they are independent.** The *delivery class* says what
  evidence a PR needs; the *risk class* says who signs it off. The class
  comes from the diff, so one repo can be `routine` on one PR and `strict`
  on another. The evaluator returns three classes:
  - `routine` — required base CI plus genuine ticket-outcome evidence;
    zero mandatory reviews; no fabricated PASS.
  - `consequential` — one independent combined review and the full check
    floor; no operator approval.
  - `strict` — the floor the evaluator falls back to: any activation other
    than the default lean policy — a malformed section, a custom section
    that is not operator-approved or was removed after approval, or a
    policy that is not the default once its solo-operator and
    integration-branch fields are cleared — path lists that cannot be
    read, incomplete or unsafe diff evidence, a delete, rename, type or
    mode change, a path or change shape outside the known one-review
    include list, and the risk-trigger, human-class, one-review-excluded
    and oversize-diff conditions. It keeps today's legacy floor and never
    fewer: one combined review on the one-review list, else two (Standards
    and Spec/security), and the operator floor on human-class paths and
    over 1500 changed lines.
- One independent review by default, covering standards and spec, when
  every changed path qualifies under `docs/roles/one-review-paths.toml`. A
  path that does not (trust boundary, identity, secrets, `ui/**`) needs
  two reviews — Standards and Spec/security — from different reviewers. No
  reviewer may be the author; prefer a different model vendor.
- A reviewer who sees auth, identity, credential, signature, secret or
  confinement logic in a one-review PR returns REVISE, names the trigger in
  `Risk:`, and asks for the second reviewer. When unsure, two.
- Notes never block. REVISE only for a correctness, security or
  ticket-requirement gap. Everything else — wording, docs, comments, style,
  extra tests — is a note: PASS with notes, and the author files one
  follow-up ticket listing them. Those fixes land in a follow-up PR, never
  in the reviewed PR, because a new head voids every verdict. A gate or
  daemon-enforced rule change without its acceptance check is a REVISE.
- A reviewer of a PR that changes or deletes a check states that no gate
  check and no isolation or fail-closed default was weakened, naming what
  was checked.
- A `human` trigger needs the operator as the second check. One reviewer
  plus the operator is enough only outside triggers 1 and 3; inside them
  two independent reviewers are required and the operator's approval does
  not stand in for the second reviewer. On a project whose `delivery:`
  policy declares a solo-operator lane, one independent registered-identity
  reviewer (never the author) may cover both axes when the operator
  approves that head. The operator's approval substitutes for the second
  reviewer, never for independence, and never lowers Browser QA, the
  `qa-verdict` status or the risk class itself. Browser QA at desktop and
  narrow widths when the PR changes `ui/**`.
- Bot reviews (Devin Review, CodeRabbit and similar) are advisory: before a
  PASS, the reviewer blocks on or refutes each severe bug and each critical
  security finding. Never make a bot's check a required check, and never
  let a bot push to a PR branch.
- Each review files one verdict note in `notes_dir` (`pm.yaml`, default
  `/var/www/agent-notes`), named `YYYYMMDD-HHMMSS-<slug>-verdict.md` (UTC):

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

  `Risk:` is `auto`, `human (<triggers>)` or `delegated (<triggers>)`. The
  `>` header lines follow the title directly; a bare `Issue:` line does not
  link the note to the ticket. The title and the section must say the same
  thing. Link the note from the ticket.
- Before enqueueing, post one ticket comment for that head: every required
  verdict note, the green CI run and, for a `human` PR, the approval id. A
  new head voids every verdict on the old head.

### Gates and security work

- A rule the daemon or an HTTP route enforces (auth, operator-only,
  exactly-once, a gate), whether new or changed, or a change that deletes
  data, ships with ONE acceptance check proving the bad case is refused (an
  agent caller, a forged field, a replay). It is written from the ticket by
  someone other than the implementer (the reviewer or the ticket author),
  and the implementer may not edit or weaken it. The reviewer confirms it
  exercises the real guard.
- A board or HTTP path must be at least as strict as the daemon RPC it
  relays, so run the same operator proof on the HTTP peer.
- Restrict actors with allowlists, not denylists.

### Paid platform calls: no spending limit

- A paid platform call (tool, model, media) runs and is charged as long as
  the account has sufficient balance; the platform's insufficient-balance
  refusal is the only money check. Do not add caps, per-call maximums or
  budgets, and do not refuse a settled charge above its quote. Where an API
  requires `max_charge_minor`, send the live quote as protocol only. Settled
  by the operator (CAD-1877); do not reopen it in plans or reviews.

## Working in this repo

### Before you start, and before every push

- `git fetch origin`, then `gh pr list -R favcrm/cadence --state all
  --search "<ISSUE-ID>"` as a separate command and read it. If another PR
  covers the issue, stop and report it. Search by topic too, because
  duplicates often carry a different id.
- Work in the lane worktree `cadence issue start <ID>` creates, never on
  main.
- One PR per feature, behind a flag if it must land incomplete, not a chain
  of small slices. Sub-tickets of one feature ship in that PR under the
  parent ticket's id; the body lists them. Don't stack a PR on an unmerged
  branch: squash merges break the stack. Before review, `git fetch origin &&
  git rebase origin/main`; after approval, rebase only if the PR conflicts.
- The PR title carries the issue id (`CAD-123: …`). A PR without a ticket
  does not merge.
- A change that touches a daemon-enforced rule (gate, lease, lock, fence,
  operator-only action, exactly-once) or a risk-class trigger 1-3 — or
  trigger 7 when it changes a gate or who approves what — carries a short
  design note in the PR description: the approach, the rules it enforces
  and the cases it refuses. The code review checks the note and the code
  together; there is no separate pre-code round.

### Approvals

- The operator records a `human` approval from an operator connection (not
  an agent pane or endpoint): `cadence audit approve --pr <n> --head
  <full-sha> --source "<who decided, where>"`. A note or ticket comment is
  not approval evidence (`docs/AUDIT.md`).
- Approve once per ticket: the operator may run `cadence audit approve
  --issue <ID> --action scope` once, and `scripts/enqueue-reviewed` then
  accepts it for every in-scope PR of that ticket. Traps: the ticket body
  must declare its triggers (`Risk: human (4, 7)`); editing the body voids
  the approval (the digest changes); a ticket outside ready, doing or review
  gets no scope approval; a diff touching a trigger 1/3 path always needs
  the per-head approval; and every counted verdict must carry
  `Scope: in-scope <ID>`. `cadence audit revoke <id>` withdraws it.
- A `delegated` PR is approved by the designated agent from its own pane:
  `cadence audit approve --pr <n> --head <full-sha> --delegated --source
  "<notes>"`. Triggers 1, 3, 4, 6 and 7 are never delegated, and the
  running daemon must support `--delegated` (`cadence audit approve
  --help`); where it does not, the operator approves.

### Merging

- Ask what is missing with `cadence delivery check --repo <owner/name> --pr <n> --head <full-sha>`. On a hosted org the daemon runs the same `scripts/enqueue-reviewed` decision and prints its refusal lines. It does not fall back to a local daemon. `cadence delivery enqueue` with the same flags signs that head and enables auto-merge pinned to it. The operator may enqueue when the check is clean. A registered agent may enqueue only when the check says an operator approval is not required. Agents never record an approval. The board's Approve change card is this check: Approve and merge is offered only when every other row holds.
- Enqueue with `scripts/enqueue-reviewed <n> --head <full-reviewed-sha>`. It
  refuses unless the PR is open at that head, every required check is
  present and green, the required verdicts are pinned to the head, a ticket
  comment lists them and, for a `human` diff, an operator approval is
  recorded. It never records an approval. It then signs the head with the
  operator's delivery-gate key (`~/.config/cadence/delivery-gate/`) and posts
  the signature on the PR; the queue's required `delivery-gate` check holds
  any PR without one, so a direct `gh pr merge` never lands (CAD-1925). The
  queue re-tests on the newest main, so you do not have to rebase first.
- If the queue ejects the PR (`failed_checks`), read the failed run: the
  cause is often a semantic clash with a PR that just landed. Fix it on the
  branch, show a `git range-diff` against the reviewed head, enqueue again.
- Auto-merge survives a force-push, so a later push would enter the queue
  unreviewed. Before sending a PR back for changes, run `gh pr merge <n> -R
  favcrm/cadence --disable-auto`; re-enable it, pinned to the new head, only
  after that head passes review.
- `--admin` only in a declared emergency, never as routine.

### Checks: trust exit codes, not filtered text

- A shell hook routes commands through `rtk`, which can print "clean" or
  nothing at all when a check really failed. Run checks as `rtk proxy <cmd>`
  and judge them by exit code: `rtk proxy cargo fmt --all -- --check`,
  `rtk proxy cargo clippy --all-targets -- -D warnings`, `rtk proxy gh pr
  checks <n>`, `rtk proxy git diff …`.
- Never skip, ignore or weaken a test or check to get green.

### Local builds and tests

- Before every push the worktree hook runs `scripts/pre-push` (fmt, the live
  doctor split check, clippy on both feature sets, the release seam, the UI
  typecheck and the active script contracts for what you changed) on Boat
  via `scripts/boat-build`; the shared host does not compile. One sandbox
  is kept for this worktree. The first command compiles cold. A later
  command within 30 minutes reuses it. After the last command it stays up
  for 30 minutes, then stops and is erased, and the next command starts
  cold. `scripts/boat-build --down` deletes it, `--fresh` replaces it, and
  `--once` deletes it when the command exits. A missing Boat CLI or helper refuses the push rather than falling
  back to a local build. `CADENCE_PRE_PUSH_LOCAL=1` is the explicit
  emergency path and runs the same plan on this host. `scripts/pre-push
  --tests` is a focused diagnostic, not a prerequisite before review;
  required CI and the merge-group regression floor are unchanged. Review
  and operator approval stay authoritative. Neither a remote run nor a
  green suite proves the ticket's outcome. The split
  manifests are retired; do not run `split-map-sync` until a reviewed
  inventory restoration establishes its inputs.
- Run `cargo check`, `cargo test`, `cargo clippy` and `cargo build` through
  `scripts/boat-build -- cargo …`. Example: `scripts/boat-build -- cargo
  test --locked --features test-seam --test daemon <FILTER> --
  --test-threads 1`. The lane `cargo` shim still queues a host build slot;
  it does not send the compile to Boat. Stay on this host only for a binary
  that has to run here (`cargo run`, a board smoke, a release artifact),
  and only when this run is explicitly local. That exception takes a build
  slot and `CARGO_BUILD_JOBS=4`.
- The copy is a shallow git snapshot of the candidate commit and the base,
  from their merge-base forward, plus a binary patch of tracked edits. It
  is not an rsync of the worktree and it does not pull from GitHub.
  Untracked files are refused; `git add` a new file before running. Ignored
  files, including `target/`, are not uploaded. A reused sandbox keeps
  `target/` and its cargo registry, so Cargo recompiles what the patch
  changed. On this repo the shallow snapshot of the current tips is about
  13MB; the full object pack is about 370MB and is not sent. The first
  command spends one Boat start and a cold compile. Later commands in that
  30 minutes spend no extra start. Start limits are the scarce quota
  (`boat limits`); `--down` when you are finished so the sandbox does not
  sit for the idle half hour.
- Prefer `cargo check` over `cargo build` for compile verification.
  `cargo check` stops at the type check. `cargo build` adds codegen and
  linking for this crate's five binary targets. On this host, same tree,
  warm sccache: 1m41s for check and 7m11s for build. A cold Boat compile is
  slower than those numbers. A warm session is the fast path.
  `scripts/pre-push` is check-level already, so a plain `cargo build`
  before every push buys nothing.
- Inside the sandbox the helper sets `CARGO_BUILD_JOBS=1`. Run filtered
  test groups only. CI runs the full suite. Copy one binary back with
  `--artifact <path-in-checkout> --output <new-local-file>` when a local
  board needs it. `--output` must not already exist. The run log is
  `<worktree-git-dir>/cadence-boat-build.log`.
- sccache is the host-wide rustc wrapper (`~/.cargo/config.toml`) for a
  local exception. The Boat sandbox does not use it. Do not override
  `RUSTC_WRAPPER` on the host; if a local build fails oddly, rerun it once
  with `RUSTC_WRAPPER=` to rule sccache out. Force `SCCACHE_DIR` (and
  `SCCACHE_CACHE_SIZE`) in that cargo config so a lane's rewritten
  `HOME`/`XDG_CACHE_HOME` cannot start the shared `:4226` server against a
  tmp cache. The durable dir is `~/.cache/sccache`.
- Tests isolate HOME, XDG and TMPDIR under a short `/tmp/<lane>` root (unix
  socket paths are limited to 107 bytes), set `CADENCE_SUITE_LOCK` and pass
  `RUSTUP_HOME` and `CARGO_HOME` through. If a test fails with a timeout or
  "daemon not reachable" while the host is loaded, rerun that test alone
  before treating the failure as real.

### Pi agents and models (CAD-559, CAD-855)

Cadence's setting is the source of truth; this file names no model.

- Read the vocabulary from the settings: `cadence settings agents show` is
  the authoritative document (`config.models`, `config.scope`,
  `config.roleDefaults`, `config.providers`, `config.pins`); `cadence
  settings agents model list` and `role list` show the same rows in list
  form. `pm.yaml`'s `[pi]` block is the retired legacy source that mirrors
  that authority. A launch uses the id and the scope the settings carry at
  that moment.
- Honor an explicitly requested provider and model; do not silently
  substitute another. If the requested model is not in `config.models`, or
  its scope does not cover the role that would run it, or its
  authentication is unavailable, stop and report it. This never waives
  independent review, head-pinned verdicts, Browser QA, operator approval or
  production-safety rules.
- Scope is the rule: a `master`-scoped model is master-only, a
  `worker`-scoped model serves workers and reviewers. Never route a worker
  or reviewer through a master-scoped model or a paid provider's metered
  route. The operator owns the master pin.
- No model named: use the role default the settings carry (`cadence settings
  agents role list`) and let the launch resolve it (`cadence join <pm> pi
  --team-role <role>`). Cost tiers come from the provider's own catalogue
  (`devin models list`) — read the tier, never quote it from memory. Switch
  a live agent with `cadence agent set <alias> --next-launch model=<m>
  effort=<e>`, then `agent stop` + `agent resume`.
- A subscription provider is used through its subscription, never a metered
  route. Catalog visibility is not proof of authentication or a successful
  launch; report launch failures without substituting another provider, and
  never read, print or copy its credentials into a prompt, note or command.
  Do not describe a subscription as free.
- An effort override documents preferences only within the applicable
  runtime allowlist: `high` means high, not max. Agents must not change
  runtime pins, credentials or installed packages to authorize their own
  launch. After an authorized launch, `cadence agent show <alias>` must show
  `model_effective` and `effort_effective` matching the request; configured
  values or catalog presence alone are not launch proof. A draft cannot
  authorize its own review.
- Cursor through Pi stays blocked (CAD-603): `src/pi_policy.rs::
  require_safe_transport` rejects the unsafe argv prompt transport, and no
  allowlist entry or doc overrides that refusal. Do not remove or bypass the
  guard. Cursor models live on the native cursor provider in Settings >
  Agents; that adapter is a different route, not a silent substitute for a
  Pi request.
- Provider mechanics — the pinned extension, the content digest, the local
  `ide` patch on a patched install — are in `docs/BOARD.md`. A launch that
  dies with `Model "devin/…" not found` or "Pi process disconnected" has
  several causes, one of them a running daemon older than the pin support:
  check `readlink ~/.local/bin/cadence` before blaming the model.

### Production safety

- A production daemon runs on this host. Never touch
  `~/.local/state/cadence`, `~/pm` (apart from `cadence issue …` commands),
  port 3010, the installed `~/.local/bin/cadence` or the tailnet. Never
  restart the daemon. Never signal or kill a process you did not start.
- A production deploy needs exactly these two lines to the operator, and
  nothing else:

  ```
  cadence update --check --as operator:<name>
  cadence update --as operator:<name>
  ```

  If `--check` shows `available` older than `current`, or a `blocked:` line
  naming an older build or unproven ancestry, report it and stop: the
  approved candidate is stale (CAD-1235). Use `--to <full main sha>` on
  both lines only when the operator asks to pin a commit. Never relay
  `rollout claim|backup|handoff|grant` or `upgrade` recipes; those are
  recovery internals (`docs/CLI.md`). If `--check` refuses, report the
  refusal text and stop.
- Boards and daemons you start use a temp state dir and a port in
  3110-3199.

### Explicitly delegated development staging

The operator may assign an agent to manage a named, isolated development
or demo instance, without a permission request per refresh. Record the
delegation in the issue before acting: state and PM directories, board
port, any tailnet mapping, process owner, permitted operations, rollback
artifact.

- State and PM directories are isolated from production; local ports stay
  in 3110-3199. A shared staging instance has one refresh owner at a time.
- Within that scope, agents may build, update app bundles and start, stop
  or refresh the staging board and daemon through supported commands.
  Operator-created staging processes need an explicit ownership handoff
  first; permission to inspect is not permission to stop.
- Reuse the recorded tailnet mapping; creating, removing or changing one
  needs an explicit delegation for that exact port and target. Never alter
  unrelated mappings, the tailnet configuration or operator-user settings.
- Before a refresh, verify instance identity, process and port ownership,
  artifact hashes and current mappings; retain data backups and a pinned
  rollback build, then validate the served build and behaviour afterwards.
- The exception changes operational scope, not caller identity. Honour
  native authorization and rollout guards; never impersonate an operator,
  clear identity variables, bypass a guard, or record operator approval as
  an agent. If a command needs an operator connection, use an actual
  operator connection or a separately reviewed bounded delegation path.
- Production paths, port 3010, the installed binary, live customer
  runtimes and production rollout stay outside this exception. Standard
  source reviews, tests and audit requirements still apply unless the
  operator grants a separate emergency delivery exception.

### Shared scratch space

The session scratchpad is shared. Write only inside your own
`<scratchpad>/<lane>/` subdirectory and delete only your own files, by
name. Never run `rm <scratchpad>/*`.

### The tracker write lock and an interrupted write
- The tracker has ONE writer lock: a kernel `flock` on
  `.git/cadence-write.flock` inside the PM dir. `.write.lock` is the legacy
  fence and `cadence-write.dirty.v2` is the snapshot of what was already
  dirty when a writer took it. The kernel lock is what proves liveness; the
  other two are evidence.
- A crashed writer leaves its staged work behind. The kernel lock is then
  FREE and the tracker still refuses every write that touches the leftovers.
  That refusal is a transaction fence, not contention: "the tracker has an
  interrupted write" is NOT fixed by waiting, by retrying the same command
  or by deleting a lock. The leftovers' outcome is uncertain, and only the
  tracker owner may settle it.
- A write whose paths are DISJOINT from the leftovers is admitted:
  `Pm::lock_for_paths` plus `Pm::commit_scoped` (`src/issue/pmlock.rs`,
  `src/issue/mod.rs`). A whole-tracker lock is disjoint from nothing, so a
  caller that writes one file must scope its lock to that file — take the
  scoped lock AND commit through the scoped wrapper. `src/issue/lock_tests.rs`
  has a guard test that fails the build when a function pairs a scoped lock
  with the unscoped commit; extend it when you add a caller.
- While a crashed writer's marker stands, a global Git hazard
  (`index.lock`, a merge/rebase/cherry-pick state) refuses EVERY write,
  scoped or not: a path-local recovery cannot settle Git-global state.
  WITHOUT that marker the hazard is not a refusal at all — the tracker
  admits the write and hands it to Git, and Git's own answer varies: an
  `index.lock` makes the commit fail, while a merge or a rebase in
  progress can accept the commit. Never rely on the tracker to refuse a
  write because of a Git-global state. A legacy or foreign `.write.lock`
  is not this build's crash to clear: never delete it as routine, and
  never treat a file's age as authority.
- The operator-facing recovery flow for an interrupted write is
  `cadence issue recover review|decide|execute|status` (CAD-1601, merged).
  Running it is a separate operator decision, not something an agent
  performs: `review` is read-only, and `decide`/`execute` require an
  operator connection. An agent may explain the evidence the review shows,
  never choose the outcome or perform that mutation.
- `~/pm` is written only through the `cadence issue …` commands. Never edit
  the PM dir, the marker, the snapshot or the index by hand, and never
  impersonate an operator to get around a refusal.

### Finish: clean up as soon as the PR merges
- Run `cadence issue reconcile --project <key>`. It marks the ticket done
  from the merge evidence. If it reports the tracker write lock, do NOT run
  it again until the PREREQUISITE changed — a refusal that names an
  interrupted write is a transaction fence, and a retry cannot settle it
  (see "The tracker write lock and an interrupted write"). Re-run it once
  the leftovers are resolved and the lock is free. If it holds the ticket
  (an open PR, a claim newer than the merge), say why on the ticket so the
  PM can close it.
- Leave the worktree to the sweep. GitHub deletes the merged branch, and a
  later `issue reconcile` (also run by `issue sync`) removes the worktree
  and local branch once no turn, shell or process stands in it. Never
  `--force` `issue finish`.
- Delete your own scratch files by name and stop the servers and processes
  you started. A PR you abandon or replace: close it with a link to what
  replaces it and say so on the ticket; do not leave a conflicted PR idle.

### Reporting

- Every lane ends with a six-field reflection (expected, evidence, cause,
  correction, lesson, next) posted to the ticket.
- Copy SHAs from `git` or `gh` output; never type them by hand.
