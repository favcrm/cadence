# A shorter, honest development loop (CAD-1088)

## The loop today

The current day-to-day loop (AGENTS.md "Delivery workflow" is the rule text):

1. Requirement and plan: state the user outcome, observable acceptance,
   non-goals and material unknowns in the ticket. Resolve consequential
   ambiguity before implementation; a short ticket plan is enough for a
   small change. One ticket and PR per end-to-end feature; split only for
   another repository or an independently usable outcome, not per step.
2. Implement the simplest change that meets acceptance. Reuse existing
   conventions and modules; put a design note in the PR when AGENTS.md
   requires it, not in a separate mandatory design-review round. Before
   requesting review, simplify your changed lines without changing behaviour:
   remove dead code, duplication and unnecessary abstractions. The
   `code-simplifier` skill can help; it is not another approval stage.
   Adjacent refactors and polish go to a follow-up ticket.
3. The worktree hook runs `scripts/pre-push` on Boat before every push; show
   the ticket's outcome. A missing Boat CLI or helper refuses the push rather
   than compiling locally (`CADENCE_PRE_PUSH_LOCAL=1` is the explicit
   emergency path). For a bug fix, show the failing case once, then fixed.
   Add checks only under AGENTS.md's behavior-first and gates/security rules;
   do not restore retired tests or routinely repeat suites. `--tests` is a
   focused diagnostic, not a blanket pre-review prerequisite; required CI and
   full merge-group regression remain unchanged.
4. Under strict requirements, one independent reviewer; two for auth, identity,
   secrets, the daemon trees and UI paths (`docs/roles/one-review-paths.toml`
   lists what gets one).
5. The version-one delivery tiers apply to clean default-equivalent policies
   by default after the reviewed implementation is installed; the introducing
   PR must still satisfy the current trusted-base gates. Routine changes need
   green exact-head CI and genuine
   ticket-outcome evidence, with no mandatory review; consequential changes
   need one independent combined standards/spec review. Sensitive changes
   keep the existing strict review, operator, Browser QA and full-check
   requirements. Unknown, incomplete, protected-mode, deletion and mixed
   sensitive cases retain strict legacy floors. Explicitly approved custom
   policies remain in force and strict; malformed or unapproved policy changes
   fail closed. Approved version-one profiles remain readable for compatibility.
   Mandatory protection checks, full merge-group regression, credential
   isolation and operator-owned deployment remain unchanged. No fabricated
   verdict/readiness counts as review or outcome evidence; dry-run is read-only
   and requirements must be freshly bound before queueing.
6. Required read-only code review may overlap CI on a stable head; pending
   CI is not readiness and all required exact-head checks must be green
   before enqueue. Rebase onto current main before requesting review, as
   AGENTS.md requires; after approval, rebase only for conflicts. Review
   correctness, security and ticket acceptance. Preserve independent refusal
   acceptance for enforced rules and desktop/narrow Browser QA for `ui/**`.
   REVISE only for correctness, security or ticket-requirement gaps. Other
   findings are notes: one follow-up ticket or a recorded reason to drop them,
   not another polish round on the reviewed head.
7. Operator approval: for a `human`-class PR the operator may record one scope
   approval per ticket with
   `cadence audit approve --issue <ID> --action scope`. It covers the ticket
   only while it is ready, doing or review, and its body declares its risk
   triggers on exactly one `Risk:` line. It is bound to the ticket body, so
   editing the body voids it, and verdict notes state `Scope: in-scope <ID>`.
   Scope approval is optional and valid only when every AGENTS.md condition
   holds. Triggers 1/3, two-review paths, uncovered scope or invalid bindings
   require per-head approval; conversation text is never approval evidence.
8. Post the required exact-head ticket evidence and enqueue with
   `scripts/enqueue-reviewed <n> --head <full-sha>`. Use the merge queue, not
   routine `--admin` merges or duplicate local integration trains. Disable
   auto-merge before editing an enqueued PR; a new head voids old verdicts.
9. Follow the [artifact-specific rollout checklist](guides/rollout-checklist.md)
   and [host release cycle](RELEASING.md). For a native host:
   merged → attested main CI artifact → staged → manually promoted with GitHub
   `production` approval → operator-installed → operationally verified.
   **Automatic staging is not production promotion.** Check the successful
   manual `staging.yml` run and its `production-candidate` receipt before
   handing over the default update commands; a green CI/staging run alone
   cannot advance the updater's candidate. Public version tags use the
   separate numbered-release path, not a tag on every merge.
   Production deployment is operator-owned; agents never restart the live
   daemon. After merge, reconcile the ticket, leave the worktree to the sweep,
   and clean up only your own scratch and processes. Record installation and
   verification separately; reconciliation is merge evidence, not deployment.

## Pragmatic design and cycle time

Generic means using standard approaches and genuinely shared modules, not
building a framework for hypothetical future callers. Keep interfaces small
and changes local. Introduce an abstraction only when current acceptance or
real variation needs it; do not force different workflows through one generic
interface. When two approaches meet acceptance, choose the smaller one and
explain why. Larger refactoring needs its own outcome and acceptance.

One bounded author simplification pass precedes review. Stop when acceptance
and required verification are complete; do not add planning documents,
reviewers, repeated checks or handoffs merely to make the process look thorough.
Reuse unchanged-head evidence and reviewer context as described below. Never
trade away approval, independent review required by the effective policy,
isolation or fail-closed guards to save time.

For a slow cycle, inspect existing `cadence issue retro <ID> --json` evidence
and CI job timings. Separate active work from waiting for scope decisions,
resource admission, CI, review, approval, queue and rollout; unknown timings
stay unknown. Compare similar risk tiers before changing the process. Use
existing timestamps for median/p90 where enough comparable evidence exists;
do not claim speed gains from one anecdote or add a mandatory evidence ledger.
Fix the demonstrated bottleneck, not whichever gate is easiest to remove.

Local role documents or generated briefings may be stale. Follow AGENTS.md and
the effective delivery policy; do not treat an untracked briefing as merge or
production authority. Changed instructions do not reload running contexts.

## Bounded review brief

A review dispatch is a contract for one bounded inspection, not a license to
audit the codebase. The brief states, on one page:

- the exact head SHA and the diff or changed paths under review;
- the ticket's acceptance items the reviewer is answering;
- the concrete risks and required checks for this diff (the risk class, the
  one-or-two-review path, any gate or security acceptance check);
- which existing evidence is still valid to reuse (a prior PASS on the same
  head, unchanged-head CI, earlier findings);
- what is excluded from this review;
- the accountable reviewer and where the verdict files (the head-pinned
  verdict note and `cadence report file` route);
- an agreed effort and checkpoint budget — roughly how much inspection the
  diff justifies and when to stop and report back.

The reviewer inspects whether the diff meets its acceptance and whether its
relevant dependencies and risks hold. They do not automatically add optional
requirements, broader audits, extra tests, extra builds, browser journeys,
refactors or additional reviewers. Wider optional inspection is allowed, but
only after naming a concrete failure scenario, the proposed scope and cost,
and getting the dispatcher's permission first — it is never folded silently
into the same verdict.

Mandatory safeguards are not optional scope. A concrete security, isolation
or approval risk the reviewer actually sees is investigated or escalated on
the spot; missing required evidence is a blocker, never a fabricated PASS.
None of this changes the automated gates or reviewer counts AGENTS.md and
the effective policy already fix.

The review stops when every acceptance item has a disposition and the
required checks are done. The verdict reports PASS or REVISE on the exact
head, the evidence read, blockers and separate non-blocking notes. If the
agreed budget runs out first, the reviewer files a scoped blocker naming the
remaining work — not an automatic PASS and not open-ended continuation.

### Converge instead of restarting

The first pass covers the complete change against ticket acceptance and its
relevant dependencies. Before returning REVISE, verify each blocking finding:
name the file/function, concrete failure scenario and supporting source or
runtime evidence. Report the bug class and all known affected instances in one
response, not one instance per round. Optional polish stays non-blocking.

A new head on the same ticket goes back to the same reviewer. The existing
handoff names the previously reviewed head, new head, prior findings and fix
evidence. Review the delta and affected behavior/dependencies rather than
restarting the whole inspection. Widen when assumptions, scope or risks change;
if the prior review is unavailable, perform a complete first pass. Do not
repeat resolved findings without new evidence that the fix is incomplete.

Delta review is a way to inspect the new head, not to carry an old PASS forward.
File a fresh exact-head verdict and revalidate the required evidence; only
still-valid evidence may be reused. New heads still invalidate old verdicts,
and required CI, Browser QA, refusal acceptance and approval bindings remain
unchanged. Read-only review may overlap CI; pending CI never permits enqueue.

If a third round still returns REVISE, stop the automatic fix/review loop and
escalate to the PM or operator with the remaining failure scenarios, attempted
fixes and a recommended approach. A round cap is not permission to auto-pass,
ignore a real bug or weaken the existing risk-class rules. Resume after the
approach or disagreement is resolved, with the required exact-head checks.

This applies the convergence lessons from
[How the Claude Code team does code review](https://claude.dev/blog/solving-code-review-at-anthropic/):
reduce repeated review and human waiting, not the safeguards.

## What #738 taught us

PR #738 took more than four hours to deliver. It was not just a small update:
it retired roughly 180,000 lines of legacy test/fixture coverage, including
originally protected controls. That deserved a human scope decision. But
much of the elapsed time was avoidable coordination and author error.

Evidence: PR CI runs 37098191418 and 37103055285, staging rehearsals
37098191451 and 37103055272, queue run 37103466054; final merge
`c4f55f8ec899dd9f431a4cc4884da9bd3b72bfc7`. Required PR jobs on the reconciled
head took fmt 18s, clippy 1m31s, build 3m21s, test 2m21s, UI 3m45s. Jobs overlap;
do not add their durations and call that PR latency. These are baseline
observations, not a controlled performance comparison.

| Friction | Cause | Correction |
| --- | --- | --- |
| Repeated failed CI | Author sent child-wrapper, fixture and format-placeholder bugs to CI without a working local floor run | Make `pre-push --tests` actually execute the floor for every explicit request; run before review |
| False expanded-scope approval | `duplicate: true` returned the approval for the original prohibitive ticket body | Amend scope text first; compute its digest; check the returned digest, not only exit 0 |
| Repeated full reviews | Fresh reviewer contexts and failure to reuse unchanged-head assessments | Keep the same reviewer handles and ask only for delta/evidence revalidation |
| Moving-main conflicts | Retirement PR overlapped newly merged test edits | Resolve only approved-scope conflicts; preserve main production changes; show delta and recheck it |
| Failed paste commands | Interactive `read` in a noninteractive runner; source over 200 characters | Provide direct noninteractive commands, short source text and pinned SHA |
| Confusing green checks | Retired shard/installer journeys retained old tooling descriptions | Label reduced/dormant consumers explicitly; keep required names but never imply restored coverage |
| Tracker contention | Shared PM writer was busy | Treat exit 75 as bounded retry; do not invent an approval/comment or bypass the lock |

The author owns these mistakes. Deleting reviews or approvals would hide the
failure, not fix it. One correctly bound scope approval, the exact-head
verdicts AGENTS.md "Review" requires (one by default, two for triggers 1/3),
one correctly bound merge approval and a queue enqueue
should be the normal path—not repeated requests for the same decision.

## Change-dependent PR feedback

A classifier read from the **base revision** selects cheap PR feedback:

| Change | PR compilation | Final merge queue |
| --- | --- | --- |
| Isolated documentation guides | None; document/contracts checks still run | All current real gates |
| Isolated UI source/assets | Frontend checks + embedded-UI build; no unrelated default Rust gates | All current real gates |
| Edits to pre-existing independent top-level `tests/<name>.rs` only | fmt/clippy/build gates plus nextest on exactly those targets, still with lib and bins suites and zero-target guards | All current real gates |
| Rust logic | All feature/build/test gates; embedded feature can depend on Rust | All current real gates |
| Workflow, scripts, dependencies, governance, mixed/unknown changes | All gates | All current real gates |

Deletions, renames, symlinks, bad SHAs, absent classifier or failed lookup
fall back to full validation. PR code cannot supply its own selection policy.
The tests scope narrows only when every changed file is a pre-existing
top-level `tests/<name>.rs` modified in place, the base manifest has no
explicit `[[test]]`/`autotests = false`, no `tests/<dir>/main.rs` cargo
target exists at base, and no other tracked base `.rs` source references
the file's name or path. `tests/fixtures/*.rs` helpers, new or deleted
test files, mixed diffs and non-PR events never narrow.
Required contexts explicitly report not-applicable commands, never 'built' or
'passed tests' when they did not execute. Merge groups do not take the fast
path: this keeps final integration coverage even when docs are read indirectly
by source/tests. Fast-path timings measure PR feedback, not total landing
latency. #758 bootstraps the policy with a full run because its base lacks it.

## One entry point, executed on Boat

```sh
scripts/pre-push --list --tests  # inspect what will run, no builds
scripts/boat-build --tests       # execute on Boat; stop at the first nonzero exit
scripts/boat-build -- cargo build --locked --bin cadence
scripts/boat-build -- cargo test --locked --lib FILTER -- --test-threads=1
scripts/boat-build --artifact target/debug/cadence --output /tmp/cadence-build -- cargo build --locked --bin cadence
scripts/boat-build --down            # delete this worktree's sandbox now
```

The worktree hook uses `scripts/boat-build` by default, so the shared host does
not compile. A missing Boat CLI or helper refuses the push; it never falls
back to a local build. `CADENCE_PRE_PUSH_LOCAL=1` is the explicit emergency
path and runs the same plan on this host.

The diff selects Rust/UI/active script checks. The live doctor source
inventory always runs. `--tests` always executes the safety floor (two test
threads), even if its source is unchanged or diff discovery is uncertain.
Show the behavior you changed; add a focused check only when AGENTS.md requires
it or the ticket's outcome cannot be shown without one. The floor is not
that proof. Remote Rust builds use one Cargo job. If Boat is unavailable,
report that limitation rather than claiming validation.

The helper keeps one Boat sandbox per worktree. A later command reuses it
while it is running, then leaves it up for 30 minutes. `--down` deletes it,
`--fresh` replaces it, and `--once` deletes it when the command exits. Idle
sandboxes were created with snapshots off, so that stop erases the disk and
the next command starts cold.

The copy is a shallow git snapshot of the candidate and the base, from their
merge-base forward, plus a binary patch of tracked edits. It is not an rsync
of the worktree and it does not pull from GitHub. The full history pack is
not uploaded. Untracked files are refused. Ignored files, including
`target/`, stay out of the snapshot; a reused sandbox keeps `target/` and the
cargo registry, so only the changed crates compile. The first command of a
session is a cold compile and costs one Boat start. Later commands in the
same 30 minutes do not. Start limits (`boat limits`) are the scarce quota.

It does not copy account environment secrets (`--no-env`). For UI-related
pre-push checks and commands starting with `bun`, it installs the exact Bun
version pinned by `ui/package.json` and the locked UI dependencies. The
create ceiling is one hour, which covers the run; the idle timer after the
command is 30 minutes. It reports memory, time and Boat usage. A failed or
interrupted creation can have an unknown outcome; do not retry or delete
another sandbox by name. The helper moves preinstalled provider CLI entry
points out of the test runner's fixed search path **inside the sandbox
only**; it does not change the host or relax the test isolation guard. Only
an explicitly requested artifact is copied back, without overwriting an
existing destination.

The hook's result gates the push, but required CI, review and operator
approval remain authoritative. Commands and their output may be retained in
Boat's command history; never put secrets in command arguments or output.

The CAD-811 public-owner fixture uses the board's typed startup receipt, not a
health probe. Its `Native::try_new_with_roles` refuses a lost bind; only native
`AddrInUse` starts a fresh state/realm on the next candidate from
`test_ports::reserve_board_port`. That helper's saturating per-process cursor
bounds attempts but is not a cross-process lease. Other startup errors and
missing receipts remain blocked. CAD-1838's competing-listener check proves the
original listener stays owned while a fresh real board preserves Native owner
attribution over HTTP; the retired-reviewer refusal is unchanged.

### Queueing without a pane (subagents, operator shells)

A caller with no registered pane or managed endpoint queues through the same
daemon instead of running cargo unqueued:

```sh
cadence build-slot run build -- cargo build      # or: run test -- cargo test ...
cadence build-slot launch <recipe>               # a build/test recipe with no env
cadence build-slot status                        # capacity, holders, waiters + reason
```

The daemon labels such a caller `unregistered:<uid>` from the socket peer's
credentials (a `--lane` or `CADENCE_ALIAS` is never read). It gets a slot
and nothing else: its waiters rank behind every registered lane, it may ask
only for `build`/`test` (not `suite`, `check`, or a hand-held `acquire`),
it cannot release, reconcile or launch an env-passing recipe, and operator
and agent actions still need their positive proof. `status` lists each
waiter's reason: `capacity`, `memory` or `disk`.

Do not push fixture fixes repeatedly just to discover compiler errors in CI.
Do not change `RUSTC_WRAPPER` or disable tests to get a passing receipt.

## Scope approval: amend first, approve once

Scope records bind `sha256(ticket.body.trim())`, not a ticket comment or the
PR description. Write explicit deletions/exceptions into the issue body
through `cadence issue` before requesting approval. Verify the current digest:

```sh
cadence issue show CAD-1088 --json | python3 -c 'import hashlib,json,sys; d=json.load(sys.stdin); print(hashlib.sha256(d["body"].strip().encode()).hexdigest())'
```

If scope approval is required, the operator runs from their own connection:

```sh
cadence audit approve --issue CAD-1088 --action scope --source "Operator in chat: approves CAD-1088 scope as currently written."
```

Compare the response's digest to the computed digest. A duplicate record is
valid only when that digest is the intended scope. Do not modify body/acceptance
after approval without obtaining a decision for the new digest. Comments can
record progress without changing the scope digest.

## Review once; revalidate only what moved

- Same head, new CI completion: supplement the evidence. Do not commission
  another full code review.
- Same head, corrected authority evidence: have the existing reviewer verify
  the correction and supersede their false assertion.
- New head: old verdicts are void for enqueue. Resume the same reviewers with
  the exact old/new SHA and a range/tree diff. They decide if focused recheck
  is adequate and file new head-bound notes; never carry PASS automatically.
- Production/auth changes, scope expansion or new severe bot findings need
  substantive review. Tiny changes are not an authority bypass.
- Quota failure is no verdict. Select an available authorized model; preserve
  completed evidence instead of rerunning both axes unnecessarily.

For a main-only reconciliation, prove both the delta from reviewed head and
zero unintended production delta against new main. Never rebase merely
because main moved: the merge queue already tests the integration tree.
Do not force-push a branch with auto-merge enabled; disable it first.

## One final operator handoff

Finish independent review and CI, post the ticket evidence, then request a
merge approval on the **literal full SHA** just reviewed. Example syntax
(the example SHA must be replaced with the actual reviewed head before use):

```sh
cadence audit approve --repo favcrm/cadence --pr <PR> --head <FULL_REVIEWED_SHA> --source "Operator in chat: approves the reviewed green PR head."
```

Provide the operator a concrete paste-ready version with no placeholders,
`read`, guessed SHA, or source over 200 characters. The agent never executes
the approval itself. Record the returned approval id on the ticket, then:

```sh
scripts/enqueue-reviewed <PR> --head <FULL_REVIEWED_SHA> --dry-run
scripts/enqueue-reviewed <PR> --head <FULL_REVIEWED_SHA>
```

The helper enforces exact-head checks, verdicts, evidence and approval. Queue
position is not merge evidence: watch the merge-group run and verify the
merged commit on `origin/main`. No `--admin`. Prioritization must not strand
other PRs or cancel another entry until this PR itself can enqueue.

## Targets and follow-up decisions

Immediate targets: one local feedback command, no skipped explicit floor,
no ghost contract discovery, no interactive approval prompts, no false scope
digest, no fresh full review for unchanged code, and no duplicate Node setup
for cargo-only retired journeys. Measure failure-before-review rate and
number of operator/reviewer handoffs as well as CI elapsed/runner minutes.

Review count follows AGENTS.md "Review" and the effective delivery policy:
eligible routine changes need no mandatory review, consequential changes one
combined review, and sensitive/unknown cases retain strict floors. Under the
strict path, use one independent review by default, two for excluded paths in
`docs/roles/one-review-paths.toml` (subject to the documented solo-operator
rule), and operator approval for human risk. Automating evidence collection,
reviewer state, head-drift notifications or an operator decision UI is a separate reviewed
implementation—not a license to self-approve. Likewise, consolidating required
build/UI checks or changing branch protection requires baseline measurements
and an operator decision. Do not change names in YAML before migrating the
required-check policy safely.

CAD-1102 ended the reduced window: the current gate is permanent and releases
are no longer frozen. New behavior checks follow AGENTS.md (CAD-1099), one
acceptance check per enforced rule, not restoration of the retired suites.

## Emergency clean-slate transition

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

## Local AgenticOS provider

A dev sandbox can use the real AgenticOS provider for Social Content source
reads, image/text generation and device publish. The app needs no fixture
variant: the host emits the same `agenticos_external` receipts production does.
The profile is sandbox-only; a production daemon still reads the fixed
root-owned `/etc/cadence/provider-deployments.json`.

Create the credentials with their exact scopes:

| File | Scopes | Used for |
| --- | --- | --- |
| `provider.token` | `provider.read` + `provider.draft` | source, destination list, image and text |
| `send.token` | `publish.send` | device publish |

Cadence keeps one connection per provider/account, so read and draft share one
connection. Publish stays a separate credential because AgenticOS does not
allow `provider.*` and `publish.send` on one token. If the workspace issues
separate read/draft tokens, provide `read.token` and `draft.token`; the command
still enrolls one connection using `read.token`, which must carry both scopes
for image/text work.

```sh
mkdir -p ~/.config/cadence/dev-tokens
chmod 0600 ~/.config/cadence/dev-tokens/*.token

cadence dev agenticos setup --name dev \
  --workspace ws_<company-uuid> \
  --token-dir ~/.config/cadence/dev-tokens

cadence dev up dev
cadence dev agenticos bind --name dev --bundle ./dist/bundle --context Default
```

`setup` writes `provider.json`, a sandbox-scoped `provider-deployments.json`
with the reviewed `agenticos-external-provider-tools@4` pin, and the credential
files under the sandbox root. `up` applies the profile to the daemon and its
children; if the sandbox is already running, `cadence dev reload --name dev`
applies it. `cadence dev env dev` prints the same exports for CLI use.

`bind` installs the bundle, approves that exact digest in the sandbox, creates
the context, enrolls the provider connection, binds every declared
`source`/`image`/`writer` slot, and sets the publication destination from the
live destination list. A changed bundle needs a fresh sandbox or a catalog
upgrade before rebinding.

To clear the sandbox's pending run and release approvals in one explicit step:

```sh
cadence dev agenticos approve --name dev --install <install-id> \
  [--context-id <id>] [--dry-run]
```

It approves only runs in `awaiting_approval` with their exact snapshot digest
and effects in `waiting` with their exact digest. `--dry-run` pages the
read-only `waiting` list and prints the same candidate IDs and digests a live
run would approve. Without `--dry-run` it approves each batch and re-queries
the `waiting` state, so later windows are reachable. Its decision is
`execute: false`: the effect is decided with its exact digest but not run, so
the command never publishes; a normal board approval still executes. A dry run
opens an existing record file read-only and creates or migrates nothing; a live
run uses the normal listing path. It refuses a non-dev store, does not create
or dispatch work, and never runs as a daemon watcher. All its RPCs use the checked sandbox socket, never an inherited
`CADENCE_SOCKET`; a symlinked or non-socket `cadence.sock` is refused.

Only the install approval is automatic, and only for the exact digest in a dev
store. The `approve` command is an explicit operator call; run, quote and
effect approvals remain the normal host checks. The device publish sender is
configured by the profile’s `CADENCE_PUBLISH_*` exports, so local publish does
not need a hosted lease. Use a test Instagram account: a device publish is a
real external side effect.
