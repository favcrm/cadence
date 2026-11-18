# Hosted workspace: local agents, shared work and apps

Use the hosted workspace as the shared source of truth. A connected local
agent uses an installed native daemon and an outbound authenticated WSS
connection; hosted board and conversation access do not require a local daemon.
An assignment is not proof that a worker ran or reported a result.

The tenant
Worker serves the hosted board and supported CLI operations. Unsupported
operations must report unavailable; do not fall back to a local tracker or claim
they ran.

See [REMOTE-AUTH](../REMOTE-AUTH.md) for authentication,
[Delivery](delivery.md) for how work is reviewed and approved,
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
or earlier checkpoint does not establish that the referenced content or a
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

An owner can review a write staged in an installed app's assistant conversation
beside that conversation. **Review** shows the stored changes and declared
effects; **Not now** leaves it pending. Approval may still fail to apply, so
the conversation's Updates shows the recorded outcome, not just the decision.
If a reply is lost, **Check again** re-reads the outcome rather than submitting
another decision. Reads and decisions retain the selected conversation; the
Home queue remains separate. Members cannot decide these requests.

Every tool execution rechecks the supplied refusal and live authorization,
including recovered executions and runtime-owned skill tools. Cancellation is
checked before and after the authority check. Tool handlers receive only the
neutral invocation identity, clock and cancellation context, not the engine's
model registry or nested-execution API.

The AgenticOS host's ticket proposal card shows the destination project key
(trimmed as the board trims it), the ticket text, the agent, its tools and
the company-visible audience before the person chooses **Create and assign**.
An incomplete or malformed project key cannot produce an actionable card. The
board still validates and authorizes the create; showing a destination neither
registers a project nor grants access.

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

The owner-only diagnostic route `/api/assist/events` (forwarded to the
runtime's `/events`) sends SSE data frames containing
`{"type":"snapshot","version":1,"view":...}` followed by
`{"type":"view","version":1,"view":...}`. Each view replaces the previous
one; these are not the individual engine events sent by the older protocol.
The host must authorize the owner on every connection. Views may contain
private conversation, reasoning and tool data even though image bytes are
redacted; they are not a member-facing feed.

Pi Durable 1.5 upgrades stored conversation and task formats. A 1.4 runtime
cannot read that upgraded state: before deploying, stop writes and take a
consistent, restorable backup. Roll forward or restore the backup rather than
starting 1.4 against upgraded data.

`HostedSession.settledOperation(id)` inspects durable records without opening or
waking the engine. It returns `settled`, `absent` or `unknown`; positive
evidence requires all conversations and background tasks to be at rest.
Malformed or inconsistent records return `unknown`, and failed reads throw.
Absence is not a fence: the host must separately prevent or await admission
before relying on it. The result is a point-in-time proof, not evidence of
external effects or of another deployment's state.

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

**Context budget.** The runtime shortens long tool results from completed
earlier turns in model requests, with an explicit incomplete-content notice.
When a request would still exceed the gateway's 256 KiB text budget, it also
shortens tool results, oldest first and including the running operation's,
keeping the newest round and at least the two newest results whole, until the
request fits (CAD-2414). This shaping never
changes the stored transcript. Artifact reads may be paged; use the returned
continuation rather than assuming an earlier excerpt is complete.

Hosted sessions compact before that budget: the runtime sets Pi's compaction
reserve so blocking compaction is due at about 48k tokens and background
compaction at about 31.6k, and keeps an 8,000-token tail (CAD-2414). Measured
on the hosted default model, a request costs 3.45 bytes a reported token in
English and 3.85 in Chinese, so 256 KiB holds about 68–76k tokens. Where Pi
estimates instead of reading reported usage (3.5 characters a token), Chinese
costs about 10.5 bytes an estimated token; the 8,000-token tail keeps the
first request after a compaction small, and the shortening above keeps a
step's newest tool results in budget.

When the gateway refuses its byte budget with a JSON 400 whose `error.type` is
`invalid_request_error` and `error.code` is `context_budget_exceeded`, the
runtime records a task-bound marker and lets the engine attempt at most one
overflow-triggered compaction per generation. The selected summary must not
consume the current operation; if a safe cut is unavailable, it is declined.
A long summary source is split into complete messages for the gateway's
per-message limit, without discarding its content.

The summary is a separately funded model request, identified by the actual
durable compaction task and its pinned checkpoint tail, not by a concurrent
generation. Missing or ambiguous task provenance refuses the request before
dispatch. If compaction fails, finds no safe cut, or the retry still exceeds the
budget, the transcript reports `The context limit was reached. Start a fresh
conversation or split the task and retry.` only when the marker and the
entry's authoring task agree. It remains hidden while recovery is in progress.
The marker survives a restart and is invalidated before a repeated generation
request; unrelated HTTP or streamed error text cannot claim this reason. Raw
gateway text stays private and the gateway's limits are unchanged.

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

**Workspace skills.** A host may supply `workspaceSkills()` on
`HostedSessionOptions` (CAD-2395) to serve the skills a workspace owner has
approved. Each entry is a name in the bundle tag grammar, a one-line
description, a `SKILL.md` body, `references/<leaf>` text files and a version,
within the bundle grammar's skill caps. The runtime reads it once when a
conversation's harness opens and lists each kept skill as `ws-<name>` after the
bundled skills, so a workspace skill never replaces a bundled procedure. Every
catalog description, body and reference it returns is framed as untrusted
guidance; references are always served as text. An entry that breaks a rule is
dropped whole, and a host refusal leaves the bundled skills unchanged. Only
approved revisions belong here; a draft never does, and a skill never grants,
adds or selects a tool.

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

**Recovering an exact reply.** `HostedSession.operationReply(operationId)` is a
read-only host RPC. Opt in with `captureOperationReplies` only for a
host-authorized dedicated conversation. The runtime records the input and
answer IDs when the engine commits terminal settlement, in the same
transaction; it never copies reply text into the receipt or backfills old
operations. The lookup uses that receipt to verify the exact assistant entry
even if unrelated work is active. `unanswered` proves the input finished
without an answer. `unconfirmed` means the runtime cannot prove an outcome
(including active work, missing receipts, conflicting links, or shared
answers). It is never permission to resend. The host must prove the reader
is entitled to the conversation and bind the lookup to its own approved
receipt. This RPC does not accept or deliver a handoff.
Opting out at startup stops capture and hides existing receipts without
deleting them. Re-enabling never backfills results completed while disabled.

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
request. These generation guards compose on the same `Models` in install
order. The compaction summarizer does not receive those generation context
messages; its separate task-bound funding check fails closed instead.

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
The watch attaches before the initial master state is read, closing the gap
between that read and subscription. The composed frame is serialized before
SSE `200`, then refreshed on turn and queue transitions. Serialization failures
(such as cycles or bigint values) and callable root `toJSON` hooks refuse with
`503`; a later state failure ends the stream. Ordinary fields remain allowed,
and values shadowed by reserved runtime fields do not invalidate the frame.

The thread stream also sends `preview` events with `{ "text": string | null }`
for in-flight assistant text. They have no `id` and are not transcript
entries: retry, cancellation or reconnect can replace them, while the committed
entry and its `seq` remain authoritative. Thinking and tool arguments are not
previewed. After the connection's observer attaches, the preview stops at the
first `{` or sensitive confirmation key anywhere in a message, including
across text blocks; that suffix stays withheld until the generation or attempt changes.
This conservative rule also stops previews at benign braces. A reconnect
previews the current snapshot and cannot detect a sensitive marker overwritten
before it attached; the operator accepted that tradeoff. Preview text and tracked
content blocks are bounded; an oversized partial is not a substitute for the
final answer.
The run's first engine submission must resolve to the operation in the current
`master` turn; an older snapshot or buffered batch is suppressed when they
differ or the submission cannot be resolved. A turn change clears the previous
preview before the new header is sent.

The browser clears a preview as soon as its connection closes, even if the
status probe cannot answer. The browser opts in to partial board feeds with
`accept_degraded=1`; when one declares degraded coverage, recovery refresh
stays enabled while local invalidations continue, and Hosted Status
(`/settings/status`) notes the background refresh without showing a
workspace-wide warning banner. Older browsers remain on polling rather than
treating the partial feed as complete.

A slow thread reader reconnects from its last received durable `id` after the
queued data crosses 256 KiB; already queued entries drain before closure. One
crossing frame, or an oversized initial master frame plus the first entry, can
exceed that threshold so the cursor can advance. The host must still
revalidate session, tenant, role and conversation access on delivery.

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

The CLI derives a replay key (`pcli_<sha256>`) from the tool, `--query`,
`--input` and, for a connected read, `--connection`, or accepts an explicit
`--request <handle>`. If a call fails with a
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

When the workspace has a qualified Search Console tool and an existing
connection, a read names that connection explicitly. The same connection and
input are needed to derive and verify its status:

```sh
REPORT='{"site_url":"sc-domain:example.test","start_date":"2026-01-01","end_date":"2026-01-31","dimensions":["query"]}'
cadence --org acme platform call search_console_report --connection con_example --input "$REPORT"
cadence --org acme platform status --tool search_console_report --connection con_example --input "$REPORT"
```

The read uses the workspace balance; `--max-charge` belongs to the separate
local device protocol. A connection does not choose a Search Console property
implicitly: enrollment links the provider account only, and the verified
property is pinned separately in
[Workspace connections and Search Console properties](#workspace-connections-and-search-console-properties).

A replay without a recorded result is not a successful report. Hosted and
local-device clients refuse to present a missing result as success and do not
automatically dispatch another read.

### Staging a draft or send tool for owner approval

A connected draft/send tool (for example a Gmail send) never runs when an agent
asks. `platform stage` sends `operation: connected_effect` (`cli.write`) and the
board records the exact call as an action for the owner to approve:

```sh
cadence --org acme platform stage gmail_send --connection con_example --input @message.json [--request <handle>]
```

The answer is the staged action (`action_id`, `outcome`, `replayed`), never a
provider result. `outcome` `pending` means the call did **not** run and waits
for an owner's approval on the board. The same `--request` (or, when omitted,
the same tool, connection and input) replays the same action; the same handle
with a different call is refused (`key_reused`). A refusal prints its code, for
example `not_allowlisted`, `no_decider` or `connection_unavailable`. A local
org refuses the subcommand, and `platform call --connection` still sends only
`connected_read`.

### Owner enrollment from the CLI (Search Console first)

An owner enrolls a provider account from a plain shell, in three steps. The
reads use `cli.read` (or `cli.write`); starting an enrollment is a mutation and
needs `cli.write`:

```sh
cadence --org acme connection providers
cadence --org acme connection ls
cadence --org acme connection enroll google_search_console --no-open
```

`connection providers` lists the workspace's open toolkits and their read
eligibility, including those that cannot yet be enrolled. The status
`enrollment: available` means a qualified read exists; starting also requires
the workspace's connector to be configured. The other requested providers
are not qualified by this build.
`connection ls` lists the accounts already linked to the workspace — a pending
authorization the provider has not completed yet does not appear there.
`connection enroll` answers one document whose `entry_url` is a one-time
provider authorization link:

```json
{"entry_url":"https://provider.example/oauth?synthetic-request"}
```

Open that link yourself, in your own browser. The hosted server never opens a
browser of its own, and this client prints the link exactly once as the
successful command's output — it is never written to a log, a receipt, an
error, or a diagnostic line. `--no-open` is the same behavior here (a hosted
enrollment never opens a browser either way). Treat the link as a bearer
secret until it is used. Provider permissions depend on the configured OAuth
app; inspect the consent screen and do not proceed if it requests more than
the intended read-only access. This flow does not verify the app's scopes.

The three operations are for the live human workspace owner. A managed caller
(an agent pane or a grant-bound child) is refused in this client before any
request exists, because the stored login is the owner's and would otherwise
act as the owner; the hosted server independently requires a live human owner.
Local-only enrollment flags refuse instead of being dropped: `--scope` (the
configured OAuth app determines provider scopes), `--shape` (the server records
the account shape the provider authorizes) and `--agent` (a hosted enrollment
binds the owner, not an agent). Enrollment links an account only: choosing a
verified Search Console property is the separate, owner-only step in
[Workspace connections and Search Console properties](#workspace-connections-and-search-console-properties).

If the answer to `connection enroll` is lost, times out or is otherwise
uncertain, the client reports an unknown outcome and never retries and never
claims a link. The link cannot be read back, and `connection ls` lists only
accounts whose provider callback already completed: an empty list does not
prove that no link or pending authorization was issued. Starting again is a
deliberate new enrollment, not a reconciliation step — it can create a second
one-time link. A hosted server that predates these operations answers the
unknown operation; that refusal says to update the hosted server first and
never invents an empty list or a missing link, and the client never falls back
to the local daemon.

An image submission may return a queued job. Poll its returned `data.job.id`
with the same workspace and `cli.write` grant, then download a succeeded job's
retained artifact (`data.job.artifacts[].ref`) to a new local file:

```sh
cadence --org acme platform call get_media_job --query '{"jobId":"med_job_from_submit"}'
cadence --org acme platform artifact med_job_from_submit.<64-hex-sha256> --out ./circle.png
```

`get_media_job` is a live GET of the existing image job (it can advance and
settle that job, never starting a second generation). For a video started by
`media_generate`, read the existing job with
`cadence --org acme platform call job_status --input '{"jobId":"med_job_from_submit"}'`.
`platform artifact` reads retained PNG/JPEG images or MP4/WebM video in bounded
pages. It verifies the full SHA-256 digest and byte length in a private spool
on the destination filesystem, then publishes `<out>` without replacing an
existing file. If the destination path changes during publication, the result
may be uncertain; inspect the original directory before retrying. Images are
limited to 10 MiB and videos to 100 MiB. Video retrieval also requires a server
that supports video artifact pages.

## Workspace connections and Search Console properties

The hosted workspace owns its provider credentials and its enrolled accounts.
Connection commands are owner-only and require a live human owner's `cli.read`
/ `cli.write` grant; a member or a managed agent is refused.

After `connection enroll` links a Search Console account (above), the account
appears in `connection ls` with its exact connection ID. A Search Console
account can expose several properties, including domain properties
(`sc-domain:example.test`) and URL-prefix properties (`https://example.test/`).
List them and pin one:

```sh
cadence --org acme connection sites <connection-id>
cadence --org acme connection select-site <connection-id> 'sc-domain:example.test'
```

The site list includes `selected`, the exact property currently pinned or
`null`.

The selected property is stored by its exact provider identifier; it is not
normalized. Only `siteOwner` and `siteFullUser` properties can be selected — a
restricted or unverified property is refused. Once a property is selected,
`search_console_report` reads for that connection must use exactly that
`site_url`; a different value is refused before pricing or dispatch. Selecting
again replaces the pin.

Revoke the account when it is no longer needed; this disables the provider
account, revokes its connection grants and drops the selected property:

```sh
cadence --org acme connection revoke <connection-id>
```

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
cadence --org acme issue project timeline demo --since 7d
```

On a board that supports retryable issue creation, add a stable caller-chosen
`--request-id` to `issue new`. If the reply is lost, repeat the same command
with the same ID and unchanged arguments, from the same user, assistant client,
and acting agent or human identity; the board returns the same issue, not a
duplicate. A changed request under the same ID is refused. A managed agent
cannot fall back to its member's login for a keyed create. If the original
acting identity is no longer available, inspect the board instead of retrying
under a different one. An older board may reject the ID or omit the receipt.
Never remove the ID and resend an ambiguous create: check the board or wait
for the board upgrade instead. Without an ID, inspect the board before
retrying. Local/native issue creation refuses this hosted-only option.

Use the exact revision returned by each corresponding read, not a guessed one.
To create a new wiki page, pass the create-only sentinel `--if-rev none`, which
refuses the write if the path already exists; to update an existing page, pass
the revision returned by its read to avoid overwriting another writer.
Search results are discovery, not evidence:
read the actual page and cite its canonical path and revision. Wiki and CLI
scopes are separate, and the target path/project still has to be authorized.
An issue `status=doing` update is not a local assignment claim or delivery.

`wiki cp` copies a file or a folder into the hosted wiki and out of it on a
board that supports it; see "Recursive copy" in
`docs/guides/wiki-knowledge.md` for the board's limits, the timeout and what each
exit code of a stopped job means. An older
board answers `not_implemented` and copies nothing; there, write pages one at a
time with `wiki put` (text) or `wiki upload` (binary).

To start the issue on the already connected local default agent, the person
operating that machine explicitly runs:

```sh
cadence --org acme issue start <issue-id> --repo /absolute/path/to/checkout --name work
cadence --org acme agent assignments
```

Hosted `issue start` requires `--repo`, accepts only its supported local placement
options, and claims through the hosted assignment coordinator; it is not an
unrestricted remote operator verb or a job launcher. Existing kickoff/member,
claim and generation fences remain. A claim/queued receipt is not proof that
execution or a result happened; inspect assignment/report state.

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
`--profile` does not read profile content; load it with `context`. Hiring creates only such a profile: no runtime, provider, sandbox or model is chosen, nothing launches, and `model` stays `inherit` until a runtime resolves it or one is set explicitly. A native board reads a profile the same way with `cadence agent context <alias>`. The
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

A `--state result` report ends the assignment but moves nothing else. Once
the work meets the project's acceptance and completion rules, use the
selected workspace's supported ticket update, for example:

```sh
cadence --org acme issue set <issue-id> status=done --if-rev <issue-rev>
```

`cadence --org <name> issue finish <ID> [--worktree <path>]` releases
the lane this machine started for that issue, and only that lane: it removes the
worktree and deletes the local branch. It takes no other option; `--force`,
`--keep-branch`, `--remote`, `--merged` and `--dry-run` are refused on a hosted
org with nothing removed. The lane is found in this machine's coordinator ledger:
its claim, generation and lane fields must match the recorded lane. After a
confirmed cancellation the immediately preceding lane binding is retained for
guarded release, not execution. The assignment must have ended (`result`,
`failed` or cancelled); a live row must not be fenced. The command uses
the native finish in-use guard and refuses, naming the cause, when the worktree
is dirty (untracked files count whatever `status.showUntrackedFiles` says), has
recently modified files, a process or agent is inside it, or the branch has not
started. It never judges whether the content is in the default branch; it asks
whether the exact tip survives after deletion. Only two things count: the tip is
an ancestor of `origin/<default branch>` (`origin/HEAD` must be set), or a
merged PR on `origin`'s own `github.com` repository has a head commit exactly
equal to the tip, a merge time, and the default branch as its base (GitHub keeps
that head after a squash). A branch pushed but not merged does not release, and
neither does a lane merged without a PR, a non-GitHub `origin`, or a case where
`gh` is missing, not signed in or times out. The release also refuses
when the worktree holds gitignored files it may not delete, naming a sample,
because removal deletes them. It may delete top-level `target/`, any
`node_modules/` or `__pycache__/` directory (or a symlink to one) at any depth,
and the lane's `.env` while it is exactly what lane setup last wrote: when every
line setup writes is its own (the cargo-shim `PATH`, the slot env), it records
the file's sha256 in the worktree's own git dir, and one changed or extra line
keeps the `.env`. A regular file named `node_modules` or `__pycache__` is kept.
Anything else ignored (for example `docs/*`, `claudedocs/`,
`docs/target/`, or a lane's `.env` from an older client, which has no record)
refuses. Git reports an ignored directory as one entry, so a file kept inside
one of those directories is deleted with it. When the
refusal will not change (the ledger cannot identify the lane, or no PR or
ancestry evidence can exist), take the manual path: verify the merge, that
nothing is using the lane and that you do not need its ignored files, then
`git worktree remove <path>` and `git branch -d <branch>` (a squash-merged branch
refuses `-d`; use `-D` only after verifying the merge). `issue reconcile` stays
native-only.

If the actor is gone but its assignment never ended, the assignment ID is shown
by `cadence --org <name> agent assignments`. A client that supports it can
cancel that exact assignment and attempt the same guarded release in one call:

```sh
cadence --org <name> agent reassign <assignment-id> --cancel --release-lane
```

Cancellation fences the claim but **does not stop the actor**; establish that
it is gone before using this command. The cancellation can succeed even if the
release is refused, for example because the lane is dirty or its exact tip is not
shown merged. Inspect the lane and the reported reason before retrying; if it
still exists and the reason is resolved, use `issue finish` rather than cancelling
again. If the old lane binding is unavailable or ambiguous, it cannot be
reconstructed by this command and the lane remains intact.
While that cancellation is pending or its lane guard remains, starting the same
issue is refused for every action and target rather than overwriting its recovery
record, even if the path is absent.

While cancellation is in flight or its lane remains unreleased, the current
client records a local safety marker. Older clients reject that workspace's
assignment ledger instead of starting or reusing a retired lane. A confirmed
cancellation with no lane reservation clears the marker; an incomplete lane
binding remains guarded for inspection. Successful release clears the marker
only if the recorded binding is unchanged and the path is absent.
If that cleanup cannot be confirmed, the release result contains a warning and
the marker stays. Cancellation and lane creation are serialized only for
current clients using the same local ledger; neither the lock nor the marker
stops an actor, an older command already in flight, or a client using a
different ledger.

Automatic release of a guarded lane also requires **provenance**: the claim must
have been acquired new by this client's local-agent protocol and the ledger must
record no kickoff reserved or delivered for that claim generation. The record is
written when the claim is acquired; replaying a claim that was already on the
board — legacy, another ledger or a restart — leaves the history unknown. Legacy
rows, external `--profile` sessions and any claim with a recorded kickoff are
therefore refused: the client keeps the guard and lane and names the manual
removal commands. Provenance covers this ledger's own protocol only — an external
writer, another machine's ledger or an older client that already acted remains
unknown, so establish that no actor is still using the lane before any manual
removal.

The board may coalesce different create actions for the same issue and target
into one assignment. The client keeps each create operation's retry key while
sharing a single custody record. When such additional receipts are present,
older clients also refuse that ledger; use a current client rather than deleting
the receipts or downgrading. They are retired with the ended assignment when
new work is started.

A cancellation that interrupts lane reservation before the worktree path was
recorded can be reconciled only with that provenance and only when the expected
checkout — derived from the repository root recorded with the reservation and
the lane name — is absent and no longer registered with Git:

```sh
cadence --org <name> agent reconcile-release <assignment-id>
```

`reconcile-release` deletes nothing and verifies no merge evidence. Without
provenance, or when the ledger records no repository, it refuses and the guard
stays; only manual inspection applies.

When a cancellation's outcome is unknown, inspect the named assignment and
original operation before deciding whether to retry; do not assume it failed.
When a retry is appropriate, the same `--cancel --release-lane` command reuses
the saved operation ID and expected generation. If no correlated confirmation
is available, the guard remains and release is blocked; a later refusal or
current assignment snapshot alone does not settle the original request.
If release already removed the lane but reports that the marker could not be
cleared, inspect the assignment and checkout, then run:

```sh
cadence --org <name> agent reconcile-release <assignment-id>
```

This clears only a matching terminal guard when the lane path is absent and no
longer registered with Git. It deletes no worktree or branch and does not verify
merge evidence; its result reports those facts separately. A present, still
registered, ambiguous or pending lane is refused.

### Managing agent profiles from the CLI

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

**A managed session has a credential of its own.** On a
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
`audit_verdict_project`). `artifact types` and `artifact submit`
ride the agent credential like the other verbs. The credential carries the
member's own subject, so the board's creator check still admits the member's
files, and a reader or reviewer assignment's submit is refused by the board's
read-only guard. Not available to a managed agent session at all:
`settings agents`, `master status` and `master resume`. They always refuse —
owner-class settings authority is never delegated — and the refusal names the
operator's plain shell and the grant-bound local path.

An artifact review rides the agent credential, so it is recorded as the agent;
on a login fallback it is recorded as the member. A member's agent can
therefore review an artifact that member attached. That is attribution, not
independent review: the merge gate, not artifact review, is what judges
independence, and it still refuses the author's principal.

**This is attribution and server-side refusal, not a security boundary.** The
session runs as the same OS user and can still read the member's login file
until the session is confined.

What the client does when the credential is not available:

| Board answer | Client |
| --- | --- |
| mint `404`, or `409 renewable_login_required` | keeps the login, one line on stderr |
| authorize or call `403 human_session_required` (a workspace not yet updated to accept it) | keeps the login for the rest of the command, one line on stderr |
| any other mint answer, or an unusable `200` | refuses; nothing is sent and the login is not used in its place |
| the agent's own refusals: `403 forbidden` and the rest | final, never retried on the login |
| a `401` to a **cached** agent credential | drop the cache and re-mint once; a `401` to one just minted is final |

### Workspace skills from the CLI (CAD-2396)

A hosted workspace can keep its own skills: a `SKILL.md` package with optional
`references/<file>` text files. An agent or a member drafts one; a live human
owner approves one exact revision by its digest, and only an approved revision
is served to the workspace's agents (see "Workspace skills" above). Name the
org with `--org` on every call, as for profiles (`org_not_named` otherwise).

```sh
cadence --org acme workspace-skill check weekly-report ./weekly-report
cadence --org acme workspace-skill draft weekly-report ./weekly-report --expect-rev 0
cadence --org acme workspace-skill list
cadence --org acme workspace-skill show weekly-report [--revision 2]
cadence --org acme workspace-skill approve weekly-report --revision 2 --digest sha256:<hex>
cadence --org acme workspace-skill revoke weekly-report --digest sha256:<hex>
cadence --org acme workspace-skill delete weekly-report --expect-rev 3
```

- The package directory holds `SKILL.md` and, optionally, `references/` with
  plain files directly inside it. `skill.json` is generated by the board, and
  `scripts/`, a nested reference or any other file is refused before anything
  is sent, with the board's own reason name (`member_reserved`,
  `scripts_not_accepted`, `reference_path`, `member_not_accepted`).
- `check` validates and prints the digest a draft would store; it saves
  nothing. The board owns the rules and the limits, and a refused package
  prints its reason (`skill_refused`, for example `description_missing` or
  `package_too_large`).
- `draft` needs `--expect-rev`: the latest revision you read, or `0` to create
  the skill. A stale revision exits 5 with the board's `revision_conflict`. The
  same bytes as the latest revision are not a new revision. A draft never moves
  the approved revision.
- `approve` names the revision and the digest `show` printed, and `revoke` the
  approved digest; a different one is `digest_mismatch` and changes nothing. A
  workspace can have at most 32 approved skills (`approved_limit`).
- `delete` removes an unapproved skill and all its revisions, fenced on the
  latest revision (a stale one exits 5). An approved skill is `approved_skill`
  until it is revoked. A workspace holds at most 200 skills (`skill_limit` on a
  new name) and keeps each skill's newest 10 revisions plus the approved one.
- **Approve, revoke and delete are the owner's decision.** A managed agent
  (`CADENCE_ALIAS`, a grant, or a pinned agent identity) is refused by the
  client before any request (`managed_caller_refused`); drafting stays open to
  it. As with profiles, this is a client check: the board refuses an agent or
  service principal, but cannot tell a managed agent on the owner's saved login
  from the owner. Keep that login out of agent environments.
- A board that predates these commands answers "update the board first".
  A native board has no workspace skill store and refuses every subcommand.
  `cadence skill` is different: it installs this build's bundled guidance on
  this machine.

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

### Submitting a document for review

```sh
cadence --org acme artifact types
cadence --org acme artifact submit <art-id>
```

- `artifact types` (`cli.read`) prints the document types and the baseline a
  type follows until the owner confirms it. `artifact submit` (`cli.write`)
  puts a file you created into review, under its label as the type. It prints
  `stage`, `type`, `typeConfirmed` and, for an unconfirmed type, `baseline`.
  The lifecycle is in [Delivery](delivery.md#document-review-for-non-code-work);
  the board side is in `docs/BOARD.md`.
- Submitting again answers the current stage and does not submit twice, so
  after a lost answer (exit 1) repeat the same command.
- `document_lifecycle_unavailable` (exit 3) means the workspace has no document
  review, and nothing was read or submitted. Only the type list decides it, and
  `submit` reads that first. The message names the cause:
  - "the workspace's server is older than this command": a `501`, or an older
    relay's `400 invalid_request` or `unknown_verb`. Update the tenant server.
  - "document review is turned off on this workspace": a `404 not_found`. It is
    not an outage, and retrying does not help.
- A submit refusal keeps the board's code and exits 3, with nothing submitted:
  `not_found` (no file you can see: on `submit` a `404` is the file, not the
  lifecycle), `not_creator`, `unlabelled`, `already_shared` (save a new version
  to submit it) and `document_archived`. If review is turned off between the
  type read and the submit, the submit reads as `not_found`; running it again
  names the unavailable lifecycle.
- `not_implemented` on `submit` is a server that lists types but predates the
  submit: update the tenant server first, then this client.
- A native board refuses both as usage errors (exit 2); they are hosted only.

### Reviewing software changes

The delivery gate, the policy and trusted lists, the evidence each review takes,
the owner's approval and the refusals are in [Delivery](delivery.md). On a hosted
org, check a PR before reviewing it, and read the gate again after:

```sh
cadence --org acme delivery requirements --issue <issue-id> \
  --pr https://github.com/<owner>/<repo>/pull/<number> --head <full-sha>
cadence --org acme delivery check --project <project-key> --issue <issue-id> \
  --repo <owner>/<repo> --pr <number> --head <full-sha>
```

The caller needs `gh` read access to the repository. The owner's approval and
enqueue are run from the owner's own terminal; an agent never runs them.

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
- When the daemon's workspace worker picks up a kickoff from the board for a
  fenced default agent (`attention`, or holding an unknown message), it does not
  queue it: the assignment is reported `blocked` with that reason instead of
  `running`. Resume the agent (reconcile or `agent unfence` first if
  it holds an unknown message) or connect another default, then run
  `cadence --org acme agent connect --local --alias <alias>` again (the saved
  checkout is kept) to pick the blocked work up. `issue start <id> --repo`
  without `--profile` sends from the CLI and is not checked this way yet.
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

## Rollout boundary

Package consent and credential enrollment do not approve a production
deployment, live data migration, live send or installed-daemon restart. Do not
expose mixed unsupported writers, and do not read a capability-unavailable
response as a fallback grant. Local operators keep responsibility for native
journal and business-data recovery. Restart replay is not proof of zero-loss
multi-store recovery.
