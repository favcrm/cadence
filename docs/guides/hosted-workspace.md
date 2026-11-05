# Hosted workspace: local agents, shared work and apps

Use the hosted workspace as the shared source of truth. A connected local
agent uses an installed native daemon and an outbound authenticated WSS
connection; hosted board and conversation access do not require a local daemon.
An assignment is not proof that a worker ran or reported a result.

The company-container fleet was retired by AgenticOS AOS-374. The tenant
Worker serves the hosted board and supported CLI operations; older references
below to waking a company container describe historical integration evidence,
not a current execution path. Unsupported operations must report unavailable;
do not fall back to a local tracker or claim they ran.

See [verification and rollout](#verification-and-rollout) for the scope of
earlier checkpoints, [REMOTE-AUTH](../REMOTE-AUTH.md) for authentication,
and [Workspace app catalog](app-catalog.md) for local package rules.

## Check your connection

`cadence --org <org> doctor` tells you whether this machine's connection to a
hosted org works and what to do if it does not, on a client-only or native
installation. It sends only a sign-in and one read, then reports each step:
`install`, `selection`, `credential`, `auth`, `read`, `wiki` and `scopes`.

- Exit 0 is ok, 1 is a warning or a result it could not determine, and 2 is a
  failure. `--json` prints the same report for a script; `--offline` runs only
  the local checks and sends nothing.
- It names `cadence login` only when the login can help. An unavailable service
  or an unreachable network says your login is not the problem, and a refusal
  by policy says a new login does not change it.
- It renews a login that is due, as any hosted command does. It does not use a
  native daemon, the local tracker or a local fallback.
- Optional access a login did not ask for is listed with its login flag and is
  not a fault. A new login replaces the saved login's access, and a `--web` or
  `--settings` login does not include the wiki.

## What runs where

| Responsibility | Current owner / effect |
| --- | --- |
| Human membership, CLI credentials and shared agent directory | Hosted authority; IDs and labels are not credentials. Presence is observed from an authenticated runner connection, not a promise that an agent can execute. |
| Assignment state, notification receipts and event outbox | Hosted durable metadata. Events are minimal invalidations; HTTPS rechecks current authority and generation before claims or reports. |
| Local execution and worktrees | A connected agent uses the native daemon; an existing session may claim a named issue with a profile without one. An FYI never starts work. |
| Hosted tickets and wiki | The tenant Worker owns supported hosted reads and writes. Availability is per operation; native-only operations do not become available merely because the board is reachable. |
| Published immutable package bytes | Hosted package storage; an exact listing/version/digest is provenance, not authority. |
| Pure app `needs.requires` compatibility rules | Canonical Rust `crates/cadence-core`, called natively and compiled to WASM for the hosted adapter. Transport, authentication, storage and app execution are not moved into this kernel. |

Directory and FYI metadata do not invoke a model. A connected local agent can
receive an explicit assignment; an FYI is not an assignment. Hosted ticket and
wiki operations use the selected workspace and current authorization. A missing
or unavailable hosted operation is not a reason to fall back to local state.

Shared hosted work stays in its selected workspace; it is not a mirror of the
teammate's standalone local tracker. Sharing currently extracts only the pure
requirements kernel, not the whole daemon or CRM backend into WASM. Each adapter
must supply its actual host version and supported contracts; a shared rule result
does not confer platform capabilities or native execution authority.

**Storage limit:** hosted ticket and wiki data are not a backup of every native
SQLite database, Git tracker or local file. A stored reference, metadata record
or historical checkpoint does not establish that the referenced content or a
native execution runtime is available.

Sources: native [`local_assignment/events.rs`](../../src/local_assignment/events.rs),
[`local_assignment/http.rs`](../../src/local_assignment/http.rs),
[`cli/remote.rs`](../../src/cli/remote.rs), and the [canonical kernel boundary](app-catalog.md#host-requirements-needsrequires).
The hosted source/revision references are listed under verification.

### Hosted conversations without a connected native runner

The separate [`worker-runtime/`](../../worker-runtime/) package owns hosted
browser conversations inside a Cloudflare Durable Object. Its public name is
`@cadence/hosted-runtime`; `HostedSession`, JSON tool contracts and opaque
generation identities do not expose a provider SDK. This is a library used by
the tenant host, not a second daemon or a new hosted service.

Cadence owns the execution loop, model transport adaptation, transcript and
task recovery, bundled prompts/skills and stream lifecycle. The current private
adapter uses Pi Durable; another backend is not implemented or claimed.
AgenticOS supplies the authenticated model proxy and domain capabilities.
Provider credentials, model routing, financial authorization, current tenant
membership and human approval decisions remain with that platform.

Every tool execution rechecks the supplied refusal and live authorization,
including recovered executions and runtime-owned skill tools. Cancellation is
checked before and after the authority check. Tool handlers receive only the
neutral invocation identity, clock and cancellation context, not the engine's
model registry or nested-execution API.

**Tool images.** A tool may return base64 PNG, JPEG or WebP parts alongside
text. The runtime accepts at most four image parts and 192 KiB of encoded image
data per result. Invalid base64, unsupported media types or oversized parts
produce an explicit tool error; the runtime does not decode image contents.
Image bytes are retained in the conversation's private engine records for
recovery, but are excluded from the board thread projection. The owner-only raw
event stream retains the image's media type and a redacted marker, not its bytes.
For each model request the runtime keeps the newest images that fit the
192 KiB total budget and a configurable image-count limit, replacing omitted
parts with an explicit notice to the model. The count defaults to one; a host
may set `maxImagesPerRequest` from one to four only when every model its alias
may resolve to permits that many. The private durable history is not changed.

A gateway refuses image input for a model that cannot take it with a JSON error
whose `error.code` is `image_input_unavailable`. A turn whose newest message holds
the image fails with that named reason. A later request whose images are all older
history is retried once with them replaced by a notice, so the conversation can
continue; its stored history is unchanged.

Image input is off by default. A host may set `acceptsToolImages: true` only
when its model gateway can receive images and verifies the resolved model and
image pricing for each request. Without that opt-in, an image result reports
that image input is unavailable instead of silently dropping it. This runtime
contract does not itself provide browser access, select a vision model or add
image support to AgenticOS's gateway.

**Operator-attached images.** A host can instead supply `attachedImages` to
resolve images belonging to the active operation IDs and their common app
scope. The host must reauthorize the actor and scope, verify the retained
bytes, and prove the selected gateway model and its image accounting before
returning PNG, JPEG or WebP parts. The runtime checks the image count, encoded
size and base64 form, injects the parts into that model request, and refuses
the attempt if the callback fails. Attachment bytes are not written to the
transcript. This callback does not enable image-returning tools; those retain
their separate `acceptsToolImages` opt-in.

**Hosted identity.** A host that has a committed assistant identity may supply
two optional domain-data callbacks on `HostedSessionOptions`: `identity()`
returns personal, workspace, company-agent or agent-profile context, and
`threadContext()` returns the thread's public projection. Both carry data only.
A host may also supply `projectThread` to add entries, such as artifact cards, to
what every thread read reports; its contract is on `HostedSessionOptions`.
Prompt text, model messages, engine hooks and SDK types are not accepted
through them, and a generic host that supplies neither keeps its current
behavior.

When `identity()` is configured, the runtime renders the relevant identity
privately, resolves it for each operation, and stores it once per operation root
in the session object's
existing `tenant_identity_snapshots` table (insert-once; a stored snapshot is
never rewritten). Admission resolves the identity before accepting new work,
and every actual generation must carry the exact message object the runtime
authored for that operation: a text marker, a look-alike object or the current
store cannot substitute for it. A profile change therefore applies to the next
operation, never to an operation already running, and a malformed or
unavailable identity refuses the operation instead of falling back to a generic
prompt. The summarizer's `completeSimple` path keeps its original
normalization, authentication and transport.

An `agent_profile` carries a name, role, revision, up to 8,000 UTF-16 code
units of instructions and up to 32 declared skills. It is context only: it
neither authenticates an agent nor grants capabilities. The host remains
responsible for resolving the profile and authorizing every operation.

**Hosted app scope.** `HostedSession.submit(text, operationId, app?)` and
`ask(prompt, operationId?, app?)` accept the host-verified `AppScope` of the
turn: `{ installId, contextId? }`, with `contextId` absent for an
installation-only binding and both parts in the board's binding grammar (1–128
characters of `[A-Za-z0-9_-]`). The host proves both against its own canonical
store before it submits; the runtime only validates the shape, so a browser
field, a model message or a scope read from somewhere else is never carried as
verified scope. Omitting `app` is Home, and a Home operation's context is
unchanged.

The engine deduplicates a submission by request id alone — a repeated id
returns its existing submission after comparing the submission type, never its
text or metadata — so the runtime owns the check: the first admission of an
operation id records a SHA-256 digest of its text and its app scope in the
session object's `tenant_operation_scopes` table, an identical retry is the
same operation, and a retry that changes the text or the scope is refused
before the engine is asked. Text is hashed as a JSON string so distinct UTF-16
strings, including unpaired surrogates, cannot collapse to the same UTF-8
replacement character. Rows written by the earlier candidate digest refuse
replay rather than being silently reinterpreted. The text itself is never
copied into that table, so the guard leaves no readable message behind when the
engine's own reset or
compaction retires its copy; the rows are kept because they are the operation
ids' bindings, and dropping one would let a replay be re-bound. A refused
admission writes nothing, and the binding is the session's SQLite, not one
instance's memory, so queueing, retries and crash recovery keep the envelope.
Before its first reservation the guard also asks the engine whether it already
holds that operation id in any state — queued, placed or settled. An id the
engine knows but this guard never admitted was submitted by a build that
predates the table, so the engine, not the caller, is the only authority for
what that operation was. The guard therefore adopts it only as the Home envelope
the engine's own records prove: the exact text the engine stored, placed as the
board's own message entry carrying that operation id and no app scope. A placed
input's text and metadata are read from the entry it was placed as; a queued
input has no entry yet, so its draft is read from the engine's own inbox
document. Placing an input and dropping its draft is one engine commit, so a
draft the inbox no longer holds is re-read once from the submission itself and
recovered from the entry it now names — never re-read in a loop — and only a
record that proves neither is refused. An identical retry of such an operation
replays across the deployment, and the adopted row is the recovered envelope,
never the caller's, so a later retry that changes the text or sends an app scope
is refused like any other. Everything else refuses rather than re-binds — a
different text, any app scope, another submission type, an entry of another kind
or with any field beyond `messageId`, a text the engine no longer stores, and a
record that cannot be read — because the engine would replay its own stored turn
while the guard recorded another. Nothing is ever adopted from the request being
admitted.

The admitted envelope is also placed with the input entry (`messageId` and
`app`), so the transcript, a rebuilt object and a replayed retry all read the
same durable record. The board's operator entry projects that stored binding as
`payload.app` in its own shape, `{install_id, context_id?}` — the same object a
`thread_send` retry sends back — and never re-derives one from a caller. A
stored scope that does not validate fails that read or stream instead of being
presented as Home.

Inside a turn, the operation's scope reaches both the model and its tools. A
run can hold more than one input — the engine appends each steering input to
the live run — so the runtime attributes a run only when every one of its
inputs carries the same scope, and refuses a run that mixes scopes instead of
attributing it to one input. Each model request then carries a private,
factual scope statement injected through `beforeRequest` (never a transcript
entry) with just the validated `install_id`/`context_id` and the explicit
limitation that it conveys no tool authority, permission or approval; the
statement is context, never enforcement.

Enforcement is a positive per-attempt attestation at the provider boundary.
The engine only *reports* a hook error and continues the attempt, so a refusal
raised in the hook would not stop the request. Instead the session installs a
guard on its own `Models` at construction, and every attempt the hook serves
authors one fresh message object into the session's set: the statement for an
app turn, and for Home a content-identical clone of the request's last
conversation message (no text is added or changed). `Models.streamSimple`
refuses any request that carries none of them, before the provider is called,
which settles the turn unanswered. A skipped hook, or a hook that cannot
establish the scope — an unattributable run, a mixed run, a malformed stored
scope — authors nothing and is therefore refused rather than sent unscoped. A
fresh object per attempt is what makes an attestation un-reusable by any later
request. Compaction's summarizer keeps the original unguarded path, and the
operation guard composes with the identity guard on the same `Models`: both
checks run, in install order.

Each tool call receives `ToolInvocation.app`, the same resolved scope, after
the refusal and live-authorization checks, so structured admission stays the
authority. A tool call with no active run to attribute it to is refused,
never run as Home. A malformed stored scope or a mixed run
therefore refuses the request or the execution rather than acting outside the
envelope.

Each `/thread-stream` connection reads `threadContext()` once. Its metadata
composes into the `master` frame under the runtime's own fields, so business
identity such as `identity` and `actor_identity` can be added but never
replaces the model, provider, session, turn or queue state. An unavailable or
malformed projection fails the connection with a `503` capability refusal
instead of serving a stale or partial header. `masterState` applies the same
precedence to the HTTP header read.
The composed master frame is serialized once before opening the watch or
returning SSE `200`. Serialization failures (such as cycles or bigint values)
and callable root `toJSON` hooks refuse with `503`. Ordinary data fields remain
allowed; values shadowed by reserved runtime fields do not invalidate the frame.

`HostedSession.recordCount()` reports active stored runtime records, not the
number of expanded UI transcript parts. A host can combine it with `state()`
for release inspection without reading engine snapshots or starting a turn.
Terminal failed turns carry only a safe `payload.status`/`payload.error`
projection. Retry and compaction errors stay hidden while their outcome is
uncertain; raw provider diagnostics, URLs and keys are not presentation data.

From `worker-runtime/`, using its pinned Bun version:

```sh
bun install --frozen-lockfile --ignore-scripts
bun run verify
```

The committed `dist/` artifact includes self-contained declarations, bundles,
Apache-2.0 provenance, third-party notices and file digests. CI rebuilds it and
refuses generated drift. Consumers pin a full Cadence source revision and exact
hashes; normal consumer CI can verify those bytes offline without fetching this
private repository or building the engine.

The existing conversation DO export, private SQLite names, history metadata and
funding identities are preserved. Browser chat does not require a connected PC;
repository work still belongs to the native runner. This module does not move
tenant business storage, add a shell, grant effect authority or establish a
production deployment or abrupt-crash qualification.

## Join once per person; connect explicitly

An existing owner can invite a teammate from an ordinary human shell:

```sh
cadence --org acme team invite teammate@example.com --role member
cadence --org acme team invites
cadence --org acme team list
```

### Join in the browser (no install)

The teammate can accept the invitation and use the shared board without
installing anything. They open the invitation link from the email, sign in with
the address that received it, choose **Join company** to accept, then choose
**Open app**. The board then shows the workspaces they belong to; picking one
opens that workspace's board. Nothing on this path creates a credential,
registers an agent or installs a CLI.

If the browser cannot join, the cause changes the next step:

- **Wrong account** — sign out and sign in with the address that received the
  invitation.
- **Expired, revoked or already used** — ask a workspace owner to invite that
  address again.
- **No matching workspace** — the invitation was not accepted under this
  account, the workspace is archived, or the slug hint supplied to `cadence
  login <slug>` names a workspace this account does not belong to. An
  invitation does not create membership by itself; ask an owner to check
  `cadence --org <slug> team list`. If the login hint is wrong, restart
  `cadence login` without a hint to choose an eligible workspace, or use the
  correct slug.

### Connect a computer (optional)

`cadence login acme` is for the machine that will run Cadence, not a
requirement for reading or updating the board. Each person has their own saved
credential and private configuration; never copy the owner's credential, browser
cookie or another agent's state. A member can approve ordinary CLI/wiki
collaboration. App lifecycle changes below require a current **human owner**.
Invitation acceptance, credential consent and local agent registration are
separate actions; installing an app does not perform any of them.

```sh
cadence login acme
cadence org inspect acme
cadence --org acme agent connect --alias my-worker --repo /absolute/path/to/checkout --name work
cadence --org acme agent directory
cadence --org acme agent assignments
```

Use an intentionally registered local alias whose adapter is configured on this
machine; `connect` is not a provider installer. `--repo` supplies the local Git
checkout for work placement. Connecting without it saves the explicit alias but
does not invent a worktree. `login` leaves your saved default unchanged unless
you request `--use`; these examples keep `--org acme` explicit.

Three IDs have different lifetimes:

- `lag_<UUID>` identifies a stable workspace agent owned by a member.
- `lri_<UUID>` identifies the installed local runner.
- `las_<UUID>` identifies an ephemeral registered session.

The daemon retains stable agent IDs across disconnect/reconnect. Labels,
`client_agent_id`, adapter metadata and any of these IDs confer no permission.
There is one explicitly selected assignment default per member/workspace; extra
sessions do not race it for claims. For an additional explicit FYI-only alias:

```sh
cadence --org acme agent connect --alias my-reviewer --fyi-only
```

This preserves the assignment default and multiplexes notices on the same
workspace connection. Directory pages contain at most 50 agents. If `after_id`
is non-null, continue with `agent directory --after-id <after_id>` until null.
Online/offline and current/expired/revoked registration describe observed
connectivity/authority, not execution readiness. No private path, transcript or
inferred conversation is published by this registration.

Sources: [`cli/commands.rs` (`TeamAction`)](../../src/cli/commands.rs),
[`cli/agent.rs`](../../src/cli/agent.rs),
[`cli/local_agent.rs`](../../src/cli/local_agent.rs),
[hosted login](../REMOTE-AUTH.md#hosted-login-and-remote-org-transport-cad-1019-cad-1329).

## Workspace platform tools

The hosted workspace owns its tool catalog and account selection. List it with
`cli.read`, or call an eligible tool with `cli.write`:

```sh
cadence --org acme platform tools
cadence --org acme platform call generate_image --input '{"prompt":"A blue circle"}'
```

An eligible call can charge the workspace balance. Pass `--org <workspace>`
explicitly when targeting a hosted workspace: if `--org` is omitted and a
remote workspace is saved as `selected` in `orgs.json`, the command routes to
that saved default workspace. Before any network or billing side effect, the
CLI prints the resolved `org`, operation, tool and stable `request` handle on
stderr (`cadence platform org=<org> operation=<op> ... request=<handle>`).
Human-facing price lines state the scale-6 decimal USD amount alongside raw
`chargeMinor` micro-units (`1 USD = 1,000,000` units, so `123456` is
`USD 0.123456`) and distinguish informational `providerCredits` from the
customer USD charge.

The tool's own effect and authorization checks still apply. An absent tool
returns not found; an unavailable catalog is reported as unavailable rather
than as an empty list. Hosted calls refuse `--platform`, `--account` and
`--project` instead of silently changing the selected workspace.

The CLI derives a replay key (`pcli_<sha256>`) from the tool, `--query` and
`--input`, or accepts an explicit `--request <handle>`. If a call fails with a
timeout, 5xx or uncertain outcome, inspect its recorded state read-only before
retrying:

```sh
cadence --org acme platform status --request <handle>
# or derive the handle from the same tool and input:
cadence --org acme platform status --tool generate_image --input '{"prompt":"A blue circle"}'
```

`platform status` uses `cli.read` (or `cli.write`) and never dispatches,
reserves, settles or polls a provider. It reports `recorded` (with the stored
job/usage and scale-6 USD billing state), `unknown` (when a job or delivery
intent is in progress or awaiting reconciliation), or `unrecorded` (when no
record exists for that key in the workspace). An `unrecorded` lookup does not
by itself prove that no upstream work occurred if a fault prevented
persistence, and **replaying `platform call` will start and charge a new call
when the original request was not recorded**. To deliberately run the same
input again after a recorded call, pass a fresh `--request <handle>`.

An image submission may return a queued job. Poll its returned `data.job.id`
with the same workspace and `cli.write` grant, then download a succeeded job's
retained artifact (`data.job.artifacts[].ref`) to a new local file:

```sh
cadence --org acme platform call get_media_job --query '{"jobId":"med_job_from_submit"}'
cadence --org acme platform artifact med_job_from_submit.<64-hex-sha256> --out ./circle.png
```

`get_media_job` is a live GET of the existing job (it can advance and settle
that job, never starting a second generation). `platform artifact` streams the
workspace's retained image bytes, verifies the SHA-256 digest and byte length
in a private spool, and writes `<out>` only after verification succeeds.

## A teammate's own-agent collaboration flow

Use a plain signed-in human shell, or an eligible managed agent's existing
hosted kickoff. **A managed local agent cannot borrow arbitrary saved human
logins:** explicit `--org` is allowed only while exactly one current running
kickoff binds it to that workspace and the original member/issuer still match.
Without that binding it is refused; omitting `--org` retains the inherited
local daemon. Registration and an FYI reference do not create such a kickoff.
Do not work around a refusal by copying tokens or impersonating an operator.

With the intended workspace/path grants, both teammates use the same commands
to deliberately read/write shared knowledge and update the hosted ticket:

```sh
cadence --org acme wiki search "handover" --json
cadence --org acme wiki cat projects/demo/handover.md --meta
cadence --org acme wiki put projects/demo/handover.md --file ./handover.md --if-rev <wiki-rev>
cadence --org acme issue new "Review shared handover" --project demo
cadence --org acme issue show <issue-id>
cadence --org acme issue comment <issue-id> -m "Read projects/demo/handover.md; updating the agreed approach."
cadence --org acme issue set <issue-id> status=doing --if-rev <issue-rev>
```

Use the exact revision returned by each corresponding read, not a guessed one.
To create a new wiki page, pass the create-only sentinel `--if-rev none`, which
refuses the write if the path already exists; to update an existing page, pass
the revision returned by its read to avoid overwriting another writer.
Search results are discovery, not evidence:
read the actual page and cite its canonical path and revision. Wiki and CLI
scopes are separate, and the target path/project still has to be authorized.
An issue `status=doing` update is not a local assignment claim or delivery.

To start the issue on the already connected local default agent, the person
operating that machine explicitly runs:

```sh
cadence --org acme issue start <issue-id> --repo /absolute/path/to/checkout --name work
cadence --org acme agent assignments
```

Hosted `issue start` accepts only its supported local placement options and
claims through the hosted assignment coordinator; it is not an unrestricted
remote operator verb or a job launcher. Dev work requires `--repo`: without one
the client refuses before claiming. Repo-less work opens no lane and needs no
checkout (see below). Existing kickoff/member, claim and generation fences
remain. A claim/queued receipt is not proof that execution or a result
happened; inspect assignment/report state.

An existing terminal or coding session can work on the named issue instead.
Check the owning repository's worktree and integration-branch rules first;
if the supported placement cannot meet them, report the conflict before
starting. Load the profile, then use the project's actual integration ref:

```sh
cadence --org acme agent context ap_<profile-id>
cadence --org acme issue start <issue-id> --repo /absolute/path/to/checkout --profile ap_<profile-id> --base <integration-ref>
cadence --org acme agent report <assignment-id> --state blocked --text "waiting for the owner"
```

`--profile` claims under `session-<profile-id>` without a daemon and targets
only the named issue. Inspect the receipt for the claim, lane and reporting
instructions. The board checks claim and generation on reports, but the
profile-derived client ID is not proof of a distinct session identity.
`--profile` does not read profile content; load it with `context`. Hiring creates only such a profile (CAD-1989): no runtime, provider, sandbox or model is chosen, nothing launches, and `model` stays `inherit` until a runtime resolves it or one is set explicitly. A native board reads a profile the same way with `cadence agent context <alias>`. The
authenticated caller, not the profile, carries authority.

Profile claims depend on the deployed board and client versions. An older
board may still require the member's registered default alias; report that
refusal rather than borrowing an alias or assuming another board has the
same behavior.

`--base <ref>` cuts the lane from that ref and echoes it in the receipt; a branch
base that origin carries resolves to origin's current tip, and an unreachable
origin refuses; without
it the lane follows the checkout's own resolution. The alias path refuses
`--base` rather than ignoring it. A board that predates profile claims refuses
the field: the claim then proceeds without it, under the same client id and
fences, and the client says on stderr that the profile was not recorded.

### Repo-less work: research, design, business and manual (CAD-2195)

The board records a `kind` for an assignment. `dev` (and a row with no `kind`,
which is what a board that predates kinds sends) is checkout work. `research`,
`design`, `business` and `manual` are repo-less: they are claimed, delivered and
reported with no lane, no worktree and no Git head.

```sh
# name the kind on creation; no --repo, no checkout
cadence --org acme issue start <issue-id> --kind research --profile ap_<profile-id>
# or claim a row the board already queued with that kind
cadence --org acme issue start <issue-id> --profile ap_<profile-id>
cadence --org acme agent report <assignment-id> --state running --text "outline drafted"
cadence --org acme agent report <assignment-id> --state result \
  --text "Three interviews and a research memo" \
  --deliverable link:https://docs.example/<memo>
cadence --org acme agent report <assignment-id> --state result \
  --text "Brand audit complete" --deliverable artifact:<artifact-ref>
```

`--deliverable` is typed as `artifact:<ref>` or `link:<https url>` and is only
accepted on a `result`; the board stores the reference and reports separately
whether it resolved it, so an echoed reference is not proof the output exists.
A digest of a document is not a commit: `--head` is refused for repo-less work,
and the board refuses a head it was not asked for. The dev contract is
unchanged: a `dev` result still requires `--head` of 40 or 64 lowercase hex
characters.

The board can store a session key as claim-contest metadata, but this client has
no verified per-runtime identity of its own. A POSIX session is shared by
nested runtimes and by every command in one terminal, so it is never sent as
the claim session. A brand-new claim therefore records no session and keeps the
board's legacy client/profile and generation fence; the client says so on
stderr. Two sessions sharing that legacy client/profile are not distinguished.

The client never copies a key out of an existing claim to impersonate its holder.
It refuses a key-bearing claim unless a person explicitly supplies
`--take-over "<reason>"`; a board that cannot record the take-over is refused
rather than silently dropping it. `claim_last_seen_at` is the time of the last
accepted claim or report receipt, not proof of execution.

`--kind` is accepted on hosted `issue start` and `dispatch`; it is only sent
when it is not `dev`, so a default create body is unchanged. A local (lane)
start or dispatch refuses `--kind`. A board that predates work kinds refuses
the field and the client says so instead of creating dev work.

A kind this client does not run is still listed and fenced, and refused where
work would start.

`review` is a named unsupported capability in this client: review work is not
created, claimed or picked up until the board proves exact PR/head binding and
a self-review refusal. A `review` row is shown and fenced, and `--kind review`
is refused with that reason. Approval, a merge and an external effect are
never inferred from a review, and the client never infers the reviewer from a
profile name, an alias or ticket text. The client announces the kinds it
can run in `X-Cadence-Client-Features`. Against a board that does not record
work kinds, a non-code claim or report is refused with `unsupported_work_kind`
and nothing local is started: it is never retried as dev work. A board that
changes a row's kind between listing and claim answers `work_kind_mismatch`,
which is a refusal, not permission to run the row as something else.

A `--state result` report ends the assignment but moves nothing else. Once
the work meets the project's acceptance and completion rules, use the
selected workspace's supported ticket update, for example:

```sh
cadence --org acme issue set <issue-id> status=done --if-rev <issue-rev>
```

Hosted `issue finish` never removes a lane or runs the local tracker finish.
Its refusal directs the worker to the reconcile sweep and reports a local
lane observation only when it can identify the recorded checkout and verify
its state. An unavailable or ambiguous observation does not mean the lane
is clean or merged; leave it intact for reconciliation.

### Managing agent profiles from the CLI (CAD-2101)

The owner can do from the CLI what the agent page does for a profile. Name the
org with `--org` on every call. A saved default org or `CADENCE_ORG` does not
count: a profile command that would use one is refused before any request
(`org_not_named`), reads included, on a full install and a client-only one
alike. The echoed `org=` line is the destination.

```sh
cadence --org acme agent profile get ap_<profile-id>
cadence --org acme agent profile create ledger --catalogue accountant [--job-file job.md|-] [--model <m>]
cadence --org acme agent profile edit ap_<profile-id> --expect-rev 3 \
  --set role="Bookkeeper" --set-json 'skills=["invoices","payroll"]' --file responsibilities=-
cadence --org acme agent profile archive ap_<profile-id> --latest
```

- Every command prints one JSON document, the stored profile with its
  `revision`. `edit` sends only the fields you name; the board merges them onto
  the profile at the revision you give and leaves the rest as stored.
- Field names are the board's. `--set FIELD=VALUE` sends text, `--set-json
  FIELD=JSON` sends a value that is not text (`skills` is an array), and
  `--file FIELD=PATH|-` sends a file's or stdin's text unchanged. A hosted
  board trims it when it saves, so a final newline doesn't survive. The client
  holds no field list and no limits: the board refuses an unknown field or a
  value that is too long, with its own code (`invalid_request`).
- `edit` and `archive` need `--expect-rev N` or `--latest`. `--latest` reads the
  current revision first and says so on stderr; it still fences that write, but
  against whatever was stored when it read. A stale revision exits 5, prints the
  board's `revision_conflict` and writes nothing. `archive` is an edit of
  `status`.
- Refusals keep the board's code: `not_permitted`, `unauthorized`, `not_found`,
  `alias_taken`, `invalid_request`. They exit 3, like every other hosted
  refusal. A write whose answer is lost or is a server error, including
  `capability_unavailable`, exits 1 (outcome unknown) with the read to run
  before repeating it (`agent profile get`, `agent profiles`).
- Creating and editing are a live human owner's act. A member is refused by the
  board. A board that predates these commands answers "update the board first";
  nothing is written.
- A native board has a daemon path for `create` only (the one behind
  `POST /api/agent-profiles`). `get`, `edit` and `archive` refuse and say what
  is missing; they never fall back to another store.

**On its own, a managed agent cannot be stopped from the server.** With `CADENCE_ALIAS`
(or a grant) set, the client refuses a profile write: "a managed agent can't
edit profiles; propose the change". That is a client check and nothing more.
The hosted board refuses an `agent` or `service` principal, but a managed agent
that runs under a member's or owner's saved human login arrives as that user,
and the board has no fact that tells the two apart. So an agent that bypasses
the client (a direct call with the saved login, or an unmanaged shell) is judged
as the person whose login it uses. Treat the saved login as the person's, keep
it out of agent environments, and have agents propose changes (`agent
propose-hire`). The next paragraph is what a hosted kickoff adds.

**A managed session has a credential of its own (CAD-2149, AOS-502).** On a
hosted kickoff, the session's hosted CLI verbs are sent on an `agent` credential
bound to its claim. A comment, an attachment and a review are recorded as the
agent. `issue new`, `issue set` and `issue say` still record the member, as do
`my_work` and the member's private artifacts. The board also refuses the
owner-only writes: profile writes, team changes and `platform call`, even on an
owner's assignment, and `team list` is refused for a managed agent. The client
mints it on the member's login at first use and caches it 0600 beside the
kickoff's `.bind` file, for at most 15 minutes; there is no refresh token, so
renewal is another mint, and the file goes when the assignment ends.

Stays on the member's login: claim, daemon reports and `agent report`; every
wiki action; `artifact put`; and the audit verdicts (`audit_verdict`,
`audit_verdict_project`). Not available to a managed agent session at all:
`settings agents` and `master status`. They always refuse, and their message
talks about a login grant; read it as "a managed agent session can't use this".

**This is attribution and server-side refusal, not a security boundary.** The
session runs as the same OS user and can still read the member's login file
until CAD-2148 confines it.

What the client does when the credential is not available:

| Board answer | Client |
| --- | --- |
| mint `404`, or `409 renewable_login_required` | keeps the login, one line on stderr |
| authorize or call `403 human_session_required` (a workspace not yet updated to accept it) | keeps the login for the rest of the command, one line on stderr |
| any other mint answer, or an unusable `200` | refuses; nothing is sent and the login is not used in its place |
| the agent's own refusals: `401`, `403 forbidden`, and the rest | final, never retried on the login |

### Leaving a message for a role on a ticket

A session can leave a message for whoever holds a role on a ticket. The role is
a label, so the next holder still gets it after the first session has gone.

```sh
cadence --org acme issue say <issue-id> --to developer --from reviewer \
  --message <stable-id> -m "REVISE: see the verdict on the new head"
cadence --org acme issue messages <issue-id> --to developer
cadence --org acme issue messages-read <issue-id> --role developer --through <last-seq>
```

- `state: "stored"` is all a send proves. Nothing wakes a session or proves a read.
- To retry a send whose answer was lost, repeat it as the same member with the
  same `--message`, `--to` and text, and with `--from` only if the first send had
  it. The same id with anything different is refused.
- Read with `--to`, and repeat with `--after <last-seq>` while `more` is true.
  Without `--to`, the unread map stops at 100 roles and `unread_more` says so.
- Pass `--through` the last `seq` you received. The read point is shared by
  everyone in the role, so one reader marking read lowers the unread count for
  the others. The messages themselves stay readable with `--after`.
- `from` is a label. `sender_subject` is verified, and it is the member, not the
  session. A message approves nothing and changes nothing on the ticket.

### Reviewing software changes

Find out what a PR needs before starting reviews. The caller needs `gh` read
access to the repository:

```sh
cadence --org acme delivery requirements --issue <issue-id> \
  --pr https://github.com/<owner>/<name>/pull/<number> --head <full-sha>
```

`requirements.reviews`, `review_kind`, `operator_approval` and `browser_qa` are the
requirement for that exact head, computed from the policy recorded on the board
and the repository's path lists (`docs/roles/risk-paths.toml` and
`docs/roles/one-review-paths.toml`), which are read at main's live tip (the PR's base for a
non-main PR), so a PR cannot weaken its own rules. The merge gate can add an operator approval: when a
head-pinned verdict's Risk is not `auto`, and under the workflow auto-paths rule
(AGENTS.md "Review"). On a hosted org, an active `routine` class reports zero
reviews and no approval here, but the merge gate also requires a pass from both
review workflows, and those rows carry the auto-paths rule.

The review standard (operator decision 2026-11-03) is a
fresh-session review: a session that is not the author and inherits no
conversation or context from the work, including a subagent (a fork or a
resumed authoring session does not count). Record the actual reviewer/session,
repository, full head, outcome, checks and limitations, and call it a
fresh-session review, never an independent party. The gate does not enforce
that standard yet: it counts hosted verdicts by attested principal and refuses
the author's principal (local notes count by their self-declared `From:`
alias, which is not attested), so a profile or separate session alone does not make a
gate-counted verdict, and in-chat findings are not one either. Until the gate
can tell a fresh session from the author (the workflow-issued reviews of
CAD-2081 are live as a cheap first pass beside fresh-session reviews on gate, security,
schema and release PRs, and two of them satisfy other PRs; AGENTS.md "Review"), a hosted
`ui/**` PR takes two workflow passes, the operator's approval and `--browser-qa <url>`,
naming an unedited comment on the PR by a repository member that is exactly the two lines
`Browser QA: pass` and `Head: <full sha>`, with the report in another comment (CAD-2173). That binds the claim to the head but cannot prove the QA ran, which is
what the approval accepts. Until it ends, it can instead merge with the temporary
`--exception --advisory ...` path (exact-head operator approval and two
advisory links, standards and spec-security, plus browser-qa for a `ui/`
change, even where the board asks for fewer or no approval; ends at
`EXCEPTION_ENDS` in `scripts/delivery-gate`, 2026-12-31); see AGENTS.md "Review".
A hosted `routine` PR has no readiness record, because a hosted board has no
delivery record to prove it from. It takes a pass from both review workflows at
its head and the operator approval a workflow row needs; the gate's refusal names
what to dispatch. The exception also still waives this until it ends. The exception
only checks that the advisory links are separate https URLs; it proves neither
head binding nor that Browser QA ran. Agents in this workspace do
not run `audit verdict` below: they post findings as a PR comment and use it as
the advisory link (AGENTS.md "Review"). The command is documented for what it does.

```sh
cadence --org acme audit verdict --project <project-key> --repo <owner/name> \
  --issue <issue-id> --pr <number> \
  --head <full-sha> --kind standards --outcome pass --risk auto --file findings.md
```

`--project` is the issue's project and is required: the evidence is that
project's, and the project is never inferred from `--repo` or the cwd. The org
refuses a repo the project has not declared (`repo_not_bound`), an issue that
resolves to another project (`project_mismatch`), a caller the owner has not
made eligible for the project (`reviewer_not_bound`) and a caller with no
attested login (`identity_unattested`). `--kind` is `standards`,
`spec-security`, `combined` or `browser-qa` (the kinds the merge gate counts;
`spec` is retired); `--outcome` is `pass` or `revise`, and `--title-outcome` /
`--section-outcome` default to it. The reviewer is the authenticated caller,
never an argument. A repeat of the same kind, head and findings returns the
latest row with `duplicate: true`; a changed outcome appends a correction, and
the gate reads the latest row per reviewer and kind.

Read them back under the same org with `cadence --org acme audit verdicts
--project <project-key> --repo <owner/name> --issue <issue-id> --pr <number>
--head <full-sha>`; a hosted selection answers from the org, never from local
notes, and refuses an answer naming another project, repository, issue or
head. Add `--body` to also print each note's findings as `body` (with
`body_truncated`), which is how a `REVISE` is read; the gate never passes it and
the text never decides a count. The text is the reviewer's, so control
characters, format characters (Unicode Cf), default-ignorable code points such
as variation selectors, and line separators print as U+FFFD. Characters outside
those sets that merely look blank, such as U+2800, and the note's `risk` print
as sent. The text is bounded at 16 KiB per note and 128 KiB per answer on the
board, with the newest notes' text kept first, so an older note can read empty
with `body_truncated` set when the budget runs out. A board that predates the field still answers
the rows, prints a line on stderr, and marks `bodies: "unavailable: …"`; local
orgs refuse `--body` because their notes are files you can open. The merge
gate also runs whichever `cadence` you point it at: set
`CADENCE_BIN` when more than one build is on the host, or a stale client's
missing hosted verb will read like a gate refusal.

A hosted verdict counts only on the owner's attestations (AOS-473, CAD-2001),
except a workflow-issued row (CAD-2081; the workflows are live), which
has no login and counts only for a principal in
`docs/roles/review-workflow-principals.toml` at main's tip (AGENTS.md "Review"):
the workspace owner binds each principal's GitHub login once for the workspace,
then admits principals to a project's review set. A code verdict needs both,
because the gate counts a hosted document by principal and excludes the author
by the attested login:

```sh
cadence --org acme audit identities set \
  --identity <subject-id>=<github-login> [--identity ...]
cadence --org acme audit reviewers set --project <project-key> \
  --reviewer <subject-id> [--reviewer ...]
cadence --org acme audit reviewers --project <project-key>   # read both joins back
cadence --org acme audit identities                           # the workspace map
```

Both writes are the owner's own act: they need the opt-in `audit.approve`
login (`cadence login --audit-approve`), and an agent's call is refused.
Leaving a principal out of the project's set stops new verdicts for that
project only; rows already filed keep their stored principal and login and
stay countable for the head they reviewed. A principal listed with no attested
login reads back with `github_login: null` and its verdicts cannot count yet.
Never impersonate a reviewer: the verdict names the authenticated caller, and
the owner's attestations are the only thing that makes that caller count.
Where the org cannot answer, the gate refuses; a missing hosted read is not a
pass, and the repository's approved merge path and any required operator
decision remain in force.

CAD-2001 release order: the tenant server must serve the project-scoped review
verbs before this client sends them. An older tenant refuses the names
(`not_implemented`/`invalid_request`/`container_required`/
`capability_unavailable`) and the client says so — update the tenant server
first, then this Cadence client. The client never falls back to the older
repo-scoped evidence when the project is known, and the gate treats that
refusal as a refusal, never as "no verdicts were filed".

To give the other teammate an informational pointer, obtain their exact `lag_`
ID from `agent directory`, then send an explicit FYI:

```sh
cadence --org acme agent notify <lag-uuid> --operation-id handover-review-1 --text "Shared handover and ticket are ready for your review." --issue <issue-id> --wiki projects/demo/handover.md
# Recipient, on their connected machine:
cadence --org acme agent inbox
cadence --org acme agent watch
```

`--issue`/`--wiki` are pointers only. They do not fetch/upload content, grant the
recipient rights, start the ticket, send an execution instruction, or trigger
inference. The recipient deliberately reads the referenced resources under
their own current authority. `watch` observes the daemon's durable inbox; it is
not a second claim-polling loop, and it may start a stopped daemon through its
normal startup path. `inbox` (including `--clear`) is observation only and does
not start a stopped daemon: a stopped daemon is reported as stopped, unchanged.

Notify requires an explicit operation key (1–120 units), text (1–4000 UTF-16
code units), at most 16 total references (each 1–200 units), and a bounded
request. Retain its operation ID/receipt. Identical intent under the same
sender/workspace/audience/key/recipient returns the same notice; changed intent
under that key conflicts. A missing or unproved response is an unknown outcome
(exit 1), not a new operation. Inspect the same workspace and use only that
identical operation identity and content for recovery. An FYI receipt means
**stored**, an event ACK means **native custody**, a claim means **claimed**, and
only the domain report says **running/result**.
None proves exactly-once external effects or supplies send/deploy approval.

Sources: [`cli/remote.rs`](../../src/cli/remote.rs),
[`local_assignment/notifications.rs`](../../src/local_assignment/notifications.rs),
[`cli/org.rs` managed resolution](../../src/cli/org.rs),
and [remote ticket writes](../REMOTE-AUTH.md#remote-ticket-writes-cad-1179).

## Offline, reconnect, revocation and capacity

- The native daemon persists a bounded private inbox/cursor by atomic replacement,
  file fsync and parent-directory fsync **before ACK**. Event IDs are deduplicated;
  sequence numbers are increasing but sparse per recipient, not contiguous.
- Reconnect resumes the saved cursor. A replay gap/resync uses all assignment and
  immutable-notice snapshot pages under one captured watermark, then resumes newer
  events. Older generations cannot replace newer local custody. A transient claim
  failure retains its pending invalidation; blocked work needs explicit resumption.
- Native custody is bounded to 512 entries / 4 MiB. The server limits unacknowledged
  events to 64 with a 30-second progress deadline. Slow consumers are disconnected,
  not treated as successful execution. When the inbox is full, inspect it and use
  `cadence --org acme agent inbox --clear` after reading: this removes displayed
  FYIs locally, not pending assignment invalidations, claims or hosted receipts.
  Durable recovery continues from its saved page; clearing does not authorize work.
- Hosted notice storage separately caps a sender at 4096 notices per
  workspace/audience. `recipient_or_storage_unavailable` is an honest refusal,
  not evidence of delivery. Clearing the local inbox does not delete that hosted
  history or create capacity there; do not manufacture new operation keys to
  bypass a refusal.
- Credential renewal/rotation may continue only for the same pinned member,
  issuer, organization and audience. It does not adopt whichever org becomes the
  global default. Use separate private native state for another identity. Current
  membership/grant removal fences commands and delivery; reconnect is not a bypass.
- `cadence --org acme agent disconnect` disables the local binding and, as a
  separate step, attempts to unregister this runner's roster/default where hosted
  authority permits. It can start a stopped daemon before disconnecting, and it
  does **not** stop existing actors, cancel claims, revoke the credential or erase
  business data.
  `directory_unregistered: false` means remote unregister was not confirmed, even
  though local delivery is off. Server revocation is a separate control.

Do not replay an uncertain mutation to make a warning disappear. Reconcile the
same workspace, member, assignment/generation or operation first. The persistence
and fencing mechanisms above are implemented contracts, not a claim of completed
hard-power-loss testing; see the actual exercised scope below.

Sources: [`local_assignment/events.rs`](../../src/local_assignment/events.rs),
[`local_assignment.rs`](../../src/local_assignment.rs),
[`local_assignment/reporting.rs`](../../src/local_assignment/reporting.rs).

## Install apps: local stays local; hosted pins a published release

The local operator journey remains `cadence app catalog install-check <path>`
then `cadence app catalog install <path> --expected-digest <sha256-digest>`;
local upgrade/approval/run behavior is unchanged. The reachable local daemon and
operator requirement remain. See [app-catalog](app-catalog.md) and
[workspace app upgrade](workspace-app-upgrade.md) for the local details.

The selected-remote steps below describe the earlier native-hosted app
journey. Check the selected tenant's actual supported operations before using
them; the retired company container cannot be woken to satisfy a refusal.

For a **selected hosted workspace**, use a plain human owner's saved renewable
login, never a managed agent spending the owner's credentials. Local paths/Git
sources are refused remotely: nothing is uploaded and there is no local fallback.
Follow the exact [selected-remote published-release commands](app-catalog.md#selected-remote-immutable-published-releases)
in the catalog guide rather than substituting local `install`/`upgrade` syntax:

1. `release-check` the exact listing/version/digest and inspect the complete JSON
   review (bytes/access, compatibility, warnings and readiness).
2. `release-install` with that unchanged review and a retained operation UUID;
   inspect `ls`/`show` readback. Apply rechecks the structured review, so changed
   bytes, access, readiness or installation generation require another check.
3. For an update, `release-check` with the current installation ID, old digest and
   server-returned `generation`, inspect the new review, then `release-update`
   with the same old pins and exact new release. `release-revoke` pins the current
   digest/generation and uses native installation-wide approval semantics.
4. After a missing response, retain the UUID printed on stderr and read
   `operation <uuid>` / `pending <app-name>`; use `reconcile <uuid>` to observe
   native state or cancel an undispatched preparation, **not** repeat a mutation.

`prepared`/`dispatched` do not mean executed. A current owner can discover/reconcile
pending work after its creator leaves. Exit 5 means unresolved; exit 3 means
refusal/unavailable. A lost response never authorizes a new UUID. Same-ID recovery
remains bound to the original actor, release, old installation pins and review;
changed input conflicts. Installation does not grant worker registration,
run approval or send/deploy/tool authority; revoke does not delete business data
or settle uncertain external effects. D1 journal metadata is not a second
install/run/business authority.

Sources: [`cli/hosted_app.rs`](../../src/cli/hosted_app.rs),
[`hosted_app_cli.rs`](../../src/hosted_app_cli.rs),
[package consent and kernel boundaries](app-catalog.md).

## Reason Network: useful patterns, different authority

[Reason Network's README at `f8efcbaa087e67f525bbc8db131140c51b8e1910`](https://github.com/reason-machines/reason-network/blob/f8efcbaa087e67f525bbc8db131140c51b8e1910/README.md#how-it-works)
describes one daemon per machine, a team directory of agent sessions, delivery
adapted to agent capabilities, and queued offline messages. Those are useful
patterns behind this pragmatic approach: reuse the installed daemon, distinguish
stable identity from a session, and distinguish storage/delivery from execution.

The architectures are not the same. Reason describes direct iroh peer-to-peer
connections with end-to-end encrypted relay fallback, session discovery and
repo/branch/activity sharing. Cadence instead uses an explicitly registered,
hosted workspace directory and one outbound WSS connection per workspace, with
server-derived membership and HTTPS domain authorization. An FYI is inert here,
not a request to wake another agent. There is no Reason daemon, protocol import,
wire interoperability or claim that the hosted service cannot read notice text.
Private transcripts are not crawled; explicit shared wiki/ticket references are
the collaboration boundary. This comparison is a source-based design comparison,
not a benchmark or a claim of a dependency on Reason's code.

## Startup and cost: bounded architectural inference

**[INFERENCE]** Metadata-only events avoid invoking a model just to discover
presence, register an agent or store an FYI. Reusing the installed native
daemon for explicitly connected local agents avoids a second local service.
The pure WASM requirements kernel answers its bounded rule question; it does
not establish availability of a hosted native domain or execution path.

No startup latency, monthly saving, infrastructure bill, commercial price or trial
allowance was measured/established by this implementation. Active WSS traffic,
metadata/database operations, package storage, actual runtime wakes and model work
still have costs; workloads/provider tariffs decide the result. Reason's product
claims are not Cadence price or performance evidence.

## Verification and rollout

### Observed scope at consolidation (2026-10-22)

- The release owner's actual native-process checkpoint exercised standalone
  daemon/status/stop, a genuine Worker OTP+PKCE-issued `hct_`, 66 FYIs over one
  verified-CA outbound WSS connection with continuous ACK-window progress,
  durable inbox/offline replay after native restart, and no root work messages
  or provider invocation. Public and lower-level stopped-daemon inbox/clear
  variants refused without restarting the daemon or doing outbound work.
  The private fixture imported the genuine issued credential: this was **not**
  proof of the interactive CLI `login` UX.
- The release owner's later full app CLI journey passed through a genuine
  owner `hct_`, real Worker, signed runtime relay and actual native UI/kernel.
  It exercised `release-check`, install, list/show and operation readback; an
  exact-old-pin update to a synthetic immutable `0.10.1` release; replay of the
  original install UUID without overwriting the updated digest; stale-generation
  revoke refusal; exact revoke readback as unapproved; and receipt/read-only
  reconcile. Published package/version metadata and release bytes were isolated
  fixtures served by the actual local Worker, not a production catalogue rollout.
  This is app-lifecycle evidence, not approval to execute a provider or deploy.
- The retained [native public app acceptance](../../tests/cad1739_hosted_app_public_acceptance.rs)
  exercised seven RPC/loopback-HTTP guard groups: operation correlation is not
  operator authority; stale update/revoke preserves consent/scopes; active-run
  refusal permits a later standalone upgrade; and journal/entered-writer
  uncertainty remains explicit, on both transports. These use synthetic
  test-seam caller/grant fixtures, not production authentication or a provider.
- A throwaway two-state native smoke on `5ed960195af9beda4e974014dd4f11c324ee1e07`
  passed 1/1: separate company/teammate native states performed real
  selected-remote wiki create/read/revision update (with stale-revision refusal)
  and ticket create/update (with stale refusal and standalone isolation); an
  explicit assignment dispatched, was read by bare GET, claimed, ACKed and
  delivered to the inbox while reporting truthfully `running`→`blocked`; a
  daemon restart preserved the original claim without duplicate delivery, and an
  explicit default switch afterwards retained the original claim, generation and
  member pins. The harness was removed after the run, so the retained evidence is
  the pass summary and asserted inbox contents — the runner suppressed console
  logs. This is one explicit collaboration path, not interactive CLI `login`
  UX, a production deployment, or a measured cost result.
- The [historical POC](https://github.com/agenticos-stack/agenticos-v2/commit/0aaafd244adbbad854ca74c4208ffb16bd67b4ee)
  exercised 13 synthetic-authority/loopback **plain WS** groups. It is not WSS,
  genuine-auth or abrupt-power-loss durability proof. Pure-kernel native/WASM
  parity is a separate check, not a substitute for the integrated native adapter
  path exercised by the two-state smoke above.

The persistence, pagination, generation, same-member renewal and revocation
mechanisms described above are source contracts; these checkpoints do not claim
that every capacity/gap, identity-rotation, held-context cancellation, concurrent
reassignment or abrupt-loss/whole-company restore case passed. The two-state smoke
covers one explicit collaboration path; it is not interactive login UX, production
deployment or a multi-host capacity trial. Restart replay is not proof of
zero-loss multi-store recovery. No startup/cost benchmark was run.

### Source and release boundary

Native source links in this guide resolve within the revision containing the
guide. The companion hosted work is [PR #949](https://github.com/agenticos-stack/agenticos-v2/pull/949):
`apps/api/src/hosted-cadence/{agent-directory,local-events,app-lifecycle}.ts`,
`apps/api/src/hosted-app-lifecycle.ts`, `apps/api/src/hosted-app-installations.ts`,
and `docs/architecture/company-data-boundary.md` (the CAD-1739 storage limitation).
Use the final release PR heads, not the historical POC SHA, to identify the
implemented native/hosted pair. The final canonical native Git revision and
source/WASM artifact hashes belong to the hosted packaging pin; a native source
freeze alone is not proof that its final pinned artifact checks passed.

Before rollout, the release owner must bind the matching native, hosted contract,
migration and kernel artifact versions to the release and satisfy the
repository's exact-head checks/reviews/approval and remaining outcome acceptance.
Metadata/event migration rollout must retain its writer fences; do not expose
mixed unsupported writers or interpret a capability-unavailable response as a
fallback grant. Local operators retain native journal/business-data recovery
responsibility. Package consent and credential enrollment do not approve a
production deployment, live data migration, live send or installed-daemon restart.
