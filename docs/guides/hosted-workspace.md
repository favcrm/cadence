# Hosted workspace: local agents, shared work and apps

**Keep the installed Cadence daemon.** It opens one outbound authenticated WSS
connection for each explicitly connected workspace, multiplexing the agents you
register. Hosted metadata tells it what changed; typed HTTPS commands still
read, claim and report work. No inbound port, second messaging daemon, peer mesh
or automatic scan of private conversations is needed. Standalone local operation
with no hosted binding keeps its existing storage, terminal and app commands.

This guide describes the matching native and hosted implementation, not an
announcement of production deployment. See [verification and rollout](#verification-and-rollout)
for the exact revision/evidence boundary. Authentication details remain in
[REMOTE-AUTH](../REMOTE-AUTH.md); local package rules remain in
[Workspace app catalog](app-catalog.md).

The event bridge, the selected-hosted app journey and a two-state native
assignment/wiki/ticket collaboration check have separate observed integration
checkpoints. Their exercised scope and fixture/rollout limitations are recorded
below; none of them covers every capacity, rotation or abrupt-loss case.

## What runs where

| Responsibility | Current owner / effect |
| --- | --- |
| Human membership, CLI credentials and shared agent directory | Hosted authority; IDs and labels are not credentials. Presence is observed from an authenticated runner connection, not a promise that an agent can execute. |
| Assignment state, notification receipts and event outbox | Hosted durable metadata. Events are minimal invalidations; HTTPS rechecks current authority and generation before claims or reports. |
| Local terminals, worktrees, agent adapters and execution | Installed native daemon. Explicit assignments may invoke the existing agent delivery path; an FYI never does. |
| Wiki/ticket domain operations and app install/approval/run/business records | Existing native domain owners. The hosted HTTP relay can still wake the company runtime for these operations. The directory/outbox do not replace those owners. |
| Published immutable package bytes | Hosted package storage; an exact listing/version/digest is provenance, not authority. |
| Pure app `needs.requires` compatibility rules | Canonical Rust `crates/cadence-core`, called natively and compiled to WASM for the hosted adapter. Transport, authentication, storage and app execution are not moved into this kernel. |

Directory registration/read, event/presence transport and storing an FYI operate
on hosted metadata without invoking a model or waking the company container.
Receiving/displaying an FYI also does not start local inference. A real assignment
can wake its explicitly selected native agent. Wiki/ticket calls and app
check/apply/reconcile paths that consult the native domain still need the native
runtime; waking or unavailable is not a reason to fall back to local state.

Shared hosted work stays in its selected workspace; it is not a mirror of the
teammate's standalone local tracker. Sharing currently extracts only the pure
requirements kernel, not the whole daemon or CRM backend into WASM. Each adapter
must supply its actual host version and supported contracts; a shared rule result
does not confer platform capabilities or native execution authority.

**Storage limit:** the D1 app-operation journal records metadata, exact consent
and recovery state; it is not a replicated backup of every native SQLite database,
Git tracker, business record or container filesystem. Reconcile can observe the
native catalog, not reconstruct lost authoritative business data. This release
neither retires the daemon nor claims zero-loss recovery of multiple native stores.
Host-compiled CRM handlers and durable domain storage remain native-owned.

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

**Hosted identity.** A host that has a committed assistant identity may supply
two optional domain-data callbacks on `HostedSessionOptions`: `identity()`
returns the current personal profile or workspace identity, and
`threadContext()` returns the thread's public projection. Both carry data only.
Prompt text, model messages, engine hooks and SDK types are not accepted
through them, and a generic host that supplies neither keeps its current
behavior.

When `identity()` is configured, the runtime renders the established personal
setup or workspace instructions privately, resolves the identity for each
operation, and stores it once per operation root in the session object's
existing `tenant_identity_snapshots` table (insert-once; a stored snapshot is
never rewritten). Admission resolves the identity before accepting new work,
and every actual generation must carry the exact message object the runtime
authored for that operation: a text marker, a look-alike object or the current
store cannot substitute for it. A profile change therefore applies to the next
operation, never to an operation already running, and a malformed or
unavailable identity refuses the operation instead of falling back to a generic
prompt. The summarizer's `completeSimple` path keeps its original
normalization, authentication and transport.

Each `/thread-stream` connection reads `threadContext()` once. Its metadata
composes into the `master` frame under the runtime's own fields, so business
identity such as `identity` and `actor_identity` can be added but never
replaces the model, provider, session, turn or queue state. An unavailable or
malformed projection fails the connection with a `503` capability refusal
instead of serving a stale or partial header. `masterState` applies the same
precedence to the HTTP header read.
The composed master frame is serialized once before opening the watch or
returning SSE `200`: cyclic or non-JSON business values refuse with `503`.
Values shadowed by the runtime's reserved fields do not invalidate the frame.

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

The teammate accepts the workspace invitation through the platform and runs
`cadence login acme` on their own machine/account. Each person has their own
saved credential and private configuration; never copy the owner's credential,
browser cookie or another agent's state. A member can approve ordinary CLI/wiki
collaboration. App lifecycle changes below require a current **human owner**.
Invitation acceptance, credential consent and local-agent registration are
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
sessions do not race it for claims. For an additional explicit FYI-only alias,
use the lower-level supported surface:

```sh
cadence --org acme local-agent connect --alias my-reviewer --fyi-only
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

Hosted `issue start` requires `--repo`, accepts only its supported local placement
options, and claims through the hosted assignment coordinator; it is not an
unrestricted remote operator verb or a job launcher. Existing kickoff/member,
claim and generation fences remain. A claim/queued receipt is not proof that
execution or a result happened; inspect assignment/report state.

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
under that key conflicts. A missing response means unknown outcome, not a new
operation. An FYI receipt means **stored**, an event ACK means **native custody**,
a claim means **claimed**, and only the domain report says **running/result**.
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

**[INFERENCE]** Metadata-only events avoid starting a company container/model
just to discover presence, register an agent or store an FYI. Reusing the installed
daemon avoids another local service/peer mesh. The pure WASM requirements kernel
can answer its bounded rule question without booting the full native domain.
These can reduce unnecessary wakeups and control-plane startup work compared with
routing every metadata event through a container; they do not remove native work
for wiki/tickets, app admission/approval/run or business records.

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
