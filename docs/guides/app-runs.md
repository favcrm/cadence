# Local app-owned runs

A workspace app owns its runs and output artifacts. A project link is
optional discovery metadata; it does not select a team, grant permission
or create a project. Legacy project plans keep their existing commands
and history.

The executor supports text production and independent text review. A reviewed
capability call may also retain one run-scoped binary asset for the reviewer.
The installable text-only example is `apps/local-content`, with workflow
`draft`; it needs no external connection or AgenticOS provider. A configured,
registered worker still supplies the actual model execution.
Use a managed Pi, Claude or Codex worker for this increment. Terminal
workers are unsupported: their notice delivery requires a separate
authenticated prompt-fetch contract. Remote polled workers are also
unsupported. Unsupported teams are refused before a run is created.

## Package contract

Keep the existing app manifest and Markdown workflow format. Declare the
local step operation explicitly:

```markdown
## Draft
agent: {{writer}}
size: S
action: local.text.produce

Write the draft.

### Acceptance
- [ ] the draft uses only supplied facts

## Review
agent: {{reviewer}}
size: S
depends_on: 1
action: local.text.review

Review the exact dependent artifact.

### Acceptance
- [ ] the review pins the artifact digest
```

Declare the input roles and `distinct: [writer, reviewer]` in workflow
frontmatter. Place `agent`, `size` and `depends_on` before `action`.
The runtime validates the actual assignments as distinct eligible
registered workers in the explicitly selected owner group. It does not
enroll workers or infer a team from the selected project.

`uses` continues to name declared connection slots. It does not name an
execution action. The local executor refuses unsupported actions, arbitrary
repository files and external sends. A workflow may declare bounded
`capability_slots`; each call uses a frozen binding, quote and run approval,
not a worker-selected provider tool. An image workflow declares
`required_asset_slot: image` in frontmatter, naming a declared capability
slot. That requirement is part of the approval digest and frozen snapshot:
the independent reviewer must pin the exact retained asset to approve, and
Local staging checks it again. A `revise` verdict can carry its rationale
without approving an asset. Text-only workflows need no asset pin. Installing
Social Content alone does not create a connection, consent, quote approval or
release authority.

## Operator flow

Use a trusted source checkout and an owned test daemon for development.
The catalog installation command returns a stable installation ID:

```sh
cadence app catalog install /trusted/cadence/apps/local-content
cadence app catalog show <installation-id>
cadence app catalog approve <installation-id> --digest <installed-digest>
```

Approval requires the exact digest returned by the current installation.
It permits the bounded local artifact capability; it creates neither a
run nor a dispatch and does not grant account-wide permissions.

Supply an explicit JSON object of string inputs in a local file:

```json
{
  "subject": "Our new lunch menu",
  "source": "Lunch is served from noon to 3pm.",
  "writer": "op-writer",
  "reviewer": "op-reviewer"
}
```

The CLI bounds this file to 32KiB before sending it. Workflow validation
also checks the declared input contract. Create and inspect a run:

```sh
cadence app run create <installation-id> --workflow draft \
  --inputs ./inputs.json --request-id lunch-draft-1 --owner-pm <registered-pm>
cadence app run show <run-id>
cadence app run approve <run-id> --digest <snapshot-digest>
cadence app run dispatch <run-id>
cadence app run show <run-id>
cadence app run artifact <artifact-id>
```

Creation freezes the workflow, inputs, actual team and capability approval
revision. Repeating the same request ID with the same snapshot returns
the original run; a conflicting request is refused. Use the returned
snapshot digest for run approval. An optional `--project-link` records
only a discovery link.

Workers receive the supported result contract in their real kickoff.
Production returns a bounded text or Markdown artifact through an
authenticated result. Review receives an artifact reference and reads it
under its current assigned turn; the kickoff does not embed an arbitrary
artifact blob. Its decision pins that exact artifact digest and producer.
A generic completion, operator reconciliation note, Git SHA or arbitrary
worker pathname cannot establish a successful artifact or review.

An assigned reviewer fetches a dependency using its current kickoff and
turn token:

```sh
cadence app run artifact <artifact-id> --message <active-message-id> --token <turn-token>
```

Both flags are required together. The daemon also checks the actual
caller, assignment and dependency; possession of a token alone is
insufficient. Operator artifact reads omit both flags.

For a producer, the strict result envelope is:

```json
{
  "schema": 1,
  "kind": "produce_text",
  "run_id": "<run-id-from-kickoff>",
  "step_id": "<step-id-from-kickoff>",
  "revision": 1,
  "outcome": "succeeded",
  "artifacts": [{"media_type": "text/markdown", "text": "The draft."}]
}
```

For its independent reviewer:

```json
{
  "schema": 1,
  "kind": "review_text",
  "run_id": "<run-id-from-kickoff>",
  "step_id": "<review-step-id-from-kickoff>",
  "revision": 1,
  "producer_step_id": "<producer-step-id>",
  "producer_revision": 1,
  "artifact_sha256": "sha256:<64-lowercase-hex-digest>",
  "decision": "approve",
  "rationale": "The draft retains the supplied facts."
}
```

Use the actual IDs, revisions and digest supplied by the run. A review
may return `revise` with its reasons. The authenticated result transport
still establishes the caller and turn; JSON fields cannot impersonate a
producer or reviewer. Extra keys and conflicting duplicate results are
refused.

Inspect runs with `cadence app run ls [--install-id <installation-id>]`.
Use `cadence app run cancel <run-id>` to prevent further execution, or
`cadence app catalog revoke <installation-id> --digest <installed-digest>`
to withdraw its local capability approval. Neither operation converts
old results into new execution authority.

## Approval and recovery boundaries

Installation capability approval, run execution approval, any priced provider
call, and outward release are separate. This executor produces run-owned text
artifacts and may retain a bounded binary capability asset; it creates no
publish effect or outbox item by itself. Retained material does not imply
Instagram/Facebook publishing or a Local release.

Every dispatch checks the current installation, approval revision, frozen
team and dependencies. Cancellation, revocation or material package change
cannot silently substitute a new team or revive old work. Durable run,
step and message associations prevent duplicate kickoffs and completion
receipts across restart. Pending work retains its original identity.
If that identity or authority no longer matches, the run records failure
and keeps its existing artifacts and dispatch receipts. An interrupted
provider turn is never automatically replayed to manufacture a review.
Start a new run with a new request ID after restoring the intended team
and approval; an old run's immutable snapshot cannot be reassigned.

New lifecycle and HTTP management surfaces are operator-only. Worker
artifact access proves the current dependent step and actual turn.
Operators can inspect retained artifacts after cancellation, revocation
or package replacement; each read verifies the stored artifact's integrity.
An audit read grants no execution authority. Local
artifact capabilities govern the broker; they do not claim shell or tool
confinement for a worker's unrelated work.

Catalog storage remains filesystem schema1. Persisted local runs began in
SQLite schema20; later additive migrations through schema25 store the
capability asset receipt and optional reviewer pin. Source merge and
production rollout are separate operations managed by the rollout owner.

`owner_pm` is registered group and routing metadata, not material-read or
execution authority. Reusing that alias does not grant app lifecycle decisions
or artifact access; actual assigned worker registrations remain frozen.
