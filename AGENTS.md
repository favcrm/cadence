<!-- cadence:begin -->
## Cadence-managed agents

This repo may be worked on by cadence-managed agents. If
`CADENCE_ALIAS` is set in your environment: run `cadence self` for
your identity and running turn token, read your briefing at the
path `cadence agent show` prints, report with
`cadence message result <msg-id> --token <turn_id> --text ...`,
and discover peers with `cadence agent list`.
<!-- cadence:end -->

This file holds how we build Cadence: our identities, our review and delivery
route, our checks and our safety rules. What the product does for any team
(the lifecycle, the `delivery:` policy, evidence per review kind, refusal codes)
is in [docs/guides/delivery.md](docs/guides/delivery.md); this file links to it
and does not restate it. Where prose and an enforcing source disagree, the
source wins.

## Reviewer agents are read-only

The word "reviewer" names two different things here.

- **A reviewer agent the platform starts** is read-only. Natively it is a
  headless process the daemon starts; on a hosted org it is a board session. A
  reviewer pane is refused at register, build, open and adoption.
- **A fresh-session review** (see "Review") is a review by a session that is
  not the author. A subagent the author's session dispatches qualifies. It runs
  with whatever access its dispatcher gives it, so brief it read-only: it
  reports, and the dispatcher posts.

For a platform reviewer, the daemon pins `gh` credentials when it starts the
process, not by brief. That is not a claim about the whole environment: the
operator's SSH agent and the git credential helpers under `HOME` are not
covered, so a local reviewer can still `git push` as the operator. The
mechanisms, in the order they apply:

- `gh` runs with a private empty `GH_CONFIG_DIR` under the reviewer's state dir
  (`adapter::reviewer_gh_env`); `GH_TOKEN` is set only from the operator's
  `CADENCE_REVIEWER_GH_TOKEN`, which must carry read-only scopes
  (`contents:read`, `pull_requests:read`, `metadata:read`). `GITHUB_TOKEN`,
  `GH_ENTERPRISE_TOKEN` and `GITHUB_ENTERPRISE_TOKEN` are emptied. With no
  token, `gh` is unauthenticated and fails closed. Every local headless launch
  applies it.
- The hosted gate refuses every tracker-mutation verb from a live
  `role='reviewer'` agent credential (`checkCliCall`, code `reviewer_read_only`,
  HTTP 403). It keeps the `cli.read` verbs, the reviewer's own verdict
  (`issue_review`, `audit_verdict`, `audit_verdict_project`) and `artifact_put`.
- A reviewer reports back (its turn result, or its ticket tools in a hosted
  session). Whoever dispatched it posts the PR comment or ticket comment.
- Never run a reviewer under the operator's login or under a bridge grant that
  carries `cli.write`. A brief is a denylist; this section names the credential
  and the gate instead.
- The pin is an environment, not an identity boundary: a same-user worker can
  override it or read the operator's saved login, and only a separate user is a
  credential boundary (`docs/REMOTE-AUTH.md`).

## Where the rules live

- **When a rule is enforced mechanically, name the enforcement, not the
  rule.** A copied rule is a rule that drifts.
- **What a PR needs is computed, not described.** The shared evaluator
  (`crates/cadence-core/src/delivery_requirements.rs`) turns the resolved
  delivery policy and the PR's diff into the requirements; read them with
  `cadence delivery requirements --issue <ID> --pr <url> --head <sha>`.
- **The path lists come from the repository, at the anchor.** The evaluator reads
  the changed set and `docs/roles/risk-paths.toml` and
  `docs/roles/one-review-paths.toml` at the PR's anchor (main's live tip for a
  main-based PR, else the PR's base), so a PR can never classify itself under
  rules it edited. Missing, unreadable or malformed lists mean the strict floor:
  two reviews and the operator's approval, no Browser QA. On a hosted org that
  holds for a repository with no lists too. The engine's no-lists product default
  (`classify_without_lists`) is applied by neither the hosted gate nor the
  native evaluator. The one exception is
  the daemon's `--delegated` approval check (`docs/AUDIT.md`), which uses the
  copy compiled into the binary; a test pins that copy to
  `docs/roles/risk-classes.md`.
- Risk classes, triggers, who approves what: `docs/roles/risk-classes.md`.
- The delivery policy: the default lean policy needs no activation, and an
  absent `delivery:` section is that default, unless a custom section was
  approved before; then that policy stays in force and the delivery counts as
  unapproved. A project declares its own in `<pm>/<key>/PROJECT.md` and the
  operator approves it with `cadence issue project approve-work <key>`
  (`docs/BOARD.md`). A malformed section, a custom section that is not
  operator-approved, or one removed after approval falls back to `strict`.
- Elsewhere: audit evidence `docs/AUDIT.md`, charter and non-goals
  `docs/CHARTER.md`, dev cycle `docs/DEV-CYCLE.md`, CI and promotion
  `docs/CI-DELIVERY.md`, Pi provider mechanics `docs/BOARD.md`, rollout order and
  rollback `docs/guides/rollout-checklist.md`.

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
and say so in the PR; otherwise defer it. Stop once acceptance and required
verification are complete. Scope never waives a check, a review, an
approval or a production-safety rule.

### Evidence: behaviour first, not test counts

Done means the ticket's expected result works. Read the acceptance items
from the ticket and build to them; do not derive the expected result from
the code you just wrote.

- The evidence is what the computed class requires (see "Review"). Do not add
  routine unit tests, snapshot or fixture tests, stress runs, sandbox demos or
  evidence ledgers unless the ticket asks for one. For a bug fix, show the
  failing case once, then fixed.
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
  transition (`docs/DEV-CYCLE.md`).

### Review: what a merge needs

This is the one description of our review and delivery route. The product
reference for the gate's mechanics is [docs/guides/delivery.md](docs/guides/delivery.md).

**Two axes, and they are independent.** The *delivery class* says what evidence
a PR needs; the *risk class* says who signs it off. The delivery class comes
from the diff, so one repo can be `routine` on one PR and `strict` on another.
The evaluator returns three classes:

- `routine`: every change is a plain add or modify of ordinary Markdown on the
  `routine` list. The evaluator asks for required base CI and genuine
  ticket-outcome evidence and counts zero reviews.
- `consequential`: known ordinary paths. One fresh-session combined review and
  the full check floor; no operator floor.
- `strict`: everything else. A malformed, unapproved or non-default policy;
  unreadable lists; incomplete diff evidence; a delete, rename, type or mode
  change; a path outside the one-review include list; any trigger-table,
  operator-table or one-review-excluded path; and a diff over 1500 changed
  lines. It asks for one combined review on the one-review list, else two
  (Standards and Spec/security) as separate passes, and the operator floor on
  paths in the `operator` table or trigger tables 2, 4, 6 and 7, and over 1500
  lines. Triggers 1 and 3 (trust boundary, security) reach the operator through
  the reviewer: a reviewer who sees one names it in `Risk: human (<n>)`, and any
  `Risk` that is not `auto` forces the operator. Their path lists alone force
  only the strict class and the two-review count.

Those are the evaluator's counts. On a hosted org the gate counts the review
axes itself (below), so a hosted `routine` PR still wants the two workflow
reviews and the operator's approval unless the PR is `class: auto`.

**Review standard.** A source review is independent when a fresh-context
session other than the author does it; a read-only subagent qualifies. A
resumed authoring session or a fork that inherits its conversation does not.
Brief the reviewer with the PR, exact head and ticket, not the author's
conclusions. A separate human, account or model vendor is not required for
source review. Record "fresh-session review," not "independent parties."
For hosted Standards and Spec/security, only eligible owner-attested verdicts
or trusted workflow results count. A fresh OMP review is evidence, not a
hosted verdict. See [Review evidence](docs/guides/delivery.md#review-evidence).
If two source reviews are required, use separate fresh, read-only sessions for
Standards and Spec/security. Use the repository's approved reviewer routing.
Fresh, read-only OMP subagents qualify, including for gate, security, schema
and release PRs. Record the full PR head and actual resolved model in each
source report. Hosted workflow verdicts do not replace required source reviews.
Review changes to this policy under the rules already on protected `main`.

**The hosted route** is this repository's supported delivery route.

1. **Author.** Work in the lane, run the checks, open the PR as a draft and
   iterate there. A non-draft same-repository PR on `main` starts two review
   workflows on opening, on leaving draft and on each push
   (`.github/workflows/review-dispatch.yml`): `review-standards.yml` and
   `review-spec.yml`. Each push to a ready PR buys two reviews.
2. **Workflow reviews.** The gate counts a workflow row only for a principal in
   `docs/roles/review-workflow-principals.toml` at main's live tip, filling the
   one kind it may fill, on a PR whose base is `main`. A row may say
   `Risk: auto`, and a prompt-injected model can write that, so a PR with any
   workflow row needs the operator's approval unless every changed path matches
   `docs/roles/workflow-auto-paths.toml` (Markdown under `docs/**` only, read at
   main's live tip) as a plain add or modify, is on no human-class path, has both
   reviews passing and fits the size limit. The gate's dry run reports that as
   `class: auto`; being on the list is not enough. In practice every hosted PR
   needs both workflow passes: a non-routine PR needs standards and
   spec-security from distinct principals (a combined review fills both; one
   qualifying one-review PR needs one), and a routine PR needs both too. Field
   shapes are in delivery.md; what a row proves is below.
3. **Browser QA for `ui/**`.** A fresh QA session at desktop and narrow widths
   reports to the dispatcher. The dispatcher posts the full report, then, only on
   a pass, a separate unedited PR comment of exactly `Browser QA: pass` and
   `Head: <full sha>` on two lines. The PR author's own pass counts once the
   operator reads the report and records a project-scoped per-head approval of
   that head. A scope, delegated or repo-scoped approval, an edited pass or a
   pass for another head does not satisfy this. A counted Browser QA verdict, or
   an unedited exact-head pass from a non-author repository member, also
   satisfies the gate. Fallback: where the approval route is not available, the
   operator posts the non-author pass personally as `favcrm-dev` after reading the
   report. An agent never switches credentials or impersonates the operator.
4. **Check before requesting approval.** Resolve the owning ticket and current
   full PR head. Run `cadence --org fav-limited delivery check --project cadence
   --issue <ID> --repo favcrm/cadence --pr <n> --head <full-sha>` (or the
   read-only `scripts/enqueue-reviewed <n> --head <full-sha> --org fav-limited
   --dry-run`). Read `audit verdicts --body` with the same selectors on REVISE.
   Clear missing CI, review and Browser QA evidence before requesting approval.
   When a matching author-posted Browser QA pass waits on the operator's
   approval, link the full report for that decision. Leave the issue in Review
   and post one handoff comment with the full head, evidence links, exact
   remaining refusal or approval ID, and the operator command(s). A new head
   needs new evidence; do not reuse approval or request it for a stale head.
5. **Operator: CLI first.** Supply a paste-ready command block with literal
   issue, PR, full head and a truthful approval source (who decided and where).
   The operator reviews the evidence, then runs the commands from their own
   authorized terminal:

   ```bash
   cadence --org fav-limited audit approve --project cadence --issue "$issue" \
     --repo favcrm/cadence --pr "$pr" --head "$head" --source "$source" &&
   cadence --org fav-limited delivery enqueue --project cadence --issue "$issue" \
     --repo favcrm/cadence --pr "$pr" --head "$head"
   ```

   If approval is already in force or not required, give only the enqueue
   command. Check the actual GitHub merge-queue entry; a null
   `autoMergeRequest` does not mean the PR is unqueued. If queued, observe
   instead of enqueuing again. A supported
   `delivery enqueue --approve "<source>"` can combine approval and enqueue.
   Offer the board's **Approve & merge** if the operator prefers it or the
   CLI route is unavailable. Email OTP authenticates the operator; it does not
   clear a review or other refusal. A workflow REVISE the operator judges
   wrong is settled with `cadence --org fav-limited audit accept-revise` and
   a stated reason. A direct `gh pr merge` is not this repository's route.
6. **Agent follows through.** Agents never approve or enqueue. After operator
   enqueue, watch the queue and verify the merge on `main` before following
   [Finish](#finish-one-procedure-as-soon-as-the-pr-merges). A queue receipt
   is not a merge. If enqueue's outcome is uncertain, check the queue and PR
   state before anyone retries.

**What a workflow row proves.** The gate also requires the workflow file at the
row's `workflow_sha` to be byte-identical to main's tip; a re-run keeps the
original commit's workflow for up to 30 days, and the check covers the workflow
file, not the actions it calls. A row proves a fresh GitHub-hosted job ran main's
workflow code for a live head, not good judgement. The tenant stops re-filing,
not re-rolling: cancel and re-dispatch, or push a commit, for a new verdict. An
owner-attested row naming a listed workflow principal counts for nothing, and a
non-pass workflow row refuses in every class. There is no Browser QA workflow.
Agent sessions act as `cc-syntax`, which holds repository admin and can bypass
the merge-queue ruleset; an admin session can change the workflows, rulesets and
branch protection the gate relies on, so treat those as trigger-7 changes.

**Native only.** `scripts/enqueue-reviewed <n> --head <sha> --dry-run`
evaluates the native path and reports the class and refusals. A native enqueue
cannot land here: `scripts/delivery-gate sign` refuses local signing and the
queue refuses unsigned heads. The temporary `--exception --advisory` evaluator
is native-only, is refused on a hosted org, and ends at `EXCEPTION_ENDS` in
`scripts/delivery-gate` (2026-12-31); do not offer it as a hosted route.

**Findings.** Notes never block. REVISE only for a correctness, security or
ticket-requirement gap. Keep non-blocking notes on the review as PASS with
notes. Batch independently useful follow-up work; don't open a ticket just to
transcribe notes or push a new head for them. A gate or daemon-enforced rule change
without its acceptance check is a REVISE. A reviewer who sees auth, identity,
credential, signature, secret or confinement logic in a one-review PR returns
REVISE, names the trigger in `Risk:` and asks for the second pass. When unsure,
two. A reviewer of a PR that changes or deletes a check states that no gate
check and no isolation or fail-closed default was weakened, naming what was
checked. A workflow review that says REVISE: fix the finding, push, and let the
new head be reviewed.

**Bot reviews** (Devin Review, CodeRabbit and similar) are advisory: before a
PASS, the reviewer blocks on or refutes each severe bug and each critical
security finding. Never make a bot's check a required check, and never let a bot
push to a PR branch.

**Who files what.** Agents file no hosted verdict in this workspace. A reviewer
reports its findings back and the dispatcher posts them as PR comments, one per
review axis, linked from a ticket comment; that report is not itself a
gate-counted verdict. `favcrm-dev` and `cc-syntax` are one person, and the gate
refuses an owner-attested row while the author's login has no attested
principal. A filed row has no retraction verb and is head-pinned; moving the
head voids every other row. Changing reviewer identities is the operator's
decision alone.

**Native verdict notes.** On a native tracker each review files one note in
`notes_dir` (`pm.yaml`), named `YYYYMMDD-HHMMSS-<slug>-verdict.md` (UTC):

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

`Risk:` is `auto` or `human (<triggers>)`. The `>` header lines follow the title
directly; a bare `Issue:` line does not link the note to the ticket. The title
and the section must say the same thing. Link the note from the ticket.

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
- Memory writes: `src/memory/policy.rs::qualify` enforces nature, provenance and
  approval; mechanical promotion stays blocked until native check evidence exists.
- A change that touches a daemon-enforced rule (gate, lease, lock, fence,
  operator-only action, exactly-once) or a risk-class trigger 1-3, or trigger 7
  when it changes a gate or who approves what, carries a short design note in
  the PR description: the approach, the rules it enforces and the cases it
  refuses. The code review checks the note and the code together; there is no
  separate pre-code round.

### Testing principles

`ci.yml` proves the code compiles for the shipping features and both targets,
lints it, and proves the release seam on the merge queue and main. It runs one
test, the board-assertion fixture test in the `build` job (`core-wasm.yml`
separately runs `cargo test -p cadence-core`). The suite runs on a schedule and
on demand (`tests.yml`), where a red test finds its owner instead of blocking an
unrelated push. `changed-tests.yml` runs, on a PR, only the daemon and suite
tests that PR adds or changes (`src/daemon/tests/**`, `tests/**`) under
`--features test-seam`; selection that cannot map an edit widens to one module
tree, the `suite` binary or both, never to the full suite
(`scripts/changed-test-filter` header has the rules). Write a test only when it
earns its place, and then to these rules:

- **Direct functions, not processes.** Business logic is tested by calling it.
  If a test needs a daemon, a board, a socket or a spawned CLI to reach the
  logic, the logic is in the wrong place: move it into a function the test can
  call.
- **Event-driven, never a sleep.** Wait on the thing that must happen (a
  sender's channel, a barrier, a condition variable, a frame that must arrive)
  and keep a timeout only as a bounded failure safeguard that fails loudly.
- **No external services.** Local fixtures only, on loopback.
- **Test the behaviour, not the copy.** Assert the observable contract: the
  refusal, the status, the bytes the caller sees.
- **One real check beats ten unit tests of it.** A rule the daemon or a route
  enforces ships its acceptance: one check that drives the real guard and shows
  the bad case refused, written by someone other than the implementer.
- **CI machinery is proved by running it.** Do not add unit or contract tests
  for `scripts/` or `.github/workflows/`. A test that re-implements the gate
  proves only the re-implementation. Prefer making the machinery cheaper over
  covering it.

The failure worth guarding is a **gate that silently checks nothing**. If
deleting a gate's wiring would leave a green run, that is worth one check.

### Paid platform calls: no spending limit

A paid platform call (tool, model, media) runs and is charged as long as the
account has sufficient balance; the platform's insufficient-balance refusal is
the only money check. Do not add caps, per-call maximums or budgets, and do not
refuse a settled charge above its quote. Where an API requires
`max_charge_minor`, send the live quote as protocol only. Settled by the
operator; do not reopen it in plans or reviews.

## Working in this repo

### Hosted workspace

- Use the selected hosted workspace for shared issues and wiki; do not fall back
  to a local tracker when a hosted operation is unavailable. See
  `docs/guides/hosted-workspace.md` for supported paths and identity boundaries.
- `issue start <ID> --repo <checkout> --profile <ap_…> [--base <ref>]` starts an
  issue for the calling session: it claims only that issue, opens the lane from
  `<ref>`, and needs no local alias or daemon. The receipt names the request and
  the outcome separately: `base` and `profile` are what you asked for; `head`,
  `branch` and `profile_recorded` are what happened. A queued or waiting
  assignment has not created a lane and is not proof of execution.
- A lane minted by `issue start` carries the repository's pre-push hook. A
  hand-made worktree does not: its push runs no gate, so run
  `scripts/boat-build --receipt -- scripts/pre-push --base origin/main` yourself
  and say so. Do not report a gate result you did not run.
- First login on a fresh machine: `docs/REMOTE-AUTH.md`, "Hosted login and remote
  org transport".
- A local connected agent has its own native daemon; hosted board access does
  not require that daemon. Do not connect a runner blindly when it might claim
  unrelated waiting work. Agent identity is established by the supported
  authenticated path, not by supplying an alias or borrowing a human login.
- Review evidence binds to the exact head and to the issue's project: the gate
  counts distinct attested principals, refuses the author's own, and voids every
  verdict when the head moves. Read it back with
  `cadence audit verdicts --project <key>`; do not file a verdict to find out.
  In-chat findings are not gate evidence. The project is explicit, never derived
  from the repository or the cwd. A hosted verdict counts only when the workspace
  owner has attested its principal's GitHub login (`cadence audit identities`)
  and admitted that principal to the project
  (`cadence audit reviewers --project <key>`). Workflow rows have no login and
  count under their own rules (`docs/guides/delivery.md`).
- When more than one Cadence build is on the host, `CADENCE_BIN` selects the one
  the gate invokes. A hosted-verb refusal can come from the client, the deployed
  service or an operation the tenant does not support: confirm the capability,
  report the exact refusal, and do not assume a stale client or bypass the gate.
  A tenant that predates a verb is refused by name (the release order is the
  tenant server first, then this client), and that refusal is never a pass.
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
  assignment actually created the lane.
- One PR per feature, behind a flag if it must land incomplete, not a chain
  of small slices. Sub-tickets of one feature ship in that PR under the
  parent ticket's id; the body lists them. Don't stack a PR on an unmerged
  branch: squash merges break the stack. Before review, `git fetch origin &&
  git rebase origin/main`; after approval, rebase only if the PR conflicts.
- The PR title carries the issue id (`CAD-123: …`). A PR without a ticket
  does not merge.

### Approvals

The operator records approvals from an operator connection (not an agent pane or
endpoint). A note or ticket comment is never approval evidence
(`docs/AUDIT.md`). On a hosted org, where the approval is recorded and how the
operator merges is under "Review" above; a hosted org that cannot reach the
store has not recorded an approval merely because an operator signed in. Do not
substitute a comment or another identity for the required proof.

- Native form: `cadence audit approve --pr <n> --head <full-sha> --source "<who
  decided, where>"`. A hosted org refuses that form; the hosted form names the
  project and ticket (`docs/AUDIT.md`).
- Approve once per ticket: the operator may run
  `cadence audit approve --issue <ID> --action scope` once, and the gate then
  accepts it for every in-scope PR of that ticket. Traps: the ticket body must
  declare its triggers (`Risk: human (4, 7)`); editing the body voids the
  approval (the digest changes); a ticket outside ready, doing or review gets no
  scope approval; a scope approval never covers a trigger 1 or 3 path, a
  one-review-excluded path or an operator-only path, so when the operator's
  approval is needed there it is the per-head one; and every counted verdict must carry `Scope: in-scope <ID>`.
  `cadence audit revoke <id>` withdraws it.
- `POST /api/approvals/:id/revoke` ("Stop merge") and
  `POST /api/approvals/send-back` ("Send back…") withdraw or return queued work.

### Merging

Use the [hosted CLI route](#review-what-a-merge-needs) when it is reachable,
even if the Board UI is down. If the CLI is unavailable, the operator can
use the Board route if it works. If neither authorized route works, report
the exact limitation. A gate refusal is not a route outage: address the
refusal instead of changing identities or bypassing the gate.

- The queue re-tests on the newest main, so you do not have to rebase first.
- If the queue ejects the PR (`failed_checks`), read the failed run: the cause is
  often a semantic clash with a PR that just landed. Fix it on the branch, show a
  `git range-diff` against the reviewed head, and hand it back for a new
  enqueue.
- Auto-merge survives a force-push, so a later push would enter the queue
  unreviewed. Before sending a PR back for changes, run `gh pr merge <n> -R
  favcrm/cadence --disable-auto`; the operator re-enables it, pinned to the new
  head, only after that head passes review.
- `--admin` only in a declared emergency, never as routine.
- The gate must be main's: `scripts/enqueue-reviewed` compares its own bytes and
  `scripts/delivery-gate` with `origin/main:scripts/<name>` (local git, no
  network). A real enqueue from a differing copy refuses and names the file;
  `--dry-run` warns and runs. To run main's copy, write both files from
  `git show origin/main:scripts/<name>` into one scratch directory and run it from
  inside the repo (`git fetch origin main` first: a stale ref is not detected).
  A PR that edits the gate is judged by main's copy, never its own.

### Checks: trust exit codes, not filtered text

- A shell hook routes commands through `rtk`, which can print "clean" or
  nothing at all when a check really failed. Run checks as `rtk proxy <cmd>`
  and judge them by exit code: `rtk proxy cargo fmt --all -- --check`,
  `rtk proxy cargo clippy --all-targets -- -D warnings`, `rtk proxy gh pr
  checks <n>`, `rtk proxy git diff …`.
- Never skip, ignore or weaken a test or check to get green.

### Local builds and tests

The shared host does not compile. The mechanics (Boat sandbox, snapshot, quota)
are in `docs/DEV-CYCLE.md`; the rules are these.

- Before every push the worktree hook runs `scripts/pre-push` on Boat via
  `scripts/boat-build`: fmt, the doctor split check, clippy on both feature sets,
  the release seam, the UI typecheck and the script contracts for what you
  changed. A missing Boat CLI or helper refuses the push rather than compiling
  locally (without the helper, our `scripts/pre-push` itself refuses); `CADENCE_PRE_PUSH_LOCAL=1` is the explicit emergency path. Required CI
  (fmt, clippy, build, ui, delivery-gate, changed-tests) is unchanged, and neither a remote run
  nor a green suite proves the ticket's outcome.
- Run the plan yourself before `git push`: `scripts/pre-push-check`. GitHub drops
  an SSH push whose hook runs for minutes (rc=141); the hook accepts a
  `tree=clean` rc=0 receipt for that exact head on a clean tree and returns at
  once. A failure, a dirty-recorded pass, a new commit or an edited tree runs
  the plan again. If a push dies with rc=141, retry it once. A push that skips
  the hook still goes through, and the missing receipt is the signal, so never
  skip it. The main checkout has no hook, and the push never compiles on the
  host.
- Run `cargo check`, `cargo test`, `cargo clippy` and `cargo build` through
  `scripts/boat-build -- cargo …`, for example `scripts/boat-build -- cargo test
  --locked --features test-seam --lib <FILTER> -- --test-threads 1`. The
  lane `cargo` shim only queues a host build slot; it does not send the compile
  to Boat. Stay on this host only for a binary that has to run here (`cargo run`,
  a board smoke, a release artifact), and then take a build slot with
  `CARGO_BUILD_JOBS=4`. Prefer `cargo check` over `cargo build`. Run filtered
  test groups only.
- Untracked files are not copied to Boat; `git add` a new file first.
  `scripts/boat-build --down` when you are finished: start limits
  (`boat limits`) are the scarce quota.
- sccache is the host-wide rustc wrapper for a local exception. Do not override
  `RUSTC_WRAPPER` on the host; if a local build fails oddly, rerun it once with
  `RUSTC_WRAPPER=` to rule sccache out.
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
  form. A launch uses the id and the scope the settings carry at that moment.
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
  (`devin models list`); read the tier, never quote it from memory. Switch
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
  `model_effective` and `effort_effective` matching the request. A draft cannot
  authorize its own review.
- Cursor through Pi stays blocked: `src/pi_policy.rs::require_safe_transport`
  rejects the unsafe argv prompt transport, and no allowlist entry or doc
  overrides that refusal. Do not remove or bypass the guard. Cursor models live on the native cursor provider in
  Settings > Agents; that adapter is a different route, not a silent substitute
  for a Pi request.
- Provider mechanics (the pinned extension, the content digest, the local `ide`
  patch) are in `docs/BOARD.md`. A launch that dies with `Model "devin/…" not
  found` or "Pi process disconnected" has several causes, one of them a running
  daemon older than the pin support: check `readlink ~/.local/bin/cadence` before
  blaming the model.

### Production safety

- A production daemon runs on this host. Never touch
  `~/.local/state/cadence`, `~/pm` (apart from `cadence issue …` commands),
  port 3010, the installed `~/.local/bin/cadence` or the tailnet. Never
  restart the daemon. Never signal or kill a process you did not start.
- A merge is not a release. Before handing over an update, confirm a build
  that contains the change is promoted and published (`docs/RELEASING.md`,
  "Prepare and promote the exact candidate"): a manual `staging.yml` dispatch
  whose `promote` job succeeded with a matching `production-candidate`
  receipt, then a successful `promotion-completion` run, its `publish-release`
  job included. Green main CI and the automatic staging run promote nothing.
  Dispatch a promotion only when the operator asks, and never approve it. Only
  then send the operator exactly these two lines, and nothing else, to run
  from a plain terminal. Never run them yourself, even when asked:

  ```
  cadence update --check --as operator:<name>
  cadence update --as operator:<name>
  ```

  If `--check` shows `available` older than `current`, or a `blocked:` line
  naming an older build or unproven ancestry, report it and stop: the
  approved candidate is stale. Published means `promotion-completion`, its
  `publish-release` job included, succeeded; before that `--check` offers the
  previous build and a pinned `--to` refuses with `release detail (not_found)`.
  If `available` is not the build you promoted, report it and stop. Use
  `--to <full main sha>` on both lines only when the operator asks to pin a
  commit; it skips the candidate receipt and runs the `upgrade --sha` checks.
  `cadence update` syncs the bundled guidance (`--check` does not);
  `cadence skill install` is the manual repair, and running sessions do not
  reload guidance. Dev work uses `cadence dev up|reload|down` on a disposable
  instance, never production. Never relay
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
- The refresh command is `cadence --state-dir <staging> staging refresh --as
  delegate:<you> --to <sha>`: it claims the rollout lease and restarts the daemon
  and board (`--no-ui` skips the board). It is admitted only by a live
  `staging_grants` row for that operation and directory and a matching
  `CADENCE_ALIAS`. Runbook: `docs/design/staging-delegation.md`.
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
  source reviews, tests and audit requirements still apply.

### Fenced workers

Agents, PMs included, cannot run `agent unfence` or `message reconcile`. A PM
escalates a fenced worker to the operator with the evidence.

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
  or by deleting a lock. Only the tracker owner may settle it.
- A write whose paths are DISJOINT from the leftovers is admitted:
  `Pm::lock_for_paths` plus `Pm::commit_scoped` (`src/issue/pmlock.rs`,
  `src/issue/mod.rs`). A caller that writes one file must scope its lock to
  that file: take the scoped lock AND commit through the scoped wrapper.
  `src/issue/lock_tests.rs` has a guard test that fails the build when a
  function pairs a scoped lock with the unscoped commit; extend it when you add
  a caller.
- While a crashed writer's marker stands, a global Git hazard (`index.lock`, a
  merge/rebase/cherry-pick state) refuses EVERY write, scoped or not. Without
  that marker the hazard is not a refusal at all: the tracker admits the write and
  hands it to Git, whose answer varies. Never rely on the tracker to refuse a
  write because of a Git-global state. A legacy or foreign `.write.lock` is not
  this build's crash to clear: never delete it as routine, and never treat a
  file's age as authority.
- The operator-facing recovery flow is
  `cadence issue recover review|decide|execute|status`. Running it is a separate
  operator decision: `review` is read-only, and `decide`/`execute` require an
  operator connection. An agent may explain the evidence the review shows, never
  choose the outcome or perform that mutation.
- `~/pm` is written only through the `cadence issue …` commands. Never edit
  the PM dir, the marker, the snapshot or the index by hand, and never
  impersonate an operator to get around a refusal.

### Finish: one procedure, as soon as the PR merges

1. Read the merged PR's merge commit (`gh pr view <n> -R favcrm/cadence
   --json state,mergeCommit`) and verify that commit is on fetched
   `origin/main`. The reviewed head itself need not survive a squash. Then
   set the ticket's status through the selected workspace's supported update.
   On a hosted org that is
   `cadence --org fav-limited issue set <ID> status=done --if-rev <rev>` once the
   acceptance and completion rules are met; on a native tracker
   `cadence issue reconcile --project <key>` marks it from the merge evidence. If
   `reconcile` reports the tracker write lock, do not run it again until the
   prerequisite changed (see "The tracker write lock and an interrupted write").
   If it holds the ticket (an open PR, a claim newer than the merge), say why on
   the ticket.
2. Release your own lane: for a hosted `--profile` lane started by your
   session, first end its running assignment with
   `cadence --org fav-limited agent report <assignment-id> --state result --head <full-sha> --text "<summary>"`
   (the assignment id is in `issue start`'s receipt or `cadence --org fav-limited agent assignments`),
   then run `cadence --org fav-limited issue finish <ID> [--worktree <path>]`
   (it takes no other option). Do not use `agent reassign --cancel --release-lane`
   on an active `--profile` lane: cancellation sets the retired-lane guard and
   blocks automatic release for external sessions. Natively, run
   `cadence issue finish <ID> --worktree <path>` (`cadence issue finish --merged
   --dry-run` previews the sweep and removes nothing). Its activity, clean-tree and
   commit-survival guards may refuse cleanup. Report the exact refusal and
   hand off to the lane's owner or operator when their connection is needed;
   don't force or remove it by hand to get around the refusal. Leave other
   worktrees to the authorized sweep. A path name or a pruned remote ref does
   not prove a lane is safe to remove.
3. Delete your own scratch files by name and stop the servers and processes you
   started. A PR you abandon or replace: close it with a link to what replaces it
   and say so on the ticket; do not leave a conflicted PR idle.

### Reporting

- For routine closeout, record the verified outcome, evidence and any remaining
  limitation or next owner in one ticket comment. Add a short reflection for a
  material blocker, recurring friction or a reusable lesson.
- Copy SHAs from `git` or `gh` output; never type them by hand.

## Operator decisions in force

- 2026-11-03: the fresh-session review standard ("Review").
- 2026-11-17: the PR author's exact Browser QA pass counts once the operator's
  per-head approval accepts it; the `favcrm-dev` pass is a fallback. Lists that
  are missing, unreadable or malformed give the strict floor with no Browser QA.
- Paid platform calls carry no spending limit ("Paid platform calls").
- `EXCEPTION_ENDS` is 2026-12-31 for the native-only exception.
