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
/// `project_policy`/`message_send`/`team_*`) rides the same envelope — the
/// caller's authority never travels as a request field, only inside the
/// verified, single-use envelope. A failed or ambiguous call is never retried
/// by the client: the wake retry is the only loop, and it fires only on a
/// pre-forward `waking` verdict. Anything else — a timeout, a dropped
/// connection, a 5xx after forward — stays explicitly unresolved: the
/// mutation may or may not have landed, so reconcile before blindly
/// re-issuing. For `project_policy`, reconcile by reading the project record;
/// a refusal is final. For `message_send`, the caller-chosen `message` id in
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
    "issue_ls",
    "issue_show",
    "issue_history",
    "message_read",
    "message_inbox",
    "team_list",
    "team_invites",
    // cli.write
    "issue_new",
    "issue_comment",
    "issue_set",
    "team_invite",
    "team_revoke_invite",
    "team_set_role",
    "team_remove",
    "project_policy",
    "message_send",
    // CAD-1586 / AOS-290: scoped settings + master diagnostics.
    "settings_agents",
    "master_status",
    // CAD-1946: the merge gate. Reads (cli.read) and the operator's merge
    // approval (`audit.approve`, which only `cadence login --audit-approve`
    // requests; the hosted org admits it for the human owner only).
    "delivery_policy",
    "audit_approval",
    "audit_scope",
    "audit_approve",
];

/// Scope a cli authorize request asks for. Existing container verbs keep
/// the historical `cli.read`+`cli.write` pair. Platform team verbs ask only
/// for the scope that verb requires.
fn cli_requested_scopes(verb: &str) -> &'static [&'static str] {
    match verb {
        "team_list" | "team_invites" | "delivery_policy" | "audit_approval" | "audit_scope" => {
            &["cli.read"]
        }
        "audit_approve" => &["audit.approve"],
        "team_invite" | "team_revoke_invite" | "team_set_role" | "team_remove" => &["cli.write"],
        _ => &["cli.read", "cli.write"],
    }
}

/// The four team mutations: the Worker commits them in the same D1 batch
/// that records the receipt, so a lost or ambiguous answer follows a
/// possible commit exactly as a lost send answer does. The two read-only
/// team verbs (`team_list`, `team_invites`) change nothing.
fn team_mutation_verb(verb: &str) -> bool {
    matches!(
        verb,
        "team_invite" | "team_revoke_invite" | "team_set_role" | "team_remove"
    )
}

/// A verb that can change server state. When the answer to one of these is
/// lost or ambiguous, the mutation may already have landed, so the outcome
/// is unknown and the caller must reconcile with a read before repeating it
/// — never a plain refusal, and never a bare retry.
///
/// `message_send` is the historical member of this set; the team mutations
/// join it for the reason above.
fn mutating_cli_verb(verb: &str) -> bool {
    verb == "message_send" || team_mutation_verb(verb)
}

/// The read a caller must run before repeating a mutating verb whose answer
/// was lost. A lost response is not a refusal: the write may have landed, so
/// the next step is a read of the same workspace, never a blind retry.
/// `message_send` reconciles on its own stable id; the team mutations
/// reconcile with the directory read.
fn lost_mutation_note(verb: &str) -> &'static str {
    if verb == "message_send" {
        "the send may or may not have been recorded"
    } else {
        "the team change may or may not have been applied — inspect `team list` or \
         `team invites` with the same explicit --org before repeating it"
    }
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

/// Known, unique capabilities that include the CLI pair. Wiki, web and
/// settings are optional. The cloud-agent scopes are optional too, and only
/// as the whole set: one or two of them is not a login this client rotates.
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
    // CAD-1568: a mutating verb's transport failure after the call was
    // attempted is outcome-unknown, not a rejection — the mutation may
    // have landed, so the caller must reconcile by idempotency key, never
    // assume nothing landed. `message_send` reconciles on `arguments.message`;
    // the team mutations reconcile with a read (`team list` / `team invites`).
    let mutating = matches!(surface, Surface::Cli) && mutating_cli_verb(verb);
    post(
        &target.endpoint,
        &format!("{relay}/call"),
        &cred.access_token,
        &body,
        request_timeout(deadline, CALL_TIMEOUT)?,
    )
    .map_err(|e| {
        if mutating && e.kind() != "unknown" {
            Error::unknown(format!("{e} — {}", lost_mutation_note(verb)))
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
    Ok((status, retry_after, serde_json::from_slice(&bytes).ok()))
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
        let (status, retry_after, body) =
            upload_once(target, &cred, logical_path, file_name, bytes, deadline)?;
        // A refusal names the status and the contract's error code only: the
        // raw body could carry anything, so it is never echoed.
        let conflict = status == 409 && body.as_ref().is_some_and(|b| b.get("conflict").is_some());
        if !(200..300).contains(&status) && status != 503 && !conflict {
            // CAD-1434: the gateway envelope is `{ok:false,error:{code}}`; the
            // decoder reads it (the old top-level read always fell through).
            return Err(refusal_error(status, body.as_ref()));
        }
        match classify(status, retry_after, body) {
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
/// reconciles with a read for the same explicit `--org` before repeating
/// it. It never advises a bare retry, and it never claims the write failed.
/// `correlation` is the bounded board correlation the refusal may carry.
fn ambiguous_mutation_error(status: u16, correlation: Option<String>) -> Error {
    let mut text = format!(
        "the remote action may already have committed (outcome_unknown, HTTP {status}); \
         inspect `team list` or `team invites` with the same explicit --org before \
         repeating the mutation. Nothing was retried; saved credentials are kept"
    );
    if let Some(correlation) = correlation {
        text.push_str(&correlation);
    }
    Error::unknown(text)
}

/// CAD-1568: actionable text for the `cli_message_*` family's typed
/// refusals, keyed on the container's bounded `error.code` — the raw
/// `error.message` the route returns is never shown (a `cli_fail` body
/// can carry arbitrary server text). Unknown or absent codes keep the
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
        ("message_read", Some("not_found")) => Some((
            "not_found",
            "no CLI message with that id is visible to this human identity; \
             reconcile the original workspace and login before retrying the same id",
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
    let (status, body) = crate::remote_enrollment::cli_session_post(
        dir,
        issuer,
        "/v1/hosted-cadence/cli-session/revoke",
        &session.refresh_token,
        serde_json::json!({"version":SESSION_VERSION,"session_id":session.session_id}),
        deadline.min(Instant::now() + RENEWAL_WAIT),
    )?;
    if status != 200
        || !exact_object(&body, &["version", "status", "session_id"])
        || body["version"] != SESSION_VERSION
        || body["status"] != "revoked"
        || body["session_id"] != session.session_id
    {
        return Err(reject(
            "remote device revocation was not confirmed; local authority retained",
        ));
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
        let refusal_code = refusal_fields(body.as_ref()).0.map(str::to_owned);
        // CAD-1568: a 5xx carrying a worker `state` is a pre-forward
        // verdict (waking handled below, wake_failed/unavailable = nothing
        // ran). A 5xx with no `state` may come from the container *after*
        // the send was attempted — outcome unknown, reconcile by
        // `arguments.message`, never a blind resubmission.
        let send_outcome_unknown = verb == "message_send"
            && status >= 500
            && body.as_ref().and_then(|b| b["state"].as_str()).is_none();
        // A team mutation is answered by the Worker itself, in the same D1
        // batch that records the receipt. A 5xx with no `state` therefore
        // follows a possible commit: the platform's own explicit
        // uncertainty codes say so, and a bare one may hide a committed
        // write behind an unanswerable reply. The board's gate refusal
        // (`hop: board`) is decided before the verb ran, so it stays a
        // definite refusal. The platform's explicit uncertainty codes keep
        // their own decoded message, which names the code.
        let team_mutation_unknown = team_mutation_verb(verb)
            && status >= 500
            && body.as_ref().and_then(|b| b["state"].as_str()).is_none()
            && !board_gate_refused(body.as_ref())
            && !matches!(
                refusal_code.as_deref(),
                Some("mutation_outcome_uncertain" | "invite_email_uncertain")
            );
        let unknown_correlation = refusal_correlation(body.as_ref());
        match classify(status, retry_after, body) {
            Verdict::Done(Err(_e)) if team_mutation_unknown => {
                return Err(ambiguous_mutation_error(status, unknown_correlation));
            }
            Verdict::Done(Err(e)) if send_outcome_unknown => {
                return Err(Error::unknown(format!(
                    "{e} — the send may or may not have been recorded"
                )));
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

#[cfg(test)]
#[path = "remote_cli_renewal_acceptance.rs"]
mod renewal_acceptance;

#[cfg(test)]
#[path = "remote_cli/cad1459_account_image_acceptance.rs"]
pub(crate) mod cad1459_account_image_acceptance;
