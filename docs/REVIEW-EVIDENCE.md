# Native review evidence and GitHub enforcement (CAD-120)

## Available increment

`cadence delivery review-evidence --request-file request.json` exports a
snapshot directly from the live daemon over an operator-authenticated Unix
socket. It does not post a GitHub check, approve a PR, enqueue a merge, or
change branch protection. There is no HTTP export route.

The request is a JSON object containing between one and 100 entries:

```json
{"requests":[{"issue":"CAD-123","pr":"https://github.com/favcrm/cadence/pull/42","sha":"0123456789abcdef0123456789abcdef01234567"}]}
```

Every entry must identify the exact PR URL and full lowercase head of a
standing native PASS, in `passed` or `enqueued` state. Under one delivery
lock, the daemon checks the project repository, assigned independent
reviewer, current delivery head and any observed GitHub head/state. It also
requires the latest native verdict-stream receipt to bind the issue, PR,
project, worker, reviewer, full head and report. A modified delivery file
or tracker verdict cannot provide that receipt. Unknown fields, duplicate
issues/PRs, empty batches and partial heads are rejected. One refused entry
refuses the entire batch; no partial proof is returned.

New native verdicts include PR/project/worker scope in their durable
receipt. Legacy receipts lack these bindings and fail closed for export;
the assigned reviewer must perform a new native review. Existing delivery
merge behavior and historical ticket evidence are unchanged.

The result uses `schema: cadence.review-evidence/1` and
`transport_only: true`. **The printed JSON is unsigned.** It is evidence
only for the trusted process that received it directly from the live
daemon. Saving it to a file does not make it an offline attestation. A
publisher must never accept a caller-supplied copy as native identity.
The daemon checks requested heads, not current remote GitHub truth; a
future publisher must read GitHub and recheck those heads itself.

## Remaining boundary before GitHub protection

The current shared GitHub credential and manual `qa-verdict` status cannot
authenticate an independent reviewer. Repository writers can publish
ordinary statuses, so the eventual required check must be bound to a
dedicated GitHub App identity, with its publishing credential confined to
a trusted bridge process outside worker/reviewer environments. The daemon
continues to run no `gh` commands and needs no GitHub token.

The remaining implementation/provisioning work is:

1. Provision an operator-owned App installation with only the necessary
   repository/check permissions, protected credential custody, webhook
   verification, and a configured allowlist of repository and App IDs.
2. Implement a trusted publisher consuming a fresh live export, or a
   daemon-signed receipt verified against an independently pinned key.
   Unsigned JSON input and caller-selected keys/App IDs are not authority.
3. For each PR event, read its current full head, verify native evidence,
   then publish a dedicated check on that exact head. A new push needs new
   review; reconcile invalidation and superseded reviews as well.
4. For `merge_group.checks_requested`, obtain an authoritative, complete
   list of constituent PRs and their current heads. Verify **every** entry
   through a single batch export and bind the result to the exact merge
   group SHA. A branch-name suffix, commit message, final PR alone or
   incomplete/paginated snapshot is insufficient. Refuse ambiguity or a
   moved/removed group and recheck the remote snapshot before publishing.
5. Demonstrate positive PR and multi-entry merge-group publication plus
   forged credentials/receipts, worker and detached callers, concurrent
   review/push, stale heads, missing constituents, API failures and restart
   reconciliation. Enable the App-bound required check only after this
   works in observation mode without starving the queue.

This increment does **not** meet the GitHub-enforcement acceptance. CAD-120
remains open until the publisher, trust provisioning and queue proofs
exist. External independent review notes are not native PASS receipts and
are intentionally ineligible for export.

GitHub documents [checks from a specific App](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets)
and the [merge-group check lifecycle](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue).

## Observation-only binding check (CAD-680)

`scripts/native-review-observation.py` is a small trusted-side module that
observes whether a single exact native review still binds a requested
pull request head. It is library-only — no CLI, no deployment, no daemon
change. `observe(scope, github, native, config)` performs exactly four
reads in fixed order (`github.snapshot()`, `native.export(batch)`, then
both again) and returns a `cadence.review-observation/1` result with
`observation_only: true`. It never writes a check, status or file, and
never propagates adapter exception text.

Trusted adapter shapes:

- `github.snapshot()` must return the authoritative, COMPLETE open-PR
  inventory for the pinned repository (all pages):
  `{repository_id, repository, complete: true, pull_requests: [...]}`
  where each entry is exactly `{number, pr, sha, base_ref, state}`.
- `native.export({"requests": [{issue, pr, sha}]})` returns a
  `cadence.review-evidence/1` export with `transport_only: true`,
  `checked_at`, and exactly one fully bound review.

Both adapters must bound their own calls; the module only checks clocks
when a call returns and cannot preempt a blocked adapter. Fixed limits:
`MAX_EXPORT_AGE_SECONDS = 30` for `checked_at` freshness against wall
time (re-checked at final wall time for both exports) and
`MAX_DURATION_SECONDS = 5` cumulative monotonic budget across the four
reads. Injectable `wall_clock`/`monotonic` are for tests; backwards or
nonfinite movement refuses.

Standing review rule: an old `reviewed_at` remains valid evidence —
freshness applies only to `checked_at`, not review age. Config pins
`repository_id`, `repository`, `project`, `base_ref`; scopes other than
`pull_request` (including every merge-group shape) refuse before any
adapter call. The same head SHA on any other open PR is ambiguous and
refuses.

The output is an observation, **not** an attestation or authorization.
Adapter identity and live provenance are not authenticated by this
policy — a malicious trusted adapter can forge plausible values, so this
cannot claim universal forgery detection. A residual non-atomic race
remains between the last read and any later use; a publisher must
treat the result as one input, never as a published check.

Remaining dependencies to productionize this observation: a
credential-free private native broker/transport between the daemon and
the observing process. The native socket already rejects different UIDs
and derives authority from the connecting operator process, but a
credentialless broker running in the daemon's own UID needs an
independently proven private-channel isolation mechanism from same-UID
workers — same UID or `chmod 0600` alone is not isolation — while the
App publishing credential must live under a separately protected service
identity. Also required: real runtime export-RPC deployment via the
rollout owner rather than test doubles; protected App credential custody
plus verified event intake; review generation, revocation and the
residual non-atomic race on the evidence path; and an authoritative,
complete original-constituent head inventory before merge groups can be
supported. None of this is implemented here, and none of it enables any
GitHub required check on its own.
