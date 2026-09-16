# Proposed favcrm/cadence — implementation plan

Status: implementation authorized by the user, 2026-09-16. Suggested binary: `cadence` (avoid collision with Harbor container registry tooling). MIT. Org repository name is currently unused; global package/trademark availability not established.

## Outcome

One Rust CLI and persistent local controller on the remote Linux host. The user opens official agent terminals through Orca SSH. A Codex or Claude PM delegates bounded work to registered Codex, Claude, Cursor and Devin workers. The original terminal visibly receives assignments and replies. Durable jobs, evidence and independent QA determine completion. No Orca API dependency.

## Proven transport basis

- Codex: native queue reaches original chat. Separate live proof shows official TUI using `codex resume --remote` displays an externally submitted app-server prompt and response.
- Claude: documented native inbox; live reply test blocked by account weekly limit.
- Devin: original the original Devin session resumed in a managed tmux pane; a bounded message and separate actual reply appeared in the official TUI. ACP and official TUI cannot concurrently own it (session_locked).
- Cursor: ACP responds; official-TUI sharing not proved. Test managed native terminal next.
- PTY delivery: submission is not receipt. The initial Devin paste needed a second Enter; prototype now delays paste submission. Timing delay alone is not a robust transport contract.

## Technology and migration

Rust workspace with CLI/controller/protocol/adapters/storage modules, initially one binary. Candidate crates: clap, tokio, serde/serde_json, rusqlite (bundled SQLite), tracing, a WebSocket client for Codex, platform Unix-socket APIs. Pin versions after compatibility review. SQLite single-writer discipline and migrations. Runtime state outside repositories under XDG state/runtime directories, restrictive permissions. Unix-domain socket local control API, newline JSON request/response and subscription events with sequence cursors.

Keep tmux as an explicit v0.1 runtime dependency for official Devin/Cursor terminal panes; invoke argv directly, never interpolate agent text as shell code. Rust is not a Python launcher. Port behavior incrementally against the current prototype's tests and evidence. Existing Python tooling stays frozen as a reference and migration bridge until parity passes. No automatic import/publication of private .state, login credentials or transcripts.

Provider capabilities: native messaging, managed process, official TUI, attach/resume, cancel, approval, structured events, native turn ID, tools, delivery certainty. Missing capability must be reported rather than emulated silently. ACP remains an optional structured worker mode with its own client view.

## Proposed CLI (not implemented)

cadence doctor
cadence daemon start
cadence job create --brief brief.md
cadence agent launch devin-review --provider devin --job JOB
cadence agent join --provider codex --job JOB --as pm
cadence agent attach devin-review
cadence task assign TASK --to devin-review
cadence message send devin-review --file message.txt --id MESSAGE
cadence message ask devin-review --file question.txt --id MESSAGE
cadence status --job JOB
cadence events --job JOB --follow
cadence agent stop devin-review

Join means supported endpoint registration; it never silently takes over arbitrary existing terminals. Existing locked sessions require an explicit user-controlled exit/resume migration, as tested with Devin.

## State and delivery

Tables: jobs, agents, registrations, tasks, attempts, messages, delivery_attempts, events, artifacts, verdicts. Stable job/agent aliases separate from native provider/session/turn IDs and pane/process IDs. Track queued/submitted/received/answered/unknown independently of planned/running/review/revise/verified/blocked work status.

Durable messages with correlation and semantic idempotency; transactional result + outbox. Per-agent turn serialization. Leases and bounded retry only where safe. Provider execution cannot be atomic with SQLite: ambiguous outcomes stop for reconciliation, not automatic repeated edits. Sender authentication scoped to local job credentials; no peer message grants additional user permissions.

PTY adapter: verify pane/process/native-session ownership, inspect prompt/approval state, preserve operator input, queue while busy, and refuse submission if readiness is uncertain. Literal paste with bounded submit confirmation; never retry the entire message because receipt is unclear. Correlated worker ACK/result through CLI is authoritative; terminal capture is additional visibility evidence. Do not mistake a prompt echo for an assistant answer.

## Development loop / roles

Codex PM: own plan, task scopes, dispatch, status and user decisions.
Devin implementer: own feature branch/worktree, implement one milestone, run checks, return revision and evidence, pause at explicit handoff.
Codex reviewer: inspect diff and independently rerun acceptance checks against the exact revision, emit pass/revise/blocked. Same GitHub login does not constitute a separate approving identity.
Human: approve initial direction/public visibility/license; resolve scope and permission escalation; final release decision for v0.1.

Loop: assign -> ACK -> implement -> report revision/tests -> independent QA -> revise or accepted -> next task. Max two automatic revision cycles per task before escalation; configurable time/token budget, no unbounded conversation loops. Default reports at task ACK, blocker, milestone and QA result; a lightweight daemon checks liveness (no model polling) and sends a status request only after a configurable stall threshold. Until implemented, use the existing CLI return bridge and explicit bounded checks; do not claim permanent autonomous monitoring.

## Milestones / acceptance

M0: review this plan, fix naming/scope; author public-safe source inventory and threat/failure cases. No publication yet.
M1: Rust build, doctor, private socket, SQLite migration, registry, queue/events and CLI. Tests: concurrent send, idempotency conflict, restart replay, unknown execution, ownership. Linux build first.
M2: native-terminal adapters, starting with original Devin and Codex. Tests: visible incoming prompt AND distinct actual reply, same native identity, preserved operator draft, idle/busy routing, permission prompt, disconnect/reconnect. Cursor official TUI and Claude inbox each get independent tests; mark blocked until passed.
M3: job/task lifecycle, worktree scope, kickoff/result schema, revision-bound QA verdict, bounded revision loop. End-to-end seeded bug: worker fix -> reviewer rejects -> revision -> reviewer passes -> PM receives completion in original TUI.
M4: shared PM and worker SKILL.md, JSON output, install/upgrade/docs and release artifacts. Linux x86_64 first, Linux aarch64 and macOS only after CI/native compatibility tests. Publish v0.1.0 with checksums and capability matrix. Agent CLIs, logins, tmux and gh remain external dependencies; no credentials embedded.

## GitHub setup via gh (after direction review)

Recommended repository: public favcrm/cadence, MIT. User reviewed the proposal, chose Cadence, and authorized starting work.

Org API reports GitHub Free. Public repositories support protection/rulesets on this plan; private enforcement requires an eligible paid plan. Do not silently replace required protections with conventions.

Bootstrap reviewed scaffold/CI, then protect main before feature work. Squash only; disable merge/rebase commits; delete merged branches; require PRs, resolved discussions, linear history, passing `fmt`, `clippy`, `test`, `build`, and exact-revision `qa-verdict` checks. Disallow force-push/delete; apply policy to administrators. Require one independent GitHub approval once a distinct eligible reviewer account/App is available. With one shared gh identity, do not create a self-approval deadlock or claim an agent review is a GitHub approval; keep merge blocked until that reviewer is configured, or obtain an explicit alternate policy decision.

Use trusted review/check identity for QA; an author-writable PASS file is not sufficient enforcement. Checks must bind to PR head SHA; changed head invalidates verdict. Required workflow runs must exist before naming required check contexts. Use read-only GITHUB_TOKEN by default, pin third-party Actions by commit, and do not run untrusted PR code with publish credentials.

Keep publication separate from merge: verified tagged revision -> CI builds -> checksums -> draft release -> review -> publish using gh. Enable automatic merge only if supported and after required reviewer/check setup is validated. Query rules/settings back with gh and record evidence; no silent bypass.

## Deferred

Windows, cross-host public brokers, custom terminal emulator, replacing native models, agent marketplace, generalized DAG editor, distributed scheduler, automatic migration of arbitrary running sessions, cost accounting where provider telemetry is unavailable.

## Review consolidation — Devin report received by native PM bridge

Devin completed a planning-only review in original session the original Devin session and sent its result through current_chat.py into this original Codex conversation. Receipt and report were read by the PM. This establishes the planning report-back path; it does not establish a persistent scheduler.

Accepted corrections:
- M0 produces PROTOCOL.md: canonical managed wire API, endpoint capability matrix, delivery/work state machines, errors, recovery rules and fixtures. Port the managed API only; legacy cooperative jobs/mail are reference material, not a second compatibility target.
- Start with one Rust crate and internal modules. Split crates only when boundaries justify it.
- Register endpoint_kind (app_server, native_inbox, pty, acp) separately from provider and ownership. PTY paste/capture alone can only establish submission/visibility. A separately correlated explicit worker acknowledgement may establish receipt; it is a distinct evidence source, not inferred from pasted text.
- Add `agent requests` and `agent respond`; preserve pending input and never automatically accept permissions.
- Daemon: foreground `daemon run` plus opt-in systemd user service for Linux. Socket and state in private XDG directories; graceful stop preserves records. No two-hour production expiry. SSH-disconnect survival must be tested against host user-service lifecycle; do not silently enable lingering.
- Model free/locked_by_external/migrating/owned session ownership. External locks are a refusal, not permission to kill or impersonate the owner.
- M2 has separate provider acceptance gates. Codex official-TUI WebSocket attach first; Devin native PTY route; Claude inbox; Cursor native terminal. ACP no-tools Devin remains a separate capability, not proof of coding support.
- Private Unix socket directory plus peer-UID verification establishes same-user access. This does not authenticate one same-UID agent against another; job-scoped credentials are routing/authorization aids, not a hostile same-user isolation boundary.
- Check repository, binary and distribution naming before publication.

PM corrections to the review itself:
- Claim leases, stale report fencing and pm_wait consumer fencing belong to the legacy cooperative API. Do not describe them as already proven managed-agent features or port all 41 tests indiscriminately. Inventory tests by API and retain only applicable guarantees; new job-layer features require their own tests.
- A Codex clientUserMessageId is useful correlation metadata. Treat provider-side deduplication as unverified until its actual contract and duplicate-submission behavior are checked. Never use it to justify blind retries after an unknown outcome.
- Recovery marks in-flight work unknown and fences that actor. Only eligible idle actors may resume automatically; do not relaunch all enabled actors without regard to uncertainty.
- PTY endpoints may receive explicit application-level ACKs; prohibit inferred receipt from paste rather than prohibiting all future acknowledgement on that endpoint.
- Selected source inspection, test reports and previous live proofs are different evidence levels; the review's broad statement that all hardest invariants are already proven is too strong.

M1 work packages: (1) scaffold/doctor; (2) protocol/socket; (3) storage/migrations/recovery; (4) registry/ownership; (5) queue/events/fake-adapter tests; (6) managed Codex adapter and approval brokering; (7) implemented CLI surface only; (8) differential checks against the applicable Python managed scenarios. Do not advertise unimplemented commands as working.
