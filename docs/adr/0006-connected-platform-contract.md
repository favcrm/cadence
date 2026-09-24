# 0006 — The connected-platform contract: declared effects, proxied credentials, pending sends

- Status: **proposed**
- Risk class **human** for every implementation ticket (trigger 1, a
  trust boundary — credential custody and the release of outward acts;
  trigger 3 wherever secret handling is built). This document is the
  contract they implement; it changes no code.
- Date: proposed 2026-09-24
- Author: `swe-365` (devin worker), dispatched by `lane-pm`
- Deciders: the operator. CAD-365's second acceptance item — "Accepted
  by the operator" — is the operator's decision on this document. §7
  holds the questions.
- Issues: CAD-365 (this ADR); parent CAD-331 (the P4 epic); blocks
  CAD-366 (the credential proxy); consumed by CAD-367 (the Cloudflare
  platform, into which CAD-368 and CAD-369 were consolidated on
  2026-09-24); relates AOS-49 (the AgenticOS v2 credential exchange —
  its acceptance requires "contract fixture shared with Cadence P4",
  §5.6).
- Code citations are pinned to
  `7de54b5c673d394a1c0d8ffb5493d603115c1aca` (origin/main, 2026-09-24).
  Line numbers move; re-locate by symbol.

## 1. Context

### 1.1 Why a contract before a proxy

Epic CAD-331 ships "connected platforms": cadence agents acting on
external services — Cloudflare first (CAD-367), AgenticOS v2 next
(AOS-49) — with a credential proxy (CAD-366) between the agent and the
platform. The epic's acceptance items are the contract's requirements:

- Agents never see a platform credential.
- Every send effect waits for an operator press.
- Cloudflare preview deploy automatic, production deploy asks.

AOS-49 adds the sibling view: a device-style code, email-OTP sign-in,
scope consent and a revocable credential — "no browser cookies copied" —
and "tools declare effects read/draft/send; send is never executed by
an agent". Two codebases will implement against this contract, so it
must be written once, here, before either builds. This ticket is the
design record only: no proxy, no adapter, no UI.

### 1.2 What exists today: the negative posture

Cadence's current platform-credential posture is *subtractive* — agents
are kept away from credentials, and nothing lets them use one:

| Rule | Where |
|---|---|
| Forge and platform tokens are scrubbed from the master's env by name: `GH_TOKEN`, `CLOUDFLARE_API_TOKEN`, `CF_API_TOKEN`, `VERCEL_TOKEN`, `NETLIFY_AUTH_TOKEN`, `FLY_API_TOKEN`, `NPM_TOKEN`, `SSH_AUTH_SOCK`, … | `src/master.rs` `DENIED_ENV` |
| "No merge, push or platform effect": no forge/platform tokens in env, `GH_CONFIG_DIR` empty, `GIT_TERMINAL_PROMPT=0` | `docs/design/AGENT-FILESYSTEM.md`, the enforcement table |
| The master reads only its own trees under Landlock confinement; credential files outside the read set are unreachable | `docs/design/AGENT-FILESYSTEM.md` (CAD-439), `src/confine.rs` |
| Credential-shaped text is refused on every durable or outward agent write; a finding carries at most a 4-char prefix and a SHA-256 fingerprint, never the value | `src/secret/mod.rs` `guard`, `Finding` |
| URL `user:pass@` credentials and token-shaped spans are redacted from display | `src/ui/delivery_sync.rs` |

This posture is correct and stays. It also makes the P4 epic
impossible as stated: an agent that can never touch a credential can
never deploy anything. The contract below turns the posture from "no
credential near an agent" into "credentials live in the proxy; agents
hold grant references and staged calls". The env scrub, confinement
and output guards remain as the containment layer around it.

### 1.3 What exists today: the pending-request mechanism

Cadence already owns the exact machinery a "pending effect" needs —
built for Claude tool-permission brokering and verified end to end:

| Mechanism | Where | What it gives the contract |
|---|---|---|
| `cadence mcp-permission` — a stdio MCP server exposing one `approve` tool to `--permission-prompt-tool mcp__cadence__approve` | `src/mcp.rs` | The proof that an agent tool call can park on a human decision and resume with the answer, with a bounded wait (`CADENCE_PERMISSION_TIMEOUT_SECS`, default 900 s) that denies rather than hangs the turn |
| `request_open` — registers a brokered request, `kind` is a free identifier defaulting to `"approval"`; a caller-named `request` handle dedupes retries (`existing:true`); sets `waiting_input`; notifies the upstream PM exactly once (uuid-v5 dedupe) | `src/daemon.rs` `rpc_request_open` | The open shape and the dedupe rule; `kind:"effect"` fits without a schema change |
| `request_wait` / `request_close` — block ≤120 s per slice; `closed` on actor exit, daemon restart or requester deadline; an answer parked at the boundary still lands (the mailbox is filled before the pending entry drops) | `src/daemon.rs` `rpc_request_wait`, `rpc_request_close` | The wait/close semantics, including the answer-can-never-surface-as-denied ordering |
| `agent_respond` — answers a pending request `accept`/`decline`/`answers`; a refused respond leaves it pending | `src/daemon.rs` `rpc_respond` | The press surface |
| Caller rules: `request_open` is bound to the owning agent's own connection (CAD-376); `agent_respond` is the proven operator or the requester's own PM — the requester never answers itself (CAD-370); every RPC has one rule in `daemon::caller_rule::RULES` (CAD-384) | `src/daemon.rs` `request_caller`, `authorize_respond`; `src/daemon/caller_rule.rs` | Who may raise, await and press a pending effect |
| `agent_requests` + the overview probe surface pending rows; the agent shows `waiting_input` | `src/overview.rs` | The visibility surface — a pending effect is already listable |
| Operator by positive proof (`operator_grant`, signed records) | ADR 0005, accepted 2026-09-24 | What "operator press" converges to as ADR 0005's tickets land |
| The operator web session (login link, cookie, board route table) | ADR 0004, PR #249 | The board-side press |

### 1.4 What is missing

No code today knows the word *platform* as an object: there is no
platform descriptor, no credential custody (the state dir holds no
secret material cadence owns), no effect vocabulary, and no staged
call awaiting release. `request_open`'s `kind` is namespaced but only
`"approval"` is used. The contract in §5 names these objects once so
CAD-366 and CAD-367 — and AgenticOS v2 under AOS-49 — implement the
same nouns.

### 1.5 Threat model

The adversary classes are ADR 0005's §1.5: **A0** the honest mistake,
**A1** the API-level evader (a prompt-injected or hostile agent using
only offered surfaces), **A2** the same-uid bypass of the API, **A3**
root. Platform specifics sharpen three of them:

- **P1 (A0/A1): an agent calls an outward tool it shouldn't.** A
  deploy, a publish, a sent message. The effect gate exists for this:
  send effects park; the agent cannot press; it cannot declare its way
  out (§5.2, C2–C3).
- **P2 (A1): an agent tries to learn the credential.** It reads tool
  results, errors, pending rows, its own env, the state dir. Custody
  plus redaction plus the secret guard cover the offered surfaces
  (§5.3). The residual is real: a same-uid process that ignores the
  API and reads daemon memory or files directly is ADR 0005's A2 — no
  single-uid design holds against it. The contract's promise is
  narrower and honest: *no offered surface ever carries credential
  bytes to an agent.*
- **P3 (A1/A2): replay or double-fire of a released send.** An
  accepted effect must execute once, observably once. The durable row
  plus handle dedupe plus the reconcile-on-restart rule (§5.4) exist
  for this.
- **P4 (A2/A3): the custody store itself.** A token in a 0600 file is
  one `cat` away from a same-uid reader. §5.3 names custody backends
  and marks this residual rather than claiming it away — the same
  honesty ADR 0005 applies to its own factor.

## 2. What "done" looks like

The contract, as testable clauses. §5 expands each; the CAD-365
acceptance items map onto C1–C3 and the operator's own decision.

- **C1 — The effect vocabulary is exactly `read`, `draft`, `send`.**
  No fourth class; a declared value outside the three is treated as
  undeclared.
- **C2 — Effects are declared in the adapter, never by the caller.** A
  tool's effect is part of the platform adapter's reviewed tool table.
  An `effect` key in a call's arguments is ignored.
- **C3 — An undeclared effect is treated as `send`.** A tool missing
  from the table, a missing/malformed `effect` field, or an unknown
  value all gate as send. Omission fails *into* the gate — friction,
  not a hole.
- **C4 — Credentials exchange operator→daemon only.** Two exchange
  shapes (§5.3): operator-enrolled scoped tokens, and consent
  exchanges (device code + OTP + scope consent, per AOS-49). Agents
  receive grant references, never credential bytes.
- **C5 — A send call produces a pending effect and nothing else.** The
  staged call is represented as a `kind:"effect"` brokered request
  with the fields of §5.4; it does not execute inside the call.
- **C6 — Release is a press that executes.** The operator's `accept`
  fires the exact staged input daemon-side; `decline` parks the
  reason. The outcome — result or platform error — is the answer the
  waiting call receives.
- **C7 — The lifecycle is durable at the dangerous edge.** A pending
  effect is a store row, not only an in-memory handle; an `accept`
  recorded but not provably executed reconciles as `unknown` after a
  restart and is never re-fired automatically.
- **C8 — Every hop is auditable.** Enrollment, grants, requests,
  decisions, executions and revocations are events; fields that can
  carry agent text are secret-guarded; no event or row holds a
  credential.

## 3. Options

### 3.1 Where effects are declared

- **A — Per-call, the agent supplies `effect`:** rejected. A
  prompt-injected agent labels its outward act `read`; the gate
  becomes decoration. The declaration is exactly what must not be
  agent input.
- **B — In the adapter's tool table, shipped and reviewed with the
  adapter:** **adopted.** The declaration is data a reviewer diffs;
  adding a tool without a declaration is a visible omission (C3 gates
  it anyway), and changing a tool's class is a reviewable event.
- **C — Inferred from verb names or input shape:** rejected. A
  heuristic that misclassifies `send` as `read` is worse than no gate
  — it fails open precisely on the case that matters.

### 3.2 The undeclared default

- **A — Treat as `send`:** **adopted** (the acceptance item). An
  undeclared tool still works — every call parks for a press — so the
  omission surfaces as friction the operator notices, not as an outage
  or a bypass.
- **B — Refuse the tool outright:** rejected. Safer-looking, but it
  makes a missing declaration a silent outage and invites the next
  author to slap `read` on everything to unbreak the adapter. `send`
  keeps function behind the gate.
- **C — Treat as `read`:** rejected. Fail-open on the trust boundary
  is the failure this contract exists to prevent.

### 3.3 Credential custody and exchange

- **A — Credentials in the agent env (inverting §1.2):** rejected.
  Violates the epic's first acceptance item outright; `DENIED_ENV`
  exists because env credentials leak into transcripts, errors and
  child processes.
- **B — Operator supplies the credential per call:** rejected. A press
  per *call* — reads included — makes the system unusable and trains
  rubber-stamping.
- **C — Proxy custody, operator enrollment, per-agent grants:**
  **adopted.** The daemon-side proxy holds credentials; the operator
  enrolls and revokes; agents hold grants (scope references) and never
  bytes. Two exchange shapes cover both platform classes (§5.3).

### 3.4 What a pending effect is

- **A — A new effect-queue mechanism beside the request system:**
  rejected. It would re-grow open/wait/respond/surface/notify with a
  second audit surface and a second set of caller rules to prove.
- **B — A brokered request of `kind:"effect"`, extended durable:**
  **adopted.** §1.3's mechanism already binds raises to the owning
  agent (CAD-376), answers to the operator or PM (CAD-370), surfaces
  rows to `agent_requests` and the board, and dedupes retries. The one
  genuine addition is durability across restart for the
  accept→execute edge (§5.4), which approvals never needed because
  their safe failure is `closed`.
- **C — Audit-only, log the send after it happens:** rejected. A
  recorded send is not a gate; the epic requires the press *before*.

### 3.5 What an `accept` does

- **A — Releases the agent to run the call itself:** rejected twice
  over. The agent holds no credential (custody), and an agent
  re-issuing after approval could change the input the operator
  approved.
- **B — Executes the staged input daemon-side:** **adopted.** The
  operator approves the exact bytes; the proxy fires exactly those;
  "send is never executed by an agent" (AOS-49) is structural, not a
  rule an agent can break.

## 4. Decision

Adopt **B** at every fork: effects are declared in the adapter's tool
table; the undeclared default is `send`; credentials live in proxy
custody behind operator enrollment and per-agent grants; a pending
effect is a `kind:"effect"` brokered request backed by a durable row;
and `accept` executes the staged input daemon-side. §5 is the
normative contract.

## 5. The contract

### 5.1 Terms

- **Platform** — an external service cadence acts on: a named
  integration (`cloudflare`, `agenticos`) plus an **account** handle
  the operator enrolls.
- **Adapter** — the code that knows a platform's API. Its tools are
  exposed to agents as **MCP tools** — the stdio shape
  `cadence mcp-permission` already proves — and it ships a **tool
  table**: `{tool name → {effect, scopes}}`, reviewed like code (C2).
- **Effect** — one of `read`, `draft`, `send` (§5.2).
- **Credential** — platform bearer material (API token, OAuth grant).
  It exists only in custody (§5.3); nothing else may hold it.
- **Grant** — `(agent, platform, account, scopes)` — the operator's
  record that an agent may call into a platform at those scopes.
- **Pending effect** — a staged send-class call awaiting the press,
  represented per §5.4.
- **Proxy** — the daemon-side executor that checks effects and scopes,
  attaches credentials, stages sends, and executes accepted effects.

### 5.2 Effects

Every tool in an adapter's table declares exactly one effect:

| Effect | Meaning | At call time |
|---|---|---|
| `read` | Observes platform state; the platform records no mutation | Executes at once through the proxy |
| `draft` | Mutates, but produces only a revocable, non-authoritative artifact — a preview deploy, a draft post, a staged config | Executes at once through the proxy |
| `send` | Commits an outward-visible or irreversible act — production deploy, publish, deliver a message, spend, delete | Never executes in the call; becomes a pending effect (§5.4) |

- **The discard test:** an artifact is `draft` only if discarding it
  needs no outward act. If deleting the preview or retracting the
  draft itself sends, the tool is `send`. A tool that both reads and
  sends is `send`.
- **Cloudflare, as the contract expects CAD-367 to classify:**
  preview deploy is `draft` (epic: "preview deploy automatic");
  production deploy is `send` ("production asks"); Workers reads are
  `read`.
- **Undeclared is `send`** (C3): absent from the table, absent field,
  malformed value, or a value outside the vocabulary — all gate as
  send. There is no `none`/`none-needed` class; a tool with truly no
  platform effect does not belong in a platform adapter.
- **The declaration is not agent input** (C2): an `effect` argument in
  a call is ignored; a tool descriptor supplied by an agent is not an
  adapter. Declarations change only by reviewed adapter change.
- Honest residual: the contract guarantees the *gate*, not that a
  platform behaves as declared. A tool misdeclared `read` that
  actually sends is a review defect, not a contract hole — the §5.6
  fixture carries per-tool effect assertions so a misdeclaration diffs
  loudly and both implementors test the same table.

### 5.3 Credential exchange

Custody is daemon-side; the agent-visible artifacts are references,
never bytes.

- **Exchange shape 1 — operator-enrolled scoped token.** For token
  platforms (Cloudflare, CAD-367): the operator mints a narrowly
  scoped token at the platform (the epic requires a Workers-scoped
  token) and enrolls it through an operator-gated verb. The token
  crosses the control socket once, operator→daemon. It never appears
  in a message, a tool result, a pending row or an event — enrollment
  records `{platform, account, scopes, fingerprint, enrolled_at, by}`
  where `fingerprint` is the SHA-256 prefix `src/secret/mod.rs` already
  uses for findings.
- **Exchange shape 2 — consent exchange.** For OAuth-class platforms
  (AgenticOS v2, AOS-49): a device-style code plus email-OTP sign-in
  plus a scope-consent screen, which the *operator* completes; the
  platform issues a scoped, revocable credential directly to the
  daemon. No browser cookies are copied. The agent that triggered
  connection sees `connected` or `refused`, nothing else.
- **Custody backend** is CAD-366's implementation choice within one
  rule: the store is outside every agent's read confinement
  (Landlock-guarded for managed agents, §1.2) and mode-restricted on
  disk; the OS keychain where the host has one (CAD-367 names it),
  else an `0600` daemon-owned store. The P4 residual stands: a
  same-uid process that ignores the API can still read a file —
  custody narrows exposure to offered surfaces, it does not claim a
  same-uid boundary.
- **Per-agent scope grants.** A call is legal only inside the agent's
  grant `(alias, platform, account, scopes)`; the proxy checks the
  grant before any platform traffic and a refusal names the missing
  scope. The operator grants and revokes; the grant list is a record
  an agent may read about itself, so a worker can ask "what am I
  allowed" without touching a credential.
- **Revocation.** Revoking a credential or a grant is an operator act
  that takes effect at the proxy immediately; pending effects bound to
  a revoked credential close unanswered (`closed`, reason named).
- **Rotation** re-enrolls under the same `(platform, account)` handle;
  grants do not change.

### 5.4 The pending-effect API

A pending effect is one staged send-class call, represented to the
system as a brokered request of `kind:"effect"` and to storage as a
durable row keyed by an `effect_id`.

**Fields** (the record `agent_requests` and the board show):

| Field | Rule |
|---|---|
| `request` | the brokered handle; dedupes a retried open (`existing:true`) |
| `kind` | `"effect"` |
| `agent` | the calling alias, connection-derived as in CAD-376 — never a request field |
| `platform`, `account` | the enrolled handle pair |
| `tool` | the adapter tool name |
| `effect` | `"send"` — reads and drafts never pend |
| `input_summary` | one bounded line for list surfaces; secret-guarded like every durable agent text — a credential-shaped span in it refuses the open |
| `input` | the exact staged call arguments the press approves; no credential field exists |
| `effect_id` | the durable identity; one effect = one request = one execution |
| `state` | `waiting` → `decided` → `executing` → `done` / `failed`, or `closed`, or `reconcile` |
| `decision` | `{by, at, reason?}` — `by` is the verified presser |
| `outcome` | the platform result or platform error, parked as the request answer |

**Lifecycle:**

1. **Stage.** A send-class call does not fire. The proxy records the
   row `waiting`, opens the `kind:"effect"` request, sets the agent
   `waiting_input`, and notifies the upstream PM once — the existing
   dedupe means a retried open can never double-notify or double-stage.
   Whether the open rides `request_open` from the agent's MCP bridge
   (already bound to the owning connection by CAD-376) or is created
   internally by the executor is CAD-366's choice; the representation
   is fixed by this contract either way.
2. **Surface.** `agent_requests` lists it; the overview/board row shows
   platform, tool, summary and input for review. What the press
   approves is *this input* — there is no re-prompt path that could
   swap bytes after approval.
3. **Press.** `agent_respond` on an effect request takes
   `accept`|`decline` and `reason`. **Accept is operator-only** — a
   narrowing of `authorize_respond`'s CAD-370 rule (operator or the
   requester's PM) for this kind: the epic's "operator press" is taken
   literally, and §7 Q1 asks the operator to confirm. Decline stays
   operator-or-PM — anyone authorized can refuse; only the operator
   releases.
4. **Execute.** On `accept` — recorded durably *before* the platform
   call fires — the proxy executes the staged input with the custody
   credential and parks the outcome as the request answer. On
   `decline` it parks the reason; nothing executes. The waiting call
   returns the answer; a platform failure is `failed` (the effect ran
   and failed), distinct from `declined` (it never ran).
5. **Deadline.** The requester's deadline closes the row; a
   boundary-parked answer still lands, per the existing
   mailbox-before-remove ordering. And because execution keys on the
   durable `decision`, not on the waiter, a press that lands in the
   boundary window still counts: the operator's accept is
   authoritative whether or not the agent is still waiting.
6. **Restart.** Today's pending map is in-memory and a restart reads
   `closed` — safe for approvals, which fail closed. For effects the
   dangerous edge is `accept` recorded, execution unproven. So the
   effect row is durable: `waiting` rows re-park after restart;
   `decided(accept)` without a recorded outcome reconciles as
   `unknown`/`reconcile` on the board for an operator decision —
   **never re-fired automatically**, because the platform call may
   have happened. Ambiguous outcomes stop for reconciliation; that is
   the standing cadence rule for non-atomic side effects, applied here.
7. **Exactly once.** One `effect_id` executes once: handle dedupe
   covers transport retries, the durable row covers restart, and
   step 6 covers the ambiguous window. Where a platform offers its own
   idempotency key the adapter attaches it; where it does not, the row
   is the guarantee and reconciliation is the honest remainder.

**What an agent can never do:** hold credential bytes; execute a send
(custody makes it structural, not policed); declare or alter an
effect class; widen its own grants; answer its own pending effect —
and for `kind:"effect"`, its PM cannot release either (step 3); learn
a credential from an error or row (redaction and the secret guard ride
every field).

### 5.5 Audit

Events, all carrying fingerprints and handles, never secrets:
`platform_connected` / `platform_disconnected`, `scope_granted` /
`scope_revoked`, `effect_requested` (the `request_opened` event with
`kind:"effect"`), `effect_decided` `{by, decision, reason?}`,
`effect_executed` / `effect_failed` `{outcome summary}`,
`credential_revoked`. The chain from request row → decision → outcome
is the audit story for every send, and `docs/AUDIT.md` gains these
event names when CAD-366 lands.

### 5.6 The shared fixture

AOS-49 requires a "contract fixture shared with Cadence P4". The
fixture is a machine-readable copy of this contract — the tool-table
schema (`{tool, effect, scopes}`), the pending-effect record schema,
and worked vectors (a declared read executes; an undeclared tool
parks; a send parks then executes on accept; a decline never fires) —
produced by CAD-366 and consumed by both repos' test suites. This ADR
names it; it does not ship it.

## 6. Consequences and residuals

- **CAD-366 builds:** the custody store, the grant records, the proxy
  (effect check → execute or stage), the durable pending-effect row
  and its restart reconciliation, the `kind:"effect"` request
  extension, and the §5.6 fixture.
- **CAD-367 builds:** the Cloudflare adapter and tool table (preview
  `draft`, production `send`), keychain custody of the Workers-scoped
  token, and the consolidated platforms UI (live/partial/planned
  services, the access matrix on grants, pending-effect rows).
- **AgenticOS v2** implements the same contract from AOS-49's side and
  validates against the same fixture.
- **Residuals, honestly:** same-uid reads of the custody store (P4);
  a misdeclared tool is only caught at review (§5.2); a platform
  outage between accept and execution reconciles rather than retries,
  so an accepted send can legitimately land `unknown` for a human to
  finish.

## 7. Questions for the operator

- **Q1 — Press authority.** §5.4 step 3 narrows `agent_respond` for
  `kind:"effect"` to the proven operator only; today a requester's own
  PM may also answer (CAD-370). The epic says "operator press" — this
  ADR reads it literally. Confirm, or say that a PM may release sends
  (and whether that holds when the PM is itself an agent).
- **Q2 — Custody backend preference.** CAD-367's acceptance says
  "Workers-scoped token in the keychain". §5.3 leaves the backend to
  CAD-366 (keychain where available, `0600` store otherwise). Say if
  keychain is *required* on hosts that have one, or advisory.
- **Q3 — Draft visibility.** Drafts execute without a press. §5.5
  still audits every one. Should a `draft` also surface on the board
  as an informational row (a soft notice, no wait), or is the event
  log enough until the platforms UI lands?
- **Q4 — Scope of "account".** §5.1 makes `account` part of custody
  and grants (e.g. work vs personal Cloudflare). Confirm one platform
  may hold several enrolled accounts, each with its own grants — the
  UI ticket's "access matrix" assumes so.
