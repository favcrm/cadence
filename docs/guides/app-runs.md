# Local app-owned runs

A workspace app owns its runs and output artifacts. A project link is
optional discovery metadata; it does not select a team, grant permission
or create a project. Legacy project plans keep their existing commands
and history.

The executor supports text production and independent text review. A reviewed
capability call may also retain one run-scoped binary asset for the reviewer.
The installable text-only example is `tests/fixtures/apps/local-content`, with workflow
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

## Skill material

A step that needs packaged instructions and helper files names one installed
skill package:

```markdown
## Draft
agent: {{writer}}
size: S
action: local.text.produce
skill: meeting-writer
```

`skill: <tag>` is static metadata. It is part of the workflow's gate digest,
so it is part of the approval digest, and a `{{input}}` placeholder in it
refuses. It must be the LAST metadata line of its step: the plan parser stops
reading metadata at the first key it does not know, so a line written after
`skill:` would land in the step body and never reach the run.

The package lives in the installed bundle at `skills/<tag>/`:

```
skills/meeting-writer/skill.json
skills/meeting-writer/SKILL.md
skills/meeting-writer/references/style.md
skills/meeting-writer/scripts/build.py
```

`skill.json` is the manifest and is the only authority on what the package
ships:

```json
{"schema": 1, "name": "meeting-writer", "files": ["SKILL.md", "references/style.md", "scripts/build.py"]}
```

The manifest is closed: `schema` must be `1`, `name` must be the package's own
directory name, `files` must declare `SKILL.md`, and an unknown field refuses.
The declared set must equal the set actually present, in both directions — a
declared member the bundle does not carry refuses, and a present member the
manifest does not declare refuses, each naming the path. `skill.json` is never
listed in its own `files`.

Allowed members: a flat root leaf with `md`, `txt`, `json`, `yaml`, `yml`,
`csv`, `css` or `svg`; `scripts/<stem>.py|sh|js|mjs`; `references/<stem>.md|txt`.
Nothing nests deeper, dotfiles refuse, and a symlink refuses. Caps: 16 declared
members, 16 KiB per member, 4 KiB for the manifest, 32 KiB per package
including the manifest.

Every declared text file is scanned for tokens that name package material —
`scripts/…`, `references/…`, `skills/<tag>/…` (including `./`-prefixed forms)
and a bare `name.ext` with an allowed extension. A token naming a member the
manifest does not declare refuses. This is a declared-inventory contract, not
import analysis: the scan reads names, never evaluates them, so a path built at
run time (`os.path.join`, `importlib`, a variable) is invisible to it, and a
bare word that happens to end in an allowed extension is treated as package
material. Write prose that does not look like a path, or declare the file.

At run creation each `skill:` tag is resolved from the installed bundle's own
file map and frozen into the run as `{name, manifest, manifest_sha256, digest,
files: [{path, sha256, text}]}` under `snapshot.workflow.steps[].skill`. The
snapshot digest covers it, so approving the run approves that exact material.
Creation refuses before any run row exists when a package is missing,
incomplete, over bounds, when a declared reference is undeclared, or when the
encoded kickoff would exceed the transport byte limit. Dispatch builds the
kickoff envelope from the snapshot alone: the frozen bytes are what a worker
receives, and updating the installed bundle afterwards cannot change what an
existing run was given. The host writes no run-scoped directory and no copy of
the material to disk — there is no host-owned file for a later writer to
replace.

**What this does not prove.** Script execution is agent-side and unattested.
The host freezes the bytes and hands them to the assigned worker; it does not
execute `scripts/` itself, does not sandbox them, and does not verify what they
did. A frozen digest proves which bytes a run was given, not that the worker
ran them or ran them honestly. A script in a package is material for the
worker's own turn, under that worker's existing confinement — not a host
capability, not an authorization, and never a substitute for the run, review or
operator decision below.

A `skill:` directive is refused, not silently dropped, on every path that does
not freeze a package: `cadence workflow check`, `cadence plan propose
--workflow <stored-name>` (including the unattended grant path) and `cadence
plan propose --workflow <app>/<workflow>`.

## Operator flow

### Start from the board

An installed app can expose a host-owned **New …** workflow action, such as
Meeting minutes' consolidation flow. This launcher uses verified workflow
metadata and supports capability-free local-text workflows. Host execution
and priced capability calls keep their existing approval surfaces.

Open the action in the intended app scope, enter the declared text inputs,
and choose **Prepare**. The app must have a configured default team; if it
is missing, the form offers the existing team settings first. The displayed
owner PM and team-covered roles are frozen explicitly into the draft, not
replaced by a default team saved in another tab. Remaining roles use eligible
registered workers in the same owner group, with distinct assignments where
the workflow requires them. No model selection, worker enrollment or provider
access is granted.

Review the server's frozen inputs, assignments and exact snapshot, then
confirm **Approve and start**. The existing operator-only approval pins its
snapshot digest, and dispatch rechecks the native installation/context proofs.
A changed bundle or context receipt cannot silently replace the form's
confirmed proof. Moving scope closes the dialog and stops further approval
or dispatch; a request already in flight remains bound to its original scope.

Text is not silently trimmed or textarea-truncated: the server refuses inputs
outside its limits while the form retains the entered text. Unseen/uncertain
responses retain their request ID. Retrying first inspects the stored run
state; only explicitly dismissing a displayed result retires that replay ID.

Workflow inputs, including a transcript, currently require a single visible
line. Newlines, control characters and leading/trailing whitespace refuse to
protect the approved plan structure. The form preserves rejected text; it
does not flatten it. Multiline source needs a separately reviewed input path,
not removal of that guard.

The app's read-only screen displays the resulting run and reviewed artifact
when its declared projection supports them. Neither the screen nor ordinary
assistant chat starts a run. This flow adds no recording, speech-to-text,
external publication or automatic project-task creation.

### Start from the CLI

Use a trusted source checkout and an owned test daemon for development.
The catalog installation command returns a stable installation ID:

```sh
cadence app catalog install /trusted/cadence/tests/fixtures/apps/local-content
cadence app catalog show <installation-id>
```

Installing is the approval (CAD-1119): the operator's install, or update,
records the approval of exactly the installed digest, audited with the
digest and the capabilities it declares. It permits the bounded local
artifact capability; it creates neither a run nor a dispatch and does not
grant account-wide permissions. Each run still waits for its own
frozen-price approval, and each publication for its own approval.
`cadence app catalog revoke <installation-id> --digest <installed-digest>` turns the installation's access off;
`cadence app catalog approve <installation-id> --digest <installed-digest>`
turns it back on.

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

The frozen snapshot embeds agent identity timestamps as JSON floats, so
approval rehashes the stored snapshot text and compares it to the frozen
digest. The build enables exact float round-tripping (`float_roundtrip`)
so a reread parses back the identical doubles; without it, epoch-scale
timestamps can decode one ULP off and approval refuses the run with
`immutable app run snapshot receipt is corrupt`. A mismatched digest is
never bypassed: forged digests and tampered snapshots stay refused.

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

Managed Codex cannot open the daemon socket from its command sandbox.
Its one fetch path is the Cadence MCP bridge on that same turn: call
`self`, then `app_run_artifact` with `artifact_id` from the kickoff and
`message` and `token` set to the `id` and `turn_id` of the held turn
`self` returns. The tool accepts only those three strings. Alias,
provider, path, and command arguments are refused, as are a missing
message or token. That MCP call uses the existing guarded
`app_run_artifact` RPC from the enrolled process. It does not launch a
subprocess, so the turn token is not copied into a child argv. Pi,
Claude, and other supported providers keep the CLI above. The daemon
applies the same caller, assignment, dependency, active-turn, approval,
and integrity checks. Operator reads stay on the flagless command.

A successful worker fetch records that artifact, the dependent message, and
the current turn. Approve requires that record for the same three. A revise
decision stays valid without it. An operator audit read does not record a
fetch.

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

The kickoff asks for exactly one bare envelope, but the result handoff
recovers deterministically when Pi wraps that single envelope in harmless
prose, either bare or inside one standalone `json` fence. The final text is
bounded before either parse path; exactly one complete envelope is accepted
and missing, truncated, oversized, conflicting, or multiple candidates fail
closed without material success. The decoded envelope still passes the same
run, active-turn, assignment, artifact, and review checks as a bare
envelope, and the already failed run stays historical: broker-retained
receipts remain independent of the app turn result and are never replayed.

Inspect runs with `cadence app run ls [--install-id <installation-id>]`.
Use `cadence app run cancel <run-id>` to prevent further execution, or
`cadence app catalog revoke <installation-id> --digest <installed-digest>`
to withdraw its local capability approval. Neither operation converts
old results into new execution authority.

## Operator decision and one selected ticket

Installation approval, run approval, quality review, and outward release stay
separate. `cadence app effect accept` still records a decision and releases
the artifact. A post-review choice that must not publish is not an effect
flag: `execute: false` stays off the CLI, because a stored non-execution is
reconciled as an interrupted accept.

Record the choice on the succeeded run instead:

```sh
cadence app run disposition <run-id> \
  --request-id <request-id> \
  --artifact-id <artifact-id> \
  --digest <sha256-digest> \
  --decision accept
```

`--decision` is `accept` or `decline`. The run must be succeeded and approved,
the digest must be the stored artifact bytes, and an independent reviewer
must have approved that digest. The producer approving their own artifact is
refused. The command does not stage a release, does not write an effect row,
and does not change upgrade readiness. One decision per run: the same request
returns the stored row, and a different request or decision conflicts.
`cadence app run show` includes the row after a daemon restart.

An accepted artifact whose text has a `## Proposed next steps` section can
become one host issue. The index is 1-based. At least two proposals are
required, and only the chosen one is written:

```sh
cadence app run ticket <run-id> \
  --request-id <ticket-request-id> \
  --artifact-id <artifact-id> \
  --digest <sha256-digest> \
  --index 1 \
  --project <project-key>
```

The daemon parses the section and reserves that suggestion. The CLI then
creates one tracker issue through the existing issue writer. The body
records the accept decision, run, artifact and digest, plus a
`cadence-app-ticket-request:` marker, and the command binds the issue
id. Repeating the command returns that issue. A different index or
request conflicts, so the unselected proposals are not written. Decline
cannot reserve a ticket.
A crash after the issue commit and before the bind retries onto the marker
instead of creating a second issue; two markers for one request refuse.

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
