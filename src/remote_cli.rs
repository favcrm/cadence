//! CAD-1019 slice 2: the remote CLI transport — allowlisted verbs over the
//! AgenticOS `POST /__platform/cli/{authorize,call}` relay (AOS-128, contract
//! `docs/design/remote-cli.md`) and the `POST /__platform/wiki/{authorize,call}`
//! wiki relay for the wiki actions (CAD-1401). A selected `Remote` org routes a
//! command here instead of the local daemon socket; there is no local fallback.
//!
//! Wire contract (pinned to `origin/cadence/aos-128-hosted-cadence-cli-allowlisted-p`,
//! `hosted-cadence-auth.ts` + `board/routes.ts` + `hosted-cadence/wiki.ts`):
//!
//! - `POST {endpoint}/__platform/cli/authorize` — bearer `hct_` → server-minted
//!   cli actor envelope. Body is exactly `{version:"hosted-cadence-cli.v1",
//!   organization_id, audience, requested_scopes:["cli.read","cli.write"],
//!   ttl_seconds}` (a strict object — an extra key is `invalid_request`).
//!   Container commands consume the envelope's `jti` in the native handler.
//!   Platform team writes claim a canonical, audited first-mutation receipt;
//!   roster reads and no-effect lookups do not consume an unused mutation nonce.
//!   [`call_once`] still mints a fresh envelope for every invocation and retry.
//!   `503 waking` is returned before any container forward, but a later failure
//!   can follow a committed effect. Never reuse an envelope or retry an uncertain
//!   mutation as though it had failed.
//! - `POST {endpoint}/__platform/cli/call` — same bearer + `{version,
//!   envelope, verb, arguments?}` — `verb` is one of
//!   [`REMOTE_VERBS`]; platform team arguments dispatch in the Worker without
//!   waking a container; other arguments reach `/api/cli/<verb>`. Authority keys
//!   (`actor`, `org`, `user`, `as`, `principal`, `wiki_as`) are refused
//!   server-side; this client never sends them. A team `role` argument is the
//!   requested *target* role, not caller authority: it is validated against
//!   `owner`/`member` and never establishes the caller's own role.
//! - `401/403` — the bearer no longer resolves to a live principal: re-login.
//! - `503 {"state":"waking","retry_after_s":N}` + `Retry-After` — the company
//!   container is waking; wait and retry inside the wake budget. A retry is
//!   safe: the refusal came back before the verb ran. `wake_failed`
//!   (`runtime_stopped`/`not_found`/`unavailable`) and every other failure
//!   end the command — never a redirect chase, never another org, never local.
//!
//! CAD-2001 adds the project-scoped review-evidence verbs
//! ([`PROJECT_SCOPED_GATE_VERBS`]). Each carries an explicit `project`, and
//! the two approvals also carry the `issue` they authorize, so no read can
//! answer from another project's evidence and no write is authorized by a
//! repo's first project. They are newer than the tenant that serves them: the
//! release order is the tenant server first, then this client, and a tenant
//! that does not know the name is refused by [`tenant_cannot_serve`] with
//! that order named. The older repo-scoped verbs stay in the tenant for old
//! clients; this build never sends them for the gate or the operator and
//! never falls back to them when the project is known.

use base64::Engine;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};
use crate::remote_auth::auth_dir;

const CLI_VERSION: &str = "hosted-cadence-cli.v1";
const WIKI_VERSION: &str = "hosted-cadence-wiki.v1";
/// `ttl_seconds` ceiling (`HOSTED_CADENCE_MAX_TTL_SECONDS`): the server also
/// clamps to the live credential's expiry, so asking the maximum is exact.
const MAX_TTL: u64 = 300;
/// Authorization/call bodies are small; answers can carry a whole tracker
/// board. Bounded either way (the relay caps upstream bodies at 16 MiB).
const MAX_BODY: u64 = 8 * 1024 * 1024;
/// No single call waits on a waking/loaded remote past this; the wake
/// budget (`--wake-timeout`, default 120 s) caps the whole retry loop.
const CALL_TIMEOUT: Duration = Duration::from_secs(90);
const AUTH_TIMEOUT: Duration = Duration::from_secs(20);
/// Default `--wake-timeout`.
pub const WAKE_TIMEOUT_DEFAULT: u64 = 120;
/// Upper bound on one wake wait, whatever the remote advertises.
const WAKE_WAIT_MAX: Duration = Duration::from_secs(30);

/// The allowlisted remote verbs this build can send — the CAD-1019 contract
/// ∩ AOS-128's `HOSTED_CADENCE_CLI_VERBS`: a command not listed here is
/// refused *locally* and its bytes never leave the process.
///
/// Platform team verbs are in this allowlist and are not native container
/// verbs: the hosted call path carries them, and the platform answers them
/// without a daemon.
///
/// Writes ride the same single-use envelope. The caller's authority never
/// travels as a request field, only inside the envelope [`call_once`] mints
/// for that attempt. A failed or ambiguous call is never retried: the wake
/// retry is the only loop, and it fires only on a pre-forward `waking`
/// verdict, re-minting because that refusal happens before the envelope is
/// consumed. Anything else stays unresolved.
///
/// The cli.write half (`issue_new`/`issue_comment`/`issue_set`/
/// `issue_attach`/`issue_review`/`project_policy`/`project_new`/
/// `message_send`/`team_*`, platform calls, the settings write operations and
/// the audit writes) rides the same envelope — the caller's authority never
/// travels as a request field, only inside the verified, single-use envelope.
/// A failed or ambiguous call is never retried by the client: the wake retry
/// is the only loop, and it fires only on a pre-forward `waking` verdict.
/// Every mutation is classified once in [`cli_effect`] / [`wiki_effect`]
/// (with its reconciliation read), so a timeout, a dropped connection, a 5xx
/// after forward, or a 2xx without a receipt stays explicitly unresolved.
/// A 2xx must also carry the verb's own correlated receipt: issue, project,
/// wiki and team writes name the thing written, and settings writes answer
/// their own bounded receipt. A malformed or foreign receipt is the same
/// unknown outcome, never a false success. The wiki `write` action is
/// classified the same way on its own surface. A proven pre-effect refusal
/// keeps its definite verdict. For `message_send`, the caller-chosen `message`
/// id in
/// `arguments` is the reconciliation key: `message_read` returns the
/// principal's own durable message by id, and resubmitting the same id
/// dedupes server-side — never a blind replay. A 2xx proves the enqueue
/// only as the complete correlated receipt
/// ([`send_receipt_correlates`]): an absent, malformed or foreign-bound
/// body is `unknown`, the same unresolved outcome a lost response has.
///
/// CAD-1586 / AOS-290: `settings_agents` and `master_status` mint narrow
/// `settings.*` envelopes (per operation, never the broad `cli.*` pair) —
/// see [`settings_op_scope`].
pub const REMOTE_VERBS: &[&str] = &[
    // cli.read
    "status",
    "my_work",
    "agent_list",
    "agent_show",
    // CAD-1964: the role catalogue. A hosted org has no daemon, so the board
    // answers from its pinned copy of this build's own catalogue.
    "agent_roles",
    // CAD-1964: reusable agent profiles. Reads only; the hosted board applies
    // the caller's own role and wiki access, and returns no agent identity.
    "agent_profile_list",
    "agent_profile_context",
    // CAD-2101: one stored profile with its revision.
    "agent_profile_show",
    "issue_ls",
    "issue_show",
    "issue_history",
    "issue_messages",
    "issue_artifact",
    // AOS-346: artifacts a worker or the assistant produced. The upload is its
    // own multipart path, `upload_artifact`.
    "artifact_find",
    "artifact_show",
    "artifact_read",
    "message_read",
    "message_inbox",
    "team_list",
    "team_invites",
    // CAD-1999: the hosted org's own project registry — the projects a
    // workspace has registered, plus the ones that exist only as their
    // tickets' key. A read; the write beside it is `project_new`.
    "project_ls",
    // cli.write
    "issue_new",
    "issue_comment",
    "issue_attach",
    "issue_review",
    "issue_message_send",
    "issue_messages_read",
    "issue_set",
    "team_invite",
    "team_revoke_invite",
    "team_set_role",
    "team_remove",
    "project_policy",
    // CAD-1999: register an empty project — its key, its human name and its
    // issue-id prefix — before any card exists. The tenant Worker answers it
    // from its own storage, and requires a live human owner: a project key
    // and prefix are workspace structure every later card inherits.
    "project_new",
    // CAD-2101: define or edit an agent profile. The board requires a live
    // human owner and owns the field limits and the revision fence.
    "agent_profile_create",
    "agent_profile_update",
    "message_send",
    // CAD-1586 / AOS-290: scoped settings + master diagnostics.
    "settings_agents",
    "master_status",
    // CAD-1678 / AOS-328: the hosted workspace's own tool catalog. `tools`
    // needs `cli.read`, `call` needs `cli.write`; the Worker decides per
    // operation, so this client asks for the historical pair.
    "platform_call",
    // CAD-1946: the merge gate. Reads (cli.read) and the operator's merge
    // approval (`audit.approve`, which only `cadence login --audit-approve`
    // requests; the hosted org admits it for the human owner only).
    "delivery_policy",
    "delivery_policy_set",
    "audit_approval",
    "audit_verdicts",
    "audit_verdict",
    "audit_scope",
    "audit_approve",
    // AOS-473: the owner's reviewer attestations. The read is scoped like
    // every other gate read; the write rides the same `audit.approve` login
    // as the merge approval, because an attestation is the owner's own act.
    "audit_reviewers",
    "audit_reviewers_set",
    // CAD-2001: the project-scoped review-evidence path. Every one of these
    // carries an explicit `project` (and `issue` for the approvals), so a
    // read can never answer from another project's evidence and a write can
    // never be authorized by a repo's first project. The tenant serves the
    // repo-scoped verbs above to old clients; this build never sends them for
    // the gate or the operator, and a tenant that predates these names is
    // refused with the release order named — never a silent fallback.
    "audit_approval_project",
    "audit_verdicts_project",
    "audit_verdict_project",
    "audit_approve_project",
    "audit_reviewers_project",
    "audit_reviewers_project_set",
    "audit_identities",
    "audit_identities_set",
];

/// Scope a cli authorize request asks for. Existing container verbs keep
/// the historical `cli.read`+`cli.write` pair. Platform team verbs ask only
/// for the scope that verb requires.
fn cli_requested_scopes(verb: &str) -> &'static [&'static str] {
    match verb {
        "team_list"
        | "team_invites"
        | "delivery_policy"
        | "audit_approval"
        | "audit_verdicts"
        | "audit_reviewers"
        | "audit_scope"
        | "audit_approval_project"
        | "audit_verdicts_project"
        | "audit_reviewers_project"
        | "audit_identities"
        | "agent_profile_list"
        | "agent_profile_context"
        | "agent_profile_show"
        | "agent_roles"
        | "project_ls" => &["cli.read"],
        // The owner's merge approval and the delivery inputs it binds both
        // ride the opt-in `audit.approve` login (`cadence login --audit-approve`).
        "audit_verdict" | "audit_verdict_project" => &["cli.write"],
        // CAD-1999: creating a project is a workspace-structure write; the
        // board also requires a live human owner. Reading the registry is a
        // plain cli.read above.
        "project_new" | "agent_profile_create" | "agent_profile_update" => &["cli.write"],
        // CAD-2001: attesting project eligibility or a principal's GitHub
        // identity decides whose verdicts count, so both writes need the same
        // `audit.approve` login as the merge approval — and the tenant admits
        // them for the live human owner only.
        "audit_approve"
        | "audit_approve_project"
        | "audit_reviewers_set"
        | "audit_reviewers_project_set"
        | "audit_identities_set"
        | "delivery_policy_set" => &["cli.write", "audit.approve"],
        "team_invite" | "team_revoke_invite" | "team_set_role" | "team_remove" => &["cli.write"],
        // Asking for one scope makes a login without it a refusal at mint, which
        // can be named. Asking for both would mint a narrower envelope, and the
        // call would then get a bare 401 that reads like an expired login.
        "issue_message_send" | "issue_messages_read" => &["cli.write"],
        "issue_messages" => &["cli.read"],
        _ => &["cli.read", "cli.write"],
    }
}

/// What one attempted hosted call can change, and — for a mutation — the read
/// that reconciles an answer which does not prove its effect.
///
/// The classification is total: [`cli_effect`] names every verb in
/// [`REMOTE_VERBS`], [`wiki_effect`] every action in [`WIKI_ACTIONS`], and an
/// unlisted name fails closed as a possible mutation, so no call that may have
/// been applied can be reported as a definite refusal. A read changes nothing,
/// so its lost answer proves nothing ran.
#[derive(Clone, Copy)]
enum Effect {
    Read,
    Mutation { reconcile: &'static str },
}

impl Effect {
    fn is_mutation(self) -> bool {
        matches!(self, Self::Mutation { .. })
    }

    /// The read that reconciles a mutation whose answer was lost or did not
    /// prove the write; `None` for a read, which has no effect to reconcile.
    fn reconciliation(self) -> Option<&'static str> {
        match self {
            Self::Mutation { reconcile } => Some(reconcile),
            Self::Read => None,
        }
    }
}

/// One mutating effect: the verb can change durable state, and `reconcile` is
/// the read a caller runs before repeating it when the answer is lost or does
/// not prove the write.
fn mutation(reconcile: &'static str) -> Effect {
    Effect::Mutation { reconcile }
}

// Reconciliation guidance for every mutation this build can send: what the
// caller reads before repeating a write whose answer was lost, post-forward or
// unusable. A note never claims the write failed and never invites a blind
// retry.
const RECONCILE_SEND: &str = "the send may or may not have been recorded";
const RECONCILE_ISSUE_NEW: &str = "the ticket may or may not have been created — read `issue ls` for the same project (and `issue show` the id or title) before filing it again";
const RECONCILE_ISSUE_COMMENT: &str = "the comment may or may not have been recorded — read `issue show` for the same issue before repeating it";
const RECONCILE_ISSUE_SET: &str = "the edit may or may not have been applied — read `issue show` for the same issue (a new rev proves it) before repeating it";
const RECONCILE_ISSUE_ATTACH: &str = "the attachment may or may not have been recorded — inspect the same issue and artifact before repeating it";
const RECONCILE_ISSUE_REVIEW: &str = "the verdict may or may not have been recorded — read the same issue before repeating it; a reviewer holds one verdict per revision";
const RECONCILE_ISSUE_MESSAGE_SEND: &str =
    "the message may or may not have been stored — repeat `issue say` with the same \
    --message, --to, --from and text: an identical retry reports `duplicate` and stores \
    nothing new, and any difference is refused as a conflict";
const RECONCILE_ISSUE_MESSAGES_READ: &str =
    "the read point may or may not have moved — repeating the same call is safe, because \
    it only moves forward";
const RECONCILE_PROJECT_POLICY: &str = "the recorded policy may or may not have been written — read the project's recorded policy again before repeating it";
const RECONCILE_PROJECT_NEW: &str = "the project may or may not have been registered — read `issue project ls` with the same explicit --org before repeating it; the same key, name and prefix is the same project";
const RECONCILE_PROFILE_CREATE: &str = "the profile may or may not have been saved — read `agent profiles` with the same explicit --org before repeating it; the same alias is refused as taken if it landed";
const RECONCILE_PROFILE_UPDATE: &str = "the edit may or may not have been applied — read `agent profile get <id>` with the same explicit --org before repeating it; the revision it shows says whether the edit landed";
const RECONCILE_PLATFORM_CALL: &str = "the tool call may or may not have run and been charged — replay the same command; the relayed key returns the door's recorded outcome instead of running it a second time";
const RECONCILE_DELIVERY_POLICY: &str = "the project's delivery inputs may or may not have been recorded — read the project's delivery policy again before repeating it";
const RECONCILE_TEAM: &str =
    "the team change may or may not have been applied — inspect `team list` or \
     `team invites` with the same explicit --org before repeating it";
const RECONCILE_SETTINGS: &str = "the settings change may or may not have been applied — read `settings agents show` (and `settings agents history`) with the same explicit --org before repeating it";
const RECONCILE_SETTINGS_GRANTS: &str = "the grant change may or may not have been applied — read `settings agents grant list` with the same explicit --org before repeating it";
const RECONCILE_MASTER_STATUS: &str = "the master resume may or may not have been applied — read `master status` with the same explicit --org before repeating it";
const RECONCILE_AUDIT_APPROVE: &str = "the approval may or may not have been recorded — read `audit approval` for the same repo and head before repeating it";
const RECONCILE_AUDIT_APPROVE_PROJECT: &str = "the approval may or may not have been recorded — read `audit approval` for the same issue, repo and head before repeating it";
const RECONCILE_AUDIT_VERDICT: &str = "the verdict may or may not have been recorded — read `audit verdicts` for the same head before repeating it";
const RECONCILE_AUDIT_REVIEWERS: &str = "the attestation may or may not have been recorded — read `audit reviewers` for the same repo before repeating it";
const RECONCILE_AUDIT_REVIEWERS_PROJECT: &str = "the project's eligibility map may or may not have been recorded — read `audit reviewers` for the same project before repeating it";
const RECONCILE_AUDIT_IDENTITIES: &str = "the identity attestations may or may not have been recorded — read `audit identities` before repeating it";
const RECONCILE_WIKI_WRITE: &str = "the wiki write may or may not have been recorded — read the same path with `cadence wiki cat` (or resume the transfer job with `wiki cp --resume <job_id>`) before repeating it";
const RECONCILE_WIKI_UPLOAD: &str = "the upload may or may not have been recorded — list the parent path with `cadence wiki ls` before uploading it again";
const RECONCILE_UNKNOWN: &str =
    "the action may or may not have been applied — read the same resource before repeating it";

/// The effect of one allowlisted CLI verb. Every name in [`REMOTE_VERBS`] is
/// classified here: the reads are listed explicitly, so a verb cannot become a
/// read by omission, and the fallback is a possible mutation rather than a
/// read — a name this table does not know must never turn a lost answer into a
/// definite refusal. (An unlisted name is refused before any request anyway.)
///
/// The team mutations commit in the same D1 batch that records their receipt;
/// the issue writes, `project_policy`, `delivery_policy_set`, the settings
/// write operations and the audit writes each change durable state. A lost or
/// ambiguous answer to any of them follows a possible commit, exactly as a
/// lost send answer does.
fn cli_effect(verb: &str) -> Effect {
    match verb {
        // cli.read: answered from state, changing none of it.
        "status"
        | "my_work"
        | "agent_list"
        | "agent_show"
        | "agent_roles"
        | "agent_profile_list"
        | "agent_profile_context"
        | "agent_profile_show"
        | "issue_ls"
        | "issue_show"
        | "issue_history"
        | "issue_messages"
        | "issue_artifact"
        | "artifact_find"
        | "artifact_show"
        | "artifact_read"
        | "message_read"
        | "message_inbox"
        | "team_list"
        | "team_invites"
        | "master_status"
        | "delivery_policy"
        | "audit_approval"
        | "audit_verdicts"
        | "audit_scope"
        | "audit_reviewers"
        | "audit_approval_project"
        | "audit_verdicts_project"
        | "audit_reviewers_project"
        | "audit_identities"
        | "project_ls" => Effect::Read,
        // cli.write: every one of these can change durable state.
        "issue_new" => mutation(RECONCILE_ISSUE_NEW),
        "issue_comment" => mutation(RECONCILE_ISSUE_COMMENT),
        "issue_set" => mutation(RECONCILE_ISSUE_SET),
        "issue_attach" => mutation(RECONCILE_ISSUE_ATTACH),
        "issue_review" => mutation(RECONCILE_ISSUE_REVIEW),
        "issue_message_send" => mutation(RECONCILE_ISSUE_MESSAGE_SEND),
        "issue_messages_read" => mutation(RECONCILE_ISSUE_MESSAGES_READ),
        "project_policy" => mutation(RECONCILE_PROJECT_POLICY),
        "project_new" => mutation(RECONCILE_PROJECT_NEW),
        "agent_profile_create" => mutation(RECONCILE_PROFILE_CREATE),
        "agent_profile_update" => mutation(RECONCILE_PROFILE_UPDATE),
        "platform_call" => mutation(RECONCILE_PLATFORM_CALL),
        "message_send" => mutation(RECONCILE_SEND),
        // `settings_agents` is refined per operation by [`call_effect`]; without
        // the operation this is a write, the same fail-closed default
        // [`settings_op_scope`] takes.
        "settings_agents" => mutation(RECONCILE_SETTINGS),
        "delivery_policy_set" => mutation(RECONCILE_DELIVERY_POLICY),
        "audit_approve" => mutation(RECONCILE_AUDIT_APPROVE),
        "audit_approve_project" => mutation(RECONCILE_AUDIT_APPROVE_PROJECT),
        "audit_verdict" | "audit_verdict_project" => mutation(RECONCILE_AUDIT_VERDICT),
        "audit_reviewers_set" => mutation(RECONCILE_AUDIT_REVIEWERS),
        "audit_reviewers_project_set" => mutation(RECONCILE_AUDIT_REVIEWERS_PROJECT),
        "audit_identities_set" => mutation(RECONCILE_AUDIT_IDENTITIES),
        "team_invite" | "team_revoke_invite" | "team_set_role" | "team_remove" => {
            mutation(RECONCILE_TEAM)
        }
        _ => mutation(RECONCILE_UNKNOWN),
    }
}

/// The effect of one wiki action. `write` is the only mutating action — a text
/// put or a transfer `begin`/`chunk`/`apply`/`finish`; `enumerate`, `read` and
/// `job_status` (status/export) change nothing.
fn wiki_effect(action: &str) -> Effect {
    match action {
        "enumerate" | "read" | "job_status" => Effect::Read,
        "write" => mutation(RECONCILE_WIKI_WRITE),
        _ => mutation(RECONCILE_UNKNOWN),
    }
}

/// The effect of one call on its surface. Settings operations have distinct
/// scopes, while platform catalogue discovery is a read and a tool call can
/// change state. Unknown operations fail closed as writes.
fn call_effect(surface: Surface, verb: &str, arguments: &Map<String, Value>) -> Effect {
    match surface {
        Surface::Cli if verb == REMOTE_SETTINGS_VERB => {
            let operation = arguments
                .get("operation")
                .and_then(Value::as_str)
                .unwrap_or_default();
            // The hosted scope is deliberately broader than this read's effect.
            if operation != "fleet.check" && settings_op_scope(operation) == "settings.write" {
                mutation(settings_reconcile(operation))
            } else {
                Effect::Read
            }
        }
        Surface::Cli
            if verb == "platform_call"
                && arguments.get("operation").and_then(Value::as_str) == Some("tools") =>
        {
            Effect::Read
        }
        Surface::Cli => cli_effect(verb),
        Surface::Wiki => wiki_effect(verb),
    }
}

/// Operation-specific read for a settings mutation whose answer was lost or
/// unusable. Ordinary config writes reconcile with `settings agents show`;
/// grant mutations with `settings agents grant list` (where requests/grants
/// live); the bounded master recovery with `master status`. Unknown operations
/// take the conservative show/history default.
fn settings_reconcile(operation: &str) -> &'static str {
    match operation {
        "master.resume" => RECONCILE_MASTER_STATUS,
        "grant.request" | "grant.allow" | "grant.reject" | "grant.revoke" => {
            RECONCILE_SETTINGS_GRANTS
        }
        _ => RECONCILE_SETTINGS,
    }
}

/// A CLI verb that can change server state. When the answer to one of these is
/// lost or ambiguous, the mutation may already have landed, so the outcome is
/// unknown and the caller must reconcile with a read before repeating it —
/// never a plain refusal, and never a bare retry.
fn mutating_cli_verb(verb: &str) -> bool {
    cli_effect(verb).is_mutation()
}

/// The read that reconciles a hosted mutation whose answer was lost or did not
/// prove the write; `None` for a read. Crate-private so the verb-specific
/// receipt checks outside this module (`delivery::hosted_gate`) name the same
/// guidance as the transport classification here.
pub(crate) fn mutation_reconcile(verb: &str) -> Option<&'static str> {
    cli_effect(verb).reconciliation()
}

/// The read a caller must run before repeating a mutating verb whose answer
/// was lost. A lost response is not a refusal: the write may have landed, so
/// the next step is a read of the same workspace, never a blind retry.
/// `message_send` reconciles on its own stable id; the team mutations
/// reconcile with the directory read.
///
/// Production code reads the note from the [`Effect`] it already classified
/// (so `settings_agents` is judged by its operation and the wiki surface keeps
/// its own note); this per-verb view is what the tests assert.
#[cfg(test)]
fn lost_mutation_note(verb: &str) -> &'static str {
    // Callers only ask about a mutation; a read has no effect to reconcile,
    // and says so rather than inventing one.
    mutation_reconcile(verb).unwrap_or("nothing was changed — this verb is a read")
}

/// The board refused the call at its own gate, before the verb reached the
/// platform. Its refusal carries the `board` hop marker; the platform's own
/// answers do not. A gate refusal is therefore a definite pre-effect
/// refusal, not an ambiguous outcome.
fn board_gate_refused(body: Option<&Value>) -> bool {
    body.and_then(|b| b.pointer("/error/hop"))
        .and_then(Value::as_str)
        == Some("board")
}

/// The explicitly uncertain answers take precedence over any wake hint: a
/// contradictory response cannot prove the write was not forwarded.
fn explicit_mutation_uncertainty(body: Option<&Value>) -> Option<&str> {
    match refusal_fields(body).0 {
        Some(code @ ("mutation_outcome_uncertain" | "invite_email_uncertain")) => Some(code),
        _ => None,
    }
}

/// Only the documented 503 waking response authorizes a wake retry.
fn worker_waking(status: u16, body: Option<&Value>) -> bool {
    status == 503
        && body.and_then(|b| b["state"].as_str()) == Some("waking")
        && explicit_mutation_uncertainty(body).is_none()
}

/// The legacy CLI relay also reports a failed wake before forwarding.
/// This state is not proof for separate upload endpoints.
fn cli_wake_failed(status: u16, body: Option<&Value>) -> bool {
    status == 503 && body.and_then(|b| b["state"].as_str()) == Some("wake_failed")
}

/// A 5xx after a possible commit is unknown unless the response proves it
/// preceded the write. CLI wake failure and the native tracker's refusal are
/// such proof only for the CLI route, not for separate upload endpoints.
fn post_forward_failure(status: u16, body: Option<&Value>, cli_route: bool) -> bool {
    status >= 500
        && status != 501
        && !worker_waking(status, body)
        && !board_gate_refused(body)
        && !(cli_route
            && (cli_wake_failed(status, body) || tracker_unavailable_before_verb(status, body)))
        && explicit_mutation_uncertainty(body).is_none()
}

/// The native CLI route opens its tracker before executing a verb and answers
/// HTTP 503 `tracker_unavailable` when that open fails.
fn tracker_unavailable_before_verb(status: u16, body: Option<&Value>) -> bool {
    status == 503 && refusal_fields(body).0 == Some("tracker_unavailable")
}

/// A usable success body for a write is a JSON object: every hosted mutation
/// answers a receipt object, so an absent, `null` or non-object 2xx body proves
/// nothing about the write it answered.
fn is_object_body(body: Option<&Value>) -> bool {
    body.is_some_and(Value::is_object)
}

/// The tenant's `requiredText` uses ECMAScript `String.trim`, whose whitespace
/// differs from Rust's (notably U+FEFF is trimmed, while U+0085 is not).
fn trim_tenant_text(value: &str) -> &str {
    value.trim_matches(|c| {
        matches!(
            c,
            '\u{0009}'..='\u{000d}'
                | '\u{0020}'
                | '\u{00a0}'
                | '\u{1680}'
                | '\u{2000}'..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
        )
    })
}

/// The issue-write receipt, correlated by the issue it names. The two surfaces
/// answer two proven variants:
///
/// - native `write_reply` (`src/ui/cli_route.rs`): `{issue, card, warnings}`
///   for `issue_new`, `issue_comment` and `issue_set`;
/// - tenant `apps/api/src/tenant-board/cli.ts`: `{issue, card, warnings}` for
///   `issue_new` (L683) and `issue_set` (L748), but
///   `{comment, issue}` for `issue_comment` (L767-770) — no card, no warnings.
///
/// A reply whose `issue.id` is not the ticket this call wrote cannot prove the
/// write landed, whatever else it carries.
fn issue_receipt_correlates(verb: &str, reply: &Value, arguments: &Map<String, Value>) -> bool {
    let issue_id = reply.pointer("/issue/id").and_then(Value::as_str);
    let card_id = reply.pointer("/card/id").and_then(Value::as_str);
    let object = |key: &str| reply.get(key).is_some_and(Value::is_object);
    let same = |sent: Option<&str>| {
        issue_id.zip(sent).is_some_and(|(got, want)| {
            !got.is_empty() && !want.is_empty() && got.eq_ignore_ascii_case(want)
        })
    };
    if !object("issue") || issue_id.is_none_or(str::is_empty) {
        return false;
    }
    match verb {
        // The server allocates the id, so a create is bound by the project and
        // title. The tenant trims both; the native route preserves the title.
        "issue_new" => {
            let project = arguments
                .get("project")
                .and_then(Value::as_str)
                .map(trim_tenant_text)
                .filter(|project| !project.is_empty());
            let sent_title = arguments.get("title").and_then(Value::as_str);
            let title = reply.pointer("/card/title").and_then(Value::as_str);
            object("card")
                && card_id == issue_id
                && reply
                    .pointer("/card/project")
                    .and_then(Value::as_str)
                    .zip(project)
                    .is_some_and(|(got, want)| got == want)
                && sent_title
                    .zip(title)
                    .is_some_and(|(sent, got)| got == sent || got == trim_tenant_text(sent))
        }
        // Either variant must name the ticket the comment was sent to.
        "issue_comment" => {
            (object("comment") || object("card"))
                && same(arguments.get("id").and_then(Value::as_str))
        }
        // An edit may legitimately be a no-op that keeps the revision, so only
        // the ticket identity is required — never `rev != if_rev`.
        "issue_set" => {
            object("card")
                && card_id == issue_id
                && same(
                    arguments
                        .get("ids")
                        .and_then(|ids| ids.get(0))
                        .and_then(Value::as_str),
                )
        }
        _ => false,
    }
}

/// `project_policy` answers one of two proven success shapes: the tenant's
/// `{state:"recorded", project, visibility, grants, revision}` and the native
/// receipt must match the project, visibility and grants that were sent.
fn policy_receipt_correlates(reply: &Value, arguments: &Map<String, Value>) -> bool {
    let Some(project) = arguments.get("project").and_then(Value::as_str) else {
        return false;
    };
    let Some(visibility) = arguments.get("visibility").and_then(Value::as_str) else {
        return false;
    };
    let Some(grants) = arguments.get("grants").and_then(Value::as_array) else {
        return false;
    };
    if reply["project"].as_str() != Some(project) {
        return false;
    }
    let tenant = reply["state"].as_str() == Some("recorded")
        && reply["revision"].as_u64().is_some()
        && reply["visibility"].as_str() == Some(visibility)
        && reply["grants"].as_array() == Some(grants);
    let native_visibility = match visibility {
        "workspace-visible" => "workspace_visible",
        "private" => "private",
        _ => return false,
    };
    let members = reply.pointer("/policy/members");
    let native = reply["committed"].as_bool() == Some(true)
        && reply
            .pointer("/policy/version")
            .and_then(Value::as_u64)
            .is_some()
        && reply.pointer("/policy/visibility").and_then(Value::as_str) == Some(native_visibility)
        && members.map_or(grants.is_empty(), |value| value.as_array() == Some(grants));
    tenant || native
}

/// Wiki servers return NFC paths even when a caller supplied decomposed text.
fn wiki_path_matches(requested: &str, received: Option<&str>) -> bool {
    use unicode_normalization::UnicodeNormalization;
    !requested.is_empty() && received.is_some_and(|path| requested.nfc().eq(path.chars()))
}

/// A text `wiki write` receipt names the page it wrote: the tenant answers
/// `{kind:"text", path, rev, size}` and the daemon's `wiki_write` answers the
/// committed record (`committed: true` and its `rev`). A reply that does not
/// echo the written path proves nothing. A transfer `write` op has its own
/// `{job_id,…}`/`{outcome,…}` receipts and is not checked here.
fn wiki_write_receipt_correlates(reply: &Value, arguments: &Map<String, Value>) -> bool {
    let path = arguments.get("path").and_then(Value::as_str);
    let echoes = path.is_some_and(|path| wiki_path_matches(path, reply["path"].as_str()));
    let committed = reply
        .get("rev")
        .and_then(Value::as_str)
        .is_some_and(|rev| !rev.is_empty())
        || reply.get("committed").and_then(Value::as_bool) == Some(true);
    echoes && committed
}

/// Does a lowercase/uppercase hex digest string name exactly `digest`? Compared
/// pairwise, so the check allocates nothing.
fn hex_digest_matches(hex: &str, digest: &[u8]) -> bool {
    hex.len() == digest.len() * 2
        && hex
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .zip(digest)
            .all(|(pair, byte)| {
                let hi = (pair[0] as char).to_digit(16);
                let lo = (pair[1] as char).to_digit(16);
                matches!((hi, lo), (Some(hi), Some(lo)) if (hi * 16 + lo) as u8 == *byte)
            })
}

/// A `write_blob` upload receipt must name the bytes and the path this call
/// sent. Four legitimate success shapes are proven by the two surfaces:
///
/// - tenant text source (`worker/wiki-routes.ts`): `{path, kind:"text", rev,
///   size}` — no digest, but `size` is the decoded text's byte length;
/// - tenant blob: `{path, kind:"blob", sha256, size, mime, rev, status}`;
/// - native text source (`src/ui/wiki.rs` relays `wiki_write`): `{rev,
///   committed:true}`;
/// - native blob (`wiki_put_blob`): `{rev, sha256}`.
///
/// A path the reply states must be the sent path. A digest it states is
/// authoritative: a present but malformed or different digest, and a present
/// `size` that is not an integer equal to the sent byte length, are
/// contradictory evidence — unknown even when another success marker also
/// matches. Truly absent optional fields stay absent (the tenant text variant
/// has no digest; a native reply has no size). Absent a digest, one of the
/// remaining bindings (text size+rev, native committed write) must match.
/// Every proven upload reply names the written path, so a pathless or
/// foreign-path receipt proves nothing.
fn upload_receipt_correlates(
    reply: &Value,
    logical_path: &str,
    expected_sha256: &[u8],
    expected_size: u64,
) -> bool {
    if !wiki_path_matches(logical_path, reply["path"].as_str()) {
        return false;
    }
    // A field the reply carries is authoritative: a present but malformed or
    // mismatched `sha256`/`size` is contradictory evidence, never "absent", so
    // no other success marker may override it. Truly absent optional fields
    // stay absent (the tenant text variant has no digest; a native reply has no
    // size).
    let digest = match reply.get("sha256") {
        Some(value) => match value.as_str() {
            Some(stated) if hex_digest_matches(stated, expected_sha256) => true,
            _ => return false,
        },
        None => false,
    };
    if let Some(size) = reply.get("size") {
        if size.as_u64() != Some(expected_size) {
            return false;
        }
    }
    let text = reply.get("kind").and_then(Value::as_str) == Some("text")
        && reply.get("size").is_some()
        && reply
            .get("rev")
            .and_then(Value::as_str)
            .is_some_and(|rev| !rev.is_empty());
    let committed = reply.get("committed").and_then(Value::as_bool) == Some(true)
        && reply
            .get("rev")
            .and_then(Value::as_str)
            .is_some_and(|rev| !rev.is_empty());
    digest || text || committed
}

/// A platform team mutation receipt names the entity this call changed, and
/// the tenant parses it with the membership wire schema before answering:
/// `{invite}` (id, email, role, status, createdAt, expiresAt), `{member}`
/// (userId, name, email, role, joinedAt) and `{removed}` (the removed
/// userId). The server normalizes an invite address to trimmed lowercase at
/// its boundary and answers `201` for a new invite (the requested role) or
/// `200` for an existing live pending invite, whose stored role may
/// legitimately differ from the requested one. A reply that does not name
/// the sent entity proves nothing about this write.
fn team_receipt_correlates(
    verb: &str,
    status: u16,
    reply: &Value,
    arguments: &Map<String, Value>,
) -> bool {
    let invite = || reply.get("invite").filter(|value| value.is_object());
    match verb {
        "team_invite" => {
            let (Some(invite), Some(email), Some(requested)) = (
                invite(),
                argument_text(arguments, "email"),
                argument_text(arguments, "role"),
            ) else {
                return false;
            };
            let stored = membership_role(invite);
            nonempty_text(invite.get("id")).is_some()
                && nonempty_text(invite.get("email")).is_some()
                && stored.is_some()
                && nonempty_text(invite.get("createdAt")).is_some()
                && nonempty_text(invite.get("expiresAt")).is_some()
                && invite.get("status").and_then(Value::as_str) == Some("pending")
                // `201` created the invite for the requested role; `200` is
                // an existing live pending invite whose role is stored state.
                && matches!(status, 200 | 201)
                && (status != 201 || stored == Some(requested))
                && invite
                    .get("email")
                    .and_then(Value::as_str)
                    .is_some_and(|got| normalized_email_matches(email, got))
        }
        "team_revoke_invite" => {
            let (Some(invite), Some(id)) = (invite(), argument_text(arguments, "invite_id")) else {
                return false;
            };
            status == 200
                && nonempty_text(invite.get("email")).is_some()
                && membership_role(invite).is_some()
                && nonempty_text(invite.get("createdAt")).is_some()
                && nonempty_text(invite.get("expiresAt")).is_some()
                && invite.get("id").and_then(Value::as_str) == Some(id)
                && invite.get("status").and_then(Value::as_str) == Some("revoked")
        }
        "team_set_role" => {
            let (Some(member), Some(user), Some(requested)) = (
                reply.get("member").filter(|value| value.is_object()),
                argument_text(arguments, "user_id"),
                argument_text(arguments, "role"),
            ) else {
                return false;
            };
            status == 200
                && member.get("userId").and_then(Value::as_str) == Some(user)
                && member.get("role").and_then(Value::as_str) == Some(requested)
                && member.get("name").and_then(Value::as_str).is_some()
                && member.get("email").and_then(Value::as_str).is_some()
                && nonempty_text(member.get("joinedAt")).is_some()
        }
        "team_remove" => {
            status == 200
                && argument_text(arguments, "user_id")
                    .is_some_and(|id| reply.get("removed").and_then(Value::as_str) == Some(id))
        }
        _ => false,
    }
}

/// One sent argument, as the nonempty string the team argument shape
/// already required.
fn argument_text<'a>(arguments: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    arguments.get(key).and_then(Value::as_str)
}

/// A present wire string that the membership schema requires nonempty.
fn nonempty_text(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

/// The stored membership role, when the reply carries the schema's enum.
fn membership_role(value: &Value) -> Option<&str> {
    value
        .get("role")
        .and_then(Value::as_str)
        .filter(|role| matches!(*role, "owner" | "member"))
}

/// The tenant normalizes an invite address to trimmed lowercase
/// (`createInviteRequestSchema`), so the receipt carries that canonical
/// spelling; compare the sent address under the same normalization, without
/// allocating.
fn normalized_email_matches(sent: &str, received: &str) -> bool {
    let received = trim_tenant_text(received);
    !received.is_empty() && lowercase_chars(trim_tenant_text(sent)).eq(lowercase_chars(received))
}

fn lowercase_chars(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars().flat_map(char::to_lowercase)
}

/// A native settings save answers the committed snapshot document: a
/// nonempty revision token plus the `config` document it wrote. The native
/// `build_snapshot` always carries the five row tables and the two maps, so a
/// document that omits any of them is not that operation's receipt (and a
/// missing table must never read as "the row was removed").
fn settings_snapshot_document(reply: &Value) -> bool {
    let Some(config) = reply.get("config").filter(|config| config.is_object()) else {
        return false;
    };
    reply
        .get("revision")
        .and_then(Value::as_str)
        .is_some_and(|revision| !revision.is_empty())
        && ["harnesses", "providers", "models", "profiles", "pins"]
            .iter()
            .all(|table| config.get(*table).and_then(Value::as_array).is_some())
        && ["scope", "roleDefaults"]
            .iter()
            .all(|map| config.get(*map).is_some_and(Value::is_object))
}

fn settings_text<'a>(reply: &'a Value, key: &str) -> Option<&'a str> {
    reply
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

fn settings_payload_text<'a>(
    payload: Option<&'a Map<String, Value>>,
    key: &str,
) -> Option<&'a str> {
    payload
        .and_then(|payload| payload.get(key))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

/// The rows of one config table, only when the document is well formed:
/// every element is an object carrying a nonempty string identity. Native
/// `parse_overlay` guarantees that shape for every successful save, so a
/// malformed row means the snapshot cannot prove absence — a remove must
/// never read a broken element as "the row is gone".
fn settings_table<'a>(config: &'a Value, table: &str, identity: &str) -> Option<&'a [Value]> {
    let rows = config.get(table).and_then(Value::as_array)?;
    rows.iter()
        .all(|row| nonempty_text(row.get(identity)).is_some())
        .then_some(rows.as_slice())
}

fn settings_row<'a>(config: &'a Value, table: &str, id: &str) -> Option<&'a Value> {
    settings_table(config, table, "id")?
        .iter()
        .find(|row| row.get("id").and_then(Value::as_str) == Some(id))
}

fn settings_pin<'a>(config: &'a Value, pkg: &str, pinned: &str) -> Option<&'a Value> {
    settings_table(config, "pins", "pkg")?.iter().find(|row| {
        row.get("pkg").and_then(Value::as_str) == Some(pkg)
            && row.get("pinned").and_then(Value::as_str) == Some(pinned)
    })
}

/// `normalize_pin`'s canonical pair: `name@version` or `name version`.
fn normalized_pin_pair(pin: &str) -> Option<(&str, &str)> {
    if pin.is_empty() {
        return None;
    }
    if let Some((name, version)) = pin.rsplit_once('@') {
        if name.is_empty()
            || version.is_empty()
            || version.contains('@')
            || name.chars().any(char::is_control)
            || version.chars().any(char::is_control)
        {
            return None;
        }
        return Some((name, version));
    }
    let (name, version) = pin.split_once(' ')?;
    if name.is_empty()
        || version.is_empty()
        || version.contains(' ')
        || name.chars().any(char::is_control)
        || version.chars().any(char::is_control)
    {
        return None;
    }
    Some((name, version))
}

/// `validate_model_id` trims a model id at the native boundary, so a request
/// may carry surrounding spaces and still commit the canonical id.
fn canonical_model_id(id: &str) -> &str {
    id.trim()
}

/// `normalize_pin` keeps `name@version` and turns `name version` into
/// `name@version`; an empty pin stays empty. A supplied pin is committed
/// under that canonical spelling.
fn provider_pin_matches(sent: &str, stored: &str) -> bool {
    if sent.is_empty() {
        return stored.is_empty();
    }
    if sent.contains('@') {
        return stored == sent;
    }
    let Some((name, version)) = sent.split_once(' ') else {
        return false;
    };
    !version.contains(' ')
        && stored.len() == name.len() + 1 + version.len()
        && stored.starts_with(name)
        && stored.as_bytes().get(name.len()) == Some(&b'@')
        && &stored[name.len() + 1..] == version
}

/// A supplied field the native merge stores verbatim must equal the committed
/// row; an absent field keeps the stored value and is not checked.
fn settings_field_matches(stored: &Value, sent: &Map<String, Value>, key: &str) -> bool {
    match sent.get(key) {
        None => true,
        Some(value) => stored.get(key) == Some(value),
    }
}

/// `opt_bool` treats an absent or null `thinking`/`images` as "preserve", so
/// only a supplied boolean is binding.
fn settings_tri_field_matches(stored: &Value, sent: &Map<String, Value>, key: &str) -> bool {
    match sent.get(key) {
        None | Some(Value::Null) => true,
        Some(value) => stored.get(key) == Some(value),
    }
}

/// A supplied model id (or a profile's model) commits under
/// `validate_model_id`'s trimmed spelling.
fn settings_canonical_model_matches(stored: &Value, sent: &Map<String, Value>, key: &str) -> bool {
    match sent.get(key).and_then(Value::as_str) {
        None => true,
        Some(value) => stored.get(key).and_then(Value::as_str) == Some(canonical_model_id(value)),
    }
}

/// A profile job is rendered and parsed with `trim()`, so the committed job
/// is the trimmed request.
fn settings_trimmed_field_matches(stored: &Value, sent: &Map<String, Value>, key: &str) -> bool {
    match sent.get(key).and_then(Value::as_str) {
        None => true,
        Some(value) => stored.get(key).and_then(Value::as_str) == Some(value.trim()),
    }
}

/// A supplied provider pin commits under `normalize_pin`'s canonical spelling;
/// an absent pin keeps the stored value.
fn settings_pin_field_matches(stored: &Value, sent: &Map<String, Value>) -> bool {
    match sent.get("pin") {
        None => true,
        Some(value) => value.as_str().is_some_and(|sent_pin| {
            stored
                .get("pin")
                .and_then(Value::as_str)
                .is_some_and(|stored_pin| provider_pin_matches(sent_pin, stored_pin))
        }),
    }
}

/// `model.put`'s supplied scope is stored on the canonical model id; an
/// absent scope keeps the stored value.
fn settings_scope_matches(config: &Value, sent: &Map<String, Value>, id: &str) -> bool {
    match sent.get("scope") {
        None => true,
        Some(value) => value.as_str().is_some_and(|scope| {
            config
                .get("scope")
                .and_then(|scopes| scopes.get(id))
                .and_then(Value::as_str)
                == Some(scope)
        }),
    }
}

/// A patch `put` proves itself only when the committed row reflects every
/// supplied field. The native merge stores supplied strings/bools verbatim,
/// trims model ids and profile models, trims profile jobs, and normalizes a
/// provider pin; a null `thinking`/`images` means "preserve".
fn settings_put_applied(
    operation: &str,
    config: &Value,
    payload: Option<&Map<String, Value>>,
) -> bool {
    let Some(payload) = payload else {
        return false;
    };
    match operation {
        "harness.put" => settings_payload_text(Some(payload), "id").is_some_and(|id| {
            settings_row(config, "harnesses", id).is_some_and(|row| {
                ["label", "cli", "note"]
                    .iter()
                    .all(|key| settings_field_matches(row, payload, key))
            })
        }),
        "provider.put" => settings_payload_text(Some(payload), "id").is_some_and(|id| {
            settings_row(config, "providers", id).is_some_and(|row| {
                ["label", "kind", "note", "gate", "paid"]
                    .iter()
                    .all(|key| settings_field_matches(row, payload, key))
                    && settings_pin_field_matches(row, payload)
            })
        }),
        "model.put" => settings_payload_text(Some(payload), "id").is_some_and(|id| {
            let id = canonical_model_id(id);
            settings_row(config, "models", id).is_some_and(|row| {
                ["provider", "ctx", "out"]
                    .iter()
                    .all(|key| settings_field_matches(row, payload, key))
                    && ["thinking", "images"]
                        .iter()
                        .all(|key| settings_tri_field_matches(row, payload, key))
                    && settings_scope_matches(config, payload, id)
            })
        }),
        "profile.put" => settings_payload_text(Some(payload), "id").is_some_and(|id| {
            settings_row(config, "profiles", id).is_some_and(|row| {
                settings_field_matches(row, payload, "role")
                    && settings_canonical_model_matches(row, payload, "model")
                    && settings_trimmed_field_matches(row, payload, "job")
            })
        }),
        "pin.put" => {
            let (Some(pkg), Some(pinned)) = (
                settings_payload_text(Some(payload), "pkg"),
                settings_payload_text(Some(payload), "pinned"),
            ) else {
                return false;
            };
            settings_pin(config, pkg, pinned).is_some()
        }
        _ => false,
    }
}

/// A patch remove proves itself only when the table is well formed and no row
/// carries the sent identity. A malformed element is unknown, never an absent
/// row.
fn settings_remove_applied(
    operation: &str,
    config: &Value,
    payload: Option<&Map<String, Value>>,
) -> bool {
    let Some(payload) = payload else {
        return false;
    };
    match operation {
        "harness.remove" => settings_payload_text(Some(payload), "id")
            .is_some_and(|id| settings_absent(config, "harnesses", id)),
        "provider.remove" => settings_payload_text(Some(payload), "id")
            .is_some_and(|id| settings_absent(config, "providers", id)),
        "model.remove" => settings_payload_text(Some(payload), "id")
            .is_some_and(|id| settings_absent(config, "models", canonical_model_id(id))),
        "profile.remove" => settings_payload_text(Some(payload), "id")
            .is_some_and(|id| settings_absent(config, "profiles", id)),
        "pin.remove" => settings_payload_text(Some(payload), "pkg")
            .is_some_and(|pkg| settings_pin_removed(config, pkg)),
        _ => false,
    }
}

fn settings_absent(config: &Value, table: &str, id: &str) -> bool {
    settings_table(config, table, "id").is_some_and(|rows| {
        rows.iter()
            .all(|row| row.get("id").and_then(Value::as_str) != Some(id))
    })
}

/// A removed pin may legitimately remain in the projected view when a
/// Pi-extension provider row declares the same package (`pi_pins` re-adds
/// provider pins). A remaining pin with no such provider source contradicts
/// the removal.
fn settings_pin_removed(config: &Value, pkg: &str) -> bool {
    let Some(pins) = settings_table(config, "pins", "pkg") else {
        return false;
    };
    let remaining: Vec<&Value> = pins
        .iter()
        .filter(|row| row.get("pkg").and_then(Value::as_str) == Some(pkg))
        .collect();
    if remaining.is_empty() {
        return true;
    }
    settings_table(config, "providers", "id").is_some_and(|providers| {
        remaining.iter().all(|row| {
            let Some(pinned) = row.get("pinned").and_then(Value::as_str) else {
                return false;
            };
            providers.iter().any(|provider| {
                provider.get("kind").and_then(Value::as_str) == Some("Pi extension")
                    && provider
                        .get("pin")
                        .and_then(Value::as_str)
                        .and_then(normalized_pin_pair)
                        == Some((pkg, pinned))
            })
        })
    })
}

/// A rename proves itself only when the canonical new id is present and the
/// old id is gone. Renaming to the same id is refused natively, so the two
/// identities differ on success.
fn settings_rename_applied(
    operation: &str,
    config: &Value,
    payload: Option<&Map<String, Value>>,
) -> bool {
    let Some(payload) = payload else {
        return false;
    };
    let (table, canonical) = match operation {
        "model.rename" => ("models", true),
        "profile.rename" => ("profiles", false),
        _ => return false,
    };
    let (Some(id), Some(to)) = (
        settings_payload_text(Some(payload), "id"),
        settings_payload_text(Some(payload), "to"),
    ) else {
        return false;
    };
    let (id, to) = if canonical {
        (canonical_model_id(id), canonical_model_id(to))
    } else {
        (id, to)
    };
    settings_row(config, table, to).is_some() && settings_absent(config, table, id)
}

/// `present_overlay` never adds harnesses or profiles, so an applied document
/// must be reflected as exactly the same identity set — an extra row is a
/// stale or foreign receipt, not a policy projection.
/// The applied identity sets are exact: `present_overlay` never adds
/// harnesses or profiles, and after apply the policy is canonical to the sent
/// overlay for providers and models. Model ids compare under
/// `validate_model_id`'s trim; provider ids are exact.
fn settings_same_id_set(config: &Value, table: &str, sent: &[Value], canonical: bool) -> bool {
    let Some(stored) = settings_table(config, table, "id") else {
        return false;
    };
    stored.len() == sent.len()
        && stored.iter().all(|row| {
            row.get("id").and_then(Value::as_str).is_some_and(|id| {
                sent.iter().any(|sent| {
                    sent.get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|sent_id| {
                            if canonical {
                                canonical_model_id(sent_id) == id
                            } else {
                                sent_id == id
                            }
                        })
                })
            })
        })
        && sent.iter().all(|row| {
            row.get("id")
                .and_then(Value::as_str)
                .is_some_and(|sent_id| {
                    let id = if canonical {
                        canonical_model_id(sent_id)
                    } else {
                        sent_id
                    };
                    stored
                        .iter()
                        .any(|stored| stored.get("id").and_then(Value::as_str) == Some(id))
                })
        })
}

/// `apply` replaces the whole overlay. The receipt proves every sent row is
/// reflected with its documented fields, exact identity sets for harnesses,
/// profiles, providers and models, the filtered Pi scope and role defaults,
/// and the full (pkg,pinned) pin union of explicit pins plus Pi-extension
/// provider pins.
fn settings_apply_applied(config: &Value, payload: Option<&Map<String, Value>>) -> bool {
    let Some(payload) = payload else {
        return false;
    };
    let Some(sent) = payload.get("config").and_then(Value::as_object) else {
        return false;
    };
    let (
        Some(harnesses),
        Some(providers),
        Some(models),
        Some(profiles),
        Some(scope),
        Some(roles),
        Some(pins),
    ) = (
        sent.get("harnesses").and_then(Value::as_array),
        sent.get("providers").and_then(Value::as_array),
        sent.get("models").and_then(Value::as_array),
        sent.get("profiles").and_then(Value::as_array),
        sent.get("scope").and_then(Value::as_object),
        sent.get("roleDefaults").and_then(Value::as_object),
        sent.get("pins").and_then(Value::as_array),
    )
    else {
        return false;
    };
    harnesses.iter().all(|row| {
        let Some(sent) = row.as_object() else {
            return false;
        };
        sent.get("id").and_then(Value::as_str).is_some_and(|id| {
            settings_row(config, "harnesses", id).is_some_and(|stored| {
                ["id", "label", "cli", "note"]
                    .iter()
                    .all(|key| settings_field_matches(stored, sent, key))
            })
        })
    }) && settings_same_id_set(config, "harnesses", harnesses, false)
        && providers.iter().all(|row| {
            let Some(sent) = row.as_object() else {
                return false;
            };
            sent.get("id").and_then(Value::as_str).is_some_and(|id| {
                settings_row(config, "providers", id).is_some_and(|stored| {
                    ["id", "label", "kind", "note", "gate", "paid"]
                        .iter()
                        .all(|key| settings_field_matches(stored, sent, key))
                        && settings_pin_field_matches(stored, sent)
                })
            })
        })
        && models.iter().all(|row| {
            let Some(sent) = row.as_object() else {
                return false;
            };
            sent.get("id").and_then(Value::as_str).is_some_and(|id| {
                let id = canonical_model_id(id);
                settings_row(config, "models", id).is_some_and(|stored| {
                    settings_canonical_model_matches(stored, sent, "id")
                        && ["provider", "ctx", "out"]
                            .iter()
                            .all(|key| settings_field_matches(stored, sent, key))
                        && ["thinking", "images"]
                            .iter()
                            .all(|key| settings_apply_field_matches(stored, sent, key))
                })
            })
        })
        && profiles.iter().all(|row| {
            let Some(sent) = row.as_object() else {
                return false;
            };
            sent.get("id").and_then(Value::as_str).is_some_and(|id| {
                settings_row(config, "profiles", id).is_some_and(|stored| {
                    settings_field_matches(stored, sent, "id")
                        && settings_field_matches(stored, sent, "role")
                        && settings_canonical_model_matches(stored, sent, "model")
                        && settings_trimmed_field_matches(stored, sent, "job")
                })
            })
        })
        && settings_same_id_set(config, "profiles", profiles, false)
        && settings_apply_scope(config, scope, models, providers)
        && roles.iter().all(|(role, value)| {
            value.as_str().is_some_and(|model| {
                config
                    .get("roleDefaults")
                    .and_then(|roles| roles.get(role))
                    .and_then(Value::as_str)
                    == Some(model)
            })
        })
        && pins.iter().all(|row| settings_apply_pin(config, row))
        && settings_apply_exact(config, sent)
}

/// `apply` stores `thinking`/`images` as the tri-state value itself: absent or
/// null becomes null, a boolean stays that boolean.
fn settings_apply_field_matches(stored: &Value, sent: &Map<String, Value>, key: &str) -> bool {
    match sent.get(key) {
        None | Some(Value::Null) => stored.get(key).is_none_or(Value::is_null),
        Some(value) => stored.get(key) == Some(value),
    }
}

/// `parse_overlay` stores a scope only for a Pi-extension model; a native-CLI
/// or unknown model's entry is dropped. Every stored Pi scope must reflect
/// the sent mode.
fn settings_apply_scope(
    config: &Value,
    scope: &Map<String, Value>,
    models: &[Value],
    providers: &[Value],
) -> bool {
    scope.iter().all(|(id, value)| {
        let Some(model) = models.iter().find(|row| {
            row.get("id")
                .and_then(Value::as_str)
                .is_some_and(|model_id| canonical_model_id(model_id) == id.as_str())
        }) else {
            return true;
        };
        let Some(provider) = model.get("provider").and_then(Value::as_str) else {
            return false;
        };
        let Some(kind) = providers
            .iter()
            .find(|row| row.get("id").and_then(Value::as_str) == Some(provider))
            .and_then(|row| row.get("kind").and_then(Value::as_str))
        else {
            return false;
        };
        if kind == "native CLI" {
            return true;
        }
        kind == "Pi extension"
            && value.as_str().is_some_and(|mode| {
                config
                    .get("scope")
                    .and_then(|scopes| scopes.get(id.as_str()))
                    .and_then(Value::as_str)
                    == Some(mode)
            })
    })
}

fn settings_apply_pin(config: &Value, sent: &Value) -> bool {
    let Some(sent) = sent.as_object() else {
        return false;
    };
    let (Some(pkg), Some(pinned)) = (
        sent.get("pkg").and_then(Value::as_str),
        sent.get("pinned").and_then(Value::as_str),
    ) else {
        return false;
    };
    settings_pin(config, pkg, pinned).is_some()
}

/// The post-apply projection is exact for the non-policy families and the
/// filtered Pi scope/role defaults; pins may only add normalized Pi-extension
/// provider specs.
fn settings_apply_exact(config: &Value, sent: &Map<String, Value>) -> bool {
    let (Some(providers), Some(models), Some(scope), Some(roles), Some(pins)) = (
        sent.get("providers").and_then(Value::as_array),
        sent.get("models").and_then(Value::as_array),
        sent.get("scope").and_then(Value::as_object),
        sent.get("roleDefaults").and_then(Value::as_object),
        sent.get("pins").and_then(Value::as_array),
    ) else {
        return false;
    };
    settings_same_id_set(config, "providers", providers, false)
        && settings_same_id_set(config, "models", models, true)
        && settings_apply_scope_exact(config, scope, models, providers)
        && settings_apply_roles_exact(config, roles)
        && settings_apply_pins_exact(config, pins, providers)
}

/// `parse_overlay` keeps a scope only for a Pi-extension model and drops
/// native-CLI/unknown keys. The stored scope must equal that filtered map.
fn settings_apply_scope_exact(
    config: &Value,
    scope: &Map<String, Value>,
    models: &[Value],
    providers: &[Value],
) -> bool {
    let mut expected: Vec<(&str, &str)> = Vec::new();
    for (id, value) in scope {
        let Some(model) = models.iter().find(|row| {
            row.get("id")
                .and_then(Value::as_str)
                .is_some_and(|model_id| canonical_model_id(model_id) == id.as_str())
        }) else {
            continue;
        };
        let Some(provider) = model.get("provider").and_then(Value::as_str) else {
            return false;
        };
        let Some(kind) = providers
            .iter()
            .find(|row| row.get("id").and_then(Value::as_str) == Some(provider))
            .and_then(|row| row.get("kind").and_then(Value::as_str))
        else {
            return false;
        };
        if kind == "native CLI" {
            continue;
        }
        if kind != "Pi extension" {
            return false;
        }
        let Some(mode) = value.as_str() else {
            return false;
        };
        expected.push((id.as_str(), mode));
    }
    let Some(stored) = config.get("scope").and_then(Value::as_object) else {
        return false;
    };
    stored.len() == expected.len()
        && stored.iter().all(|(key, value)| {
            value.as_str().is_some_and(|mode| {
                expected
                    .iter()
                    .any(|(id, expected_mode)| *id == key.as_str() && *expected_mode == mode)
            })
        })
}

fn settings_apply_roles_exact(config: &Value, roles: &Map<String, Value>) -> bool {
    let Some(stored) = config.get("roleDefaults").and_then(Value::as_object) else {
        return false;
    };
    stored.len() == roles.len()
        && roles.iter().all(|(role, value)| {
            value.as_str().is_some_and(|model| {
                stored.get(role.as_str()).and_then(Value::as_str) == Some(model)
            })
        })
}

/// The post-apply `config.pins` is built from the policy's provider specs, so
/// it is exactly the full (pkg,pinned) union of explicit sent pins and
/// normalized Pi-extension provider pins (deduped), never a pkg-only subset.
fn settings_apply_pins_exact(config: &Value, sent: &[Value], providers: &[Value]) -> bool {
    let Some(stored) = settings_table(config, "pins", "pkg") else {
        return false;
    };
    let mut expected: Vec<(&str, &str)> = Vec::new();
    for row in sent {
        let (Some(pkg), Some(pinned)) = (
            row.get("pkg").and_then(Value::as_str),
            row.get("pinned").and_then(Value::as_str),
        ) else {
            return false;
        };
        if !expected.iter().any(|(p, v)| *p == pkg && *v == pinned) {
            expected.push((pkg, pinned));
        }
    }
    for provider in providers {
        if provider.get("kind").and_then(Value::as_str) != Some("Pi extension") {
            continue;
        }
        let Some((pkg, pinned)) = provider
            .get("pin")
            .and_then(Value::as_str)
            .and_then(normalized_pin_pair)
        else {
            continue;
        };
        if !expected.iter().any(|(p, v)| *p == pkg && *v == pinned) {
            expected.push((pkg, pinned));
        }
    }
    stored.len() == expected.len()
        && stored.iter().enumerate().all(|(index, row)| {
            let (Some(pkg), Some(pinned)) = (
                row.get("pkg").and_then(Value::as_str),
                row.get("pinned").and_then(Value::as_str),
            ) else {
                return false;
            };
            expected.iter().any(|(p, v)| *p == pkg && *v == pinned)
                && !stored[..index].iter().any(|previous| {
                    previous.get("pkg").and_then(Value::as_str) == Some(pkg)
                        && previous.get("pinned").and_then(Value::as_str) == Some(pinned)
                })
        })
        && expected.iter().all(|(pkg, pinned)| {
            stored.iter().any(|row| {
                row.get("pkg").and_then(Value::as_str) == Some(*pkg)
                    && row.get("pinned").and_then(Value::as_str) == Some(*pinned)
            })
        })
}

/// The committed snapshot must show the effect this patch operation wrote:
/// the row it put/removed/renamed, the scope it set, the role default it
/// changed, or the document it applied. A snapshot that contradicts the
/// operation proves nothing, so the outcome stays unknown.
fn settings_target_applied(
    operation: &str,
    config: &Value,
    payload: Option<&Map<String, Value>>,
) -> bool {
    match operation {
        "harness.put" | "provider.put" | "model.put" | "profile.put" | "pin.put" => {
            settings_put_applied(operation, config, payload)
        }
        "harness.remove" | "provider.remove" | "model.remove" | "profile.remove" | "pin.remove" => {
            settings_remove_applied(operation, config, payload)
        }
        "model.rename" | "profile.rename" => settings_rename_applied(operation, config, payload),
        "model.scope" => payload.is_some_and(|payload| {
            settings_payload_text(Some(payload), "id")
                .is_some_and(|id| settings_scope_matches(config, payload, canonical_model_id(id)))
        }),
        "role.set" => settings_payload_text(payload, "role")
            .zip(settings_payload_text(payload, "model"))
            .is_some_and(|(role, model)| {
                config
                    .get("roleDefaults")
                    .and_then(|roles| roles.get(role))
                    .and_then(Value::as_str)
                    == Some(model)
            }),
        "role.clear" => settings_payload_text(payload, "role").is_some_and(|role| {
            config
                .get("roleDefaults")
                .and_then(|roles| roles.get(role))
                .and_then(Value::as_str)
                == Some("")
        }),
        "apply" => settings_apply_applied(config, payload),
        _ => false,
    }
}

/// `catalog.import` adds each returned id to `config.models` with the
/// requested provider in the same transaction, so the saved snapshot must
/// show that binding. A listed id that is absent or bound to another
/// provider contradicts the receipt.
fn catalog_import_visible(reply: &Value, payload: Option<&Map<String, Value>>) -> bool {
    let Some(provider) = settings_payload_text(payload, "provider") else {
        return false;
    };
    let Some(ids) = reply.get("imported").and_then(Value::as_array) else {
        return false;
    };
    if !ids
        .iter()
        .all(|id| id.as_str().is_some_and(|id| !id.is_empty()))
    {
        return false;
    }
    if !catalog_import_selected(ids, payload) {
        return false;
    }
    let config = &reply["snapshot"]["config"];
    ids.iter().all(|id| {
        id.as_str().is_some_and(|id| {
            settings_row(config, "models", id)
                .and_then(|row| row.get("provider").and_then(Value::as_str))
                == Some(provider)
        })
    })
}

/// An explicit `models` selection bounds which ids may be reported; `all-new`
/// is server-discovered and skips only this subset check.
fn catalog_import_selected(ids: &[Value], payload: Option<&Map<String, Value>>) -> bool {
    match payload.and_then(|payload| payload.get("models")) {
        Some(Value::String(selection)) if selection.as_str() == "all-new" => true,
        Some(Value::Array(selection)) => ids.iter().all(|id| {
            id.as_str()
                .is_some_and(|id| selection.iter().any(|want| want.as_str() == Some(id)))
        }),
        _ => false,
    }
}

/// `catalog.refresh` names the provider it fetched: the native source string
/// is fixed to `omp models <provider> --json --no-extensions`.
fn catalog_source_matches(source: &str, provider: &str) -> bool {
    source
        .strip_prefix("omp models ")
        .and_then(|rest| rest.strip_suffix(" --json --no-extensions"))
        == Some(provider)
}

/// The native grant binds the subject it was minted for: an internally
/// tagged `kind` plus the connection identity (`agent`) or hosted principal
/// (`cli-agent`). A receipt without that binding cannot be used as a grant.
fn grant_bound_subject(subject: &Value) -> bool {
    match subject.get("kind").and_then(Value::as_str) {
        Some("agent") => {
            subject.get("alias").and_then(Value::as_str).is_some()
                && subject.get("generation").and_then(Value::as_str).is_some()
                && subject.get("uid").and_then(Value::as_u64).is_some()
        }
        Some("cli-agent") => {
            subject.get("actor").and_then(Value::as_str).is_some()
                && subject.get("sub").and_then(Value::as_str).is_some()
        }
        _ => false,
    }
}

/// `profile.sync` reports every selected profile's Pi and Claude exports: a
/// named profile must exist in the snapshot and produce exactly the two
/// harness rows; the all-profile form derives the selection from the snapshot
/// profiles (empty is allowed when there are none). Statuses stay the native
/// `unmanaged`/`in sync` pair.
fn settings_sync_files_exact(
    config: &Value,
    reply: &Value,
    payload: Option<&Map<String, Value>>,
) -> bool {
    let Some(files) = reply.get("files").and_then(Value::as_array) else {
        return false;
    };
    let Some(profiles) = settings_table(config, "profiles", "id") else {
        return false;
    };

    if profiles.iter().enumerate().any(|(index, row)| {
        row.get("id").and_then(Value::as_str).is_some_and(|id| {
            profiles[..index]
                .iter()
                .any(|previous| previous.get("id").and_then(Value::as_str) == Some(id))
        })
    }) {
        return false;
    }
    let named = settings_payload_text(payload, "profile");
    if named.is_some_and(|id| {
        !profiles
            .iter()
            .any(|row| row.get("id").and_then(Value::as_str) == Some(id))
    }) {
        return false;
    }
    let profile_count = if named.is_some() { 1 } else { profiles.len() };
    let selected = |profile: &str| match named {
        Some(id) => profile == id,
        None => profiles
            .iter()
            .any(|row| row.get("id").and_then(Value::as_str) == Some(profile)),
    };
    let covered = |id: &str| {
        ["pi", "claude"].iter().all(|harness| {
            files.iter().any(|file| {
                file.get("profile").and_then(Value::as_str) == Some(id)
                    && file.get("harness").and_then(Value::as_str) == Some(*harness)
            })
        })
    };
    files.len() == profile_count * 2
        && files.iter().enumerate().all(|(index, file)| {
            let Some(profile) = file.get("profile").and_then(Value::as_str) else {
                return false;
            };
            let Some(harness) = file.get("harness").and_then(Value::as_str) else {
                return false;
            };
            selected(profile)
                && matches!(harness, "pi" | "claude")
                && matches!(
                    file.get("status").and_then(Value::as_str),
                    Some("unmanaged" | "in sync")
                )
                && file
                    .get("path")
                    .and_then(Value::as_str)
                    .is_some_and(|path| !path.is_empty())
                && !files[..index].iter().any(|previous| {
                    previous.get("profile").and_then(Value::as_str) == Some(profile)
                        && previous.get("harness").and_then(Value::as_str) == Some(harness)
                })
        })
        && match named {
            Some(id) => covered(id),
            None => profiles
                .iter()
                .all(|row| row.get("id").and_then(Value::as_str).is_some_and(covered)),
        }
}

/// A settings write answers its operation's own receipt family. The snapshot
/// document covers the config writes; the nested shapes, the catalogue and
/// the grant decisions answer their own bounded records. `fleet.check` is a
/// read that changes nothing (its `settings.write` scope only satisfies the
/// hosted contract), so its document is not a write receipt.
fn settings_receipt_correlates(
    operation: &str,
    reply: &Value,
    arguments: &Map<String, Value>,
) -> bool {
    let payload = arguments.get("payload").and_then(Value::as_object);
    match operation {
        "apply" | "harness.put" | "harness.remove" | "provider.put" | "provider.remove"
        | "model.put" | "model.rename" | "model.remove" | "model.scope" | "role.set"
        | "role.clear" | "profile.put" | "profile.rename" | "profile.remove" | "pin.put"
        | "pin.remove" => {
            settings_snapshot_document(reply)
                && settings_target_applied(operation, &reply["config"], payload)
        }
        "catalog.import" => {
            settings_snapshot_document(&reply["snapshot"]) && catalog_import_visible(reply, payload)
        }
        "profile.sync" => {
            settings_snapshot_document(&reply["snapshot"])
                && settings_sync_files_exact(&reply["snapshot"]["config"], reply, payload)
        }
        "catalog.refresh" => settings_payload_text(payload, "provider").is_some_and(|provider| {
            reply
                .get("source")
                .and_then(Value::as_str)
                .is_some_and(|source| catalog_source_matches(source, provider))
                && settings_text(reply, "fetchedAt").is_some()
                && reply.get("models").and_then(Value::as_array).is_some()
        }),
        "grant.request" => settings_text(reply, "request_id").is_some(),
        "grant.allow" => {
            settings_text(reply, "grant_id").is_some()
                && reply.get("expires_at").and_then(Value::as_i64).is_some()
                && reply.get("bound").is_some_and(|bound| {
                    settings_text(bound, "operation").is_some()
                        && bound
                            .get("payload_sha256")
                            .and_then(Value::as_str)
                            .is_some_and(|digest| {
                                digest.len() == 64
                                    && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                            })
                        && bound.get("expected_revision").is_some_and(|revision| {
                            revision.is_null() || revision.as_str().is_some()
                        })
                        && bound.get("subject").is_some_and(grant_bound_subject)
                        && settings_text(bound, "org").is_some()
                })
        }
        "grant.reject" => settings_payload_text(payload, "request_id").is_some_and(|sent| {
            reply.get("request_id").and_then(Value::as_str) == Some(sent)
                && reply.get("status").and_then(Value::as_str) == Some("rejected")
        }),
        "grant.revoke" => settings_payload_text(payload, "grant_id").is_some_and(|sent| {
            reply.get("grant_id").and_then(Value::as_str) == Some(sent)
                && reply.get("status").and_then(Value::as_str) == Some("revoked")
        }),
        "master.resume" => {
            reply.get("alias").and_then(Value::as_str) == Some("master")
                && matches!(
                    reply.get("state").and_then(Value::as_str),
                    Some("starting" | "attention")
                )
        }
        "fleet.check" => true,
        _ => false,
    }
}

/// CAD-2001: the project-scoped review-evidence verbs. Every one carries an
/// explicit `project` — plus `issue` for the two approvals — so the answer is
/// that project's evidence and no other's. They are newer than the tenant
/// that must serve them, so a refusal meaning "this tenant does not know that
/// verb" is reported as a release-order problem (see
/// [`tenant_cannot_serve`]): never a pass, and never a fallback to the older
/// repo-scoped verbs, which stay served for old clients only.
pub const PROJECT_SCOPED_GATE_VERBS: &[&str] = &[
    "audit_approval_project",
    "audit_verdicts_project",
    "audit_verdict_project",
    "audit_approve_project",
    "audit_reviewers_project",
    "audit_reviewers_project_set",
    "audit_identities",
    "audit_identities_set",
];

/// Why a project-scoped verb could not be served, when the answer means "this
/// tenant cannot serve that call". Two shapes say it:
///
/// - the verb is unknown — the tenant board answers `501 not_implemented`, an
///   older relay rejects the name in its strict request schema
///   (`400 invalid_request`), and `409 container_required` marks a verb the
///   tenant routes to a runtime it does not have here;
/// - the tenant's own store cannot answer (`503 capability_unavailable`).
///
/// The release order is the tenant server first, then this client. The
/// message names that order and says what was not read or recorded; it never
/// lets the caller read the refusal as "no evidence was filed" or as a reason
/// to fall back to repo-scoped evidence.
fn tenant_cannot_serve(verb: &str, status: u16, body: Option<&Value>) -> Option<String> {
    let state = body.and_then(|b| b["state"].as_str()).unwrap_or_default();
    if state == "waking" || state == "wake_failed" {
        return None;
    }
    let code = body
        .and_then(|b| b.pointer("/error/code"))
        .and_then(Value::as_str);
    let nothing = if mutating_cli_verb(verb) {
        "Nothing was recorded"
    } else {
        "Nothing was read"
    };
    // A project the tenant has no proven policy for is a real refusal, not a
    // version problem: name the remedy instead of the release order.
    if status == 409 && code == Some("project_policy_unavailable") {
        return Some(format!(
            "the tenant stores no access policy for that project, so `{verb}` was refused \
             before it ran — the owner records the project's policy first. {nothing}"
        ));
    }
    let cannot_serve = match status {
        501 => true,
        400 => matches!(code, Some("invalid_request" | "unknown_verb")),
        404 => matches!(code, Some("unknown_verb" | "not_implemented")),
        409 => true,
        // A tenant that predates the verb answers 503 for a capability it does
        // not have. For a *mutation* the same code is not proof the verb never
        // ran: the tenant wraps a hub RPC that threw *after* the write in
        // `503 capability_unavailable` (`apps/api/src/tenant-board/cli.ts`,
        // `setProjectAccess`/`setProjectEligibility` catches). A read keeps the
        // release-order refusal; a write is left to the mutation classifier,
        // which reports an unknown outcome.
        503 => {
            !mutating_cli_verb(verb)
                && matches!(
                    code,
                    Some("capability_unavailable" | "not_implemented" | "unknown_verb")
                )
        }
        _ => false,
    };
    if !cannot_serve {
        return None;
    }
    Some(format!(
        "the tenant could not serve `{verb}` (HTTP {status}{}) — CAD-2001 moved the merge \
         gate's review evidence to the project scope, and the tenant must serve these verbs. \
         Release order: update the tenant server first, then this Cadence client; a tenant \
         that predates them refuses the name, and this client never falls back to the older \
         repo-scoped evidence. {nothing}",
        code.map_or(String::new(), |c| format!(", {c}"))
    ))
}

/// Argument keys a platform team verb may carry. Other verbs keep their
/// existing grammar. An unknown key, a missing key, or a role other than
/// `owner`/`member` is refused before a credential is loaded. `role` here
/// is the requested target role, never caller authority.
fn team_argument_shape(verb: &str, arguments: &Map<String, Value>) -> Result<()> {
    let allowed: &[&str] = match verb {
        "team_list" | "team_invites" => &[],
        "team_invite" => &["email", "role"],
        "team_revoke_invite" => &["invite_id"],
        "team_set_role" => &["user_id", "role"],
        "team_remove" => &["user_id"],
        _ => return Ok(()),
    };
    if let Some(key) = arguments
        .keys()
        .find(|key| !allowed.contains(&key.as_str()))
    {
        return Err(reject(format!(
            "team {verb} argument '{key}' is not allowed - never sent"
        )));
    }
    for key in allowed {
        let Some(value) = arguments.get(*key) else {
            return Err(reject(format!("team {verb} requires {key} - never sent")));
        };
        let Some(text) = value
            .as_str()
            .filter(|text| !text.is_empty() && !text.chars().any(|c| c.is_control()))
        else {
            return Err(reject(format!(
                "team {verb} {key} must be a nonempty string - never sent"
            )));
        };
        if *key == "role" && text != "owner" && text != "member" {
            return Err(reject("team role must be owner or member - never sent"));
        }
    }
    Ok(())
}

/// CAD-1586 / AOS-290: `settings_agents` carries the explicit
/// `{operation, expected_revision, payload}` resource command for
/// Settings > Agents — the same object the local `agent_settings_command`
/// RPC takes, forwarded verbatim to the hosted daemon. `master_status`
/// takes no arguments. Both mint narrow envelopes (`settings.read` /
/// `settings.write` per operation; `settings.read` for master status)
/// instead of the broad cli.* pair the other verbs request.
pub const REMOTE_SETTINGS_VERB: &str = "settings_agents";
pub const REMOTE_MASTER_STATUS_VERB: &str = "master_status";

/// The hosted scope a `settings_agents` operation needs — the AOS-290
/// operation table in `hosted-cadence-auth.ts` classifies the same names;
/// anything not an explicit read fails closed as a write.
pub fn settings_op_scope(operation: &str) -> &'static str {
    match operation {
        "snapshot" | "history" | "master.status" | "harness.list" | "harness.show"
        | "provider.list" | "provider.show" | "model.list" | "model.show" | "role.list"
        | "profile.list" | "profile.show" | "profile.preview" | "pin.list" | "grant.list" => {
            "settings.read"
        }
        // master.resume is write explicitly — it starts a process;
        // every other non-read fails closed the same way.
        "master.resume" => "settings.write",
        _ => "settings.write",
    }
}

/// The wiki actions this build sends over `/__platform/wiki/call` (CAD-1401,
/// CAD-914): `enumerate` is `wiki ls` or `wiki search`, `read` is `wiki cat`,
/// `write` is a text `wiki put` or a `wiki cp` transfer (`begin`/`chunk`/
/// `apply`/`finish`), and `job_status` is transfer `status`/`export`.
/// Any other verb is refused locally, before a request.
pub const WIKI_ACTIONS: &[&str] = &["enumerate", "read", "write", "job_status"];

/// The argument keys each wiki action may send (CAD-1401). Any other key —
/// an authority-shaped one such as `actor` or `as` included — is refused
/// locally, before a request. Unknown actions get no keys (fail closed).
fn wiki_argument_keys(action: &str) -> &'static [&'static str] {
    match action {
        "enumerate" => &["path", "q"],
        "read" => &["path", "offset", "bytes", "rev"],
        "write" => &["path", "text", "if_rev", "transfer"],
        "job_status" => &["path", "transfer"],
        _ => &[],
    }
}

/// `write` is either the existing text put or a transfer object, never both.
/// `job_status` carries only status/export. Transfer keys are the native
/// adapter allowlist. The outer `path` is the op target: manifest root,
/// `root`, or export `path`. A client cannot supply `_transfer_binding`.
fn wiki_transfer_shape(action: &str, arguments: &Map<String, Value>) -> Result<()> {
    const KEYS: &[&str] = &[
        "op",
        "manifest",
        "job_id",
        "root",
        "entry_id",
        "offset",
        "data_base64",
        "limit",
        "path",
        "bytes",
        "rev",
    ];
    let Some(transfer) = arguments.get("transfer") else {
        if action == "job_status" {
            return Err(reject(
                "wiki job_status requires a transfer object — never sent",
            ));
        }
        return Ok(());
    };
    if action == "write" && (arguments.contains_key("text") || arguments.contains_key("if_rev")) {
        return Err(reject(
            "wiki write transfer cannot be combined with text — never sent",
        ));
    }
    let object = transfer
        .as_object()
        .ok_or_else(|| reject("wiki transfer must be an object — never sent"))?;
    if let Some(key) = object.keys().find(|key| !KEYS.contains(&key.as_str())) {
        return Err(reject(format!(
            "wiki {action} transfer field '{key}' is not allowed — never sent"
        )));
    }
    let op = object.get("op").and_then(Value::as_str).unwrap_or("");
    let allowed: &[&str] = match action {
        "write" => &["begin", "chunk", "apply", "finish"],
        "job_status" => &["status", "export"],
        _ => &[],
    };
    if !allowed.contains(&op) {
        return Err(reject(format!(
            "wiki {action} transfer op '{op}' is not allowed — never sent"
        )));
    }
    let path = arguments.get("path").and_then(Value::as_str).unwrap_or("");
    if crate::wiki::normalize(path).ok().as_deref() != Some(path) || path.is_empty() {
        return Err(reject(
            "wiki transfer path must be a canonical nonempty wiki path — never sent",
        ));
    }
    match op {
        "begin" => {
            let root = object
                .get("manifest")
                .and_then(|manifest| manifest.get("root"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if root != path {
                return Err(reject(
                    "wiki transfer manifest root must match path — never sent",
                ));
            }
            let entries = object
                .get("manifest")
                .and_then(|manifest| manifest.get("entries"))
                .and_then(Value::as_array);
            let Some(entries) = entries else {
                return Err(reject("wiki transfer begin requires entries — never sent"));
            };
            for entry in entries {
                let relative = entry.get("path").and_then(Value::as_str).unwrap_or("");
                if relative.is_empty()
                    || relative.starts_with('/')
                    || crate::wiki::normalize(relative).ok().as_deref() != Some(relative)
                {
                    return Err(reject(
                        "wiki transfer entry path must be canonical and relative — never sent",
                    ));
                }
            }
        }
        "chunk" | "apply" | "finish" | "status" => {
            if object.get("root").and_then(Value::as_str) != Some(path) {
                return Err(reject("wiki transfer root must match path — never sent"));
            }
        }
        "export" if object.get("path").and_then(Value::as_str) != Some(path) => {
            return Err(reject(
                "wiki export path must match the outer path — never sent",
            ));
        }
        _ => {}
    }
    Ok(())
}

/// The hosted relay a verb rides: the cli envelope or the wiki envelope. It is
/// chosen by the verb name on the shared path, so a wiki action never travels
/// the cli route and a cli verb never travels the wiki route.
#[derive(Clone, Copy)]
enum Surface {
    Cli,
    Wiki,
}

impl Surface {
    /// The relay path prefix and its protocol version.
    fn relay(self) -> (&'static str, &'static str) {
        match self {
            Surface::Cli => ("/__platform/cli", CLI_VERSION),
            Surface::Wiki => ("/__platform/wiki", WIKI_VERSION),
        }
    }
}

/// A resolved remote org destination, pinned once per invocation.
/// Ordinary selection reads the registry (`cli::org::resolve`): `org` is
/// the issuer-verified slug, and `endpoint`/`org_id` come from the grant
/// recorded at login. A managed caller with one live kickoff copies those
/// three fields from that binding instead, then still requires the saved
/// credential for this destination to name the original member and issuer.
/// Nothing here is caller-mutable mid-command.
#[derive(Clone, Debug)]
pub struct RemoteTarget {
    pub org: String,
    pub endpoint: String,
    pub org_id: String,
    /// Original actor identity for assignment-bound calls; checked on every
    /// credential load, including renewal and post-resolution reloads.
    pub identity: Option<CliIdentityPin>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliIdentityPin {
    pub subject_id: String,
    pub issuer: String,
}

const SESSION_VERSION: &str = "hosted-cadence-cli-session.v1";
/// Every capability this client knows. A stored login holds a subset that
/// includes `cli.read` and `cli.write` (the default login); a renewal keeps
/// exactly the stored set, so a CLI-only login stays CLI-only.
const CAPABILITIES: &[&str] = &[
    "cli.read",
    "cli.write",
    "wiki.read",
    "wiki.write",
    "web.read",
    "web.prepare",
    // CAD-1586: settings management scopes are opt-in like wiki — an
    // existing login without them stays valid for every other verb.
    "settings.read",
    "settings.write",
    // CAD-1946: opt-in like settings — the operator's merge approval.
    "audit.approve",
    // CAD-1922: the first run's one sign-in/registration grant may carry the
    // optional owner-only `billing.read`. Recognizing the scope here keeps
    // renewal's exact-equality check working for a grant that includes it;
    // it is never added to an ordinary login and `billing.cancel` is never
    // recognized (it can only be minted by the separate owner action).
    "billing.read",
];
const RENEW_BEFORE: u64 = 60;
const SKEW: u64 = 5;
const RENEWAL_WAIT: Duration = Duration::from_secs(20);

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CliSession {
    session_id: String,
    refresh_token: String,
    generation: u64,
    device_label: String,
    idle_expires_at: u64,
    absolute_expires_at: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingRenewal {
    operation_id: String,
    expected_generation: u64,
}

/// Legacy credential fields remain readable; only new grants have the immutable
/// identity and renewable-session fields needed to refresh.
#[derive(Clone)]
struct Credential {
    organization_id: String,
    organization_slug: Option<String>,
    audience: String,
    issuer: Option<String>,
    subject_id: Option<String>,
    capabilities: Vec<String>,
    access_token: String,
    expires_at: u64,
    cli_session: Option<CliSession>,
    pending: Option<PendingRenewal>,
}

fn reject(message: impl Into<String>) -> Error {
    Error::rejected(message.into())
}

fn now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| reject("invalid system clock"))?
        .as_secs())
}

/// Read the stored `hct_` for `target` from `dir` (the remote-auth dir).
/// The file must still name exactly this org's workspace and endpoint — a
/// credential recorded for another org is never sent to this host (contract
/// I1). Expired means "run `cadence login` again", not retry.
fn private_directory(dir: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(reject(
                "no remote credential directory — run `cadence login` first; nothing was sent",
            ));
        }
        Err(error) => return Err(error.into()),
    };
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(reject(
            "remote credential directory must be owned by you, mode 0700, and not a symlink",
        ));
    }
    Ok(())
}

/// Command use retains the legacy access-only expiry refusal. Offline status
/// reads the same validated metadata without treating expiry as corrupt storage.
fn load_credential(dir: &Path, target: &RemoteTarget) -> Result<Credential> {
    let cred = read_credential_metadata(dir, target)?;
    if cred.cli_session.is_none() && cred.expires_at <= now()? {
        return Err(reject(
            "the remote credential has expired — run `cadence login` again",
        ));
    }
    Ok(cred)
}

fn read_credential_metadata(dir: &Path, target: &RemoteTarget) -> Result<Credential> {
    private_directory(dir)?;
    let path = dir.join(format!("cli-{}.json", target.org));
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|_| {
            reject(format!(
                "no remote credential for org '{}' — run `cadence login --issuer \
                 <issuer> --org {} --slug {}` first",
                target.org, target.org_id, target.org
            ))
        })?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.len() > 8192
    {
        return Err(reject(
            "remote credential must be a small private owned regular file",
        ));
    }
    let mut raw = Vec::new();
    file.take(8193).read_to_end(&mut raw)?;
    let body: Value =
        serde_json::from_slice(&raw).map_err(|_| reject("invalid remote credential"))?;
    let object = body
        .as_object()
        .ok_or_else(|| reject("invalid remote credential"))?;
    let allowed = [
        "version",
        "organization_id",
        "organization_slug",
        "audience",
        "issuer",
        "subject_id",
        "capabilities",
        "access_token",
        "expires_at",
        "cli_session",
        "pending_renewal",
    ];
    if ![
        "version",
        "organization_id",
        "audience",
        "access_token",
        "expires_at",
    ]
    .iter()
    .all(|key| object.contains_key(*key))
        || object.keys().any(|key| !allowed.contains(&key.as_str()))
        || body["version"] != "hosted-cadence-auth.v1"
    {
        return Err(reject("invalid remote credential"));
    }
    let cred = Credential {
        organization_id: body["organization_id"]
            .as_str()
            .ok_or_else(|| reject("invalid remote credential"))?
            .to_owned(),
        organization_slug: body
            .get("organization_slug")
            .and_then(Value::as_str)
            .map(str::to_owned),
        audience: body["audience"]
            .as_str()
            .ok_or_else(|| reject("invalid remote credential"))?
            .to_owned(),
        issuer: body
            .get("issuer")
            .and_then(Value::as_str)
            .map(str::to_owned),
        subject_id: body
            .get("subject_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        capabilities: body
            .get("capabilities")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        access_token: body["access_token"]
            .as_str()
            .ok_or_else(|| reject("invalid remote credential"))?
            .to_owned(),
        expires_at: body["expires_at"]
            .as_u64()
            .ok_or_else(|| reject("invalid remote credential"))?,
        cli_session: body
            .get("cli_session")
            .filter(|v| !v.is_null())
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| reject("invalid renewable CLI session"))?,
        pending: body
            .get("pending_renewal")
            .filter(|v| !v.is_null())
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| reject("invalid pending CLI renewal"))?,
    };
    if cred.organization_id != target.org_id || cred.audience != target.endpoint {
        return Err(reject(
            "the stored credential belongs to a different workspace or host; \
             it is never sent to another destination — run `cadence login` again",
        ));
    }
    if let Some(identity) = &target.identity {
        if identity.subject_id.is_empty()
            || identity.issuer.is_empty()
            || cred.subject_id.as_deref() != Some(identity.subject_id.as_str())
            || cred.issuer.as_deref() != Some(identity.issuer.as_str())
        {
            return Err(reject(
                "saved login changed member or issuer; the original assignment identity was retained",
            ));
        }
    }
    if !valid_token(&cred.access_token, "hct_") {
        return Err(reject("invalid remote credential"));
    }
    Ok(cred)
}

#[cfg(test)]
thread_local! {
    pub(crate) static CREDENTIAL_WAITING: std::cell::RefCell<Option<std::sync::mpsc::Sender<()>>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn private_lock(dir: &Path, slug: &str, deadline: Instant) -> Result<File> {
    private_directory(dir)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(format!(".cli-{slug}.lock")))?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(reject(
            "remote credential lock must be a private owned file",
        ));
    }
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(file);
        }
        #[cfg(test)]
        CREDENTIAL_WAITING.with(|observer| {
            if let Some(sender) = observer.borrow_mut().take() {
                let _ = sender.send(());
            }
        });
        if Instant::now() >= deadline {
            return Err(Error::busy_coded(
                "timeout",
                "remote credential lock deadline elapsed",
            ));
        }
        std::thread::sleep(
            Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

fn save_credential(dir: &Path, slug: &str, cred: &Credential) -> Result<()> {
    private_directory(dir)?;
    let path = dir.join(format!("cli-{slug}.json"));
    match fs::symlink_metadata(&path) {
        Ok(meta)
            if meta.is_file()
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.mode() & 0o077 == 0 => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => {
            return Err(reject(
                "remote credential destination must be a private owned regular file",
            ))
        }
    }
    let mut random = [0u8; 16];
    getrandom::fill(&mut random)
        .map_err(|_| reject("unable to create remote credential temporary path"))?;
    let tmp = dir.join(format!(
        ".cli-{slug}.tmp-{}-{}",
        std::process::id(),
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random)
    ));
    let body = serde_json::json!({"version":"hosted-cadence-auth.v1","organization_id":cred.organization_id,
        "organization_slug":cred.organization_slug,"audience":cred.audience,"issuer":cred.issuer,
        "subject_id":cred.subject_id,"capabilities":cred.capabilities,"access_token":cred.access_token,
        "expires_at":cred.expires_at,"cli_session":cred.cli_session,"pending_renewal":cred.pending});
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .map_err(|_| reject("remote credential temporary path refused"))?;
    let result = (|| {
        file.write_all(
            &serde_json::to_vec(&body).map_err(|_| reject("cannot encode remote credential"))?,
        )?;
        file.sync_all()?;
        fs::rename(&tmp, &path)?;
        File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

fn renewed_credential(dir: &Path, target: &RemoteTarget, deadline: Instant) -> Result<Credential> {
    renewed_credential_with(
        dir,
        target,
        deadline,
        crate::remote_enrollment::cli_session_post_detailed,
    )
}

/// The renewal state machine (lock, load, due check, pending persist, POST,
/// validate, save) with the issuer POST injected. Production passes the real
/// `cli_session_post`; tests pass a closure that answers in place of the wire
/// call, after the real pre-send checks, which live inside `cli_session_post`.
fn renewed_credential_with(
    dir: &Path,
    target: &RemoteTarget,
    deadline: Instant,
    post: impl FnOnce(
        &Path,
        &str,
        &str,
        &str,
        Value,
        Instant,
    ) -> Result<crate::remote_enrollment::IssuerAnswer>,
) -> Result<Credential> {
    renewed_credential_for(dir, target, deadline, CredentialPurpose::Cli, post)
}

/// CAD-1853: rotate a login that carries the cloud-agent scopes. A board
/// login without those scopes is not cloud consent. An older grant of
/// exactly the three cloud scopes still renews.
fn renewed_cloud_credential_with(
    dir: &Path,
    target: &RemoteTarget,
    deadline: Instant,
    post: impl FnOnce(
        &Path,
        &str,
        &str,
        &str,
        Value,
        Instant,
    ) -> Result<crate::remote_enrollment::IssuerAnswer>,
) -> Result<Credential> {
    renewed_credential_for(dir, target, deadline, CredentialPurpose::CloudAgent, post)
}

/// The cloud transport boundary's custody: rotate the saved cloud-purpose
/// login when it is due and return the credential whose token is actually
/// sent. Not due means no wire call. The lock is taken here, so the frozen
/// owner and the saved consent are re-read after any concurrent replacement.
fn renewed_cloud_credential(dir: &Path, target: &RemoteTarget) -> Result<Credential> {
    renewed_cloud_credential_with(
        dir,
        target,
        Instant::now() + RENEWAL_WAIT,
        crate::remote_enrollment::cli_session_post_detailed,
    )
}

/// What a cloud send actually uses: the saved cloud-purpose login, rotated
/// under its own purpose only when renewal is due. A login with no renewable
/// session, or one whose access token is still fresh, is used as it is — a
/// grant this purpose may not rotate (say, one saved without enrollment
/// authority) can still serve the scope it carries until it expires. Once
/// renewal is due the strict purpose rule decides, so an unrecognized grant
/// is refused instead of being sent with an expired token.
fn cloud_send_credential(
    dir: &Path,
    target: &RemoteTarget,
    cred: Credential,
) -> Result<Credential> {
    let at = now()?;
    if cred.cli_session.is_none()
        || (cred.pending.is_none() && cred.expires_at > at.saturating_add(RENEW_BEFORE))
    {
        return Ok(cred);
    }
    renewed_cloud_credential(dir, target)
}

fn renewed_credential_for(
    dir: &Path,
    target: &RemoteTarget,
    deadline: Instant,
    purpose: CredentialPurpose,
    post: impl FnOnce(
        &Path,
        &str,
        &str,
        &str,
        Value,
        Instant,
    ) -> Result<crate::remote_enrollment::IssuerAnswer>,
) -> Result<Credential> {
    let _lock = private_lock(dir, &target.org, deadline)?;
    if Instant::now() >= deadline {
        return Err(Error::busy_coded(
            "timeout",
            "remote command deadline elapsed before session renewal",
        ));
    }
    let cred = load_credential(dir, target)?;
    renew_credential_locked(dir, target, deadline, cred, purpose, post)
}

/// Caller holds the source lock, including any frozen-identity check.
fn renew_credential_locked(
    dir: &Path,
    target: &RemoteTarget,
    deadline: Instant,
    mut cred: Credential,
    purpose: CredentialPurpose,
    post: impl FnOnce(
        &Path,
        &str,
        &str,
        &str,
        Value,
        Instant,
    ) -> Result<crate::remote_enrollment::IssuerAnswer>,
) -> Result<Credential> {
    let at = now()?;
    // Cloud renewal checks the saved scopes even while the access token is
    // fresh, so a board login is never rotated as cloud consent. The CLI
    // purpose keeps the legacy tolerance: a login without a renewable
    // session is refused only once renewal is actually due.
    if purpose == CredentialPurpose::CloudAgent && !purpose.admits(&cred.capabilities) {
        return Err(purpose_refusal(purpose));
    }
    let due = cred.pending.is_some() || cred.expires_at <= at.saturating_add(RENEW_BEFORE);
    if !due {
        return Ok(cred);
    }
    let Some(session) = cred.cli_session.clone() else {
        return Err(purpose_refusal(purpose));
    };
    let issuer = cred.issuer.as_deref().ok_or_else(|| {
        reject(format!(
            "renewable session issuer missing — run `{}` again",
            purpose.login_hint()
        ))
    })?;
    if cred.organization_slug.as_deref() != Some(target.org.as_str())
        || cred.subject_id.as_deref().is_none_or(|s| !valid_id(s))
        || !purpose.admits(&cred.capabilities)
        || session.idle_expires_at <= at
        || session.absolute_expires_at <= at
        || session.idle_expires_at > session.absolute_expires_at
        || session.session_id.strip_prefix("hcs_").is_none_or(|v| {
            v.len() != 36 || !v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        || !valid_token(&session.refresh_token, "hcr_")
        || session.device_label.is_empty()
        || session.device_label.len() > 120
        || session.generation == 0
    {
        return Err(purpose_refusal(purpose));
    }
    let pending = match cred.pending.clone() {
        Some(pending) => pending,
        None => {
            let mut random = [0u8; 32];
            getrandom::fill(&mut random)
                .map_err(|_| reject("unable to create renewal operation"))?;
            let operation_id = format!(
                "op_{}",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random)
            );
            let pending = PendingRenewal {
                operation_id,
                expected_generation: session.generation,
            };
            crate::remote_enrollment::validate_cli_session_pin(dir, issuer)?;
            cred.pending = Some(pending.clone());
            save_credential(dir, &target.org, &cred)?;
            pending
        }
    };
    if pending.expected_generation != session.generation
        || !valid_operation_id(&pending.operation_id)
    {
        return Err(reject(
            "pending CLI renewal is inconsistent — run `cadence login` again",
        ));
    }
    let request = serde_json::json!({"version":SESSION_VERSION,"session_id":session.session_id,
        "operation_id":pending.operation_id,"expected_generation":pending.expected_generation,
        "organization_id":cred.organization_id,"organization_slug":target.org,"audience":cred.audience,
        "subject_id":cred.subject_id,"device_label":session.device_label,"capabilities":cred.capabilities});
    let request_deadline = deadline.min(Instant::now() + RENEWAL_WAIT);
    let answer = post(
        dir,
        issuer,
        "/v1/hosted-cadence/cli-session/renew",
        &session.refresh_token,
        request,
        request_deadline,
    )?;
    if answer.status != 200 {
        return Err(renewal_refusal(
            answer.status,
            &answer.body,
            answer.correlation.as_ref(),
        ));
    }
    let body = answer.body;
    validate_renewal_response(&body, &cred, target, &pending)?;
    let next = serde_json::from_value::<CliSession>(body["cli_session"].clone())
        .map_err(|_| reject("remote renewal returned invalid session metadata"))?;
    cred.access_token = body["credential"]["access_token"]
        .as_str()
        .unwrap()
        .to_owned();
    cred.expires_at = body["credential"]["expires_at"].as_u64().unwrap();
    cred.cli_session = Some(next);
    cred.pending = None;
    save_credential(dir, &target.org, &cred)?;
    Ok(cred)
}

/// CAD-1434: renewal's separate adapter. The issuer's renewal route answers
/// `{error:"<code>"}` (AOS `cli-session.ts`), not the gateway envelope, so it
/// has its own reader. Only the issuer's terminal refresh-grant refusals ask
/// for a fresh login. Anything else (a transient 503, a malformed answer) keeps
/// the saved login and pending operation for a later retry, with no consent.
fn renewal_refusal(
    status: u16,
    body: &Value,
    correlation: Option<&crate::remote_enrollment::IssuerCorrelation>,
) -> Error {
    let code = body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // The issuer's id is shown only when the header was valid (see
    // `issuer_correlation`). It is a support reference, never an authority.
    let request = correlation
        .map(|c| format!(" (issuer request {})", c.request_id))
        .unwrap_or_default();
    match (status, code) {
        (401, "invalid_token") | (400, "invalid_grant") => reject(format!(
            "the renewable CLI session was rejected by the issuer — run `cadence login` again{request}"
        )),
        _ => reject(format!(
            "remote CLI session renewal is not complete (HTTP {status}); your saved login \
             is kept, retry the command later{request}"
        )),
    }
}

fn validate_renewal_response(
    body: &Value,
    prior: &Credential,
    target: &RemoteTarget,
    pending: &PendingRenewal,
) -> Result<()> {
    let top = body
        .as_object()
        .ok_or_else(|| reject("remote renewal returned invalid response"))?;
    let allowed = [
        "version",
        "organization_id",
        "organization_slug",
        "audience",
        "principal",
        "capabilities",
        "credential",
        "cli_session",
    ];
    let old_session = prior
        .cli_session
        .as_ref()
        .ok_or_else(|| reject("missing prior CLI session"))?;
    if top.keys().any(|key| !allowed.contains(&key.as_str()))
        || !(top.len() == 7 || top.len() == 8)
        || ![
            "version",
            "organization_id",
            "audience",
            "principal",
            "capabilities",
            "credential",
            "cli_session",
        ]
        .iter()
        .all(|key| top.contains_key(*key))
        || body["version"] != "hosted-cadence-auth.v1"
        || body["organization_id"] != prior.organization_id
        || body
            .get("organization_slug")
            .is_some_and(|v| v.as_str() != Some(target.org.as_str()))
        || body["audience"] != target.endpoint
        || !exact_object(&body["principal"], &["kind", "current_role", "subject_id"])
        || body["principal"]["kind"] != "user"
        || body["principal"]["current_role"]
            .as_str()
            .is_none_or(|role| !crate::remote_enrollment::human_workspace_role(role))
        || body["principal"]["subject_id"].as_str() != prior.subject_id.as_deref()
        || !same_capabilities_json(&body["capabilities"], &prior.capabilities)
        || !exact_object(
            &body["credential"],
            &[
                "credential_id",
                "access_token",
                "token_type",
                "issued_at",
                "expires_at",
                "renewal",
            ],
        )
        || body["credential"]["token_type"] != "Bearer"
        || body["credential"]["renewal"] != "reexchange"
        || body["credential"]["credential_id"]
            .as_str()
            .is_none_or(|v| !valid_id(v))
        || body["credential"]["access_token"]
            .as_str()
            .is_none_or(|v| !valid_token(v, "hct_"))
        || !exact_object(
            &body["cli_session"],
            &[
                "session_id",
                "refresh_token",
                "generation",
                "device_label",
                "idle_expires_at",
                "absolute_expires_at",
            ],
        )
        || body["cli_session"]["session_id"] != old_session.session_id
        || body["cli_session"]["generation"].as_u64() != pending.expected_generation.checked_add(1)
        || body["cli_session"]["device_label"] != old_session.device_label
        || body["cli_session"]["refresh_token"]
            .as_str()
            .is_none_or(|v| !valid_token(v, "hcr_") || v == old_session.refresh_token)
    {
        return Err(reject("remote renewal identity or scope mismatch"));
    }
    let issued = body["credential"]["issued_at"]
        .as_u64()
        .ok_or_else(|| reject("invalid renewed credential"))?;
    let expires = body["credential"]["expires_at"]
        .as_u64()
        .ok_or_else(|| reject("invalid renewed credential"))?;
    let at = now()?;
    if issued > at.saturating_add(SKEW)
        || expires <= at
        || expires <= issued
        || expires - issued > 300
    {
        return Err(reject("remote renewal returned an invalid access expiry"));
    }
    let metadata = &body["cli_session"];
    if metadata.as_object().is_none_or(|m| m.len() != 6)
        || metadata["idle_expires_at"]
            .as_u64()
            .is_none_or(|t| t <= at || t > old_session.absolute_expires_at)
        || metadata["absolute_expires_at"] != old_session.absolute_expires_at
    {
        return Err(reject("remote renewal returned invalid session expiry"));
    }
    Ok(())
}

/// CAD-1853: which saved consent a renewal may rotate. The purpose comes
/// from the caller's boundary, never from the credential's contents, so a
/// cloud-only grant is never rotated into CLI authority and a CLI login is
/// never rotated as cloud consent.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CredentialPurpose {
    /// The default CLI/wiki/settings login: the CLI pair is required.
    Cli,
    /// A login that carries [`crate::remote_enrollment::CLOUD_AGENT_LOGIN_SCOPES`].
    /// That is the board login plus those scopes, or an older grant of
    /// exactly those three.
    CloudAgent,
}

impl CredentialPurpose {
    fn login_hint(self) -> &'static str {
        match self {
            CredentialPurpose::Cli => "cadence login",
            CredentialPurpose::CloudAgent => "cadence login --cloud-agent",
        }
    }

    /// Known, unique capabilities this purpose may renew. The renewal
    /// response must still match the saved set exactly; this predicate is
    /// what refuses a saved consent that purpose may never rotate.
    fn admits(self, values: &[String]) -> bool {
        match self {
            CredentialPurpose::Cli => valid_capabilities(values),
            CredentialPurpose::CloudAgent => {
                let scopes = crate::remote_enrollment::CLOUD_AGENT_LOGIN_SCOPES;
                let once = |scope: &str| {
                    values
                        .iter()
                        .filter(|value| value.as_str() == scope)
                        .count()
                        == 1
                };
                scopes.iter().all(|scope| once(scope))
                    && (values.len() == scopes.len() || valid_capabilities(values))
            }
        }
    }
}

fn purpose_refusal(purpose: CredentialPurpose) -> Error {
    match purpose {
        CredentialPurpose::Cli => {
            reject("renewable CLI session is invalid or expired — run `cadence login` again")
        }
        CredentialPurpose::CloudAgent => reject(
            "the saved login does not carry the cloud-agent scopes, or its renewable \
             session is invalid or expired — run `cadence login <slug> --cloud-agent` again",
        ),
    }
}

/// The issuer's own text for a renewal that did not complete, so a terminal
/// refusal stays actionable.
fn renewal_message(error: &Error) -> String {
    match error {
        Error::Rejected(text) | Error::Internal(text) => text.clone(),
        _ => "the saved cloud login could not be renewed, so nothing was sent".to_string(),
    }
}

/// A READ whose saved cloud consent could not be renewed has proved no
/// current read authority: the cloud rows are withheld and the rest of the
/// page stays. Nothing was sent, so this is never an uncertain send outcome.
fn cloud_renewal_withheld(error: Error) -> Error {
    Error::gate_coded("cloud_agent_unavailable", renewal_message(&error))
}

/// A SEND (or an enrollment) whose saved cloud consent could not be renewed
/// never reached the contract. It is a DEFINITE refusal — `cloud_agent_unavailable`
/// would classify the operation as a possibly-accepted send and strand it as
/// unknown, blocking the conversation until a readback of a turn that never
/// existed settles it.
fn cloud_renewal_refused(error: Error) -> Error {
    Error::gate_coded("cloud_agent_authority_missing", renewal_message(&error))
}

/// Known, unique capabilities that include the CLI pair. Wiki, web,
/// settings and the optional first-run `billing.read` are optional. The
/// cloud-agent scopes are optional too, and only as the whole set: one or
/// two of them is not a login this client rotates.
fn valid_capabilities(values: &[String]) -> bool {
    let once = |c: &str| values.iter().filter(|v| v.as_str() == c).count() == 1;
    let cloud = crate::remote_enrollment::CLOUD_AGENT_LOGIN_SCOPES;
    let cloud_present = cloud.iter().filter(|scope| once(scope)).count();
    let known = |scope: &str| CAPABILITIES.contains(&scope) || cloud.contains(&scope);
    values.iter().all(|v| known(v.as_str()))
        && values
            .iter()
            .enumerate()
            .all(|(i, v)| !values[..i].contains(v))
        && once("cli.read")
        && once("cli.write")
        && (cloud_present == 0 || cloud_present == cloud.len())
}
fn same_capabilities_json(value: &Value, expected: &[String]) -> bool {
    value.as_array().is_some_and(|values| {
        values.len() == expected.len()
            && expected.iter().all(|item| {
                values
                    .iter()
                    .filter(|v| v.as_str() == Some(item.as_str()))
                    .count()
                    == 1
            })
    })
}
fn exact_object(value: &Value, keys: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))
    })
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn valid_operation_id(value: &str) -> bool {
    value.strip_prefix("op_").is_some_and(|s| {
        (32..=80).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    })
}

fn valid_token(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|s| {
        s.len() == 43
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    })
}

/// One bounded POST to `{endpoint}{path}` with the `hct_` bearer. Redirects
/// are never followed (a 3xx is a verdict, not a hint: contract refusal
/// cases). Returns `(status, retry_after_seconds, body)`; `body` is `None`
/// when the answer is not JSON.
fn post(
    endpoint: &str,
    path: &str,
    bearer: &str,
    body: &Value,
    timeout: Duration,
) -> Result<(u16, Option<u64>, Option<Value>)> {
    let builder = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None);
    let response = crate::issuer_transport::agent(builder)
        .post(format!("{endpoint}{path}"))
        .header("Authorization", format!("Bearer {bearer}"))
        .send_json(body)
        .map_err(|_| reject("remote request failed"))?;
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let mut bytes = Vec::new();
    response
        .into_body()
        .as_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BODY {
        return Err(reject("remote response too large"));
    }
    let value = serde_json::from_slice(&bytes).ok();
    Ok((status, retry_after, value))
}

fn request_timeout(deadline: Instant, cap: Duration) -> Result<Duration> {
    let exhausted = || {
        Error::busy_coded(
            "waking",
            "remote wake budget exhausted — retry or increase --wake-timeout",
        )
    };
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(exhausted)?;
    if remaining.is_zero() {
        return Err(exhausted());
    }
    Ok(remaining.min(cap))
}

/// `POST /__platform/{cli,wiki}/authorize` — exchange the live `hct_` for one
/// actor envelope scoped to `scopes`. The request is the strict relay shape;
/// the answer must echo the version and carry a compact-JWS envelope.
fn authorize(
    target: &RemoteTarget,
    cred: &Credential,
    surface: Surface,
    verb: &str,
    scopes: &[&str],
    timeout: Duration,
) -> Result<String> {
    let (relay, version) = surface.relay();
    let (status, _, body) = post(
        &target.endpoint,
        &format!("{relay}/authorize"),
        &cred.access_token,
        &json!({
            "version": version,
            "organization_id": target.org_id,
            "audience": target.endpoint,
            "requested_scopes": scopes,
            "ttl_seconds": MAX_TTL,
        }),
        timeout,
    )?;
    match status {
        200 => {
            let body = body.ok_or_else(|| reject("remote authorize answered non-JSON"))?;
            let envelope = body["envelope"].as_str().unwrap_or_default();
            let compact_jws = envelope.split('.').all(|p| {
                !p.is_empty()
                    && p.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            }) && envelope.split('.').count() == 3;
            if body["version"].as_str() != Some(version) || !compact_jws {
                return Err(reject("remote authorize returned an invalid envelope"));
            }
            Ok(envelope.to_string())
        }
        s if (300..400).contains(&s) => Err(reject("remote redirect refused")),
        403 if matches!(verb, "issue_message_send" | "issue_messages_read")
            && body
                .as_ref()
                .and_then(|b| b.pointer("/error/code"))
                .and_then(Value::as_str)
                == Some("forbidden") =>
        {
            Err(Error::invalid(
                "insufficient_scope",
                "this login cannot write to this workspace (it lacks cli.write), and a new login \
                 only helps if your role allows it. Nothing was sent (insufficient_scope, HTTP 403)",
            ))
        }
        _ => Err(refusal_error(status, body.as_ref())),
    }
}

/// One `POST /__platform/{cli,wiki}/call` attempt: fresh envelope, strict
/// body, never an authority-shaped key. A wiki envelope requests only the
/// scope its action needs (`write` → `wiki.write`, else `wiki.read`).
/// `arguments` carries only the verb's own payload (identity/org/destination
/// live only in the verified envelope).
fn call_once(
    target: &RemoteTarget,
    cred: &Credential,
    surface: Surface,
    verb: &str,
    arguments: &Map<String, Value>,
    deadline: Instant,
) -> Result<(u16, Option<u64>, Option<Value>)> {
    let scopes: &[&str] = match surface {
        // CAD-1586: the settings verb mints a narrow envelope scoped to
        // the operation (read vs write); master status exposes the
        // configured model/allowlist, so it mints settings.read (manager
        // ruling — generic cli.read never implies Settings access).
        Surface::Cli if verb == REMOTE_SETTINGS_VERB => &[settings_op_scope(
            arguments
                .get("operation")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )],
        Surface::Cli if verb == REMOTE_MASTER_STATUS_VERB => &["settings.read"],
        Surface::Cli => cli_requested_scopes(verb),
        Surface::Wiki if verb == "write" => &["wiki.write"],
        Surface::Wiki => &["wiki.read"],
    };
    let envelope = authorize(
        target,
        cred,
        surface,
        verb,
        scopes,
        request_timeout(deadline, AUTH_TIMEOUT)?,
    )?;
    let (relay, version) = surface.relay();
    // A wiki body always carries `arguments` (`{}` when there are none).
    let mut body = match surface {
        Surface::Cli => json!({"version": version, "envelope": envelope, "verb": verb}),
        Surface::Wiki => {
            json!({"version": version, "envelope": envelope, "action": verb, "arguments": {}})
        }
    };
    if !arguments.is_empty() {
        body["arguments"] = Value::Object(arguments.clone());
    }
    // A mutation's transport failure after the call was attempted is
    // outcome-unknown, not a rejection — the write may have landed, so the
    // caller must reconcile with the verb's own read, never assume nothing
    // landed. This covers the CLI writes and the wiki `write` action alike:
    // the surface decides, not a CLI-only verb list.
    let effect = call_effect(surface, verb, arguments);
    post(
        &target.endpoint,
        &format!("{relay}/call"),
        &cred.access_token,
        &body,
        request_timeout(deadline, CALL_TIMEOUT)?,
    )
    .map_err(|e| {
        if effect.is_mutation() && e.kind() != "unknown" {
            Error::unknown(format!(
                "{e} — {}",
                effect.reconciliation().unwrap_or(RECONCILE_UNKNOWN)
            ))
        } else {
            e
        }
    })
}

/// CAD-913: one `POST /__platform/wiki/upload` multipart request — the same
/// bearer and a fresh `wiki.write` envelope as `call_once`. The envelope rides
/// in the `X-Cadence-Wiki-Envelope` header (never logged); the body carries
/// `version`, `action=write_blob` and `path` as form fields and the bytes in
/// one `file` part. Returns `(status, retry_after, body)` like `post`, so the
/// caller applies the same `classify` verdicts (wake, conflict, refusal). The
/// refusal path never surfaces the raw body.
fn upload_once(
    target: &RemoteTarget,
    cred: &Credential,
    logical_path: &str,
    file_name: &str,
    bytes: &[u8],
    deadline: Instant,
) -> Result<(u16, Option<u64>, Option<Value>)> {
    let envelope = authorize(
        target,
        cred,
        Surface::Wiki,
        "",
        &["wiki.write"],
        request_timeout(deadline, AUTH_TIMEOUT)?,
    )?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| reject("unable to create upload boundary"))?;
    let boundary = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
    let fields = [
        ("version", WIKI_VERSION),
        ("action", "write_blob"),
        ("path", logical_path),
    ];
    let body = crate::wiki::upload::build_multipart(&boundary, &fields, file_name, bytes);
    let timeout = request_timeout(deadline, CALL_TIMEOUT)?;
    let builder = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None);
    let response = crate::issuer_transport::agent(builder)
        .post(format!("{}/__platform/wiki/upload", target.endpoint))
        .header("Authorization", format!("Bearer {}", cred.access_token))
        .header("X-Cadence-Wiki-Envelope", envelope.as_str())
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .send(&body[..])
        .map_err(|_| upload_outcome_unknown("remote request failed"))?;
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let mut bytes = Vec::new();
    response
        .into_body()
        .as_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| upload_outcome_unknown("the upload answer could not be read"))?;
    if bytes.len() as u64 > MAX_BODY {
        return Err(upload_outcome_unknown("remote response too large"));
    }
    Ok((status, retry_after, serde_json::from_slice(&bytes).ok()))
}

/// What one `artifact put` asks for. The server's answer says what happened.
pub struct ArtifactUpload<'a> {
    pub kind: &'a str,
    pub title: &'a str,
    pub id: Option<&'a str>,
    pub issue: Option<&'a str>,
    pub label: Option<&'a str>,
    pub customer: Option<&'a str>,
    pub file_name: &'a str,
    pub bytes: &'a [u8],
}

/// AOS-346: one `POST /__platform/artifacts/upload`, the wiki upload's shape on
/// the CLI surface: the bearer, a fresh `cli.write` envelope, then one multipart
/// body. `request_id` is fixed for the command, so a wake retry cannot add a
/// second version.
fn artifact_upload_once(
    target: &RemoteTarget,
    cred: &Credential,
    upload: &ArtifactUpload,
    request_id: &str,
    deadline: Instant,
) -> Result<(u16, Option<u64>, Option<Value>)> {
    let envelope = authorize(
        target,
        cred,
        Surface::Cli,
        "",
        &["cli.write"],
        request_timeout(deadline, AUTH_TIMEOUT)?,
    )?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| reject("unable to create upload boundary"))?;
    let boundary = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
    let mut fields = vec![
        ("version", CLI_VERSION),
        ("kind", upload.kind),
        ("request_id", request_id),
    ];
    if !upload.title.is_empty() {
        fields.push(("title", upload.title));
    }
    for (name, value) in [
        ("id", upload.id),
        ("issue", upload.issue),
        ("label", upload.label),
        ("customer", upload.customer),
    ] {
        if let Some(value) = value {
            fields.push((name, value));
        }
    }
    let body =
        crate::wiki::upload::build_multipart(&boundary, &fields, upload.file_name, upload.bytes);
    let builder = ureq::Agent::config_builder()
        .timeout_global(Some(request_timeout(deadline, CALL_TIMEOUT)?))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None);
    let response = crate::issuer_transport::agent(builder)
        .post(format!("{}/__platform/artifacts/upload", target.endpoint))
        .header("Authorization", format!("Bearer {}", cred.access_token))
        .header("X-Cadence-Cli-Envelope", envelope.as_str())
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .send(&body[..])
        .map_err(|_| artifact_outcome_unknown())?;
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let mut bytes = Vec::new();
    response
        .into_body()
        .as_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| artifact_outcome_unknown())?;
    if bytes.len() as u64 > MAX_BODY {
        return Err(artifact_outcome_unknown());
    }
    Ok((status, retry_after, serde_json::from_slice(&bytes).ok()))
}

/// AOS-346 `artifact put`: hand one local file to a hosted workspace. A tenant
/// that predates the route is named as such, never reported as an outage.
pub fn upload_artifact(
    target: &RemoteTarget,
    upload: &ArtifactUpload,
    wake_timeout: u64,
    note: impl Fn(&str),
) -> Result<Value> {
    let dir = auth_dir(None).map_err(|_| {
        reject("remote credential directory is unavailable — set HOME or XDG_CONFIG_HOME")
    })?;
    upload_artifact_in(&dir, target, upload, wake_timeout, note)
}

/// Upload with an explicit credential directory, as in `upload_file_in`.
fn upload_artifact_in(
    dir: &Path,
    target: &RemoteTarget,
    upload: &ArtifactUpload,
    wake_timeout: u64,
    note: impl Fn(&str),
) -> Result<Value> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(wake_timeout))
        .ok_or_else(|| reject("invalid wake timeout"))?;
    let waking = || {
        Error::busy_coded(
            "waking",
            format!(
                "org '{}' is still waking after {}s — retry, or pass a larger --wake-timeout",
                target.org, wake_timeout
            ),
        )
    };
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| reject("unable to create a request id"))?;
    let request_id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
    loop {
        request_timeout(deadline, AUTH_TIMEOUT).map_err(|_| waking())?;
        let cred = renewed_credential(dir, target, deadline)?;
        if !cred.capabilities.is_empty() && !cred.capabilities.iter().any(|c| c == "cli.write") {
            return Err(reject(
                "artifact put needs the cli.write capability, which this remote login \
                 lacks — nothing was sent",
            ));
        }
        let (status, retry_after, body) =
            artifact_upload_once(target, &cred, upload, &request_id, deadline)?;
        if explicit_mutation_uncertainty(body.as_ref()).is_some() {
            return Err(artifact_outcome_unknown());
        }
        if status == 501 || (status == 404 && body.is_none()) {
            return Err(reject(artifact_tenant_too_old(
                "artifact put",
                status,
                "stored",
            )));
        }
        // A board gate or the documented waking response is pre-forward.
        // Other 5xx replies may hide a committed upload; the native CLI
        // tracker's refusal codes do not prove anything for this endpoint.
        let pre_forward = board_gate_refused(body.as_ref()) || worker_waking(status, body.as_ref());
        if status >= 500 && !pre_forward {
            return Err(artifact_outcome_unknown());
        }
        if !(200..300).contains(&status) && status != 503 {
            return Err(artifact_refusal(status, body.as_ref()));
        }
        match classify(status, retry_after, body) {
            Verdict::Done(Ok(answer)) => {
                // Reject a foreign receipt before hashing the uploaded bytes.
                if upload
                    .id
                    .is_some_and(|id| answer["file"]["id"].as_str() != Some(id))
                {
                    return Err(artifact_outcome_unknown());
                }
                return artifact_receipt(answer, upload.bytes);
            }
            Verdict::Done(Err(_)) if (200..300).contains(&status) => {
                return Err(artifact_outcome_unknown());
            }
            Verdict::Done(Err(error)) => return Err(error),
            Verdict::Conflict(_) => return Err(reject("remote upload conflicted")),
            Verdict::Waking(wait) => {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .filter(|remaining| !remaining.is_zero())
                    .ok_or_else(waking)?;
                if wait >= remaining {
                    return Err(waking());
                }
                note(&format!("waking {}… ({}s)", target.org, wait.as_secs()));
                std::thread::sleep(wait);
            }
        }
    }
}

fn artifact_outcome_unknown() -> Error {
    Error::unknown(
        "the upload may or may not have been recorded — check `artifact find` before \
         repeating it, because a repeat can add a version",
    )
}

/// A success must name a usable version and the digest of the sent bytes.
fn artifact_receipt(answer: Value, bytes: &[u8]) -> Result<Value> {
    let file = &answer["file"];
    if !file["id"].as_str().is_some_and(crate::store::artifact_id)
        || !file["version"].as_u64().is_some_and(|version| version > 0)
    {
        return Err(artifact_outcome_unknown());
    }
    let digest = file["digest"]
        .as_str()
        .filter(|digest| digest.len() == 64)
        .ok_or_else(artifact_outcome_unknown)?;
    let hex = b"0123456789abcdef";
    if !Sha256::digest(bytes)
        .iter()
        .zip(digest.as_bytes().as_chunks::<2>().0.iter())
        .all(|(byte, pair)| {
            pair[0] == hex[(byte >> 4) as usize] && pair[1] == hex[(byte & 0x0f) as usize]
        })
    {
        return Err(artifact_outcome_unknown());
    }
    Ok(answer)
}

/// The store's refusals carry a safe code and message of its own; anything else
/// goes through the shared decoder, which never echoes a raw body.
fn artifact_refusal(status: u16, body: Option<&Value>) -> Error {
    const STORE_CODES: &[&str] = &[
        "invalid_request",
        "not_found",
        "conflict",
        "payload_too_large",
    ];
    let error = body.and_then(|b| b.get("error"));
    let code = error.and_then(|e| e["code"].as_str());
    let message = error.and_then(|e| e["message"].as_str());
    match (code, message) {
        (Some(code), Some(message)) if STORE_CODES.contains(&code) => {
            let message: String = message.chars().take(300).collect();
            let message = message.trim_end_matches('.');
            reject(format!(
                "artifact put refused ({code}): {message}. Nothing was stored"
            ))
        }
        _ => refusal_error(status, body),
    }
}

/// Deploy order is the tenant first, then this client. The fallback is a named
/// refusal, so a mismatch never reads as an outage or an empty result.
pub fn artifact_tenant_too_old(verb: &str, status: u16, nothing: &str) -> String {
    format!(
        "this workspace's server does not serve `{verb}` yet (HTTP {status}): update the \
         tenant server first, then this Cadence client. Nothing was {nothing}"
    )
}

/// CAD-913 `wiki upload`: send one local file (already read and size-checked
/// by `wiki::upload::read_local_file`) to the remote wiki at `logical_path`.
/// Without `overwrite`, the parent is listed first and an existing name is
/// refused before any upload. The wake loop and verdicts match `call_verb`.
/// The bearer and the envelope are never printed.
///
/// The overwrite check is advisory, not atomic: a file created between the
/// listing and the upload is replaced. Closing that needs a server-side
/// create-only precondition, which the upload route does not send yet.
pub fn upload_file(
    target: &RemoteTarget,
    logical_path: &str,
    file_name: &str,
    bytes: &[u8],
    overwrite: bool,
    wake_timeout: u64,
    note: impl Fn(&str),
) -> Result<Value> {
    let dir = auth_dir(None).map_err(|_| {
        reject("remote credential directory is unavailable — set HOME or XDG_CONFIG_HOME")
    })?;
    upload_file_in(
        &dir,
        target,
        logical_path,
        file_name,
        bytes,
        overwrite,
        wake_timeout,
        note,
    )
}

/// CAD-913: does the parent listing show `name`? Fails closed on an allowlist:
/// the listing is complete only when it is an object whose keys are exactly
/// `path` and `entries`, with `entries` an array. Any other key (a paging or
/// truncation field such as `next_token`, which a denylist would miss), a
/// missing `entries`, or a non-object body means the listing may be partial,
/// so a name absent from it proves nothing. The error never includes the
/// listing contents.
fn listing_taken(body: &Value, name: &str) -> std::result::Result<bool, &'static str> {
    const KNOWN: &[&str] = &["path", "entries"];
    const INCOMPLETE: &str = "the listing may be incomplete";
    let object = body.as_object().ok_or(INCOMPLETE)?;
    if object.len() != KNOWN.len() || object.keys().any(|k| !KNOWN.contains(&k.as_str())) {
        return Err(INCOMPLETE);
    }
    let items = object["entries"].as_array().ok_or(INCOMPLETE)?;
    Ok(items.iter().any(|e| e["name"].as_str() == Some(name)))
}

/// `upload_file` with the credential dir injectable, so the test points it at
/// a fixture and never touches the real config dir.
#[allow(clippy::too_many_arguments)]
fn upload_file_in(
    dir: &Path,
    target: &RemoteTarget,
    logical_path: &str,
    file_name: &str,
    bytes: &[u8],
    overwrite: bool,
    wake_timeout: u64,
    note: impl Fn(&str),
) -> Result<Value> {
    // The sent bytes are fixed for the whole command. The digest is computed
    // lazily, once: a preflight, capability or deadline refusal before the
    // first dispatched upload never hashes them, and a wake retry reuses it.
    let mut expected = None;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(wake_timeout))
        .ok_or_else(|| reject("invalid wake timeout"))?;
    if wake_timeout == 0 {
        return Err(Error::busy_coded(
            "waking",
            format!(
                "org '{}' is still waking after {}s — retry, or pass a \
                 larger --wake-timeout",
                target.org, wake_timeout
            ),
        ));
    }
    if !overwrite {
        let (parent, name) = crate::wiki::upload::split_logical(logical_path);
        let mut args = Map::new();
        args.insert("path".into(), json!(parent));
        let listing = call_verb_until(
            target,
            "enumerate",
            args,
            deadline,
            wake_timeout,
            dir,
            &note,
            std::thread::sleep,
        )?;
        let RemoteAnswer::Ok(body) = listing else {
            return Err(reject("remote listing answered a conflict"));
        };
        let taken = listing_taken(&body, &name).map_err(|why| {
            reject(format!(
                "wiki upload refused: cannot prove '{logical_path}' is free ({why}); \
                 pass --overwrite to replace it. Nothing was sent"
            ))
        })?;
        if taken {
            return Err(reject(format!(
                "wiki upload refused: '{logical_path}' already exists; pass --overwrite \
                 to replace it. Nothing was sent"
            )));
        }
    }
    loop {
        request_timeout(deadline, AUTH_TIMEOUT)?;
        let cred = renewed_credential(dir, target, deadline)?;
        if !cred.capabilities.is_empty() && !cred.capabilities.iter().any(|c| c == "wiki.write") {
            return Err(reject(
                "wiki upload needs the wiki.write capability, which this remote login \
                 lacks — nothing was sent",
            ));
        }
        if Instant::now() >= deadline {
            return Err(Error::busy_coded(
                "timeout",
                "remote command deadline elapsed during session renewal",
            ));
        }
        // Digest the bytes immediately before the first dispatched upload —
        // every refusal above returns without hashing — and reuse it on a wake
        // retry.
        let (expected_sha256, expected_size) =
            expected.get_or_insert_with(|| (Sha256::digest(bytes), bytes.len() as u64));
        let (status, retry_after, body) =
            upload_once(target, &cred, logical_path, file_name, bytes, deadline)?;
        if let Some(code) = explicit_mutation_uncertainty(body.as_ref()) {
            return Err(upload_outcome_unknown(&format!(
                "the relay reported {code}"
            )));
        }
        // A refusal names the status and the contract's error code only: the
        // raw body could carry anything, so it is never echoed.
        let conflict = status == 409 && body.as_ref().is_some_and(|b| b.get("conflict").is_some());
        // The upload is a mutation: a 5xx with no pre-forward proof, or a 2xx
        // without an object receipt, may follow a stored blob. Its outcome is
        // unknown — the caller lists the parent path before uploading again.
        let after_forward = post_forward_failure(status, body.as_ref(), false);
        let unusable_success = (200..300).contains(&status) && !is_object_body(body.as_ref());
        if !(200..300).contains(&status) && status != 503 && !conflict {
            // CAD-1434: the gateway envelope is `{ok:false,error:{code}}`; the
            // decoder reads it (the old top-level read always fell through).
            if after_forward {
                return Err(upload_outcome_unknown(&format!(
                    "the relay answered HTTP {status}"
                )));
            }
            return Err(refusal_error(status, body.as_ref()));
        }
        if unusable_success {
            return Err(upload_outcome_unknown(
                "remote answer was not a usable receipt",
            ));
        }
        // The receipt must name the bytes and path this call sent: the digest
        // of the exact bytes, the tenant text variant's size plus rev, or the
        // native committed write. A 2xx without one proves nothing.
        if (200..300).contains(&status)
            && !body.as_ref().is_some_and(|reply| {
                upload_receipt_correlates(reply, logical_path, &expected_sha256[..], *expected_size)
            })
        {
            return Err(upload_outcome_unknown(
                "remote answer carried no receipt for the bytes and path sent",
            ));
        }
        match classify(status, retry_after, body) {
            Verdict::Done(Err(error)) if after_forward => {
                return Err(upload_outcome_unknown(&error.to_string()));
            }
            Verdict::Done(Err(error)) => return Err(error),
            Verdict::Done(result) => return result,
            Verdict::Conflict(_) => return Err(reject("remote upload conflicted")),
            Verdict::Waking(wait) => {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .filter(|remaining| !remaining.is_zero())
                    .ok_or_else(|| {
                        Error::busy_coded(
                            "waking",
                            format!(
                                "org '{}' is still waking after {}s — retry, or pass a \
                                 larger --wake-timeout",
                                target.org, wake_timeout
                            ),
                        )
                    })?;
                if wait >= remaining {
                    return Err(Error::busy_coded(
                        "waking",
                        format!(
                            "org '{}' is still waking after {}s — retry, or pass a \
                             larger --wake-timeout",
                            target.org, wake_timeout
                        ),
                    ));
                }
                note(&format!("waking {}… ({}s)", target.org, wait.as_secs()));
                std::thread::sleep(wait);
            }
        }
    }
}

/// Outcome of one call attempt: either the answer is final, the remote
/// answered a checked write conflict (HTTP 409 + `{conflict,…}` — the
/// caller resyncs, never a retry), or the remote is waking and `wait`
/// is how long to sleep before the next attempt.
enum Verdict {
    Done(Result<Value>),
    Conflict(Value),
    Waking(Duration),
}

/// CAD-1434: the safe decoding of a gateway refusal, shared by authorize,
/// cli/wiki call and blob upload. It reads the AOS gateway envelope
/// `{ok:false,error:{code,reason?}}` (or a legacy top-level `code`). The raw
/// body is never echoed. Renewal is a separate adapter; see `renewal_refusal`.
///
/// The gateway's real refusal shapes (AOS-243, `apps/api/src/board/routes.ts`):
/// - authorize/call/upload refusals are `401` with `error.code: "unauthorized"`
///   and, when proven at that layer, `error.reason` naming a policy outcome;
/// - a bare `401 unauthorized` without a reason means the bearer did not
///   resolve, but the server cannot say whether it expired or was revoked;
/// - explicit mutation uncertainty is not a safe retry; other 5xx are availability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The caller was authenticated and a policy outcome refused the action.
    /// A fresh login does not change it, so it never advises login.
    Authorization,
    /// The gateway proved the bearer's session is gone (a terminal code). The
    /// one precise case where a fresh login is the known remedy.
    Authentication,
    /// A 401 the server did not explain. It is not confirmed expiry or
    /// revocation: diagnose read-only before any login.
    Uncertain,
    /// An action may have committed; reconcile it before any repeat.
    MutationUncertain,
    /// Other 5xx: retry later; saved credentials and pending renewal are kept.
    Availability,
    /// An answer whose shape we do not recognise. Conservative fallback.
    Unknown,
}

impl Refusal {
    fn category(self) -> &'static str {
        match self {
            Self::Authorization => "authorization",
            Self::Authentication => "authentication",
            Self::Uncertain => "uncertain",
            Self::MutationUncertain => "outcome_unknown",
            Self::Availability => "availability",
            Self::Unknown => "unknown",
        }
    }
}

/// Policy outcomes the gateway names on a 401 (`error.reason`). Each is proven
/// at its own gate, so a fresh login would not change it. Allowlisted: an
/// unknown reason is never trusted as policy.
const POLICY_REASONS: &[&str] = &["scope_not_granted", "grant_narrowed", "binding_mismatch"];

/// The Cadence container's own envelope gate (`src/ui/wiki_envelope.rs`) answers
/// a 403 `insufficient_scope` when the signed grant does not cover the action or
/// the target path. It runs before any daemon mutation. It is authorization, not
/// a login problem, and only on 403: the same code on another status is unknown.
const CONTAINER_SCOPE_CODE: &str = "insufficient_scope";

/// `envelope_invalid` is a structural, expired or legacy-shape envelope. The
/// bearer may be fine, so it is neither terminal session loss nor policy.
const ENVELOPE_REASONS: &[&str] = &["envelope_invalid"];

/// Gateway codes that the issuer or gateway states outright: the session is
/// gone. Only these may ask for a fresh login as the known remedy.
const TERMINAL_CODES: &[&str] = &[
    "invalid_token",
    "invalid_grant",
    "session_expired",
    "session_revoked",
    "lineage_terminal",
];

/// Read the safe `error.code` and `error.reason` (or legacy top-level `code`).
/// Only lowercase, bounded tokens survive; anything else is dropped.
fn refusal_fields(body: Option<&Value>) -> (Option<&str>, Option<&str>) {
    fn safe(v: Option<&str>) -> Option<&str> {
        v.filter(|c| c.len() <= 64 && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
    }
    let error = body.and_then(|b| b.get("error"));
    let code = error
        .and_then(|e| e.get("code"))
        .or_else(|| body.and_then(|b| b.get("code")))
        .and_then(Value::as_str);
    let reason = error.and_then(|e| e.get("reason")).and_then(Value::as_str);
    (safe(code), safe(reason))
}

/// CAD-1434 / AOS-243: the correlation a refusal carries, read only when it is
/// safe to show. `hop` is an allowlisted name (today only the board names a
/// hop) and `request_id` must be a bounded UUID. Anything else is dropped, so
/// no arbitrary server text reaches the message or a log.
fn refusal_correlation(body: Option<&Value>) -> Option<String> {
    let error = body.and_then(|b| b.get("error"))?;
    let hop = error
        .get("hop")
        .and_then(Value::as_str)
        .filter(|h| *h == "board")?;
    let id = error
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|id| id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))?;
    Some(format!(" (hop {hop}, request {id})"))
}

/// Decode one refusal to its category plus the safe reason to show. The reason
/// is returned only when it is a known allowlisted value, never raw server text.
fn decode_refusal(status: u16, body: Option<&Value>) -> (Refusal, Option<String>) {
    let (code, reason) = refusal_fields(body);
    if matches!(
        code,
        Some("mutation_outcome_uncertain" | "invite_email_uncertain")
    ) {
        return (Refusal::MutationUncertain, code.map(str::to_owned));
    }
    let known_reason =
        reason.filter(|r| POLICY_REASONS.contains(r) || ENVELOPE_REASONS.contains(r));
    let category = match status {
        // Explicit post-effect uncertainty was handled above.
        500..=599 => Refusal::Availability,
        // A non-auth refusal keeps its safe code (for example `payload_too_large`
        // or a legacy `too_large`) so the caller sees what was refused. It is not
        // a login or availability claim.
        400 | 409 | 413 | 422 => Refusal::Unknown,
        401 | 403 => match (code, known_reason) {
            // A policy outcome proven at a gate is authorization, whatever the code.
            (_, Some(r)) if POLICY_REASONS.contains(&r) => Refusal::Authorization,
            (_, Some(_)) => Refusal::Uncertain, // envelope_invalid: not terminal, not policy
            (Some(c), None) if TERMINAL_CODES.contains(&c) => Refusal::Authentication,
            (Some("forbidden"), None) => Refusal::Authorization,
            // The container's path/scope refusal: authorization on 403 only.
            (Some(CONTAINER_SCOPE_CODE), None) if status == 403 => Refusal::Authorization,
            // A generic 403 with no safe code is still an access denial, never a
            // login or expiry claim.
            (None, None) if status == 403 => Refusal::Authorization,
            (Some("unauthorized"), None) | (None, None) => Refusal::Uncertain,
            _ => Refusal::Unknown,
        },
        _ => Refusal::Unknown,
    };
    // The shown reason: an allowlisted policy/envelope reason on 401/403, or the
    // safe `error.code` on a non-auth refusal (a payload-size or validation code).
    // Auth-class codes are never shown as a free reason.
    let shown = match status {
        400 | 409 | 413 | 422 => code.map(str::to_owned),
        403 if code == Some(CONTAINER_SCOPE_CODE) => Some(CONTAINER_SCOPE_CODE.to_owned()),
        _ => known_reason.map(str::to_owned),
    };
    (category, shown)
}

/// The next action for a decoded refusal. Login is named only for a terminal
/// session loss. Everything else is diagnosed read-only and never retried.
fn refusal_message(refusal: Refusal, reason: Option<&str>, status: u16) -> String {
    let code = reason.map_or(String::new(), |r| format!(", reason {r}"));
    let category = refusal.category();
    match refusal {
        Refusal::Authentication => format!(
            "the remote session ended ({category}, HTTP {status}{code}) — run `cadence login` again once"
        ),
        Refusal::Authorization => format!(
            "the remote refused this action as not permitted ({category}, HTTP {status}{code}); \
             a new login does not grant it — check the workspace role and target path (see \
             docs/REMOTE-AUTH.md, read-only diagnosis), then ask the owner. Nothing was retried"
        ),
        Refusal::Uncertain => format!(
            "the remote did not confirm why it refused the credential ({category}, HTTP {status}{code}); \
             it may or may not be expired — run the read-only diagnosis in docs/REMOTE-AUTH.md \
             before any login. Nothing was retried"
        ),
        Refusal::MutationUncertain => format!(
            "the remote action may already have committed ({category}, HTTP {status}{code}); \
             inspect `team list` or `team invites` with the same explicit --org before \
             repeating the mutation. Nothing was retried; saved credentials are kept"
        ),
        Refusal::Availability => format!(
            "the remote is unavailable ({category}, HTTP {status}{code}) — retry later; \
             saved credentials are kept"
        ),
        Refusal::Unknown => format!(
            "the remote refused the request ({category}, HTTP {status}{code}); it did not match \
             a known refusal, so nothing was retried"
        ),
    }
}

fn classify(status: u16, retry_after: Option<u64>, body: Option<Value>) -> Verdict {
    match status {
        200..=299 => Verdict::Done(body.ok_or_else(|| reject("remote answer was not JSON"))),
        503 => {
            let state = body
                .as_ref()
                .and_then(|b| b["state"].as_str())
                .unwrap_or_default();
            let advertised = body
                .as_ref()
                .and_then(|b| b["retry_after_s"].as_u64())
                .or(retry_after)
                .unwrap_or(5);
            match state {
                // Only an explicit `waking` reply may retry — a bare 503
                // (`board_unreachable`, `capability_unavailable`,
                // `runtime_unavailable`) is terminal, like `wake_failed`.
                "waking" => Verdict::Waking(Duration::from_secs(advertised).min(WAKE_WAIT_MAX)),
                "wake_failed" => Verdict::Done(Err(reject(format!(
                    "remote org is not running ({})",
                    body.and_then(|b| b["code"].as_str().map(str::to_string))
                        .unwrap_or_else(|| "wake_failed".into())
                )))),
                // CAD-1434: a bare 503 is availability, decoded the same as
                // every other refusal (no retry, saved login kept).
                _ => Verdict::Done(Err(refusal_error(status, body.as_ref()))),
            }
        }
        401 | 403 => Verdict::Done(Err(refusal_error(status, body.as_ref()))),
        s if (300..400).contains(&s) => Verdict::Done(Err(reject("remote redirect refused"))),
        409 => {
            // A checked conflict (`{conflict, current_rev, card, error}`
            // — the payload `write_reply` maps a stale `if_rev` to) is a
            // definite "not written" verdict, never a refusal of the
            // call itself and never retried. The body rides up so the
            // CLI can resync on the spot. Settings RPC uses the typed
            // `{ok:false,error:{code:"revision_conflict",message}}` shape.
            // Other 409 bodies remain generic refusals.
            match body {
                Some(b)
                    if b.get("conflict").is_some()
                        || b.pointer("/error/code").and_then(Value::as_str)
                            == Some("revision_conflict") =>
                {
                    Verdict::Conflict(b)
                }
                _ => Verdict::Done(Err(refusal_error(status, body.as_ref()))),
            }
        }
        // CAD-1434: 400/413/422/5xx and any other status use the same decoder,
        // so the safe code or availability category is shown, never a bare
        // generic message. None of these retries.
        _ => Verdict::Done(Err(refusal_error(status, body.as_ref()))),
    }
}

/// CAD-1434: the one place a non-success gateway answer becomes an error. It
/// decodes the safe category and reason, then names the next action.
fn refusal_error(status: u16, body: Option<&Value>) -> Error {
    let (refusal, reason) = decode_refusal(status, body);
    let mut text = refusal_message(refusal, reason.as_deref(), status);
    if let Some(correlation) = refusal_correlation(body) {
        text.push_str(&correlation);
    }
    if refusal == Refusal::MutationUncertain {
        Error::unknown(text)
    } else {
        reject(text)
    }
}

/// The answer to a mutating verb whose reply was ambiguous: the write may
/// already have committed, so the outcome is unknown and the caller
/// reconciles with the verb's own read before repeating it. It never advises
/// a bare retry, and it never claims the write failed. `correlation` is the
/// bounded board correlation the refusal may carry.
fn ambiguous_mutation_error(status: u16, correlation: Option<String>, reconcile: &str) -> Error {
    let mut text = format!(
        "the remote action may already have committed (outcome_unknown, HTTP {status}); \
         {reconcile}. Nothing was retried; saved credentials are kept"
    );
    if let Some(correlation) = correlation {
        text.push_str(&correlation);
    }
    Error::unknown(text)
}

/// A 2xx that cannot be a receipt for the write it answered — no JSON body at
/// all, or a body that is not an object. The call may have committed, so the
/// outcome is unknown: the caller reconciles with the verb's own read rather
/// than replaying the write or reading the empty answer as a success.
fn unusable_success_error(status: u16, reconcile: &str) -> Error {
    Error::unknown(format!(
        "the remote answered HTTP {status} without a usable success body — the outcome is \
         unknown; {reconcile}. Nothing was retried; saved credentials are kept"
    ))
}

/// A wiki upload whose request was sent but whose answer was lost, oversized
/// or unreadable: the blob may already be stored, so the outcome is unknown
/// and the caller lists the parent path before uploading again.
fn upload_outcome_unknown(detail: &str) -> Error {
    Error::unknown(format!(
        "{detail} — {RECONCILE_WIKI_UPLOAD}. Nothing was retried"
    ))
}

const PROFILE_WRITE_NOT_PERMITTED: &str = "defining or editing a profile is a live human \
    workspace owner's act, and this login is a member or not a user principal. If you are the \
    owner and this login may have ended, run `cadence login` once. Nothing was written";

/// CAD-1568: actionable text for a verb's typed refusals, keyed on the
/// container's bounded `error.code` — the raw `error.message` the route
/// returns is never shown (a `cli_fail` body can carry arbitrary server
/// text). Today it covers the `cli_message_*` family and CAD-1999's
/// `project_new` registry refusals. Unknown or absent codes keep the
/// generic decoded refusal unchanged.
fn message_family_refusal(verb: &str, status: u16, code: Option<&str>, base: Error) -> Error {
    let action: Option<(&'static str, &'static str)> = match (verb, code) {
        ("message_send", Some("recipient_class_unsupported")) => Some((
            "recipient_class_unsupported",
            "remote send reaches `master` for a human workspace owner only; \
             personal assistants require separate assistant authority, and local \
             workers use the local-assignment plane",
        )),
        ("message_send", Some("message_id_conflict")) => Some((
            "message_id_conflict",
            "that --message id was already used with different content (or \
             belongs to another member) — mint a fresh id; different content \
             under a reused id is a conflict, never a resubmit",
        )),
        ("message_send", Some("rejected")) => Some((
            "rejected",
            "the recipient or the --message id was refused — `sys-` ids are \
             daemon-owned; inspect the selected workspace's master and use a \
             non-reserved stable message id",
        )),
        ("message_send", Some("not_permitted")) => Some((
            "not_permitted",
            "remote `send master` requires a verified human workspace owner; \
             agent, service and ordinary-member credentials do not grant master chat",
        )),
        ("message_send", Some("insufficient_scope")) => Some((
            "insufficient_scope",
            "this remote login lacks the cli.write grant a send needs — re-login \
             with write scope, then retry",
        )),
        ("message_send", Some("invalid_request")) => Some((
            "invalid_request",
            "the send was refused at the request boundary — the remote call \
             needs a non-empty body and the stable --message id",
        )),
        ("message_read", Some("not_permitted")) | ("message_inbox", Some("not_permitted")) => {
            Some((
                "not_permitted",
                "remote message readback requires the sending human's identity; \
                 passive inbox inspection requires a human workspace owner",
            ))
        }
        ("issue_message_send", Some("message_id_conflict")) => Some((
            "message_id_conflict",
            "that --message id was already used with different content, on another ticket, \
             or by another member — pick a new id; nothing was stored",
        )),
        ("issue_message_send" | "issue_messages" | "issue_messages_read", Some("not_found")) => {
            Some((
                "not_found",
                "this workspace has no ticket with that id; nothing was read or changed",
            ))
        }
        // Not what today's relay sends, which refuses at mint (see `authorize`).
        ("issue_message_send" | "issue_messages_read", Some("insufficient_scope")) => Some((
            "insufficient_scope",
            "this remote login lacks the cli.write grant this needs — nothing was sent",
        )),
        // An older relay refuses a verb it does not know the same way, so this
        // cannot say which of the two happened.
        (
            "issue_message_send" | "issue_messages" | "issue_messages_read",
            Some("invalid_request"),
        ) => Some((
            "invalid_request",
            "the call was refused at the request boundary: a value was invalid, or this \
             workspace's hosted service predates ticket messages. Nothing was read or changed",
        )),
        ("message_read", Some("not_found")) => Some((
            "not_found",
            "no CLI message with that id is visible to this human identity; \
             reconcile the original workspace and login before retrying the same id",
        )),
        // CAD-2063: the tenant has no runtime to serve these, so a retry or a
        // new login cannot help.
        (
            "agent_show" | "message_inbox" | "master_status" | "settings_agents",
            Some("container_required"),
        ) => Some((
            "container_required",
            "a hosted workspace cannot serve this command. Retrying will not help, and \
             nothing was changed",
        )),
        // CAD-2101: the profile verbs' typed refusals. The code stays in the
        // text and the board's own message is not shown. (A stale revision is
        // not here: it is a conflict answer, which prints the board's body.)
        ("agent_profile_create" | "agent_profile_update", Some("not_permitted")) => {
            Some(("not_permitted", PROFILE_WRITE_NOT_PERMITTED))
        }
        ("agent_profile_create" | "agent_profile_update", Some("unauthorized")) => {
            Some(("unauthorized", PROFILE_WRITE_NOT_PERMITTED))
        }
        ("agent_profile_show", Some("not_permitted")) => Some((
            "not_permitted",
            "this login may not read that profile; nothing was read",
        )),
        ("agent_profile_show" | "agent_profile_update", Some("not_found")) => Some((
            "not_found",
            "this workspace has no profile with that id, or it is archived and only the \
             owner can read it; nothing was read or changed",
        )),
        ("agent_profile_create", Some("alias_taken")) => Some((
            "alias_taken",
            "a profile or a runtime already holds that alias; pick another. Nothing was written",
        )),
        (
            "agent_profile_show" | "agent_profile_create" | "agent_profile_update",
            Some("invalid_request"),
        ) => Some((
            "invalid_request",
            "the board refused the request: a value or field was not accepted (the board \
                 owns the field names and limits), or this workspace's board predates agent \
                 profile commands — if the values are right, update the board first. Nothing \
                 was written",
        )),
        // A write's 503 with no `state` is an unknown outcome (exit 1) before
        // it gets here, so only the read reaches this arm.
        ("agent_profile_show", Some("capability_unavailable")) => Some((
            "capability_unavailable",
            "the board could not reach its profile store; nothing was read. Try again later",
        )),
        // CAD-1999: the hosted project registry's own typed refusals. A key
        // that already exists as a ticket-only project conflicts exactly like
        // a registered one; only the identical key+name+prefix is a reuse.
        ("project_new", Some("project_exists")) => Some((
            "project_exists",
            "that project key is already registered, or already exists as its \
             tickets' project with a different name or prefix — `issue project \
             ls` shows the stored record; pick another key, or repeat the same \
             key, name and prefix to reuse it. Nothing was written",
        )),
        ("project_new", Some("prefix_conflict")) => Some((
            "prefix_conflict",
            "that issue-id prefix already belongs to another project (registered, \
             or spelled by existing ticket ids) — pass a different --prefix; \
             nothing was written",
        )),
        ("project_new", Some("not_permitted")) => Some((
            "not_permitted",
            "remote `project new` requires a verified human workspace owner; an \
             agent, service or ordinary-member credential cannot register \
             workspace structure",
        )),
        ("project_new", Some("invalid_request")) => Some((
            "invalid_request",
            "the project was refused at the request boundary — the key must be \
             1-32 lowercase letters, digits or hyphens, --name 1-200 bytes, and \
             --prefix 1-16 letters/digits starting with a letter; nothing was written",
        )),
        _ => None,
    };
    match action {
        Some((wire_code, text)) => {
            Error::invalid(wire_code, format!("{text} ({wire_code}, HTTP {status})"))
        }
        None => base,
    }
}

/// Run one allowlisted remote verb: a [`REMOTE_VERBS`] cli verb or a
/// [`WIKI_ACTIONS`] wiki action. The verb must be in one of them (a bug in
/// the caller is a local refusal, never a request). The credential is loaded and checked against `target` before
/// the first byte is sent; wake retries re-mint the envelope per attempt
/// and are bounded by `wake_timeout` seconds. Progress goes to `note`
/// (stderr at the call site); the returned Value is the remote's JSON body.
/// What one remote verb answered: `Ok` — the verb's JSON body (a read
/// result or a confirmed write), or `Conflict` — the checked write
/// conflict body (`{conflict, current_rev, card, error}`) the remote
/// answered instead of writing. A conflict is a verdict, not an
/// error: the call itself succeeded and proved nothing was written.
#[derive(Debug)]
pub enum RemoteAnswer {
    Ok(Value),
    Conflict(Value),
}

/// CAD-1434: local-only credential state for `org list`/`org inspect`. It reads
/// the stored credential with no network call and never returns a token. The
/// result is a claim about what this machine holds, not about live connectivity
/// or permission: `renewable` means the saved refresh session can be used on
/// the next command; the server still decides each operation.
pub fn local_status(target: &RemoteTarget) -> Value {
    match auth_dir(None) {
        Ok(dir) => local_status_at(&dir, target),
        Err(_) => json!({"state": "unavailable", "live_verified": false}),
    }
}

/// `local_status` with the credential dir injectable, so tests never touch the
/// real config directory. Only a missing file is absence. A permission or I/O
/// error is `unavailable`, never a claim that no credential exists.
fn local_status_at(dir: &Path, target: &RemoteTarget) -> Value {
    let path = dir.join(format!("cli-{}.json", target.org));
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return json!({"state": "no_credential", "live_verified": false});
        }
        Err(_) => {
            return json!({"state": "unavailable", "live_verified": false,
                "note": "the local credential store cannot be inspected; check its permissions"});
        }
    };
    let cred = match read_credential_metadata(dir, target) {
        Ok(cred) => cred,
        // Malformed, foreign or unreadable storage is a local problem, never
        // proof of absence or of server-side revocation.
        Err(_) => {
            return json!({"state": "invalid_local_credential", "live_verified": false,
                "note": "the saved credential cannot be read; run `cadence login` again"})
        }
    };
    let at = now().unwrap_or(0);
    let scopes: Vec<&str> = cred.capabilities.iter().map(String::as_str).collect();
    let mut out = json!({
        "scopes": scopes,
        "access_expires_at": cred.expires_at,
        "live_verified": false,
        // Local observation only: when this machine last wrote the credential
        // file. It is not a server renewal result and proves no health.
        "stored_at": meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs()),
        "observed_at": at,
    });
    // Session expiry is reported whether or not the session is still live, so
    // the reader sees exactly why a renewal is no longer possible.
    if let Some(session) = cred.cli_session.as_ref() {
        out["session_idle_expires_at"] = json!(session.idle_expires_at);
        out["session_absolute_expires_at"] = json!(session.absolute_expires_at);
        out["pending_renewal"] = json!(cred.pending.is_some());
    }
    let state = match cred.cli_session.as_ref() {
        Some(session) if session.idle_expires_at > at && session.absolute_expires_at > at => {
            "renewable"
        }
        Some(_) => "reauthentication_required",
        None if cred.expires_at > at => "access_valid_until_expiry",
        None => "reauthentication_required",
    };
    out["state"] = json!(state);
    out
}

/// The exact capability the published cloud-agent contract requires on the
/// saved hosted login. Kept separate from `cli.write`: the default login
/// never authorizes a paid model turn, and a login missing this scope is
/// refused before a request is built.
pub fn cloud_agent_credential(target: &RemoteTarget, scope: &str) -> Result<()> {
    cloud_agent_credential_owned(target, scope, None)
}

/// Scope AND full-owner check against the saved login. `owner` is the source
/// already admitted locally; the saved credential's verified issuer + subject
/// must still be exactly that parent. This runs immediately before every
/// transport call AND before serving any cached row, so a replaced human can
/// neither spend a new turn nor read an earlier owner's reply.
pub fn cloud_agent_credential_owned(
    target: &RemoteTarget,
    scope: &str,
    owner: Option<&crate::store::cloud_agent::Owner>,
) -> Result<()> {
    let dir = auth_dir(None).map_err(|_| {
        reject("remote credential directory is unavailable — set HOME or XDG_CONFIG_HOME")
    })?;
    let cred = load_credential(&dir, target)?;
    if !cred.capabilities.iter().any(|cap| cap == scope) {
        return Err(Error::gate_coded(
            "cloud_agent_scope_revoked",
            format!(
                "this remote login for org '{org}' does not carry the {scope} capability — \
                 nothing was sent. Run `cadence login {org} --cloud-agent` to add it to \
                 this login",
                org = target.org,
            ),
        ));
    }
    if let Some(owner) = owner {
        let pin = identity_of(&cred, target)?;
        if pin.issuer != owner.issuer || pin.subject_id != owner.subject {
            return Err(Error::gate_coded(
                "cloud_agent_owner_changed",
                "the saved cloud login now names a different verified owner (issuer/subject) \
                 than this source was admitted under; no request or cached reply is served",
            ));
        }
    }
    Ok(())
}

/// The saved login's verified logical owner for this exact target: the stable
/// issuer + subject pair, never a credential/token/session generation. A
/// legacy login without either half is refused rather than approximated.
pub fn cloud_agent_identity(target: &RemoteTarget) -> Result<CliIdentityPin> {
    let dir = auth_dir(None).map_err(|_| {
        reject("remote credential directory is unavailable — set HOME or XDG_CONFIG_HOME")
    })?;
    let cred = load_credential(&dir, target)?;
    identity_of(&cred, target)
}

fn identity_of(cred: &Credential, target: &RemoteTarget) -> Result<CliIdentityPin> {
    let issuer = cred
        .issuer
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            reject(format!(
                "the saved cloud login for org '{}' records no verified issuer — run \
                 `cadence login --cloud-agent` again; nothing was sent",
                target.org
            ))
        })?;
    let subject_id = cred
        .subject_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            reject(
                "the saved cloud login records no verified subject id — run \
                 `cadence login --cloud-agent` again; nothing was sent",
            )
        })?;
    if cred.organization_id != target.org_id || cred.audience != target.endpoint {
        return Err(reject(
            "the saved credential does not name this exact workspace; nothing was sent",
        ));
    }
    Ok(CliIdentityPin { subject_id, issuer })
}

/// CAD-1853: the owner-only record of one enrolled cloud-peer credential.
/// The token is never returned to a child, prompt, argv or log; this module
/// makes exactly one HTTP call with it and drops it.
const PEER_RECORD_VERSION: &str = "hosted-cadence-cloud-agent-peer.v1";

fn peer_dir(target: &RemoteTarget) -> Result<PathBuf> {
    let base = auth_dir(None).map_err(|_| {
        reject("remote credential directory is unavailable — set HOME or XDG_CONFIG_HOME")
    })?;
    let dir = base.join("cloud-agent");
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    let meta = fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(reject("cloud-agent credential directory is not private"));
    }
    let _ = target;
    Ok(dir)
}

fn peer_path(
    target: &RemoteTarget,
    owner: &crate::store::cloud_agent::Owner,
    uid: &str,
    epoch: i64,
) -> Result<PathBuf> {
    // The key includes BOTH halves of the verified parent identity and the
    // native sender uid/stamp: the same uid+stamp enrolled under another
    // human is a different credential, never a replacement of this one.
    let digest = Sha256::digest(
        format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            target.org_id, target.endpoint, owner.issuer, owner.subject, uid, epoch
        )
        .as_bytes(),
    );
    let name: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    Ok(peer_dir(target)?.join(format!("{name}.json")))
}

fn read_peer_record(path: &Path) -> Option<Value> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return None;
    }
    let mut raw = Vec::new();
    file.take(16385).read_to_end(&mut raw).ok()?;
    if raw.len() > 16384 {
        return None;
    }
    serde_json::from_slice(&raw).ok()
}

fn peer_records(dir: &Path) -> Result<Vec<(PathBuf, Value)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)?.take(64) {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if let Some(record) = read_peer_record(&path) {
            out.push((path, record));
        }
    }
    Ok(out)
}

fn peer_matches(record: &Value, target: &RemoteTarget, uid: &str, epoch: i64) -> bool {
    record["version"] == PEER_RECORD_VERSION
        && record["purpose"] == "cloud_agent"
        && record["organization_id"] == target.org_id
        && record["audience"] == target.endpoint
        && record["uid"] == uid
        && record["registration_epoch"] == epoch
}

fn peer_parent(record: &Value) -> Result<CliIdentityPin> {
    let issuer = record["issuer"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| reject("cloud-peer credential is missing its verified parent issuer"))?;
    let subject_id = record["parent_subject"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| reject("cloud-peer credential is missing its verified parent subject"))?;
    Ok(CliIdentityPin {
        subject_id: subject_id.to_string(),
        issuer: issuer.to_string(),
    })
}

/// Persist the enrolled child credential for exactly this source. The caller
/// has already validated the contract response; no secret is logged. Any
/// other-owner record for the same native uid/stamp is removed first: one
/// peer enrollment has exactly one verified parent, so resolution can never
/// silently pick between two owners.
pub fn cloud_agent_save_peer(
    target: &RemoteTarget,
    owner: &crate::store::cloud_agent::Owner,
    uid: &str,
    epoch: i64,
    record: &Value,
) -> Result<()> {
    let path = peer_path(target, owner, uid, epoch)?;
    for (other, existing) in peer_records(&peer_dir(target)?)? {
        if other != path && peer_matches(&existing, target, uid, epoch) {
            let _ = fs::remove_file(&other);
        }
    }
    let body = serde_json::to_vec(&json!({
        "version": PEER_RECORD_VERSION,
        "purpose": "cloud_agent",
        "organization_id": target.org_id,
        "audience": target.endpoint,
        "issuer": owner.issuer,
        "parent_subject": owner.subject,
        "uid": uid,
        "registration_epoch": epoch,
        "capabilities": ["cloud_agent.read", "cloud_agent.write"],
        "record": record,
    }))?;
    let tmp = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    file.write_all(&body)?;
    file.sync_all()?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Refuse a persisted cloud-peer child credential whose recorded
/// `expires_at` has passed. The platform would refuse that token, so nothing
/// is derived, sent or read from it; the operator must enroll the peer again.
fn peer_unexpired(record: &Value) -> Result<()> {
    let expires_at = record["record"]["expires_at"]
        .as_u64()
        .ok_or_else(|| reject("cloud-peer credential is missing its expiry"))?;
    if expires_at <= now()? {
        return Err(reject(
            "the enrolled cloud-peer credential has expired — the operator must enroll this \
             peer again",
        ));
    }
    Ok(())
}

/// Resolve the ONE verified parent recorded for this native source. Peers
/// never fall back to the current human login: their custody is this record.
pub fn cloud_agent_peer_owner(
    target: &RemoteTarget,
    uid: &str,
    epoch: i64,
) -> Result<CliIdentityPin> {
    let mut found: Option<CliIdentityPin> = None;
    for (_path, record) in peer_records(&peer_dir(target)?)? {
        if !peer_matches(&record, target, uid, epoch) {
            continue;
        }
        // The record IS this source's authority: once its recorded expiry has
        // passed the stored token cannot be used, so no source is derived from
        // it and nothing is traced upstream. Re-enrollment is the only exit.
        peer_unexpired(&record)?;
        let pin = peer_parent(&record)?;
        if found.as_ref().is_some_and(|seen| seen != &pin) {
            return Err(reject(
                "ambiguous cloud-peer enrollment for this native source; re-enroll it \
                 explicitly under the intended owner",
            ));
        }
        found = Some(pin);
    }
    found.ok_or_else(|| reject("no enrolled cloud-peer credential for this source"))
}

fn load_peer(
    target: &RemoteTarget,
    owner: &crate::store::cloud_agent::Owner,
    uid: &str,
    epoch: i64,
    scope: Option<&str>,
) -> Result<String> {
    let path = peer_path(target, owner, uid, epoch)?;
    let record = read_peer_record(&path)
        .ok_or_else(|| reject("no enrolled cloud-peer credential for this source"))?;
    if !peer_matches(&record, target, uid, epoch) {
        return Err(reject("cloud-peer credential does not match this source"));
    }
    let parent = peer_parent(&record)?;
    if parent.issuer != owner.issuer || parent.subject_id != owner.subject {
        return Err(reject(
            "cloud-peer credential belongs to a different verified parent owner",
        ));
    }
    // A persisted child credential is only usable while it is unexpired: the
    // platform would refuse an expired token, so this peer's cloud authority
    // ends at its recorded `expires_at` instead of being traced upstream.
    peer_unexpired(&record)?;
    let scopes = record["capabilities"]
        .as_array()
        .ok_or_else(|| reject("cloud-peer credential is missing its granted scopes"))?;
    if scopes.len() != 2
        || !["cloud_agent.read", "cloud_agent.write"]
            .iter()
            .all(|scope| scopes.iter().any(|value| value.as_str() == Some(*scope)))
    {
        return Err(reject(
            "cloud-peer credential does not carry exactly the cloud-agent scopes",
        ));
    }
    if let Some(scope) = scope {
        if !scopes.iter().any(|value| value.as_str() == Some(scope)) {
            return Err(Error::gate_coded(
                "cloud_agent_scope_revoked",
                "the enrolled cloud-peer credential does not carry this action's scope",
            ));
        }
    }
    record["record"]["token"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| reject("cloud-peer credential is missing its token"))
}

/// POST the bridge enrollment call with the saved bridge login (never a
/// service token). Only the cloud-purpose body is built by the caller.
pub fn cloud_agent_enroll_post(
    target: &RemoteTarget,
    body: &Value,
) -> Result<(u16, Option<Value>, CliIdentityPin)> {
    let dir = auth_dir(None).map_err(|_| reject("remote credential directory unavailable"))?;
    // ONE credential snapshot: the same verified owner and token that prove
    // the parent also perform the enrollment POST, so a login replaced in
    // between cannot enroll a child under a different human. Renewal is
    // cloud-purpose: a cloud-only grant refreshes under its own consent, and
    // a refusal here is definite (nothing was sent), never `unknown`.
    let cred = load_credential(&dir, target)?;
    let cred = cloud_send_credential(&dir, target, cred).map_err(cloud_renewal_refused)?;
    let pin = identity_of(&cred, target)?;
    let issuer = pin.issuer.trim_end_matches('/').to_string();
    post(
        &issuer,
        "/v1/hosted-cadence/enroll",
        &cred.access_token,
        body,
        Duration::from_secs(20),
    )
    .map(|(status, _, body)| (status, body, pin))
    .map_err(|_| Error::unknown("cloud-peer enrollment transport failed; outcome unknown"))
}

/// Whether this exact source has a private enrolled peer credential under
/// this verified parent. The token is loaded only to validate the record and
/// immediately dropped.
pub fn cloud_agent_peer_ready(
    target: &RemoteTarget,
    owner: &crate::store::cloud_agent::Owner,
    uid: &str,
    epoch: i64,
    scope: &str,
) -> Result<()> {
    load_peer(target, owner, uid, epoch, Some(scope)).map(|_| ())
}

/// One contract POST authenticated by an enrolled peer's own credential.
/// There is no fallback to the current human login: a peer whose own custody
/// is missing or was re-enrolled under another owner is refused here.
pub fn cloud_agent_peer_post(
    target: &RemoteTarget,
    owner: &crate::store::cloud_agent::Owner,
    uid: &str,
    epoch: i64,
    path: &str,
    body: &Value,
    scope: &str,
) -> Result<(u16, Option<Value>)> {
    if !path.starts_with("/__platform/") || path.contains(['\\', '#', '?']) {
        return Err(reject("cloud-agent contract path is invalid"));
    }
    let token = load_peer(target, owner, uid, epoch, Some(scope))?;
    let origin = target.endpoint.trim_end_matches('/');
    post(origin, path, &token, body, Duration::from_secs(20))
        .map(|(status, _, body)| (status, body))
        .map_err(|_| Error::unknown("cloud-peer transport failed; outcome uncertain"))
}

/// One bounded POST for the published cloud-agent contract. The saved
/// credential is loaded, checked against the exact stored org/audience and
/// never returned; only the HTTP status and parsed JSON leave this function.
/// The caller supplies the contract path (a single published constant) and
/// the credential is never placed in a prompt, argument or response.
pub fn cloud_agent_post(
    target: &RemoteTarget,
    path: &str,
    body: &Value,
    scope: &str,
) -> Result<(u16, Option<Value>)> {
    if !path.starts_with("/__platform/") || path.contains(['\\', '#', '?']) || path.len() > 200 {
        return Err(reject("cloud-agent contract path is invalid"));
    }
    let dir = auth_dir(None).map_err(|_| {
        reject("remote credential directory is unavailable — set HOME or XDG_CONFIG_HOME")
    })?;
    // The scope refusal is decided on the saved login before custody touches
    // the issuer, so a narrowed grant is a revoked scope (nothing sent), not
    // a failed renewal. The target pins the verified owner, so a login
    // replaced between the daemon's early check and this request is refused
    // here instead of being charged under the new human.
    let cred = load_credential(&dir, target)?;
    if !cred.capabilities.iter().any(|cap| cap == scope) {
        return Err(Error::gate_coded(
            "cloud_agent_scope_revoked",
            "this saved login does not carry the action's cloud-agent scope",
        ));
    }
    // CAD-1853: the access token lives 300 s, so custody rotates the saved
    // cloud consent here, under its own purpose and the same source lock,
    // whenever renewal is due. The action's scope was just proven on this
    // login, and renewal can only return the identical capability set, so the
    // token sent is admitted for the same action. A cloud-only grant never
    // reaches the CLI purpose.
    let cred = cloud_send_credential(&dir, target, cred).map_err(|error| {
        if scope == crate::cloud_agent::SCOPE_READ {
            cloud_renewal_withheld(error)
        } else {
            cloud_renewal_refused(error)
        }
    })?;
    // The saved audience is an origin without a trailing slash; normalize so
    // one slash joins it to the contract path (never `//__platform/...`).
    let origin = target.endpoint.trim_end_matches('/');
    post(
        origin,
        path,
        &cred.access_token,
        body,
        Duration::from_secs(20),
    )
    .map(|(status, _, body)| (status, body))
    .map_err(|error| match error {
        // A transport failure may happen after the request reached the
        // server (a queued turn). Never a rejection and never a retry.
        Error::Internal(_) | Error::Rejected(_) => Error::unknown(
            "cloud-agent transport failed before an answer — the remote outcome is unknown",
        ),
        other => other,
    })
}

/// Revoke one remote device/workspace session before removing its protected
/// local authority. A failed or offline issuer request leaves the record intact.
pub fn logout(dir: &Path, target: &RemoteTarget) -> Result<i32> {
    let deadline = Instant::now() + RENEWAL_WAIT;
    let _lock = private_lock(dir, &target.org, deadline)?;
    let cred = load_credential(dir, target)?;
    let session = cred.cli_session.as_ref().ok_or_else(|| {
        reject("no renewable CLI session for this workspace — run `cadence login` once")
    })?;
    let issuer = cred
        .issuer
        .as_deref()
        .ok_or_else(|| reject("renewable session issuer missing"))?;
    let unknown = || {
        Error::unknown(format!(
            "remote revocation of session {} for org {} is unknown; local authority is retained. \
             Reconcile by retrying `cadence auth logout --org {}` with this same saved session",
            session.session_id, target.org, target.org
        ))
    };
    let (status, body) = crate::remote_enrollment::cli_session_post(
        dir,
        issuer,
        "/v1/hosted-cadence/cli-session/revoke",
        &session.refresh_token,
        serde_json::json!({"version":SESSION_VERSION,"session_id":session.session_id}),
        deadline.min(Instant::now() + RENEWAL_WAIT),
    )
    .map_err(|error| {
        if error.kind() == "unknown" {
            unknown()
        } else {
            error
        }
    })?;
    if matches!(status, 400 | 401 | 403) {
        return Err(reject(
            "remote device revocation was refused; local authority retained",
        ));
    }
    if status != 200
        || !exact_object(&body, &["version", "status", "session_id"])
        || body["version"] != SESSION_VERSION
        || body["status"] != "revoked"
        || body["session_id"] != session.session_id
    {
        return Err(unknown());
    }
    let path = dir.join(format!("cli-{}.json", target.org));
    let meta = fs::symlink_metadata(&path)?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(reject(
            "remote credential changed during logout; local authority retained",
        ));
    }
    fs::remove_file(path)?;
    File::open(dir)?.sync_all()?;
    println!("Remote CLI session revoked for {}", target.org);
    Ok(0)
}

pub struct CliMaterial {
    pub access_token: String,
    pub subject_id: Option<String>,
    pub issuer: Option<String>,
}

/// The live hosted CLI bearer for `target`, renewed when it is due.
/// The subject and issuer come from the stored login, not from the caller.
pub fn cli_material(dir: &Path, target: &RemoteTarget) -> Result<CliMaterial> {
    cli_material_until(dir, target, Instant::now() + Duration::from_secs(20))
}

/// Same saved-login renewal as [`cli_material`], bounded by the caller's
/// total deadline. The deadline covers lock, refresh, and return; it does
/// not start a second clock.
pub fn cli_material_until(
    dir: &Path,
    target: &RemoteTarget,
    deadline: Instant,
) -> Result<CliMaterial> {
    let cred = renewed_credential(dir, target, deadline)?;
    if !valid_token(&cred.access_token, "hct_") {
        return Err(reject("invalid remote credential"));
    }
    Ok(CliMaterial {
        access_token: cred.access_token,
        subject_id: cred.subject_id,
        issuer: cred.issuer,
    })
}

/// Execute within the command-owned deadline. Continuations reuse it rather
/// than starting a fresh wake budget for each range.
pub fn call_verb(
    target: &RemoteTarget,
    verb: &str,
    arguments: Map<String, Value>,
    deadline: Instant,
    wake_timeout: u64,
    note: impl Fn(&str),
) -> Result<RemoteAnswer> {
    let dir = auth_dir(None).map_err(|_| {
        reject("remote credential directory is unavailable — set HOME or XDG_CONFIG_HOME")
    })?;
    call_verb_until(
        target,
        verb,
        arguments,
        deadline,
        wake_timeout,
        &dir,
        note,
        std::thread::sleep,
    )
}

/// `call_verb` with the credential dir and the sleep injectable — tests
/// drive the wake loop without wall-clock cost and point the credential
/// dir at a fixture. The verb allowlist is checked *here*, on the shared
/// path, so no caller can bypass it.
#[cfg(test)]
fn call_verb_in(
    target: &RemoteTarget,
    verb: &str,
    arguments: Map<String, Value>,
    wake_timeout: u64,
    dir: &Path,
    note: impl Fn(&str),
    sleep: impl Fn(Duration),
) -> Result<RemoteAnswer> {
    // I2: an unlisted verb is refused before a credential is even loaded.
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(wake_timeout))
        .ok_or_else(|| reject("invalid wake timeout"))?;
    call_verb_until(
        target,
        verb,
        arguments,
        deadline,
        wake_timeout,
        dir,
        note,
        sleep,
    )
}

#[allow(clippy::too_many_arguments)]
fn call_verb_until(
    target: &RemoteTarget,
    verb: &str,
    arguments: Map<String, Value>,
    deadline: Instant,
    wake_timeout: u64,
    dir: &Path,
    note: impl Fn(&str),
    sleep: impl Fn(Duration),
) -> Result<RemoteAnswer> {
    call_verb_until_retry(
        target,
        verb,
        arguments,
        deadline,
        wake_timeout,
        dir,
        true,
        note,
        sleep,
    )
}

/// One remote verb under an existing deadline. `retry_wake` is false for
/// wiki transfer writes and `job_status`: a busy or unknown result is not
/// replayed. Reads and enumerates keep the existing wake loop.
#[allow(clippy::too_many_arguments)]
pub(crate) fn call_bounded(
    target: &RemoteTarget,
    verb: &str,
    arguments: Map<String, Value>,
    deadline: Instant,
    wake_timeout: u64,
    retry_wake: bool,
    note: impl Fn(&str),
) -> Result<RemoteAnswer> {
    let dir = auth_dir(None).map_err(|_| {
        reject("remote credential directory is unavailable — set HOME or XDG_CONFIG_HOME")
    })?;
    call_verb_until_retry(
        target,
        verb,
        arguments,
        deadline,
        wake_timeout,
        &dir,
        retry_wake,
        note,
        std::thread::sleep,
    )
}

/// CAD-2101: a profile write's 2xx must carry a `profile` object with a
/// profile id (the named one, for an update) and a numeric revision.
fn profile_receipt_correlates(verb: &str, receipt: &Value, arguments: &Map<String, Value>) -> bool {
    let Some(profile) = receipt.get("profile") else {
        return false;
    };
    let id = profile.get("id").and_then(Value::as_str);
    let named = if verb == "agent_profile_update" {
        id.is_some() && id == arguments.get("id").and_then(Value::as_str)
    } else {
        id.is_some_and(|id| !id.is_empty())
    };
    named && profile.get("revision").is_some_and(Value::is_u64)
}

/// CAD-2101: a tenant board that predates the profile verbs answers
/// `501 not_implemented` before it reads anything. Say to update the board,
/// never that coordination is unavailable. A newer tenant behind an older
/// control answers a bare `400 invalid_request`, which a bad value gets too,
/// so that case is left to the verb's own refusal text.
fn profile_board_too_old(verb: &str, status: u16) -> Option<Error> {
    if !verb.starts_with("agent_profile_") || status != 501 {
        return None;
    }
    let nothing = if mutating_cli_verb(verb) {
        "Nothing was written"
    } else {
        "Nothing was read"
    };
    Some(Error::invalid(
        "not_implemented",
        format!(
            "this workspace's board predates `{verb}` (HTTP {status}, not_implemented) — update \
             the board first, then use this client. {nothing}"
        ),
    ))
}

/// Whether a ticket message write's 2xx names the write that was asked for.
/// The ticket id can come back in its stored case.
fn ticket_receipt_correlates(verb: &str, receipt: &Value, arguments: &Map<String, Value>) -> bool {
    let sent = |key: &str| arguments.get(key).and_then(Value::as_str);
    let got = |key: &str| receipt.get(key).and_then(Value::as_str);
    let same_ticket = sent("id")
        .zip(got("id"))
        .is_some_and(|(sent, got)| sent.eq_ignore_ascii_case(got));
    same_ticket
        && if verb == "issue_message_send" {
            sent("message").is_some()
                && got("message") == sent("message")
                && got("to") == sent("to")
                && got("state") == Some("stored")
        } else {
            sent("role").is_some()
                && got("role") == sent("role")
                && receipt.get("read_through").is_some_and(Value::is_u64)
        }
}

/// CAD-1568 revision: a `message_send` 2xx is a usable receipt only when it
/// carries the complete correlated enqueue binding:
///
/// - `message` — the caller-chosen stable id echoed exactly (the
///   reconciliation key a lost reply is retried under);
/// - `state` — a known lifecycle state and `duplicate` — a real bool;
/// - `alias` — the requested canonical recipient echoed exactly (only
///   `master` is ever requested);
/// - `workspace_id` — the immutable organization id for this destination
///   (the `org_id` pinned at login, never the registry slug);
/// - `sender_subject` — the authenticated human's canonical subject; when
///   the saved login recorded one it must be exactly that subject;
/// - `sender` — the retained `cli-<handle>` display attribution, nonempty;
/// - `conversation` — the persisted message_conversation Home thread id
///   the enqueue fixed, nonempty (returned again on duplicates/readback).
///
/// Anything less cannot establish that THIS send landed as THIS caller's
/// message in the target workspace — the outcome is unknown and is
/// reconciled by the stable id, never called a success or replayed.
fn send_receipt_correlates(
    receipt: &Value,
    arguments: &Map<String, Value>,
    target: &RemoteTarget,
    cred: &Credential,
) -> bool {
    let bound = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|expected| {
                !expected.is_empty() && receipt.get(key).and_then(Value::as_str) == Some(expected)
            })
    };
    bound("message")
        && bound("alias")
        && matches!(
            receipt.get("state").and_then(Value::as_str),
            Some(
                "queued"
                    | "submitting"
                    | "running"
                    | "completed"
                    | "failed"
                    | "cancelled"
                    | "unknown"
            )
        )
        && receipt.get("duplicate").and_then(Value::as_bool).is_some()
        && receipt.get("workspace_id").and_then(Value::as_str) == Some(target.org_id.as_str())
        && receipt
            .get("sender_subject")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty() && cred.subject_id.as_deref().is_none_or(|sub| sub == s))
        && receipt
            .get("sender")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
        && receipt
            .get("conversation")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
}

/// A hosted issue-id prefix as the registry stores it: 1-16 ASCII
/// letters/digits starting with a letter (the board normalizes case, so the
/// shape is what matters; the caller's own spelling is compared
/// case-insensitively).
fn valid_hosted_prefix(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 16
        && value.starts_with(|c: char| c.is_ascii_alphabetic())
        && value.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// CAD-1999: a `project_new` 2xx is usable only when it carries the
/// requested project back — a `project` object whose `key` and `name` are
/// exactly the ones sent, and whose `prefix` is a valid stored issue-id
/// prefix that matches the requested `--prefix` case-insensitively (when
/// one was sent). Anything less (a `null` or `{ok:true}` body, a foreign
/// project, a missing/invalid prefix or another project's prefix) cannot
/// establish that THIS create landed under the intended namespace, so the
/// outcome is unknown and is reconciled with `issue project ls`, never
/// called a failure and never replayed blindly.
fn project_receipt_correlates(receipt: &Value, arguments: &Map<String, Value>) -> bool {
    let project = receipt.get("project").and_then(Value::as_object);
    let bound = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|expected| {
                !expected.is_empty()
                    && project
                        .and_then(|fields| fields.get(key))
                        .and_then(Value::as_str)
                        == Some(expected)
            })
    };
    let prefix_ok = project
        .and_then(|fields| fields.get("prefix"))
        .and_then(Value::as_str)
        .is_some_and(|actual| {
            valid_hosted_prefix(actual)
                && arguments
                    .get("prefix")
                    .and_then(Value::as_str)
                    .is_none_or(|requested| actual.eq_ignore_ascii_case(requested))
        });
    project.is_some() && bound("key") && bound("name") && prefix_ok
}

#[allow(clippy::too_many_arguments)]
fn call_verb_until_retry(
    target: &RemoteTarget,
    verb: &str,
    arguments: Map<String, Value>,
    deadline: Instant,
    wake_timeout: u64,
    dir: &Path,
    retry_wake: bool,
    note: impl Fn(&str),
    sleep: impl Fn(Duration),
) -> Result<RemoteAnswer> {
    // I2: an unlisted verb is refused before a credential is even loaded.
    let surface = if REMOTE_VERBS.contains(&verb) {
        Surface::Cli
    } else if WIKI_ACTIONS.contains(&verb) {
        Surface::Wiki
    } else {
        return Err(reject(format!(
            "verb '{verb}' is not in the remote allowlist — never sent"
        )));
    };
    if matches!(surface, Surface::Wiki) {
        let allowed = wiki_argument_keys(verb);
        if let Some(key) = arguments.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(reject(format!(
                "wiki {verb} argument '{key}' is not in the allowlist — never sent"
            )));
        }
        wiki_transfer_shape(verb, &arguments)?;
    }
    team_argument_shape(verb, &arguments)?;
    // What this call can change, and the read that reconciles an answer which
    // does not prove the effect. Fixed for the whole call (the wake retry only
    // ever follows a pre-forward `waking` verdict), so a lost or unusable
    // answer is classified once, the same way on every surface.
    let effect = call_effect(surface, verb, &arguments);
    let budget_over = || -> Error {
        Error::busy_coded(
            "waking",
            format!(
                "org '{}' is still waking after {}s — retry, or pass a \
                 larger --wake-timeout",
                target.org, wake_timeout
            ),
        )
    };
    if wake_timeout == 0 {
        return Err(budget_over());
    }
    loop {
        request_timeout(deadline, AUTH_TIMEOUT)?;
        let cred = renewed_credential(dir, target, deadline)?;
        // A login that records its capabilities must hold the wiki scope the
        // action needs; a legacy login with none is left to the server's check.
        if matches!(surface, Surface::Wiki) && !cred.capabilities.is_empty() {
            let scope = if verb == "write" {
                "wiki.write"
            } else {
                "wiki.read"
            };
            if !cred.capabilities.iter().any(|c| c == scope) {
                return Err(reject(format!(
                    "wiki {verb} needs the {scope} capability, which this remote login \
                     lacks — nothing was sent. CLI commands are unaffected. `cadence login` \
                     does not request wiki access by default; wiki access needs a login the \
                     workspace owner grants with {scope}"
                )));
            }
        }
        // CAD-1586: `settings_agents` needs the operation's narrow
        // settings.* scope; `master_status` needs settings.read. A login
        // without the needed scope is refused here, before a request is
        // built — never downgraded to another verb or the local org.
        if verb == REMOTE_SETTINGS_VERB && !cred.capabilities.is_empty() {
            let operation = arguments
                .get("operation")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let scope = settings_op_scope(operation);
            if !cred.capabilities.iter().any(|c| c == scope) {
                return Err(reject(format!(
                    "`settings agents` operation '{operation}' needs the {scope} \
                     capability, which this remote login lacks — nothing was sent. \
                     `cadence login` does not request settings access by default; \
                     settings access needs a login the workspace owner grants with {scope}"
                )));
            }
        }
        if verb == REMOTE_MASTER_STATUS_VERB
            && !cred.capabilities.is_empty()
            && !cred.capabilities.iter().any(|c| c == "settings.read")
        {
            return Err(reject(
                "`master status` needs the settings.read capability, which this \
                 remote login lacks — nothing was sent. `cadence login` does not \
                 request settings access by default; it needs a login the workspace \
                 owner grants with settings.read",
            ));
        }
        if Instant::now() >= deadline {
            return Err(Error::busy_coded(
                "timeout",
                "remote command deadline elapsed during session renewal",
            ));
        }
        let (status, retry_after, body) =
            call_once(target, &cred, surface, verb, &arguments, deadline)?;
        if effect.is_mutation() {
            if let Some(code) = explicit_mutation_uncertainty(body.as_ref()) {
                return Err(Error::unknown(format!(
                    "the remote action may already have committed \
                     (outcome_unknown, HTTP {status}, reason {code}); {}. \
                     Nothing was retried; saved credentials are kept",
                    effect.reconciliation().unwrap_or(RECONCILE_UNKNOWN)
                )));
            }
        }
        let platform_operation = (verb == "platform_call")
            .then(|| arguments.get("operation").and_then(Value::as_str))
            .flatten();
        if (200..300).contains(&status) {
            match platform_operation {
                Some("call")
                    if !body.as_ref().is_some_and(|answer| {
                        answer.get("ok").and_then(Value::as_bool) == Some(true)
                            && answer.get("data").is_some_and(Value::is_object)
                    }) =>
                {
                    return Err(Error::unknown(format!(
                        "remote tool returned no usable receipt — {}",
                        effect.reconciliation().unwrap_or(RECONCILE_UNKNOWN)
                    )));
                }
                Some("tools")
                    if !body
                        .as_ref()
                        .and_then(|answer| answer.get("tools"))
                        .is_some_and(Value::is_array) =>
                {
                    return Err(reject("the remote tool catalog is unavailable"));
                }
                _ => {}
            }
        }
        // A successful status without the complete, correlated enqueue
        // receipt cannot establish whether — or as whose message, in which
        // workspace and conversation — the mutation landed. Unknown, never
        // a rejection and never a false success to replay blindly.
        if verb == "message_send"
            && (200..300).contains(&status)
            && !body
                .as_ref()
                .is_some_and(|receipt| send_receipt_correlates(receipt, &arguments, target, &cred))
        {
            return Err(Error::unknown(
                "remote send returned no usable correlated receipt — enqueue outcome is unknown",
            ));
        }
        // A 2xx that does not echo this write is not evidence it landed. Reading
        // it as a refusal invites a new --message and a second stored copy, and
        // reading it as success claims something nobody saw.
        if matches!(verb, "issue_message_send" | "issue_messages_read")
            && (200..300).contains(&status)
            && !body
                .as_ref()
                .is_some_and(|receipt| ticket_receipt_correlates(verb, receipt, &arguments))
        {
            return Err(Error::unknown(format!(
                "the remote answered without a matching receipt — {}",
                mutation_reconcile(verb).unwrap_or(RECONCILE_UNKNOWN)
            )));
        }
        if verb == "issue_attach" && (200..300).contains(&status) {
            let expected_id = arguments.get("id").and_then(Value::as_str);
            let expected_name = arguments
                .get("name")
                .and_then(Value::as_str)
                .map(|name| format!("hosted:{name}"));
            let encoded = arguments.get("data").and_then(Value::as_str);
            let expected_size = encoded.filter(|data| data.len() % 4 == 0).and_then(|data| {
                (data.len() / 4 * 3)
                    .checked_sub(data.bytes().rev().take_while(|byte| *byte == b'=').count())
            });
            let correlated = body.as_ref().is_some_and(|reply| {
                reply
                    .get("id")
                    .and_then(Value::as_str)
                    .zip(expected_id)
                    .is_some_and(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
                    && reply.get("artifact").and_then(Value::as_str) == expected_name.as_deref()
                    && reply.get("size").and_then(Value::as_u64)
                        == expected_size.map(|size| size as u64)
                    && expected_id.is_some()
                    && expected_name.is_some()
                    && expected_size.is_some()
            });
            if !correlated {
                return Err(Error::unknown(
                    "remote attachment returned no correlated receipt — outcome is unknown",
                ));
            }
        }
        if verb == "issue_review" && (200..300).contains(&status) {
            // The committed verdict must be the one this call asked for: same
            // artifact, and the digest the tenant verified from storage. A
            // reply that names anything else is not evidence this verdict
            // landed, so the outcome stays unknown rather than becoming a
            // refusal.
            let correlated = arguments
                .get("artifact")
                .and_then(Value::as_str)
                .zip(arguments.get("digest").and_then(Value::as_str))
                .zip(arguments.get("decision").and_then(Value::as_str))
                .is_some_and(|((artifact, digest), decision)| {
                    body.as_ref()
                        .and_then(|reply| reply.get("review"))
                        .is_some_and(|review| {
                            review.get("artifact").and_then(Value::as_str) == Some(artifact)
                                && review.get("digest").and_then(Value::as_str) == Some(digest)
                                && review.get("decision").and_then(Value::as_str) == Some(decision)
                        })
                });
            if !correlated {
                return Err(Error::unknown(
                    "remote review returned no correlated receipt — outcome is unknown",
                ));
            }
        }
        // A 2xx is a success only when it carries a JSON object: an absent,
        // null or non-object body cannot be a receipt for the write it
        // answered. The call may have committed, so the outcome is unknown —
        // never a false success, never a rejection, never a replay. (The
        // verbs with a required correlated receipt check their own shape
        // above, before this.)
        if effect.is_mutation() && (200..300).contains(&status) && !is_object_body(body.as_ref()) {
            return Err(unusable_success_error(
                status,
                effect.reconciliation().unwrap_or(RECONCILE_UNKNOWN),
            ));
        }
        // A 2xx object is a receipt only when it names what this call wrote —
        // the tenant's own reply contract, mirrored by the native route. A
        // malformed or foreign receipt cannot prove the write landed, so it is
        // the same unknown outcome a lost answer is, never a false success.
        let receipt_missing = effect.is_mutation()
            && (200..300).contains(&status)
            && body.as_ref().is_some_and(|reply| match verb {
                "issue_new" | "issue_comment" | "issue_set" => {
                    !issue_receipt_correlates(verb, reply, &arguments)
                }
                "project_policy" => !policy_receipt_correlates(reply, &arguments),
                "project_new" => !project_receipt_correlates(reply, &arguments),
                "agent_profile_create" | "agent_profile_update" => {
                    !profile_receipt_correlates(verb, reply, &arguments)
                }
                "team_invite" | "team_revoke_invite" | "team_set_role" | "team_remove" => {
                    !team_receipt_correlates(verb, status, reply, &arguments)
                }
                REMOTE_SETTINGS_VERB => !settings_receipt_correlates(
                    arguments
                        .get("operation")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    reply,
                    &arguments,
                ),
                "write" if matches!(surface, Surface::Wiki) && arguments.contains_key("text") => {
                    !wiki_write_receipt_correlates(reply, &arguments)
                }
                _ => false,
            });
        if receipt_missing {
            return Err(Error::unknown(format!(
                "remote `{verb}` returned no correlated receipt — outcome is unknown; {}",
                effect.reconciliation().unwrap_or(RECONCILE_UNKNOWN)
            )));
        }
        let refusal_code = refusal_fields(body.as_ref()).0.map(str::to_owned);
        // A 5xx with no pre-forward proof may come from the platform *after*
        // the mutation was attempted: the write may have landed behind an
        // unanswerable reply, so it is an unknown outcome, never availability
        // ("retry later") and never a refusal. Only a documented waking
        // response, the board's gate refusal, a CLI tracker refusal or a
        // 501 not-implemented are treated as pre-effect.
        let after_forward = effect.is_mutation()
            && post_forward_failure(status, body.as_ref(), matches!(surface, Surface::Cli));
        // A funded platform call may still be in progress or uncertain on
        // a same-key replay; preserve its dedicated reconciliation guidance.
        let platform_outcome_unknown = platform_operation == Some("call")
            && !board_gate_refused(body.as_ref())
            && !tracker_unavailable_before_verb(status, body.as_ref())
            && ((status == 409
                && matches!(
                    refusal_code.as_deref(),
                    Some("uncertain" | "idempotency_in_progress")
                ))
                || (status >= 500
                    && status != 501
                    && body.as_ref().and_then(|b| b["state"].as_str()).is_none()));
        let unknown_correlation = refusal_correlation(body.as_ref());
        // CAD-2101: a board without the profile verbs answers 501; say to
        // update it, not that coordination is unavailable.
        if let Some(error) = profile_board_too_old(verb, status) {
            return Err(error);
        }
        // CAD-2001: a project-scoped gate verb the tenant cannot serve is a
        // release-order problem, not an availability blip and not "no
        // evidence". Report it before the generic decoder can call it
        // retryable. A checked write conflict keeps its own verdict: the call
        // ran and proved nothing was written.
        if status == 501
            && matches!(
                verb,
                "issue_message_send" | "issue_messages" | "issue_messages_read"
            )
        {
            let nothing = if verb == "issue_messages" {
                "read"
            } else {
                "changed"
            };
            let command = match verb {
                "issue_message_send" => "issue say",
                "issue_messages_read" => "issue messages-read",
                _ => "issue messages",
            };
            return Err(Error::invalid(
                "not_implemented",
                artifact_tenant_too_old(command, status, nothing),
            ));
        }
        if status == 501 && matches!(verb, "artifact_find" | "artifact_show" | "artifact_read") {
            let verb = verb.replacen('_', " ", 1);
            return Err(reject(artifact_tenant_too_old(&verb, status, "read")));
        }
        if PROJECT_SCOPED_GATE_VERBS.contains(&verb) {
            let checked_conflict = body.as_ref().is_some_and(|b| {
                b.get("conflict").is_some()
                    || b.pointer("/error/code").and_then(Value::as_str) == Some("revision_conflict")
            });
            if !checked_conflict {
                if let Some(why) = tenant_cannot_serve(verb, status, body.as_ref()) {
                    return Err(reject(why));
                }
            }
        }
        match classify(status, retry_after, body) {
            Verdict::Done(Err(_e)) if platform_outcome_unknown => {
                return Err(Error::unknown(format!(
                    "remote platform call returned HTTP {status} — {}",
                    effect.reconciliation().unwrap_or(RECONCILE_UNKNOWN)
                )));
            }
            Verdict::Done(Err(_e)) if after_forward => {
                return Err(ambiguous_mutation_error(
                    status,
                    unknown_correlation,
                    effect.reconciliation().unwrap_or(RECONCILE_UNKNOWN),
                ));
            }
            Verdict::Done(Err(e)) => {
                return Err(message_family_refusal(
                    verb,
                    status,
                    refusal_code.as_deref(),
                    e,
                ));
            }
            Verdict::Done(result) => return result.map(RemoteAnswer::Ok),
            Verdict::Conflict(body) => return Ok(RemoteAnswer::Conflict(body)),
            Verdict::Waking(wait) => {
                if !retry_wake {
                    return Err(budget_over());
                }
                let now = Instant::now();
                let remaining = deadline
                    .checked_duration_since(now)
                    .ok_or_else(budget_over)?;
                if wait >= remaining {
                    return Err(budget_over());
                }
                note(&format!(
                    "Starting {}… waiting for it to become ready",
                    target.org
                ));
                sleep(wait);
            }
        }
    }
}

/// Non-secret saved-login identity. Generation and bearer rotation are not
/// identity changes; a different login session is. Deserialization grants no
/// authority: every operation compares this receipt with private custody.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountImageIdentity {
    pub org: String,
    pub organization_id: String,
    pub audience: String,
    pub issuer: String,
    pub subject_id: String,
    pub session_id: String,
    pub absolute_expires_at: u64,
}

#[derive(Debug, Clone)]
pub enum AccountImageOperation {
    Quote,
    /// Image-to-image rate. Text-to-image stays `Quote`.
    QuoteReference,
    Submit {
        input: Value,
        idempotency_key: String,
    },
    Job {
        job_id: String,
    },
    Artifact {
        reference: String,
    },
}

/// JSON operations use `body`; only Artifact returns `bytes`/`content_type`.
/// Neither representation contains a credential or a caller-selected URL.
pub struct AccountImageResponse {
    pub status: u16,
    pub body: Option<Value>,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

impl AccountImageIdentity {
    pub fn target(&self) -> RemoteTarget {
        RemoteTarget {
            identity: None,
            org: self.org.clone(),
            endpoint: self.audience.clone(),
            org_id: self.organization_id.clone(),
        }
    }
}

fn account_image_identity_from(
    target: &RemoteTarget,
    cred: &Credential,
) -> Result<AccountImageIdentity> {
    crate::proto::identifier(&target.org, "saved account")?;
    let at = now()?;
    let issuer = cred.issuer.as_deref().filter(|s| !s.is_empty());
    let subject = cred.subject_id.as_deref().filter(|s| valid_id(s));
    let (Some(issuer), Some(subject)) = (issuer, subject) else {
        return Err(reject(
            "saved account identity is missing or expired — run `cadence login` again",
        ));
    };
    if cred.organization_slug.as_deref() != Some(target.org.as_str())
        || cred.organization_id != target.org_id
        || cred.audience != target.endpoint
        || !valid_capabilities(&cred.capabilities)
    {
        return Err(reject(
            "saved account identity does not match its selected destination",
        ));
    }
    // A renewable record must stay renewable: never downgrade a malformed,
    // removed or replaced session to an old access-only approval.
    let (session_id, absolute_expires_at) = match &cred.cli_session {
        Some(session) => {
            if !session.session_id.strip_prefix("hcs_").is_some_and(|id| {
                id.len() == 36 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }) || session.generation == 0
                || !valid_token(&session.refresh_token, "hcr_")
                || session.device_label.is_empty()
                || session.device_label.len() > 120
                || session.idle_expires_at <= at
                || session.absolute_expires_at <= at
                || session.idle_expires_at > session.absolute_expires_at
            {
                return Err(reject(
                    "saved renewable account identity is invalid or expired",
                ));
            }
            (session.session_id.clone(), session.absolute_expires_at)
        }
        None if cred.expires_at > at.saturating_add(SKEW) && cred.pending.is_none() => {
            use sha2::{Digest, Sha256};
            // Internal non-secret lineage for a login with no issuer credential
            // id. A replacement bearer changes the source, never its authority.
            (
                format!(
                    "access-sha256:{:x}",
                    Sha256::digest(cred.access_token.as_bytes())
                ),
                cred.expires_at,
            )
        }
        None => {
            return Err(reject(
                "saved account login expired — run `cadence login` again",
            ))
        }
    };
    Ok(AccountImageIdentity {
        org: target.org.clone(),
        organization_id: cred.organization_id.clone(),
        audience: cred.audience.clone(),
        issuer: issuer.to_owned(),
        subject_id: subject.to_owned(),
        session_id,
        absolute_expires_at,
    })
}

/// Saved remote destinations from the CLI org registry. This read-only view
/// deliberately ignores `selected` and CADENCE_ORG: inventory is not selection
/// or authority. Each row still has to pass private credential identity checks.
pub fn account_image_targets() -> Result<Vec<RemoteTarget>> {
    let dir = auth_dir(None)?;
    let path = dir
        .parent()
        .ok_or_else(|| reject("saved org registry directory unavailable"))?
        .join("orgs.json");
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.len() > 65536 {
        return Err(reject(
            "saved org registry must be a bounded owned regular file",
        ));
    }
    let mut raw = Vec::new();
    file.take(65537).read_to_end(&mut raw)?;
    if raw.len() > 65536 {
        return Err(reject("saved org registry exceeds 64 KiB"));
    }
    let registry: Value =
        serde_json::from_slice(&raw).map_err(|_| reject("invalid saved org registry"))?;
    let rows = registry["connections"]
        .as_array()
        .ok_or_else(|| reject("invalid saved org registry"))?;
    let mut targets = Vec::new();
    let mut names = std::collections::HashSet::new();
    for row in rows {
        let org = row["selection"]["org"]
            .as_str()
            .ok_or_else(|| reject("saved org name is missing"))?;
        crate::proto::identifier(org, "saved org")?;
        if !names.insert(org) {
            return Err(reject("duplicate saved org"));
        }
        match row["destination"]["mode"].as_str() {
            Some("local") => continue,
            Some("remote") => {}
            _ => return Err(reject("invalid saved org destination")),
        }
        let endpoint = row["destination"]["endpoint"]
            .as_str()
            .ok_or_else(|| reject("saved org audience is missing"))?;
        let org_id = row["destination"]["org_id"]
            .as_str()
            .filter(|id| valid_id(id))
            .ok_or_else(|| reject("saved org workspace is missing"))?;
        targets.push(RemoteTarget {
            identity: None,
            org: org.to_owned(),
            endpoint: endpoint.to_owned(),
            org_id: org_id.to_owned(),
        });
    }
    targets.sort_by(|a, b| a.org.cmp(&b.org));
    Ok(targets)
}

/// Offline discovery only: no renewal, default lookup or account request.
/// The target is the already selected saved org, not a billing authority.
pub fn account_image_identity(target: &RemoteTarget) -> Result<AccountImageIdentity> {
    crate::proto::identifier(&target.org, "saved account")?;
    let dir = auth_dir(None)?;
    // Writers atomically replace the private file. Offline discovery must
    // never wait on a login lock held across renewal/network I/O.
    let identity = account_image_identity_from(target, &read_credential_metadata(&dir, target)?)?;
    crate::remote_enrollment::validate_account_image_destination(
        &dir,
        &identity.issuer,
        &identity.audience,
    )?;
    Ok(identity)
}

/// Custody owns renewal and the fixed image HTTP operation. The source lock
/// serializes logout/replacement with the entire operation, not only refresh.
/// Check the frozen identity BEFORE any renewal POST and again afterwards.
pub fn account_image_request(
    identity: &AccountImageIdentity,
    operation: AccountImageOperation,
    timeout_secs: u64,
) -> Result<AccountImageResponse> {
    crate::proto::identifier(&identity.org, "saved account")?;
    if !(1..=120).contains(&timeout_secs) {
        return Err(reject(
            "account image timeout is outside the supported bound",
        ));
    }
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let dir = auth_dir(None)?;
    let target = identity.target();
    let _lock = private_lock(&dir, &identity.org, deadline)?;
    let cred = load_credential(&dir, &target)?;
    if account_image_identity_from(&target, &cred)? != *identity {
        return Err(reject(
            "saved account source changed; frozen image authority is stale",
        ));
    }
    crate::remote_enrollment::validate_account_image_destination(
        &dir,
        &identity.issuer,
        &identity.audience,
    )?;
    if cred.cli_session.is_some() {
        renew_credential_locked(
            &dir,
            &target,
            deadline,
            cred,
            CredentialPurpose::Cli,
            crate::remote_enrollment::cli_session_post_detailed,
        )?;
    }
    let cred = load_credential(&dir, &target)?;
    if account_image_identity_from(&target, &cred)? != *identity {
        return Err(reject("saved account source changed during renewal"));
    }
    crate::remote_enrollment::account_image_send(
        &dir,
        &identity.issuer,
        &identity.audience,
        &cred.access_token,
        operation,
        deadline,
    )
}

/// CAD-1429: a Publish Web management request. Only a verified, live `hct_`
/// credential for `target` is used; the bearer is never returned or printed.
/// The required scope is checked against the credential's own stored grant
/// (`web.read` or `web.prepare`); the server enforces it again per route.
/// Redirects are refused and the body is bounded, as for every relay call.
/// The answer is the server's `{ok,data|error}` envelope, unwrapped to `data`
/// on success and to the stable server `code` on refusal.
pub struct WebResponse {
    pub status: u16,
    pub body: Option<Value>,
}

/// CAD-1429: the verified identity a web record is bound to. It comes from the
/// same live credential check as every web request (owner file, issuer pin,
/// audience and org match, renewal). It never carries the token.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WebIdentity {
    pub org: String,
    pub organization_id: String,
    pub audience: String,
    pub issuer: String,
}

/// Verify the stored credential for `target` and return its identity. Fails
/// closed when the credential has no issuer, expired, or was minted for
/// another workspace or audience.
/// Read-only local guard for a command whose requests go through the
/// daemon: the saved credential must exist and be current, but renewal
/// belongs to the daemon. The refusal text is the pre-cutover web path's.
pub fn web_credential_saved(target: &RemoteTarget) -> Result<()> {
    let dir = auth_dir(None)?;
    load_credential(&dir, target).map(|_| ())
}

pub fn web_identity(target: &RemoteTarget) -> Result<WebIdentity> {
    let dir = auth_dir(None).map_err(|_| {
        reject("remote credential directory is unavailable — set HOME or XDG_CONFIG_HOME")
    })?;
    let deadline = Instant::now() + Duration::from_secs(WAKE_TIMEOUT_DEFAULT);
    let cred = renewed_credential(&dir, target, deadline)?;
    let issuer = cred.issuer.clone().ok_or_else(|| {
        reject("the stored login has no pinned issuer — run `cadence login --web` again")
    })?;
    Ok(WebIdentity {
        org: target.org.clone(),
        organization_id: cred.organization_id.clone(),
        audience: cred.audience.clone(),
        issuer,
    })
}

/// One bounded web request. `headers` carry only the web contract's own
/// headers (`X-Workspace-Id`, `X-Content-Sha256`, `Content-Type`); a bearer
/// or envelope is never taken from the caller.
pub fn web_request(
    target: &RemoteTarget,
    scope: &str,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: Option<&[u8]>,
    wake_timeout: u64,
) -> Result<WebResponse> {
    if !matches!(scope, "web.read" | "web.prepare") {
        return Err(reject("web request scope is not allowed"));
    }
    let dir = auth_dir(None).map_err(|_| {
        reject("remote credential directory is unavailable — set HOME or XDG_CONFIG_HOME")
    })?;
    let deadline = Instant::now() + Duration::from_secs(wake_timeout);
    let cred = renewed_credential(&dir, target, deadline)?;
    if !cred.capabilities.iter().any(|c| c == scope) {
        return Err(reject(format!(
            "this login for '{}' lacks {scope} — run `cadence login --web` for that workspace",
            target.org
        )));
    }
    // `/v1/web-sites` is a platform route on the public issuer. Board hosts are
    // answered by the board catch-all before any `/v1` route, so the board
    // audience is never sent a web request. The issuer is the one the saved,
    // renewed credential was minted by; it is never taken from an argument.
    let issuer = cred.issuer.clone().ok_or_else(|| {
        reject("the stored login has no pinned issuer — run `cadence login --web` again")
    })?;
    let (status, body) = crate::remote_enrollment::web_issuer_request(
        &dir,
        &issuer,
        crate::remote_enrollment::WebRequest {
            method,
            path,
            bearer: &cred.access_token,
            headers,
            body,
        },
        deadline,
    )?;
    Ok(WebResponse { status, body })
}

/// Unwrap the server envelope: `data` on `ok:true`, else a stable refusal
/// carrying the server's own `error.code` (never the raw body).
pub fn web_envelope(response: WebResponse) -> Result<Value> {
    let body = response
        .body
        .ok_or_else(|| reject("remote web answer was not JSON"))?;
    if body["ok"] == Value::Bool(true) {
        return Ok(body["data"].clone());
    }
    let code = body["error"]["code"].as_str().unwrap_or("unavailable");
    if response.status == 409 && body["ok"] == Value::Bool(false) && code == "revision_conflict" {
        return Err(Error::invalid(
            "revision_conflict",
            format!("remote web request refused ({} {code})", response.status),
        ));
    }
    let message = match response.status {
        401 | 403 if code == "unauthorized" || code == "forbidden" => {
            "the remote refused the credential — run `cadence login` again"
        }
        _ => "remote web request refused",
    };
    Err(Error::rejected(format!(
        "{message} ({} {code})",
        response.status
    )))
}

#[cfg(test)]
mod tests;
