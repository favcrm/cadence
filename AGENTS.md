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
  The evaluator reads the changed set and the path lists at the PR's anchor
  (main's live tip for a main-based PR, else the PR's base), so a PR can never
  classify itself under rules it edited.
- The local merge gate is `scripts/enqueue-reviewed <pr> --head <sha>`
  (`--dry-run` enqueues nothing). It cannot supply a missing hosted evaluator
  or approval store. If it is unavailable, report the refusal; do not infer
  a pass or authorize a bypass. Hosted delivery is tracked by CAD-1946.
- The gate must be main's. It compares its own bytes and the sibling
  `scripts/delivery-gate` with `origin/main:scripts/<name>` (local git, current
  directory, no network, CAD-2108). A real enqueue from a differing copy refuses
  and names the file; `--dry-run` warns and runs. A lane merely behind main passes
  while the gate files match. To run main's copy, write both files from
  `git show origin/main:scripts/<name>` into one scratch directory and run it from
  inside the repo (`git fetch origin main` first: a stale ref is not detected). A PR
  editing the gate is judged by main's copy, never its own. No `origin/main` here
  means no check; with `origin/main` present, a gate file missing or unreadable
  on either side refuses. It guards accidents, not an adversary.
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
  `consequential` and `strict` add one or two fresh-session reviews of
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
  - `consequential` — one fresh-session combined review and the full check
    floor; no operator approval.
  - `strict` — the floor the evaluator falls back to: any activation other
    than the default lean policy — a malformed section, a custom section
    that is not operator-approved or was removed after approval, or a
    policy that is not the default once its `solo_operator` and
    integration-branch fields are cleared — path lists that cannot be
    read, incomplete or unsafe diff evidence, a delete, rename, type or
    mode change, a path or change shape outside the known one-review
    include list, and the risk-trigger, human-class, one-review-excluded
    and oversize-diff conditions. It keeps today's legacy floor and never
    fewer: one combined review on the one-review list, else two (Standards
    and Spec/security), and the operator floor on human-class paths and
    over 1500 changed lines.
- **Review standard (operator decision 2026-11-03, CAD-1946).** A review is
  valid when a fresh-context session that is not the author does it; a
  subagent qualifies. Fresh means it inherits no conversation or context from
  the work: a forked subagent or a resumed authoring session does not count,
  and its brief points at the change (PR, exact head, ticket) without the
  author's conclusions. A second model vendor is optional. A separate human or
  account is not required. Records say "fresh-session review" and never claim
  independent parties. Standards and Spec/security stay two separate passes
  where two are required, each by its own fresh session. Owner approval of the
  exact head still applies on `human`-class paths; this decision changes no
  approval rule for send, publish, spend or deploy.
- **What the gate enforces today.** Not this standard directly.
  `scripts/enqueue-reviewed` counts hosted verdicts by attested principal and,
  since CAD-2049, refuses the author's principal; local notes count by their
  self-declared `From:` alias, which is not attested. It cannot tell a fresh
  session from the author when both act under one login. Two routes exist.
  Workflow-issued reviews (below) are the intended one; their workflows are in
  `.github/workflows/` (CAD-2088) but are not live until the `cadence-review`
  environment and its keys are set up and the first run files a verdict. Until
  then, merge through the temporary exception (`--exception --advisory
  standards=<url> --advisory spec-security=<url>`, plus `browser-qa=<url>` for a `ui/` change), which ends
  at `EXCEPTION_ENDS` in `scripts/delivery-gate` (2026-12-31). Of the advisory
  links, it checks only that they are separate https URLs; it proves
  neither head binding nor that Browser QA ran. A verdict filed under the
  author's identity still refuses under the exception, so a same-login
  session posts its findings as a PR comment and files no verdict. A second
  attested human is not required.
- **Post findings as a PR comment; file no verdict.** Do not file a hosted
  verdict (`cadence audit verdict`) unless you are a distinct attested principal
  from the PR author. A verdict filed under the author's own person, or under a
  login attested to that person, is permanent (no verb retracts it) and
  head-pinned: the gate refuses it, `--exception` included, and only moving the
  head clears it, which voids every other row. The exception is a
  workflow-issued row (`identity_source: workflow-oidc`), which carries no
  login or person; it is a first pass on gate, security, schema and release PRs,
  not a replacement for a real review, and `--exception --advisory` remains a
  route that merges today.
- **The changed set is anchored to main's live tip.** GitHub's PR base SHA can stay
  older than the real merge base after main is merged into the branch, so a PR
  that merges main and restores an old file diffs as docs-only against it.
  `scripts/enqueue-reviewed` therefore diffs a main-based PR from the merge base
  with main's live tip (the changed files, line count and every path class read
  from that diff) and refuses if the tip cannot be read. `risk-paths.toml` and
  `one-review-paths.toml` are read at that same tip, and `cadence delivery
  requirements` classifies from the same merge base and lists (CAD-2100; it
  refuses a file list the merge-base and head trees do not confirm, since GitHub's
  `changedFiles` counts from its own, possibly stale, base). The tip is resolved
  once per evaluation. A PR that edits the lists is judged by main's copy, not its
  own, and `docs/roles/` is human-class, so the operator approves it. The gate also
  refuses a `routine` or `consequential` class its own diff does not support (not on
  the one-review list, human-class by path or size, or for `routine` not plain
  markdown docs), because the classifier on a host can be an older build. The
  evidence comment's `Base:` stays the PR's recorded base commit; `Merge base
  judged:` and `Main tip:` record what was judged. A PR on another base keeps the
  PR's base.
- **Workflow-issued reviews (CAD-2081; workflows in CAD-2088, not live until set up).** The
  gate counts a hosted row whose `identity_source` is `workflow-oidc` only when
  all of these hold, each read at gate time: its principal is in
  `docs/roles/review-workflow-principals.toml` at the live tip of `main` (not the
  PR's base, which can lag; the tip never holds an unmerged PR, so a PR cannot add
  its own reviewer) and the row's kind is the one kind that principal may fill;
  the PR's current base is `main`; the row carries a `base_sha` (a presence and
  format check only, it proves nothing about the base); and the
  principal's workflow file at the row's `workflow_sha` is a regular file
  byte-identical to the same file at main's tip. A re-run keeps the original
  commit's workflow for up to 30 days, so the comparison keeps a since-fixed
  workflow from still filing a pass. It covers the workflow file itself, not the
  actions it calls. An unlisted principal, a wrong kind, an unreadable tip or
  allowlist, a retargeted PR or a non-pass row refuses and counts for nothing, and
  under `--exception` too. An owner-attested row that names a listed workflow
  principal also counts for nothing; the list loads whenever a hosted document has
  a row, and if it cannot be read only a workflow row or a `svc_*` owner row
  refuses.
  Such a row has no login, so the author-login checks do not apply. Standards plus
  Spec/security from the two workflow principals satisfy the two-review
  requirement. The `consequential` class (an active delivery profile and an
  ordinary one-review path) wants one combined review; two counted workflow
  reviews, Standards plus Spec/security from distinct principals, are stricter and
  satisfy it, so that class does not lose its route when the exception ends.
  What this proves: a fresh GitHub-hosted job ran workflow code from `main`,
  filed for a head live at filing. It does not prove good judgement. A workflow
  row may carry `risk: auto`, and a prompt-injected model can write it: a PR that
  fools both models skips the operator unless a human path or size trigger
    applies. The gate therefore inverts the rule: a PR with any workflow-issued
  row needs the operator's approval unless EVERY changed path matches
  `docs/roles/workflow-auto-paths.toml`, read at main's live tip, and every
  change is a plain add or modify (mode 100644). That list is EMPTY (`paths =
  []`) and the reader and rules stay, so today the operator approves every
  merge, and workflow rows only satisfy the two reviews. Enabling a path later
  is one reviewed line and a later, deliberate decision, made only after reading
  what the path holds. Whatever the list says, a path with a segment starting
  with `.` or a basename starting `agents.`, `claude.` or `gemini.` is never
  eligible. `cadence delivery requirements` does not report this rule; the
  script adds it. Rule-7 paths, the size trigger and a non-`auto` `Risk:` also
  force the operator. A workflow row that is not a pass refuses in every class.
  The tenant stops re-filing, not re-rolling: a run can be cancelled and
  re-dispatched, or an empty commit pushed, for a new head and a new verdict. A
  session holding admin can still rewrite the workflow or sign directly until
  agents lose admin. There is no Browser QA workflow: a `ui/` PR still needs a
  Browser QA verdict from a session, and refuses without it. `--exception` is
  otherwise unchanged, but a workflow row that does not hold refuses under it
  too.
- One fresh-session review by default, covering standards and spec, when
  every changed path qualifies under `docs/roles/one-review-paths.toml`. A
  path that does not (trust boundary, identity, secrets, `ui/**`) needs
  two reviews, Standards and Spec/security, as separate passes by two fresh
  sessions. No reviewer
  may be the author.
- A reviewer who sees auth, identity, credential, signature, secret or
  confinement logic in a one-review PR returns REVISE, names the trigger in
  `Risk:`, and asks for the second pass. When unsure, two.
- Notes never block. REVISE only for a correctness, security or
  ticket-requirement gap. Everything else — wording, docs, comments, style,
  extra tests — is a note: PASS with notes, and the author files one
  follow-up ticket listing them. Those fixes land in a follow-up PR, never
  in the reviewed PR, because a new head voids every verdict. A gate or
  daemon-enforced rule change without its acceptance check is a REVISE.
- A reviewer of a PR that changes or deletes a check states that no gate
  check and no isolation or fail-closed default was weakened, naming what
  was checked.
- A `human` trigger needs the operator as the second check. One fresh-session
  review plus the operator is enough only outside triggers 1 and 3; inside
  them two separate passes by two fresh sessions (Standards and Spec/security)
  are required and the
  operator's approval does not stand in for the second pass. The operator's
  approval of the exact head never lowers Browser QA, the `qa-verdict`
  status or the risk class itself. Browser QA at desktop and narrow widths
  when the PR changes `ui/**`.
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

### Testing principles

CI does not run tests. `ci.yml` proves the code compiles for the shipping
features and both targets, lints it, and proves the release seam on the merge
queue and main; the suite runs on a schedule and on demand (`tests.yml`), where
a red test finds its owner instead of blocking an unrelated push. Per-module
acceptance suites, integration targets and contract tests for the CI scripts
themselves are retired. Write a test only when it earns its place, and
then to these rules:

- **Direct functions, not processes.** Business logic is tested by calling it.
  If a test needs a daemon, a board, a socket or a spawned CLI to reach the
  logic, the logic is in the wrong place: move it into a function the test can
  call.
- **Event-driven, never a sleep.** Wait on the thing that must happen — a
  sender's channel, a barrier, a condition variable, a frame that must arrive —
  and keep a timeout only as a bounded failure safeguard that fails loudly. A
  fixed `sleep` that passes on a loaded machine proves the machine was fast,
  not that the code is correct.
- **No external services.** Local fixtures only, on loopback. A test that needs
  a provider, a registry or the network is not a test; it is an outage waiting
  to happen.
- **Test the behaviour, not the copy.** A test that restates the implementation
  agrees with itself. Assert the observable contract — the refusal, the status,
  the bytes the caller sees.
- **One real check beats ten unit tests of it.** A rule the daemon or a route
  enforces still ships its acceptance: one check that drives the real guard and
  shows the bad case refused, written by someone other than the implementer.
- **CI machinery is proved by running it.** Do not add unit or contract tests
  for `scripts/` or `.github/workflows/`. A workflow that cannot load, a
  required check that never reports, or a gate that passes on `cancelled` all
  surface on the next real run, and a test that re-implements the gate proves
  only the re-implementation. Prefer making the machinery cheaper — fewer
  steps, fewer jobs, less duplicated work — over covering it.

The failure worth guarding is a **gate that silently checks nothing**. If
deleting a gate's wiring would leave a green run, that is worth one check. The
cost of a wrong gate is not its own code; it is every green run after it stopped
looking.

### Paid platform calls: no spending limit

- A paid platform call (tool, model, media) runs and is charged as long as
  the account has sufficient balance; the platform's insufficient-balance
  refusal is the only money check. Do not add caps, per-call maximums or
  budgets, and do not refuse a settled charge above its quote. Where an API
  requires `max_charge_minor`, send the live quote as protocol only. Settled
  by the operator (CAD-1877); do not reopen it in plans or reviews.

## Working in this repo

### Hosted workspace

- Use the selected hosted workspace for shared issues and wiki; do not fall
  back to a local tracker when a hosted operation is unavailable. See
  `docs/guides/hosted-workspace.md` for supported paths and identity boundaries.
- `issue start <ID> --repo <checkout> --profile <ap_…> [--base <ref>]` starts an
  issue for the calling session: it claims only that issue, opens the lane from
  `<ref>`, and needs no local alias or daemon. The receipt names the request and
  the outcome separately — `base` and `profile` are what you asked for; `head`,
  `branch` and `profile_recorded` are what happened. A queued or waiting
  assignment has not created a lane and is not proof of execution.
- A lane minted by `issue start` carries the repository's pre-push hook. A
  hand-made worktree does not: its push runs no gate, so run
  `scripts/boat-build --receipt -- scripts/pre-push --base origin/main` yourself
  and say so. Do not report a gate result you did not run.
- A local connected agent has its own native daemon; hosted board access does
  not require that daemon. Do not connect a runner blindly when it might claim
  unrelated waiting work. Agent identity is established by the supported
  authenticated path, not by supplying an alias or borrowing a human login.
- Review evidence binds to the exact head and, on a hosted workspace, to the
  issue's project (CAD-2001): the merge gate counts distinct attested principals, refuses
  the author's own, and voids every verdict when the head moves. Read it back
  with `cadence audit verdicts --project <key>` and confirm a verdict counts
  before treating a review as landed; in-chat findings are not automatically
  gate evidence. The project is explicit — never derived from the repository or
  the cwd. A hosted verdict counts only when the workspace owner has attested
  its principal's GitHub login (`cadence audit identities`) and admitted that
  principal to the project (`cadence audit reviewers --project <key>`); the
  gate counts principals, excludes the author by the attested login, and
  refuses an unattested, contradictory or ambiguous note. The one exception is a
  workflow-issued row (CAD-2081, "Review" above), which has no login and counts
  only under its own rules — see
  `docs/guides/hosted-workspace.md` ("Reviewing software changes"; AOS-473,
  CAD-2001).
- When more than one Cadence build is on the host, `CADENCE_BIN` selects the one
  the gate invokes. A hosted-verb refusal can come from the client, the deployed
  service or an operation the tenant does not support: confirm the capability,
  report the exact refusal, and do not assume a stale client or bypass the gate.
  A tenant that predates the project-scoped review verbs is refused by name —
  the release order is the tenant server first, then this client — and that
  refusal is never a pass.
- Hosted project work and local software delivery are distinct. If the merge gate
  refuses, report its exact refusal; an exception remains operator-controlled and
  does not grant an agent merge or approval authority.

### Before you start, and before every push

- `git fetch origin`, then `gh pr list -R favcrm/cadence --state all
  --search "<ISSUE-ID>"` as a separate command and read it. If another PR
  covers the issue, stop and report it. Search by topic too, because
  duplicates often carry a different id.
- Work in the lane worktree `cadence issue start <ID>` creates, never on
  main. On a hosted org use `--repo <local-checkout>` and confirm that the
  assignment actually created the lane; a waiting assignment has not created
  one.
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

These commands describe the native approval store. A hosted org that cannot
reach it has not recorded an approval merely because an operator signed in.
Do not substitute a comment or another identity for the required proof.

- The operator records a `human` approval from an operator connection (not
  an agent pane or endpoint): `cadence audit approve --pr <n> --head
  <full-sha> --source "<who decided, where>"`. A note or ticket comment is
  not approval evidence (`docs/AUDIT.md`). On a hosted org that form is
  refused; the hosted form names the project and ticket: `cadence audit
  approve --project <key> --issue <ID> --pr <n> --head <full-sha> --source
  "<who>"` (see `docs/AUDIT.md`, CAD-2001).
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

The following is the native reviewed-enqueue path, not a claim that a selected
hosted workspace can run it. If the gate cannot run, stop and report the
limitation; only an authorized operator can decide an exception.

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
- Temporary path (CAD-1946, remove this note when it ends): while the hosted
  reviewer attestation cannot be produced, `scripts/enqueue-reviewed <pr>
  --head <sha> --exception --advisory standards=<url> --advisory
  spec-security=<url>` (plus `--advisory browser-qa=<url>` for a `ui/` change)
  waives only the missing reviewer attestation and, on a hosted routine PR, the
  readiness proof hosted cannot yet produce; the evidence names both. The advisory reviews are PR
  comments from fresh-session reviews (see the review standard above); the gate
  cannot prove freshness, and the operator accepts that and approves each
  exact head. The path ends at `EXCEPTION_ENDS` in `scripts/delivery-gate`
  (2026-12-31).

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
  required CI (fmt, clippy, build, ui, delivery-gate required; cross-build runs but is not yet required) is unchanged, and the
  suite runs in `tests.yml` on a schedule and on demand (CAD-1970). Review
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
  fresh-session review, head-pinned verdicts, Browser QA, operator approval or
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
