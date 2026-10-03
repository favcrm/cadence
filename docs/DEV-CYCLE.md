# A shorter, honest development loop (CAD-1088)

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
failure, not fix it. One correctly bound scope approval, two independent
exact-head verdicts, one correctly bound merge approval and a queue enqueue
should be the normal path—not repeated requests for the same decision.

## Change-dependent PR feedback

A classifier read from the **base revision** selects cheap PR feedback:

| Change | PR compilation | Final merge queue |
| --- | --- | --- |
| Isolated documentation guides | None; document/contracts checks still run | All current real gates |
| Isolated UI source/assets | Frontend checks + embedded-UI build; no unrelated default Rust gates | All current real gates |
| Rust logic | All feature/build/test gates; embedded feature can depend on Rust | All current real gates |
| Workflow, scripts, dependencies, governance, mixed/unknown changes | All gates | All current real gates |

Deletions, renames, symlinks, bad SHAs, absent classifier or failed lookup
fall back to full validation. PR code cannot supply its own selection policy.
Required contexts explicitly report not-applicable commands, never 'built' or
'passed tests' when they did not execute. Merge groups do not take the fast
path: this keeps final integration coverage even when docs are read indirectly
by source/tests. Fast-path timings measure PR feedback, not total landing
latency. #758 bootstraps the policy with a full run because its base lacks it.

## One local entry point

```sh
scripts/pre-push --list --tests  # inspect what will run, no builds
scripts/pre-push --tests        # execute; stop at the first nonzero exit
```

The diff selects Rust/UI/active script checks. The live doctor source
inventory always runs. `--tests` always executes the safety floor (two test
threads), even if its source is unchanged or diff discovery is uncertain.
Add filtered tests for the behavior you changed; the floor is not that proof.
Host Rust builds use four jobs and normal build-slot admission. If admission
is unavailable, report that limitation rather than claiming local validation.

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

Review count follows AGENTS.md "Review" (CAD-1099): one independent review by
default, two for the excluded paths in `docs/roles/one-review-paths.toml`, and
operator approval for human risk. Automating evidence collection, reviewer state,
head-drift notifications or an operator decision UI is a separate reviewed
implementation—not a license to self-approve. Likewise, consolidating required
build/UI checks or changing branch protection requires baseline measurements
and an operator decision. Do not change names in YAML before migrating the
required-check policy safely.

During the reduced window, restoring approval-lineage, inventory/shard and
remote-CLI adversarial contracts is more important than optimizing dormant
legacy benchmarks. Releases stay frozen until restoration is independently
reviewed and authorized.
