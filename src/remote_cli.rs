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
//!   The envelope is **single-use** — the container (#744) consumes its `jti`.
//!   [`call_once`] mints a fresh one for every command invocation, and the wake
//!   loop re-mints on every retry: a `503 waking` is produced by the worker's
//!   `wakeBoardRuntime` *before* any container forward, so the just-minted
//!   envelope's `jti` was never consumed — but re-minting is still the only
//!   safe posture (a `503` that came later in the pipeline would have burned
//!   it). An envelope is never reused across calls or retries.
//! - `POST {endpoint}/__platform/cli/call` — same bearer + `{version,
//!   envelope, verb, arguments?}` — `verb` is one of
//!   [`REMOTE_VERBS`]; `arguments` is an object the container's
//!   `/api/cli/<verb>` dispatch consumes. Top-level authority-shaped keys
//!   (`actor`, `role`, `org`, `user`, `as`, `principal`, `wiki_as`) are
//!   refused server-side; this client never sends them.
//! - `401/403` — the bearer no longer resolves to a live principal: re-login.
//! - `503 {"state":"waking","retry_after_s":N}` + `Retry-After` — the company
//!   container is waking; wait and retry inside the wake budget. A retry is
//!   safe: the refusal came back before the verb ran. `wake_failed`
//!   (`runtime_stopped`/`not_found`/`unavailable`) and every other failure
//!   end the command — never a redirect chase, never another org, never local.

use base64::Engine;
use serde_json::{json, Map, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
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
/// ∩ AOS-128's `HOSTED_CADENCE_CLI_VERBS`, minus `team_list` (no local
/// `team` command exists to map to it): a command not listed here
/// is refused *locally* and its bytes never leave the process.
///
/// The cli.write half (`issue_new`/`issue_comment`/`issue_set`/
/// `project_policy`/`message_send`) rides the same envelope — the caller's authority
/// never travels as a request field, only inside the verified, single-use
/// envelope. A failed or ambiguous call is never retried by the client:
/// the wake retry is the only loop, and it fires only on a pre-forward
/// `waking` verdict. Anything else — a timeout, a dropped connection, a
/// 5xx after forward — stays explicitly unresolved: the mutation may or
/// may not have landed, so reconcile before blindly re-issuing. For
/// `project_policy`, reconcile by reading the project record; a refusal
/// is final. For `message_send`, the caller-chosen `message` id in
/// `arguments` is the reconciliation key: `message_read` returns the
/// principal's own durable message by id, and resubmitting the same id
/// dedupes server-side — never a blind replay. A 2xx proves the enqueue
/// only as the complete correlated receipt
/// ([`send_receipt_correlates`]): an absent, malformed or foreign-bound
/// body is `unknown`, the same unresolved outcome a lost response has.
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
    // cli.write
    "issue_new",
    "issue_comment",
    "issue_set",
    "project_policy",
    "message_send",
    // CAD-1586 / AOS-290: scoped settings + master diagnostics.
    "settings_agents",
    "master_status",
];

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
    let _lock = private_lock(dir, &target.org, deadline)?;
    if Instant::now() >= deadline {
        return Err(Error::busy_coded(
            "timeout",
            "remote command deadline elapsed before session renewal",
        ));
    }
    let cred = load_credential(dir, target)?;
    renew_credential_locked(dir, target, deadline, cred, post)
}

/// Caller holds the source lock, including any frozen-identity check.
fn renew_credential_locked(
    dir: &Path,
    target: &RemoteTarget,
    deadline: Instant,
    mut cred: Credential,
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
    let due = cred.pending.is_some() || cred.expires_at <= at.saturating_add(RENEW_BEFORE);
    if !due {
        return Ok(cred);
    }
    let Some(session) = cred.cli_session.clone() else {
        return Err(reject(
            "remote credential has no renewable session — run `cadence login` again",
        ));
    };
    let issuer = cred
        .issuer
        .as_deref()
        .ok_or_else(|| reject("renewable session issuer missing — run `cadence login` again"))?;
    if cred.organization_slug.as_deref() != Some(target.org.as_str())
        || cred.subject_id.as_deref().is_none_or(|s| !valid_id(s))
        || !valid_capabilities(&cred.capabilities)
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
        return Err(reject(
            "renewable CLI session is invalid or expired — run `cadence login` again",
        ));
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

/// Known, unique capabilities that include the CLI pair. Wiki scopes are
/// optional, so a CLI-only login remains valid.
fn valid_capabilities(values: &[String]) -> bool {
    let once = |c: &str| values.iter().filter(|v| v.as_str() == c).count() == 1;
    values.iter().all(|v| CAPABILITIES.contains(&v.as_str()))
        && values
            .iter()
            .enumerate()
            .all(|(i, v)| !values[..i].contains(v))
        && once("cli.read")
        && once("cli.write")
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
        Surface::Cli => &["cli.read", "cli.write"],
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
    // assume nothing landed. `message_send` reconciles on `arguments.message`.
    let mutating = matches!(surface, Surface::Cli) && verb == "message_send";
    post(
        &target.endpoint,
        &format!("{relay}/call"),
        &cred.access_token,
        &body,
        request_timeout(deadline, CALL_TIMEOUT)?,
    )
    .map_err(|e| {
        if mutating && e.kind() != "unknown" {
            Error::unknown(format!("{e} — the send may or may not have been recorded"))
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
/// - any `5xx` is availability (waking, unavailable, runtime).
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
    /// Any 5xx: retry later; saved credentials and any pending
    /// renewal are kept.
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
    let known_reason =
        reason.filter(|r| POLICY_REASONS.contains(r) || ENVELOPE_REASONS.contains(r));
    let category = match status {
        // Every 5xx is the gateway or runtime being unavailable: retry later.
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
    reject(text)
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
        match classify(status, retry_after, body) {
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
mod tests {
    #[cfg(feature = "test-seam")]
    mod native_chain_acceptance {
        include!("remote_cli_native_chain_acceptance.rs");
    }

    use super::*;
    use serde_json::json;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::thread;

    const ORG: &str = "acme";
    const ORG_ID: &str = "ws_real";
    const BRIDGE: &str = "hct_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

    fn target(listener: &TcpListener) -> RemoteTarget {
        RemoteTarget {
            identity: None,
            org: ORG.into(),
            endpoint: format!("http://{}", listener.local_addr().unwrap()),
            org_id: ORG_ID.into(),
        }
    }

    /// Write the credential the login slice writes — the same on-disk
    /// layout (`cli-<slug>.json`, the `hct_` bound to org id + audience).
    fn store_credential_at(dir: &Path, org: &str, org_id: &str, endpoint: &str, expires_at: u64) {
        let body = json!({"version": "hosted-cadence-auth.v1",
            "organization_id": org_id, "audience": endpoint,
            "access_token": BRIDGE, "expires_at": expires_at});
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(dir.join(format!("cli-{org}.json")))
            .unwrap();
        file.write_all(body.to_string().as_bytes()).unwrap();
    }
    fn store_credential(dir: &Path, endpoint: &str) {
        store_credential_at(dir, ORG, ORG_ID, endpoint, now().unwrap() + 300);
    }

    #[test]
    fn assignment_identity_survives_saved_login_reloads() {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut frozen = target(&listener);
        frozen.identity = Some(CliIdentityPin {
            subject_id: "user_original".into(),
            issuer: "https://original.invalid".into(),
        });
        store_credential(dir.path(), &frozen.endpoint);
        let path = dir.path().join(format!("cli-{ORG}.json"));
        let mut original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        original["subject_id"] = json!("user_original");
        original["issuer"] = json!("https://original.invalid");
        fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        assert_eq!(
            cli_material(dir.path(), &frozen).unwrap().access_token,
            BRIDGE
        );

        for (subject, issuer) in [
            (json!("user_replacement"), json!("https://original.invalid")),
            (json!("user_original"), json!("https://replacement.invalid")),
            (Value::Null, Value::Null),
            (json!("user_original"), Value::Null),
        ] {
            let mut replacement = original.clone();
            replacement["subject_id"] = subject;
            replacement["issuer"] = issuer;
            fs::write(&path, serde_json::to_vec(&replacement).unwrap()).unwrap();
            assert!(cli_material(dir.path(), &frozen).is_err());
            assert_eq!(
                listener.accept().err().unwrap().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
        fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        assert_eq!(
            cli_material(dir.path(), &frozen).unwrap().access_token,
            BRIDGE
        );
    }

    fn read_request(socket: std::net::TcpStream) -> (std::net::TcpStream, Request) {
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let mut parts = line.trim_end().split(' ');
        let (method, path) = (
            parts.next().unwrap().to_string(),
            parts.next().unwrap().to_string(),
        );
        assert_eq!(method, "POST");
        let mut bearer = String::new();
        let mut length = 0_usize;
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            let (key, value) = line.split_once(':').unwrap();
            if key.eq_ignore_ascii_case("authorization") {
                bearer = value.trim().trim_start_matches("Bearer ").to_string();
            }
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut raw = vec![0; length];
        reader.read_exact(&mut raw).unwrap();
        (
            socket,
            Request {
                path,
                bearer,
                body: serde_json::from_slice(&raw).unwrap(),
            },
        )
    }

    /// One request the fake accepted — path, bearer and parsed body.
    struct Request {
        path: String,
        bearer: String,
        body: Value,
    }

    fn answer(mut socket: std::net::TcpStream, status: &str, extra: &str, body: &Value) {
        let bytes = serde_json::to_vec(body).unwrap();
        write!(
            socket,
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n",
            bytes.len()
        )
        .unwrap();
        socket.write_all(&bytes).unwrap();
    }

    /// The strict `/__platform/cli/authorize` gate (mirrors
    /// `hostedCadenceCliAuthorizeRequestSchema`): every key required, no key
    /// extra, org and audience pinned to this board, scopes a subset of
    /// `cli.*`, ttl within 1..=300.
    fn check_authorize(req: &Request) -> bool {
        req.path == "/__platform/cli/authorize"
            && req.bearer == BRIDGE
            && req.body.as_object().map(Map::len) == Some(5)
            && req.body["version"] == json!(CLI_VERSION)
            && req.body["organization_id"] == json!(ORG_ID)
            && req.body["audience"].as_str().is_some()
            && req.body["requested_scopes"].as_array().is_some_and(|s| {
                !s.is_empty()
                    && s.iter()
                        .all(|v| v.as_str().is_some_and(|s| s.starts_with("cli.")))
            })
            && req.body["ttl_seconds"]
                .as_u64()
                .is_some_and(|t| (1..=300).contains(&t))
    }

    /// The strict `/__platform/cli/call` gate (mirrors
    /// `hostedCadenceCliCallRequestSchema` + the forbidden-key rule):
    /// version, a compact-JWS envelope, an allowlisted verb, optional
    /// arguments object — and no authority-shaped top-level key.
    fn check_call(req: &Request) -> bool {
        const FORBIDDEN: &[&str] = &["wiki_as", "actor", "role", "org", "user", "as", "principal"];
        let keys_ok = req
            .body
            .as_object()
            .is_some_and(|o| o.keys().all(|k| !FORBIDDEN.contains(&k.as_str())));
        let envelope_ok = req.body["envelope"].as_str().is_some_and(|e| {
            e.split('.').count() == 3
                && e.split('.').all(|p| {
                    !p.is_empty()
                        && p.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                })
        });
        req.path == "/__platform/cli/call"
            && req.bearer == BRIDGE
            && keys_ok
            && req.body["version"] == json!(CLI_VERSION)
            && envelope_ok
            && REMOTE_VERBS.contains(&req.body["verb"].as_str().unwrap_or_default())
            && req.body.get("arguments").is_none_or(|a| a.is_object())
    }

    /// A loopback fake worker: one authorize (strict schema → mint a fixed
    /// envelope shape), then `calls` call-attempts answered in order.
    /// Returns the bound listener plus a `Vec` the server records every
    /// request path + body into — so a test can prove nothing was sent.
    fn fake_worker(
        answers: Vec<(u16, &'static str, Value)>,
    ) -> (
        TcpListener,
        std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = listener.try_clone().unwrap();
        let seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
        let requests = seen.clone();
        // The container (#744) consumes each cli envelope's `jti` once —
        // the fake does the same so a reused envelope is refused, not
        // silently accepted.
        let spent: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let consumed = spent.clone();
        let handle = thread::spawn(move || {
            loop {
                let Ok((socket, _)) = server.accept() else {
                    return;
                };
                let req = read_request(socket);
                let (socket, req) = (req.0, req.1);
                requests.lock().unwrap().push(json!({
                    "path": req.path, "body": req.body,
                }));
                if req.path == "/__platform/cli/authorize" {
                    if !check_authorize(&req) {
                        answer(
                            socket,
                            "400 Bad Request",
                            "",
                            &json!({"code": "invalid_request"}),
                        );
                        continue;
                    }
                    // Envelope shape only — the worker signs the real one.
                    // A distinct envelope per mint lets a test prove the
                    // client never reuses one across commands/retries (#744:
                    // the container treats each cli envelope as single-use).
                    let mint = requests
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|r| r["path"] == json!("/__platform/cli/authorize"))
                        .count();
                    let envelope = format!("h.e.{mint}");
                    answer(
                        socket,
                        "200 OK",
                        "",
                        &json!({"version": CLI_VERSION, "envelope": envelope,
                            "bearer": format!("wikienv_{envelope}"), "expires_at": 0,
                            "actor": "users/o", "scope": ["cli.read"], "issuer": "i"}),
                    );
                    continue;
                }
                if req.path == "/__platform/cli/call" {
                    let n = requests
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|r| r["path"] == json!("/__platform/cli/call"))
                        .count();
                    if !check_call(&req) {
                        answer(
                            socket,
                            "400 Bad Request",
                            "",
                            &json!({"code": "invalid_request"}),
                        );
                        continue;
                    }
                    // Single-use: a replayed envelope is unauthorized —
                    // the jti was already spent by an earlier call.
                    let env = req.body["envelope"].as_str().unwrap().to_string();
                    {
                        let mut spent = consumed.lock().unwrap();
                        if spent.contains(&env) {
                            answer(
                                socket,
                                "401 Unauthorized",
                                "",
                                &json!({"code": "envelope_reused"}),
                            );
                            continue;
                        }
                        spent.push(env);
                    }
                    let Some((status, extra, body)) = answers.get(n - 1) else {
                        // More attempts than answers — a test bug or an
                        // unbounded retry loop; refuse loudly.
                        answer(socket, "500", "", &json!({"code": "unexpected_call"}));
                        continue;
                    };
                    let line = match status {
                        200 => "200 OK",
                        302 => "302 Found",
                        401 => "401 Unauthorized",
                        403 => "403 Forbidden",
                        409 => "409 Conflict",
                        503 => "503 Service Unavailable",
                        _ => "400 Bad Request",
                    };
                    answer(socket, line, extra, &body.clone());
                    continue;
                }
                answer(socket, "404 Not Found", "", &json!({"code": "not_found"}));
            }
        });
        (listener, seen, handle)
    }

    /// Run one command against the strict fake: returns the call result
    /// and every request the worker saw (to count mints / reads).
    fn run(
        answers: Vec<(u16, &'static str, Value)>,
        verb: &str,
        args: Map<String, Value>,
        wake_timeout: u64,
    ) -> (Result<Value>, Vec<Value>) {
        let (listener, calls, _server) = fake_worker(answers);
        let target = target(&listener);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        store_credential(&dir, &target.endpoint);
        let out = call_verb_in(&target, verb, args, wake_timeout, &dir, |_| {}, |_| {});
        let calls_out = calls.lock().unwrap().clone();
        // Tests assert on the verb's JSON body — a conflict answer
        // still carries one, so unwrap it the same way.
        let out = out.map(|a| match a {
            RemoteAnswer::Ok(b) | RemoteAnswer::Conflict(b) => b,
        });
        (out, calls_out)
    }

    /// Number of requests the fake accepted at `path` — 0 proves no byte
    /// for that route ever left the client.
    fn hits(requests: &[Value], path: &str) -> usize {
        requests.iter().filter(|r| r["path"] == json!(path)).count()
    }

    #[test]
    fn cad1434_blocker_authorize_refusals_never_forward_or_retry_user_writes() {
        let request_id = "12345678-1234-4234-8234-123456789abc";
        let unavailable = json!({"ok": false, "error": {
            "code": "runtime_unavailable", "hop": "board", "request_id": request_id,
            "message": "do-not-print-server-text"}})
        .to_string();
        let unsafe_code = json!({"code": "do-not-print-server-text /private/token",
            "message": "do-not-print-server-text"})
        .to_string();
        for (status, body, expected) in [
            (
                401,
                "<html>do-not-print-server-text</html>",
                "uncertain, HTTP 401",
            ),
            (
                403,
                "<html>do-not-print-server-text</html>",
                "authorization, HTTP 403",
            ),
            (503, unavailable.as_str(), "availability, HTTP 503"),
            (400, unsafe_code.as_str(), "unknown, HTTP 400"),
            (
                302,
                "<html>do-not-print-server-text</html>",
                "remote redirect refused",
            ),
            (
                200,
                "<html>do-not-print-server-text</html>",
                "remote authorize answered non-JSON",
            ),
        ] {
            for operation in ["issue_new", "write", "upload"] {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                listener.set_nonblocking(true).unwrap();
                let target = target(&listener);
                let server = listener.try_clone().unwrap();
                let response = body.to_owned();
                let (stop, stopped) = std::sync::mpsc::channel();
                let worker = thread::spawn(move || {
                    let mut paths = Vec::new();
                    let mut stopping = false;
                    let deadline = Instant::now() + Duration::from_secs(10);
                    while Instant::now() < deadline {
                        stopping |= stopped.try_recv().is_ok();
                        let (mut socket, _) = match server.accept() {
                            Ok(connection) => connection,
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                if stopping {
                                    break;
                                }
                                thread::sleep(Duration::from_millis(2));
                                continue;
                            }
                            Err(e) => panic!("refusal worker accept: {e}"),
                        };
                        socket.set_nonblocking(false).unwrap();
                        socket
                            .set_read_timeout(Some(Duration::from_secs(10)))
                            .unwrap();
                        let mut reader = BufReader::new(socket.try_clone().unwrap());
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        paths.push(line.split(' ').nth(1).unwrap().to_owned());
                        let mut length = 0;
                        loop {
                            line.clear();
                            reader.read_line(&mut line).unwrap();
                            if line == "\r\n" {
                                break;
                            }
                            if let Some((key, value)) = line.split_once(':') {
                                if key.eq_ignore_ascii_case("content-length") {
                                    length = value.trim().parse::<usize>().unwrap();
                                }
                            }
                        }
                        // Read bytes, not JSON: an incorrectly forwarded multipart
                        // upload must also be recorded and fail the path assertion.
                        let mut raw = vec![0; length];
                        reader.read_exact(&mut raw).unwrap();
                        write!(socket, "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}", response.len()).unwrap();
                    }
                    paths
                });
                let root = tempfile::tempdir().unwrap();
                let dir = root.path();
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
                store_credential(dir, &target.endpoint);
                let saved = fs::read(dir.join(format!("cli-{ORG}.json"))).unwrap();
                let result = if operation == "upload" {
                    upload_file_in(
                        dir,
                        &target,
                        "global/test.txt",
                        "test.txt",
                        b"test",
                        true,
                        5,
                        |_| panic!("authorize refusal retried"),
                    )
                } else {
                    let arguments = if operation == "write" {
                        json!({"path": "global/test", "text": "test"})
                    } else {
                        json!({"project": "test", "title": "test"})
                    };
                    call_verb_in(
                        &target,
                        operation,
                        arguments.as_object().unwrap().clone(),
                        5,
                        dir,
                        |_| panic!("authorize refusal retried"),
                        |_| panic!("authorize refusal slept"),
                    )
                    .map(|_| json!(null))
                };
                stop.send(()).unwrap();
                let paths = worker.join().unwrap();
                let relay = if operation == "issue_new" {
                    "cli"
                } else {
                    "wiki"
                };
                assert_eq!(
                    paths,
                    [format!("/__platform/{relay}/authorize")],
                    "{operation}, HTTP {status}"
                );
                let text = result.unwrap_err().to_string();
                assert!(text.contains(expected), "{operation}: {text}");
                assert!(!text.contains("do-not-print-server-text"), "{text}");
                assert!(!text.contains("/private/token"), "{text}");
                assert!(!text.contains("cadence login"), "{text}");
                assert!(!text.contains(BRIDGE), "{text}");
                if status == 503 {
                    assert!(
                        text.contains(&format!("hop board, request {request_id}")),
                        "{text}"
                    );
                }
                assert_eq!(
                    fs::read(dir.join(format!("cli-{ORG}.json"))).unwrap(),
                    saved
                );
            }
        }
    }

    #[test]
    fn wiki_refuses_forged_authority_before_mint_and_never_uses_cli_relay() {
        // CAD-1401 independent refusal acceptance. The worker deliberately
        // accepts every payload: only the real client guard can refuse it.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let target = target(&listener);
        let server = listener.try_clone().unwrap();
        let seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
        let requests = seen.clone();
        let (stop, stopped) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            while stopped.try_recv().is_err() {
                let socket = match server.accept() {
                    Ok((socket, _)) => socket,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("fake worker accept: {e}"),
                };
                let (socket, req) = read_request(socket);
                let version = if req.path.starts_with("/__platform/wiki/") {
                    "hosted-cadence-wiki.v1"
                } else {
                    "hosted-cadence-cli.v1"
                };
                let response = if req.path.ends_with("/authorize") {
                    json!({"version": version, "envelope": "h.e.1"})
                } else {
                    json!({"ok": true})
                };
                requests.lock().unwrap().push(json!({
                    "path": req.path, "bearer": req.bearer, "body": req.body,
                }));
                answer(socket, "200 OK", "", &response);
            }
        });
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            root.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        store_credential(root.path(), &target.endpoint);
        let mut failures = Vec::new();

        for dir in [root.path().to_path_buf(), root.path().join("missing")] {
            let before = seen.lock().unwrap().len();
            let out = call_verb_in(&target, "delete", Map::new(), 10, &dir, |_| {}, |_| {});
            if !matches!(out, Err(ref e) if e.kind() == "rejected" && e.to_string().contains("allowlist"))
                || seen.lock().unwrap().len() != before
            {
                failures
                    .push("delete was not refused before credential access/request".to_string());
            }
        }
        // Observe successful calls too, so a missing route or a wrong schema
        // cannot masquerade as a local refusal. Include an empty arguments map.
        for (action, args) in [
            ("enumerate", json!({"path": "global/", "q": "acceptance"})),
            ("read", json!({"path": "global/acceptance.md"})),
            (
                "write",
                json!({"path": "global/acceptance.md", "text": "acceptance", "if_rev": "none"}),
            ),
            ("enumerate", json!({})),
        ] {
            let args = args.as_object().unwrap().clone();
            let before = seen.lock().unwrap().len();
            let out = call_verb_in(
                &target,
                action,
                args.clone(),
                10,
                root.path(),
                |_| {},
                |_| {},
            );
            let observed = seen.lock().unwrap();
            let pair = &observed[before..];
            let scope = if action == "write" {
                "wiki.write"
            } else {
                "wiki.read"
            };
            if !matches!(out, Ok(RemoteAnswer::Ok(_)))
                || pair.len() != 2
                || pair[0]["path"] != "/__platform/wiki/authorize"
                || pair[0]["bearer"] != BRIDGE
                || pair[0]["body"]["version"] != "hosted-cadence-wiki.v1"
                || pair[0]["body"]["organization_id"] != ORG_ID
                || pair[0]["body"]["audience"] != target.endpoint
                || pair[0]["body"]["requested_scopes"] != json!([scope])
                || pair[1]["path"] != "/__platform/wiki/call"
                || pair[1]["bearer"] != BRIDGE
                || !exact_object(
                    &pair[1]["body"],
                    &["version", "envelope", "action", "arguments"],
                )
                || pair[1]["body"]["version"] != "hosted-cadence-wiki.v1"
                || pair[1]["body"]["envelope"] != "h.e.1"
                || pair[1]["body"]["action"] != action
                || pair[1]["body"]["arguments"] != Value::Object(args)
            {
                failures.push(format!(
                    "{action}: wrong wiki route, scope, or exact call body"
                ));
            }
        }
        for action in ["enumerate", "read", "write"] {
            for field in ["actor", "role", "org", "user", "wiki_as", "as", "principal"] {
                let mut args = json!({"path": "global/acceptance.md", "text": "acceptance"})
                    .as_object()
                    .unwrap()
                    .clone();
                args.insert(field.into(), json!("forged"));
                let before = seen.lock().unwrap().len();
                let out = call_verb_in(&target, action, args, 10, root.path(), |_| {}, |_| {});
                if !matches!(out, Err(ref e) if e.kind() == "rejected")
                    || seen.lock().unwrap().len() != before
                {
                    failures.push(format!(
                        "{action}: {field} was not refused before mint/request"
                    ));
                }
            }
        }
        stop.send(()).unwrap();
        worker.join().unwrap();
        let requests = seen.lock().unwrap();
        if requests.iter().any(|r| r["path"] == "/__platform/cli/call") {
            failures.push("a wiki action reached /__platform/cli/call".into());
        }
        for request in requests
            .iter()
            .filter(|r| r["path"] == "/__platform/wiki/call")
        {
            if request["body"]["arguments"]
                .as_object()
                .is_some_and(|args| {
                    args.keys().any(|key| {
                        ["actor", "role", "org", "user", "wiki_as", "as", "principal"]
                            .contains(&key.as_str())
                    })
                })
            {
                failures.push("a wiki call carried authority in arguments".into());
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn credential_for_another_org_is_never_sent_to_this_host() {
        // The stored file names ws_real/acme's endpoint; the resolved org is
        // beta — the token must not ride to a foreign host (contract I1).
        let (listener, _c, _s) = fake_worker(vec![]);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        store_credential(&dir, "https://acme.cadencecloud.app");
        let foreign = RemoteTarget {
            identity: None,
            org: ORG.into(),
            endpoint: format!("http://{}", listener.local_addr().unwrap()),
            org_id: "ws_other".into(),
        };
        let out = call_verb_in(&foreign, "status", Map::new(), 10, &dir, |_| {}, |_| {});
        assert!(out.unwrap_err().to_string().contains("different workspace"));
    }

    /// Envelopes the calls presented, in order — the value the container
    /// would consume as single-use `jti`.
    fn envelopes(requests: &[Value]) -> Vec<String> {
        requests
            .iter()
            .filter(|r| r["path"] == json!("/__platform/cli/call"))
            .map(|r| r["body"]["envelope"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn every_command_mints_a_fresh_envelope() {
        // Single-use contract (#744): the container consumes the envelope's
        // `jti`, so two commands must present two different mints — an
        // envelope is never reused across calls.
        let (listener, requests, _s) =
            fake_worker(vec![(200, "", json!({"a": 1})), (200, "", json!({"b": 2}))]);
        let target = target(&listener);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        store_credential(&dir, &target.endpoint);
        for verb in ["status", "agent_list"] {
            call_verb_in(&target, verb, Map::new(), 10, &dir, |_| {}, |_| {}).unwrap();
        }
        let envs = envelopes(&requests.lock().unwrap());
        assert_eq!(envs.len(), 2);
        assert_ne!(envs[0], envs[1], "two commands reused one envelope");
    }

    #[test]
    fn a_wake_retry_remints_the_envelope() {
        // The waking 503 came from the worker before any forward — the
        // envelope's jti was verified there but never consumed by a
        // container. Retrying still mints a fresh one: a 503 that fired
        // later in the pipeline would have burned the first.
        let (out, requests) = run(
            vec![
                (503, "", json!({"state": "waking", "retry_after_s": 1})),
                (200, "", json!({"ok": true})),
            ],
            "status",
            Map::new(),
            120,
        );
        assert!(out.is_ok());
        assert_eq!(hits(&requests, "/__platform/cli/authorize"), 2);
        let envs = envelopes(&requests);
        assert_eq!(envs.len(), 2);
        assert_ne!(envs[0], envs[1], "a wake retry reused the envelope");
    }

    /// The complete correlated receipt a `message_send` 2xx must carry
    /// (CAD-1568 revision): the request's stable `message` id and canonical
    /// `alias` echoed, a known lifecycle `state`, a real `duplicate` bool,
    /// the immutable `workspace_id`, the sender's canonical
    /// `sender_subject` and the persisted `conversation` Home thread id.
    fn send_receipt(id: &str) -> Value {
        json!({
            "message": id,
            "alias": "master",
            "state": "queued",
            "duplicate": false,
            "workspace_id": ORG_ID,
            "sender_subject": "sub_owner_1",
            "sender": "cli-owner",
            "conversation": "conv_home_1",
        })
    }

    #[test]
    fn successful_send_without_a_receipt_is_unknown_and_not_retried() {
        for receipt in [
            Value::Null,
            json!({"message":"lost-receipt","state":"waking","duplicate":false}),
            json!({"message":"another-request","state":"queued","duplicate":false}),
        ] {
            let (out, requests) = run(
                vec![(200, "", receipt)],
                "message_send",
                json!({"alias": "master", "text": "coordinate", "message": "lost-receipt"})
                    .as_object()
                    .unwrap()
                    .clone(),
                10,
            );
            assert_eq!(out.unwrap_err().kind(), "unknown");
            assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
        }
    }

    #[test]
    fn send_with_a_missing_or_malformed_binding_is_unknown_and_not_retried() {
        // Each corruption removes or falsifies one correlation field of the
        // complete receipt: a 2xx carrying it cannot prove THIS send landed
        // as this caller's message in this workspace's conversation, so the
        // outcome is `unknown` — and the send is still never replayed.
        let tampered = |damage: &dyn Fn(&mut Map<String, Value>)| {
            let mut receipt = send_receipt("lost-receipt");
            damage(receipt.as_object_mut().unwrap());
            receipt
        };
        let receipts = [
            tampered(&|r| {
                r.remove("workspace_id");
            }),
            tampered(&|r| {
                r["workspace_id"] = json!("ws_foreign");
            }),
            tampered(&|r| {
                r["workspace_id"] = json!("");
            }),
            tampered(&|r| {
                r.remove("sender_subject");
            }),
            tampered(&|r| {
                r["sender_subject"] = json!("");
            }),
            tampered(&|r| {
                r.remove("conversation");
            }),
            tampered(&|r| {
                r["conversation"] = json!("");
            }),
            tampered(&|r| {
                r["alias"] = json!("assistant-personal");
            }),
            tampered(&|r| {
                r.remove("sender");
            }),
            tampered(&|r| {
                r["sender"] = json!("");
            }),
            tampered(&|r| {
                r["duplicate"] = json!("yes");
            }),
            tampered(&|r| {
                r["state"] = json!("delivered");
            }),
        ];
        for receipt in receipts {
            let (out, requests) = run(
                vec![(200, "", receipt)],
                "message_send",
                json!({"alias": "master", "text": "coordinate", "message": "lost-receipt"})
                    .as_object()
                    .unwrap()
                    .clone(),
                10,
            );
            assert_eq!(out.unwrap_err().kind(), "unknown");
            assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
        }
    }

    #[test]
    fn send_without_expected_message_identity_is_unknown_and_not_retried() {
        let mut receipt = send_receipt("lost-receipt");
        receipt.as_object_mut().unwrap().remove("message");
        let (out, requests) = run(
            vec![(200, "", receipt)],
            "message_send",
            json!({"alias": "master", "text": "coordinate"})
                .as_object()
                .unwrap()
                .clone(),
            10,
        );
        assert_eq!(out.unwrap_err().kind(), "unknown");
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
    }

    #[test]
    fn send_receipt_for_another_subject_is_unknown_and_not_retried() {
        // A login that recorded its canonical subject binds the receipt to
        // it: a 2xx answering a different `sender_subject` cannot be this
        // send's enqueue proof.
        let (listener, requests, _s) = fake_worker(vec![(200, "", send_receipt("lost-receipt"))]);
        let target = target(&listener);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        store_credential(&dir, &target.endpoint);
        let path = dir.join(format!("cli-{ORG}.json"));
        let mut cred: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        cred["subject_id"] = json!("sub_the_login");
        fs::write(&path, serde_json::to_vec(&cred).unwrap()).unwrap();
        let out = call_verb_in(
            &target,
            "message_send",
            json!({"alias": "master", "text": "coordinate", "message": "lost-receipt"})
                .as_object()
                .unwrap()
                .clone(),
            10,
            &dir,
            |_| {},
            |_| {},
        );
        assert_eq!(out.unwrap_err().kind(), "unknown");
        assert_eq!(
            hits(&requests.lock().unwrap(), "/__platform/cli/call"),
            1,
            "an unknown send outcome was replayed"
        );
    }

    #[test]
    fn a_send_wake_retry_keeps_its_durable_message_id() {
        // A pre-forward wake retry needs a fresh envelope but must not
        // mint another durable message id.
        let args: Map<String, Value> = json!({
            "alias": "master",
            "text": "please coordinate",
            "message": "cad1568-proof-1",
        })
        .as_object()
        .unwrap()
        .clone();
        let (out, requests) = run(
            vec![
                (503, "", json!({"state": "waking", "retry_after_s": 1})),
                (200, "", send_receipt("cad1568-proof-1")),
            ],
            "message_send",
            args,
            120,
        );
        out.unwrap();
        let mut calls = 0;
        for request in requests
            .iter()
            .filter(|r| r["path"] == json!("/__platform/cli/call"))
        {
            calls += 1;
            assert_eq!(request["body"]["arguments"]["message"], "cad1568-proof-1");
        }
        assert_eq!(calls, 2);
        let envs = envelopes(&requests);
        assert_ne!(envs[0], envs[1], "a send retry reused the envelope");
    }

    #[test]
    fn cli_message_refusals_map_bounded_codes_never_server_text() {
        // CAD-1568: the container's `cli_fail` body is
        // `{ok:false,error:{code,message}}` and `message` is arbitrary
        // server text — the client answers the bounded code with its own
        // actionable text, and the server string never surfaces.
        let poisonous = |code: &str| {
            json!({"ok": false, "error": {
                "code": code,
                "message": "internal-server-text /private/path"}})
        };
        for (verb, status, code) in [
            ("message_send", 400u16, "recipient_class_unsupported"),
            ("message_send", 400, "message_id_conflict"),
            ("message_send", 403, "not_permitted"),
            ("message_read", 404, "not_found"),
        ] {
            let (out, requests) = run(vec![(status, "", poisonous(code))], verb, Map::new(), 10);
            let err = out.unwrap_err();
            let shown = err.to_string();
            assert_eq!(
                err.code(),
                Some(code),
                "{verb}: machine-readable refusal code changed"
            );
            assert!(
                !shown.contains("internal-server-text") && !shown.contains("/private/path"),
                "{verb} {code} leaked server text: {shown}"
            );
            assert_eq!(
                hits(&requests, "/__platform/cli/call"),
                1,
                "a refusal was retried"
            );
        }
        // An unlisted code on a message verb keeps the generic refusal —
        // never a fabricated action.
        let (out, _) = run(
            vec![(400, "", poisonous("some_future_code"))],
            "message_send",
            Map::new(),
            10,
        );
        let shown = out.unwrap_err().to_string();
        assert!(shown.contains("unknown"), "{shown}");
        assert!(!shown.contains("internal-server-text"), "{shown}");
    }

    #[test]
    fn a_checked_conflict_rides_up_as_the_conflict_answer() {
        // A 409 `{conflict, current_rev, card}` is the remote's checked
        // "not written" verdict — the body must reach the caller (the
        // CLI prints it and exits 5), not collapse into a generic
        // refusal. One attempt only: a conflict is never retried.
        let body = json!({"conflict": "if_rev",
            "current_rev": "fnv1a:829ba77079943a03",
            "card": {"id": "CAD-1"},
            "error": "if_rev does not match issue.md — re-read and retry"});
        let (listener, requests, _s) = fake_worker(vec![(409, "", body)]);
        let target = target(&listener);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        store_credential(&dir, &target.endpoint);
        let out = call_verb_in(&target, "issue_set", Map::new(), 10, &dir, |_| {}, |_| {}).unwrap();
        let RemoteAnswer::Conflict(body) = out else {
            panic!("a 409 conflict answer must surface as Conflict, got {out:?}");
        };
        assert_eq!(body["current_rev"], json!("fnv1a:829ba77079943a03"));
        assert_eq!(body["card"]["id"], json!("CAD-1"));
        assert_eq!(
            hits(&requests.lock().unwrap(), "/__platform/cli/call"),
            1,
            "a conflict was retried"
        );
    }

    #[test]
    fn a_bare_409_without_the_conflict_shape_is_a_generic_refusal() {
        // Only the contract's `{conflict: …}` payload is a verdict; a
        // 409 carrying anything else stays the generic refusal.
        let (out, requests) = run(
            vec![(409, "", json!({"error": "midway collision"}))],
            "issue_set",
            Map::new(),
            10,
        );
        // CAD-1434: decoded, not a bare generic message. It shows the safe
        // category, never the raw body, and is never retried.
        let shown = out.unwrap_err().to_string();
        assert!(shown.contains("unknown"), "{shown}");
        assert!(!shown.contains("midway collision"), "{shown}");
        assert!(!shown.contains("cadence login"), "{shown}");
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
    }

    #[test]
    fn a_bare_503_is_availability_and_is_not_retried_or_login() {
        // A 503 without a waking state is terminal for this command: retry
        // later, keep the saved login, never ask for a fresh one.
        let (out, requests) = run(
            vec![(503, "", json!({"state": "board_unreachable"}))],
            "issue_set",
            Map::new(),
            10,
        );
        let shown = out.unwrap_err().to_string();
        assert!(shown.contains("unavailable"), "{shown}");
        assert!(shown.contains("retry later"), "{shown}");
        assert!(!shown.contains("cadence login"), "{shown}");
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "a bare 503 was retried"
        );
    }

    /// The exact Cadence container refusal (`src/ui/wiki_envelope.rs`): a path
    /// outside the signed grant is a 403 `insufficient_scope` with a message.
    /// On the call path it is authorization, shows its safe code, asks for no
    /// login, is sent once, and never echoes the body.
    #[test]
    fn a_container_403_insufficient_scope_is_authorization_on_the_call_path() {
        let body = json!({"ok": false, "error": {"code": "insufficient_scope",
            "message": "the path is outside this envelope's wiki write prefix do-not-print"}});
        let (out, requests) = run(vec![(403, "", body)], "issue_set", Map::new(), 10);
        let shown = out.unwrap_err().to_string();
        assert!(shown.contains("authorization"), "{shown}");
        assert!(shown.contains("insufficient_scope"), "{shown}");
        assert!(!shown.contains("cadence login"), "{shown}");
        assert!(!shown.contains("do-not-print"), "{shown}");
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "a container 403 was retried"
        );
    }

    /// A generic 403 (no safe code) is still an access denial: conservative
    /// authorization guidance, never a login claim or an unknown-shape answer.
    #[test]
    fn a_generic_403_is_conservative_access_denial_not_login() {
        let (refusal, reason) = decode_refusal(403, Some(&json!({"error": {"code": "forbidden"}})));
        assert_eq!(refusal, Refusal::Authorization);
        assert_eq!(reason, None);
        let text = refusal_message(refusal, reason.as_deref(), 403);
        assert!(text.contains("not permitted"), "{text}");
        assert!(!text.contains("cadence login"), "{text}");
        let bare = decode_refusal(403, None);
        assert_eq!(bare.0, Refusal::Authorization);
        assert!(!refusal_message(bare.0, bare.1.as_deref(), 403).contains("cadence login"));
    }

    /// The container's `insufficient_scope` is authorization only on 403. The same
    /// code on a 401 is not trusted as a policy proof, so it stays uncertain.
    #[test]
    fn insufficient_scope_is_authorization_only_on_403() {
        let body = json!({"error": {"code": "insufficient_scope"}});
        assert_eq!(decode_refusal(403, Some(&body)).0, Refusal::Authorization);
        assert_eq!(decode_refusal(401, Some(&body)).0, Refusal::Unknown);
    }

    /// The same container refusal on the blob upload path: one upload request,
    /// safe code shown, no login advice, the raw body never printed.
    #[test]
    fn a_container_403_insufficient_scope_is_authorization_on_the_upload_path() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let (listener, handle) = upload_fake(
            403,
            r#"{"ok":false,"error":{"code":"insufficient_scope","message":"outside prefix do-not-print"}}"#,
        );
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        store_credential(dir, &endpoint);
        let target = RemoteTarget {
            identity: None,
            org: ORG.into(),
            endpoint,
            org_id: ORG_ID.into(),
        };
        let err = upload_file_in(
            dir,
            &target,
            "projects/x/a.pdf",
            "a.pdf",
            b"%PDF-bytes",
            true,
            5,
            |_| {},
        )
        .unwrap_err();
        let _ = handle.join().unwrap();
        let shown = err.to_string();
        assert!(shown.contains("authorization"), "{shown}");
        assert!(shown.contains("insufficient_scope"), "{shown}");
        assert!(!shown.contains("cadence login"), "{shown}");
        assert!(!shown.contains("do-not-print"), "{shown}");
    }

    #[test]
    fn a_400_shows_its_safe_code_and_is_not_retried() {
        // A payload/validation refusal keeps its safe code so the caller sees
        // what was refused. It is not a login or availability claim.
        let (out, requests) = run(
            vec![(
                400,
                "",
                json!({"ok": false, "error": {"code": "payload_too_large"}}),
            )],
            "issue_set",
            Map::new(),
            10,
        );
        let shown = out.unwrap_err().to_string();
        assert!(shown.contains("payload_too_large"), "{shown}");
        assert!(!shown.contains("cadence login"), "{shown}");
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "a 400 was retried"
        );
    }

    /// The exact AOS gateway refusal (`routes.ts` `gateRefusal`): 401, code
    /// `unauthorized`, plus the proven policy reason. Each is driven through the
    /// real call path (`call_verb_in` -> `call_once` -> fake relay), not only the
    /// decoder.
    fn aos_refusal(reason: &str) -> Value {
        json!({"ok": false, "error": {"code": "unauthorized",
            "message": "Wiki call refused.", "reason": reason}})
    }

    #[test]
    fn a_policy_401_scope_not_granted_never_recommends_login_and_is_not_retried() {
        // Paired AOS shape: 401 unauthorized + reason scope_not_granted is a
        // policy outcome. It must not say "cadence login", and exactly one
        // attempt is sent (no retry, no mutation replay).
        let (out, requests) = run(
            vec![(401, "", aos_refusal("scope_not_granted"))],
            "issue_set",
            Map::new(),
            10,
        );
        let shown = out.unwrap_err().to_string();
        assert!(!shown.contains("cadence login"), "{shown}");
        assert!(shown.contains("authorization"), "{shown}");
        assert!(shown.contains("scope_not_granted"), "{shown}");
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "a policy 401 was retried"
        );
    }

    #[test]
    fn every_allowlisted_policy_reason_is_authorization_on_the_real_path() {
        for reason in ["scope_not_granted", "grant_narrowed", "binding_mismatch"] {
            let (out, requests) = run(
                vec![(401, "", aos_refusal(reason))],
                "issue_set",
                Map::new(),
                10,
            );
            let shown = out.unwrap_err().to_string();
            assert!(!shown.contains("cadence login"), "{reason}: {shown}");
            assert!(shown.contains(reason), "{reason}: {shown}");
            assert_eq!(
                hits(&requests, "/__platform/cli/call"),
                1,
                "{reason} retried"
            );
        }
    }

    #[test]
    fn envelope_invalid_is_not_terminal_session_loss_and_is_not_retried() {
        // envelope_invalid (expired/legacy envelope) is neither policy nor a
        // proven session loss. It must not claim expiry or tell the user to log in.
        let (out, requests) = run(
            vec![(401, "", aos_refusal("envelope_invalid"))],
            "status",
            Map::new(),
            10,
        );
        let shown = out.unwrap_err().to_string();
        assert!(!shown.contains("cadence login"), "{shown}");
        assert!(shown.contains("did not confirm"), "{shown}");
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "an envelope refusal was retried"
        );
    }

    #[test]
    fn a_generic_unauthorized_without_reason_is_uncertain_not_confirmed_expiry() {
        // A 401 unauthorized with no reason means the bearer did not resolve, but
        // the server cannot tell expiry from revocation. It is uncertain: no
        // confirmed-expiry claim, one attempt, read-only diagnosis advised.
        let (out, requests) = run(
            vec![(
                401,
                "",
                json!({"ok": false, "error": {"code": "unauthorized"}}),
            )],
            "status",
            Map::new(),
            10,
        );
        let shown = out.unwrap_err().to_string();
        assert!(shown.contains("did not confirm"), "{shown}");
        assert!(shown.contains("read-only diagnosis"), "{shown}");
        assert!(!shown.contains("has expired"), "{shown}");
        assert_eq!(
            hits(&requests, "/__platform/cli/call"),
            1,
            "a generic 401 was retried"
        );
    }

    #[test]
    fn a_non_json_401_is_uncertain_and_never_echoes_the_body() {
        // A gateway or ingress 401 that is not the envelope (HTML, empty) must not
        // be read as confirmed expiry, and its body must not be echoed.
        let (refusal, reason) = decode_refusal(401, None);
        assert_eq!(refusal, Refusal::Uncertain);
        assert_eq!(reason, None);
        let text = refusal_message(refusal, reason.as_deref(), 401);
        assert!(text.contains("did not confirm"), "{text}");
        assert!(!text.contains("<html"), "{text}");
    }

    #[test]
    fn only_terminal_issuer_and_session_codes_ask_for_login() {
        for code in [
            "invalid_token",
            "invalid_grant",
            "session_expired",
            "session_revoked",
            "lineage_terminal",
        ] {
            let (refusal, _) = decode_refusal(401, Some(&json!({"error": {"code": code}})));
            assert_eq!(refusal, Refusal::Authentication, "{code}");
            let text = refusal_message(refusal, None, 401);
            assert!(text.contains("cadence login"), "{code}: {text}");
        }
    }

    #[test]
    fn an_unknown_policy_looking_reason_is_not_trusted() {
        // Only the allowlisted reasons count. A free-form or unknown reason is
        // dropped, so it can neither assert policy nor leak into the message.
        let (refusal, reason) = decode_refusal(
            401,
            Some(&json!({"error": {"code": "unauthorized", "reason": "some raw server text"}})),
        );
        assert_eq!(reason, None);
        assert_eq!(refusal, Refusal::Uncertain);
    }

    #[test]
    fn a_board_refusal_shows_its_hop_and_request_id_and_never_the_raw_body() {
        // The AOS board names its hop and a fresh UUID. Both reach the message;
        // the raw body and any extra server text never do.
        let id = "0b2e1f4a-7c3d-4e5f-8a9b-1c2d3e4f5a6b";
        let body = json!({"ok": false, "error": {"code": "unauthorized",
            "reason": "scope_not_granted", "hop": "board", "request_id": id,
            "message": "raw server text do-not-print"}});
        let (out, requests) = run(vec![(401, "", body)], "issue_set", Map::new(), 10);
        let shown = out.unwrap_err().to_string();
        assert!(shown.contains("hop board"), "{shown}");
        assert!(shown.contains(id), "{shown}");
        assert!(!shown.contains("do-not-print"), "{shown}");
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
    }

    #[test]
    fn an_unlisted_hop_or_a_non_uuid_request_id_is_dropped() {
        for body in [
            json!({"error": {"code": "unauthorized", "hop": "evil.example",
                "request_id": "0b2e1f4a-7c3d-4e5f-8a9b-1c2d3e4f5a6b"}}),
            json!({"error": {"code": "unauthorized", "hop": "board",
                "request_id": "not a uuid <script>"}}),
        ] {
            assert_eq!(refusal_correlation(Some(&body)), None, "{body}");
        }
        // A legacy envelope with no correlation still decodes exactly as before.
        assert_eq!(
            refusal_correlation(Some(&json!({"error": {"code": "unauthorized"}}))),
            None
        );
    }

    #[test]
    fn a_renewal_refusal_uses_the_issuer_shape_not_the_gateway_envelope() {
        // Renewal reads the issuer's {error:"<code>"}. Only its terminal
        // refresh-grant refusals ask for login; a transient 503 keeps the login.
        let terminal = renewal_refusal(401, &json!({"error": "invalid_token"}), None);
        assert!(terminal.to_string().contains("cadence login"), "{terminal}");
        let transient = renewal_refusal(503, &json!({"error": "authority_unavailable"}), None);
        assert!(
            !transient.to_string().contains("cadence login"),
            "{transient}"
        );
        assert!(transient.to_string().contains("kept"), "{transient}");
    }

    #[test]
    fn a_transient_503_is_availability_and_keeps_saved_login() {
        let (refusal, _) = decode_refusal(503, Some(&json!({"state": "wake_failed"})));
        assert_eq!(refusal, Refusal::Availability);
        let text = refusal_message(refusal, None, 503);
        assert!(text.contains("retry later"), "{text}");
        assert!(text.contains("kept"), "{text}");
        assert!(!text.contains("cadence login"), "{text}");
    }

    #[test]
    fn local_status_is_offline_redacted_and_never_claims_live_verification() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        let endpoint = "https://acme.cadencecloud.app";
        let target = RemoteTarget {
            identity: None,
            org: ORG.into(),
            endpoint: endpoint.into(),
            org_id: ORG_ID.into(),
        };
        let at = now().unwrap();
        let body = json!({"version": "hosted-cadence-auth.v1", "organization_id": ORG_ID,
            "audience": endpoint, "access_token": BRIDGE, "expires_at": at + 300,
            "capabilities": ["cli.read", "cli.write"],
            "cli_session": {"session_id": "hcs_12345678-1234-4234-8234-123456789abc",
                "refresh_token": "hcr_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "generation": 1, "device_label": "cadence CLI",
                "idle_expires_at": at + 1000, "absolute_expires_at": at + 10_000}});
        std::fs::write(dir.join(format!("cli-{ORG}.json")), body.to_string()).unwrap();
        std::fs::set_permissions(
            dir.join(format!("cli-{ORG}.json")),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();
        let status = local_status_at(&dir, &target);
        assert_eq!(status["state"], "renewable");
        assert_eq!(status["live_verified"], false);
        assert_eq!(status["scopes"], json!(["cli.read", "cli.write"]));
        let text = status.to_string();
        assert!(!text.contains(BRIDGE), "{text}");
        assert!(!text.contains("hcr_AAAA"), "{text}");
        assert!(!text.contains("hct_"), "{text}");
        // Freshness is a local observation, not renewal health.
        assert!(status["observed_at"].as_u64().is_some());
        assert!(status["stored_at"].as_u64().is_some());
        assert!(status.get("renewal_health").is_none(), "{text}");
    }

    #[test]
    fn cad1434_blocker_local_status_legacy_expiry_keeps_command_refusal() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let target = target(&listener);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join(format!("cli-{ORG}.json"));
        let at = now().unwrap();
        store_credential_at(dir, ORG, ORG_ID, &target.endpoint, at + 300);
        assert_eq!(
            local_status_at(dir, &target)["state"],
            "access_valid_until_expiry"
        );
        assert!(load_credential(dir, &target).is_ok());

        store_credential_at(dir, ORG, ORG_ID, &target.endpoint, at - 1);
        let mut body: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        body["capabilities"] = json!(["cli.read", "cli.write"]);
        fs::write(&path, body.to_string()).unwrap();
        let status = local_status_at(dir, &target);
        assert_eq!(status["state"], "reauthentication_required");
        assert_eq!(status["access_expires_at"], at - 1);
        assert_eq!(status["scopes"], json!(["cli.read", "cli.write"]));
        assert_eq!(status["live_verified"], false);
        assert!(status["observed_at"].as_u64().is_some());
        let text = status.to_string();
        assert!(!text.contains("hct_"), "{text}");
        assert!(!text.contains("hcr_"), "{text}");
        assert!(!text.contains(dir.to_string_lossy().as_ref()), "{text}");
        assert_eq!(
            load_credential(dir, &target).err().unwrap().to_string(),
            "the remote credential has expired — run `cadence login` again"
        );

        for (key, value) in [
            ("version", json!("invalid")),
            ("organization_id", json!("ws_other")),
            ("audience", json!("https://other.cadencecloud.app")),
            ("access_token", json!("invalid")),
            ("unexpected", json!(true)),
        ] {
            let mut invalid = body.clone();
            invalid[key] = value;
            fs::write(&path, invalid.to_string()).unwrap();
            assert_eq!(
                local_status_at(dir, &target)["state"],
                "invalid_local_credential",
                "{key}"
            );
            assert!(load_credential(dir, &target).is_err());
        }
        fs::write(&path, "not JSON").unwrap();
        assert_eq!(
            local_status_at(dir, &target)["state"],
            "invalid_local_credential"
        );
        fs::write(&path, body.to_string()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            local_status_at(dir, &target)["state"],
            "invalid_local_credential"
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            local_status_at(dir, &target)["state"],
            "invalid_local_credential"
        );
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        let original = dir.join("saved.json");
        fs::rename(&path, &original).unwrap();
        std::os::unix::fs::symlink(&original, &path).unwrap();
        assert_eq!(
            local_status_at(dir, &target)["state"],
            "invalid_local_credential"
        );
        assert!(load_credential(dir, &target).is_err());
        assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    }

    #[test]
    fn a_permission_error_is_unavailable_never_no_credential() {
        // Only a missing file is absence. A stat failure (here: the credential
        // directory is unreadable) is unavailable, not proof that nothing is stored.
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o000))
            .unwrap();
        let target = RemoteTarget {
            identity: None,
            org: ORG.into(),
            endpoint: "https://acme.cadencecloud.app".into(),
            org_id: ORG_ID.into(),
        };
        let status = local_status_at(&dir, &target);
        // Restore before asserting so the tempdir can be removed either way.
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        assert_ne!(status["state"], "no_credential", "{status}");
        assert!(
            matches!(
                status["state"].as_str(),
                Some("unavailable" | "invalid_local_credential")
            ),
            "{status}"
        );
    }

    #[test]
    fn an_expired_session_keeps_its_expiry_fields() {
        // The expired branch must still report the session's idle/absolute
        // expiry, so a reader sees why renewal is no longer possible.
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        let endpoint = "https://acme.cadencecloud.app";
        let target = RemoteTarget {
            identity: None,
            org: ORG.into(),
            endpoint: endpoint.into(),
            org_id: ORG_ID.into(),
        };
        let at = now().unwrap();
        let body = json!({"version": "hosted-cadence-auth.v1", "organization_id": ORG_ID,
            "audience": endpoint, "access_token": BRIDGE, "expires_at": at - 10,
            "capabilities": ["cli.read", "cli.write"],
            "cli_session": {"session_id": "hcs_12345678-1234-4234-8234-123456789abc",
                "refresh_token": "hcr_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "generation": 1, "device_label": "cadence CLI",
                "idle_expires_at": at - 5, "absolute_expires_at": at - 1}});
        let path = dir.join(format!("cli-{ORG}.json"));
        std::fs::write(&path, body.to_string()).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .unwrap();
        let status = local_status_at(&dir, &target);
        assert_eq!(status["state"], "reauthentication_required");
        assert_eq!(status["session_idle_expires_at"], json!(at - 5));
        assert_eq!(status["session_absolute_expires_at"], json!(at - 1));
        assert_eq!(status["live_verified"], false);
    }

    #[test]
    fn an_unsafe_server_code_is_dropped_from_the_decoded_reason() {
        // Free text or wrong case from a server never becomes the shown reason.
        let (refusal, reason) = decode_refusal(
            418,
            Some(&json!({"error": {"code": "Bad Code With Spaces"}})),
        );
        assert_eq!(refusal, Refusal::Unknown);
        assert_eq!(reason, None);
        let text = refusal_message(refusal, reason.as_deref(), 418);
        assert!(!text.contains("Bad Code"), "{text}");
    }

    #[test]
    fn a_cli_only_login_stays_valid_and_a_wiki_verb_needs_its_scope() {
        // CAD-1401 repair: the stored CLI pair is a valid login and its renewal
        // keeps exactly that set; a wiki verb on it is refused before any request.
        let cli: Vec<String> = vec!["cli.read".into(), "cli.write".into()];
        assert!(valid_capabilities(&cli));
        assert!(!valid_capabilities(&["cli.read".into()]));
        assert!(same_capabilities_json(
            &json!(["cli.read", "cli.write"]),
            &cli
        ));
        assert!(!same_capabilities_json(
            &json!(["cli.read", "cli.write", "wiki.read"]),
            &cli
        ));

        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            root.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let target = RemoteTarget {
            identity: None,
            org: ORG.into(),
            endpoint: "http://127.0.0.1:9".into(),
            org_id: ORG_ID.into(),
        };
        let body = json!({"version": "hosted-cadence-auth.v1", "organization_id": ORG_ID,
            "audience": target.endpoint, "capabilities": cli,
            "access_token": BRIDGE, "expires_at": now().unwrap() + 300});
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(root.path().join(format!("cli-{ORG}.json")))
            .unwrap();
        file.write_all(body.to_string().as_bytes()).unwrap();
        let mut args = Map::new();
        args.insert("path".into(), json!("global/"));
        let err =
            call_verb_in(&target, "enumerate", args, 10, root.path(), |_| {}, |_| {}).unwrap_err();
        assert!(err.to_string().contains("wiki.read capability"), "{err}");
        assert!(err.to_string().contains("nothing was sent"), "{err}");
    }

    /// CAD-913: a fake relay that answers one authorize and one upload. It
    /// records the upload's raw multipart bytes and bearer so the test checks
    /// the wire form itself: the contract fields, one `file` part, the bearer,
    /// and a refusal that prints only status and code.
    fn upload_fake(
        upload_status: u16,
        upload_body: &'static str,
    ) -> (TcpListener, thread::JoinHandle<(Vec<u8>, String, String)>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = listener.try_clone().unwrap();
        server.set_nonblocking(true).unwrap();
        let handle = thread::spawn(move || {
            let mut seen_upload = (Vec::new(), String::new(), String::new());
            // Bounded: the client may refuse before the second request, so the
            // fake gives up on its own instead of hanging the test.
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut answered = 0;
            while answered < 2 && Instant::now() < deadline {
                let socket = match server.accept() {
                    Ok((socket, _)) => socket,
                    Err(_) => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split(' ').nth(1).unwrap().to_string();
                let mut bearer = String::new();
                let mut envelope_header = String::new();
                let mut length = 0_usize;
                let mut content_type = String::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    let (key, value) = line.split_once(':').unwrap();
                    if key.eq_ignore_ascii_case("authorization") {
                        bearer = value.trim().trim_start_matches("Bearer ").to_string();
                    }
                    if key.eq_ignore_ascii_case("x-cadence-wiki-envelope") {
                        envelope_header = value.trim().to_string();
                    }
                    if key.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                    if key.eq_ignore_ascii_case("content-type") {
                        content_type = value.trim().to_string();
                    }
                }
                let mut raw = vec![0; length];
                reader.read_exact(&mut raw).unwrap();
                answered += 1;
                if path == "/__platform/wiki/authorize" {
                    let env = "eyJhIjoxfQ.eyJiIjoyfQ.c2ln";
                    answer(
                        socket,
                        "200 OK",
                        "",
                        &json!({"version": WIKI_VERSION, "envelope": env}),
                    );
                } else {
                    assert_eq!(path, "/__platform/wiki/upload");
                    assert!(content_type.starts_with("multipart/form-data; boundary="));
                    seen_upload = (raw, bearer, envelope_header);
                    let body: Value = serde_json::from_str(upload_body).unwrap_or(json!({}));
                    answer(socket, &format!("{upload_status} X"), "", &body);
                }
            }
            seen_upload
        });
        (listener, handle)
    }

    #[test]
    fn upload_sends_one_multipart_body_and_a_refusal_shows_only_the_code() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        // The overwrite pre-check is off here: the test drives the upload
        // request itself, and the listing path is covered by `overwrite`.
        let (listener, handle) =
            upload_fake(409, r#"{"code":"too_large","secret":"do-not-print"}"#);
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        store_credential(dir, &endpoint);
        let target = RemoteTarget {
            identity: None,
            org: ORG.into(),
            endpoint,
            org_id: ORG_ID.into(),
        };
        let err = upload_file_in(
            dir,
            &target,
            "sources/a.pdf",
            "a.pdf",
            b"%PDF-bytes",
            true,
            5,
            |_| {},
        )
        .unwrap_err();
        let (raw, bearer, envelope_header) = handle.join().unwrap();
        // The bearer rides the Authorization header and the envelope rides
        // X-Cadence-Wiki-Envelope. The body carries the contract fields and
        // the single file part with the raw bytes, and no envelope field.
        assert_eq!(bearer, BRIDGE);
        assert_eq!(envelope_header, "eyJhIjoxfQ.eyJiIjoyfQ.c2ln");
        let text = String::from_utf8_lossy(&raw);
        for needle in [
            "name=\"version\"\r\n\r\nhosted-cadence-wiki.v1",
            "name=\"action\"\r\n\r\nwrite_blob",
            "name=\"path\"\r\n\r\nsources/a.pdf",
            "filename=\"a.pdf\"",
        ] {
            assert!(text.contains(needle), "missing {needle}");
        }
        assert!(!text.contains("name=\"envelope\""), "envelope in body");
        assert!(!text.contains("eyJhIjoxfQ"), "envelope in body");
        assert_eq!(text.matches("name=\"file\"").count(), 1);
        // A refusal names the status and code and never the raw body.
        let shown = err.to_string();
        assert!(
            shown.contains("409") && shown.contains("too_large"),
            "{shown}"
        );
        assert!(!shown.contains("do-not-print"), "{shown}");
    }

    /// CAD-913: the overwrite check fails closed on any listing that may be
    /// partial, and reads a plain listing exactly.
    #[test]
    fn overwrite_check_fails_closed_on_a_possibly_partial_listing() {
        let plain = json!({"path": "sources", "entries": [{"name": "a.pdf"}]});
        assert_eq!(listing_taken(&plain, "a.pdf"), Ok(true));
        assert_eq!(listing_taken(&plain, "b.pdf"), Ok(false));
        for paged in [
            json!({"path": "sources", "entries": [], "truncated": true}),
            json!({"path": "sources", "entries": [], "next_cursor": "c2"}),
            json!({"path": "sources", "entries": [], "has_more": true}),
        ] {
            assert!(listing_taken(&paged, "a.pdf").is_err(), "{paged}");
        }
        // A missing or non-array `entries` cannot prove the name is free.
        assert!(listing_taken(&json!({"path": "sources"}), "a.pdf").is_err());
        // Any extra key is refused, even one that reads as "complete".
        let done = json!({"path": "sources", "entries": [], "truncated": false});
        assert!(listing_taken(&done, "a.pdf").is_err());
        // An unknown paging key that carries an existing name is refused too.
        let token = json!({"path": "sources", "entries": [], "next_token": "t2"});
        assert!(listing_taken(&token, "a.pdf").is_err());
        // A non-object body cannot prove the name is free.
        assert!(listing_taken(&json!([{"name": "a.pdf"}]), "a.pdf").is_err());
    }

    /// CAD-913 overwrite guard: an existing destination without `--overwrite`
    /// is refused from the listing alone. The fake records every request path.
    /// Authorize and the listing are reads and may be sent; the test asserts
    /// the upload endpoint is never reached.
    #[test]
    fn overwrite_refusal_sends_no_upload_request() {
        let (message, order) = refused_listing_fixture(json!({
            "path": "sources",
            "entries": [{"name": "a.pdf"}]
        }));
        assert!(message.contains("already exists"), "{message}");
        assert!(message.contains("Nothing was sent"), "{message}");
        // Order: authorize, then the listing call. No upload request at all.
        assert_eq!(
            order.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
            ["/__platform/wiki/authorize", "/__platform/wiki/call"]
        );
        assert_eq!(order[1].1, "enumerate");
    }

    /// CAD-913: an unknown paging key on the listing refuses the upload even
    /// when the existing name is on that page, and sends no upload request.
    #[test]
    fn unknown_paging_key_refuses_without_sending_upload() {
        let (message, order) = refused_listing_fixture(json!({
            "path": "sources",
            "entries": [{"name": "a.pdf"}],
            "next_token": "page-2"
        }));
        assert!(message.contains("may be incomplete"), "{message}");
        assert!(message.contains("Nothing was sent"), "{message}");
        assert!(!message.contains("page-2"), "{message}");
        assert!(
            !order.iter().any(|(p, _)| p.ends_with("/upload")),
            "upload reached: {order:?}"
        );
    }

    /// Runs `upload_file_in` without `--overwrite` against a fake whose listing
    /// answer is `listing`. Returns the refusal message and every request seen.
    fn refused_listing_fixture(listing: Value) -> (String, Vec<(String, String)>) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = listener.try_clone().unwrap();
        server.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_fake = Arc::clone(&stop);
        let fake = thread::spawn(move || {
            let mut paths: Vec<(String, String)> = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !stop_fake.load(Ordering::SeqCst) && Instant::now() < deadline {
                let socket = match server.accept() {
                    Ok((socket, _)) => socket,
                    Err(_) => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split(' ').nth(1).unwrap().to_string();
                let mut length = 0_usize;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    let (key, value) = line.split_once(':').unwrap();
                    if key.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut raw = vec![0; length];
                reader.read_exact(&mut raw).unwrap();
                let action = serde_json::from_slice::<Value>(&raw)
                    .ok()
                    .and_then(|b| b["action"].as_str().map(str::to_string))
                    .unwrap_or_default();
                paths.push((path.clone(), action));
                if path == "/__platform/wiki/authorize" {
                    answer(
                        socket,
                        "200 OK",
                        "",
                        &json!({"version": WIKI_VERSION, "envelope": "eyJhIjoxfQ.eyJiIjoyfQ.c2ln"}),
                    );
                } else if path == "/__platform/wiki/call" {
                    answer(socket, "200 OK", "", &listing);
                } else {
                    answer(socket, "500 X", "", &json!({}));
                }
            }
            paths
        });
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        store_credential(dir, &endpoint);
        let target = RemoteTarget {
            identity: None,
            org: ORG.into(),
            endpoint,
            org_id: ORG_ID.into(),
        };
        let err = upload_file_in(
            dir,
            &target,
            "sources/a.pdf",
            "a.pdf",
            b"%PDF-bytes",
            false,
            5,
            |_| {},
        )
        .unwrap_err();
        stop.store(true, Ordering::SeqCst);
        let paths = fake.join().unwrap();
        (err.to_string(), paths)
    }
    fn credentialed(listener: &TcpListener) -> (RemoteTarget, tempfile::TempDir, PathBuf) {
        let target = target(listener);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        store_credential(&dir, &target.endpoint);
        (target, root, dir)
    }

    fn write_answer(socket: &mut std::net::TcpStream, status: &str, extra: &str, body: &Value) {
        let bytes = serde_json::to_vec(body).unwrap();
        let _ = write!(
            socket,
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n",
            bytes.len()
        );
        let _ = socket.write_all(&bytes);
        let _ = socket.flush();
    }

    fn finite_worker(
        n: usize,
        serve: impl Fn(usize, &mut std::net::TcpStream, &Request) + Send + 'static,
    ) -> (
        TcpListener,
        std::sync::Arc<std::sync::Mutex<Vec<Request>>>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = listener.try_clone().unwrap();
        let seen: std::sync::Arc<std::sync::Mutex<Vec<Request>>> = Default::default();
        let requests = seen.clone();
        let handle = thread::spawn(move || {
            for i in 0..n {
                let Ok((socket, _)) = server.accept() else {
                    return;
                };
                let (mut socket, req) = {
                    let (s, r) = read_request(socket);
                    (s, r)
                };
                requests.lock().unwrap().push(clone_request(&req));
                serve(i, &mut socket, &req);
            }
        });
        (listener, seen, handle)
    }

    fn clone_request(req: &Request) -> Request {
        Request {
            path: req.path.clone(),
            bearer: req.bearer.clone(),
            body: req.body.clone(),
        }
    }

    /// Read an HTTP request without assuming its body is JSON; uploads are
    /// multipart and the server fixture must observe the bytes as sent.
    fn read_raw_request(socket: std::net::TcpStream) -> (std::net::TcpStream, String, Vec<u8>) {
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let path = line.split(' ').nth(1).unwrap().to_string();
        let mut length = 0_usize;
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                if key.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
            }
        }
        let mut raw = vec![0; length];
        reader.read_exact(&mut raw).unwrap();
        (socket, path, raw)
    }

    /// Bounded loopback fixture for upload deadline tests. A quiet half-second
    /// after the last request ends the fixture, including when the client
    /// correctly refuses before sending a later phase.
    fn upload_deadline_worker(
        serve: impl Fn(&str, &mut std::net::TcpStream, &[u8]) + Send + 'static,
    ) -> (
        TcpListener,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = listener.try_clone().unwrap();
        server.set_nonblocking(true).unwrap();
        let paths = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = paths.clone();
        let handle = thread::spawn(move || {
            let overall = Instant::now() + Duration::from_secs(8);
            let mut last_request = Instant::now();
            while Instant::now() < overall {
                match server.accept() {
                    Ok((socket, _)) => {
                        let (mut socket, path, raw) = read_raw_request(socket);
                        observed.lock().unwrap().push(path.clone());
                        serve(&path, &mut socket, &raw);
                        last_request = Instant::now();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if !observed.lock().unwrap().is_empty()
                            && last_request.elapsed() >= Duration::from_millis(500)
                        {
                            break;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        (listener, paths, handle)
    }

    fn upload_authorize_answer(socket: &mut std::net::TcpStream) {
        write_answer(
            socket,
            "200 OK",
            "",
            &json!({"version": WIKI_VERSION, "envelope": "h.e.1"}),
        );
    }

    #[test]
    fn upload_total_deadline_covers_authorization() {
        let (listener, paths, server) = upload_deadline_worker(|path, socket, _raw| {
            if path == "/__platform/wiki/authorize" {
                thread::sleep(Duration::from_secs(3));
                upload_authorize_answer(socket);
            } else {
                write_answer(socket, "200 OK", "", &json!({"ok": true}));
            }
        });
        let (target, _root, dir) = credentialed(&listener);
        let start = Instant::now();
        let out = upload_file_in(&dir, &target, "a.pdf", "a.pdf", b"pdf", true, 1, |_| {});
        let elapsed = start.elapsed();
        assert!(
            out.is_err(),
            "slow upload authorization unexpectedly succeeded"
        );
        assert!(
            elapsed < Duration::from_millis(2300),
            "1s upload budget took {elapsed:?}"
        );
        server.join().unwrap();
        assert_eq!(*paths.lock().unwrap(), ["/__platform/wiki/authorize"]);
    }

    #[test]
    fn upload_total_deadline_covers_response_body() {
        let (listener, paths, server) = upload_deadline_worker(|path, socket, _raw| {
            if path == "/__platform/wiki/authorize" {
                upload_authorize_answer(socket);
            } else {
                let _ = write!(socket, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{{");
                let _ = socket.flush();
                thread::sleep(Duration::from_secs(3));
                let _ = socket.write_all(b"\"ok\":true}");
            }
        });
        let (target, _root, dir) = credentialed(&listener);
        let start = Instant::now();
        let out = upload_file_in(&dir, &target, "a.pdf", "a.pdf", b"pdf", true, 1, |_| {});
        let elapsed = start.elapsed();
        assert!(
            out.is_err(),
            "slow upload response body unexpectedly succeeded"
        );
        assert!(
            elapsed < Duration::from_millis(2300),
            "1s upload budget took {elapsed:?}"
        );
        server.join().unwrap();
        assert_eq!(
            *paths.lock().unwrap(),
            ["/__platform/wiki/authorize", "/__platform/wiki/upload"]
        );
    }

    #[test]
    fn upload_total_deadline_includes_overwrite_preflight() {
        let wiki_authorizes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let authorizes = wiki_authorizes.clone();
        let (listener, paths, server) =
            upload_deadline_worker(move |path, socket, _raw| match path {
                "/__platform/wiki/authorize"
                    if authorizes.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 =>
                {
                    upload_authorize_answer(socket);
                }
                "/__platform/wiki/authorize" => {
                    thread::sleep(Duration::from_secs(3));
                    upload_authorize_answer(socket);
                }
                "/__platform/wiki/call" => {
                    thread::sleep(Duration::from_millis(1200));
                    write_answer(socket, "200 OK", "", &json!({"path":"a", "entries":[]}));
                }
                _ => write_answer(socket, "200 OK", "", &json!({"ok": true})),
            });
        let (target, _root, dir) = credentialed(&listener);
        let start = Instant::now();
        let out = upload_file_in(&dir, &target, "a/a.pdf", "a.pdf", b"pdf", false, 2, |_| {});
        let elapsed = start.elapsed();
        assert!(out.is_err(), "upload exceeded the shared preflight budget");
        assert!(
            elapsed < Duration::from_millis(2700),
            "2s total upload budget took {elapsed:?}"
        );
        server.join().unwrap();
        let paths = paths.lock().unwrap();
        assert_eq!(paths[0], "/__platform/wiki/authorize");
        assert_eq!(paths[1], "/__platform/wiki/call");
        assert_eq!(paths[2], "/__platform/wiki/authorize");
        assert_eq!(paths.len(), 3, "timeout must not send or replay an upload");
    }

    #[test]
    fn a_slow_authorize_is_capped_by_the_wake_budget() {
        let (listener, seen, server) = finite_worker(1, |_i, socket, _req| {
            thread::sleep(Duration::from_secs(3));
            write_answer(
                socket,
                "200 OK",
                "",
                &json!({"version": CLI_VERSION, "envelope": "h.e.1"}),
            );
        });
        let (target, _root, dir) = credentialed(&listener);
        let start = Instant::now();
        let out = call_verb_in(&target, "status", Map::new(), 1, &dir, |_| {}, |_| {});
        let elapsed = start.elapsed();
        assert!(out.is_err());
        assert!(
            elapsed < Duration::from_secs(2),
            "a 1s budget let authorize run {elapsed:?}"
        );
        server.join().unwrap();
        let reqs = seen.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].path, "/__platform/cli/authorize");
    }

    #[test]
    fn authorize_and_call_share_one_deadline() {
        let (listener, seen, server) = finite_worker(2, |i, socket, _req| {
            if i == 0 {
                thread::sleep(Duration::from_secs(1));
                write_answer(
                    socket,
                    "200 OK",
                    "",
                    &json!({"version": CLI_VERSION, "envelope": "h.e.1"}),
                );
            } else {
                thread::sleep(Duration::from_secs(3));
                write_answer(socket, "200 OK", "", &json!({"ok": true}));
            }
        });
        let (target, _root, dir) = credentialed(&listener);
        let start = Instant::now();
        let out = call_verb_in(&target, "status", Map::new(), 2, &dir, |_| {}, |_| {});
        let elapsed = start.elapsed();
        assert!(out.is_err());
        assert!(
            elapsed < Duration::from_millis(2750),
            "authorize+call did not share the 2s deadline: {elapsed:?}"
        );
        server.join().unwrap();
        let reqs = seen.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        assert!(check_authorize(&reqs[0]), "authorize schema");
        assert!(check_call(&reqs[1]), "call schema");
    }

    #[test]
    fn call_response_body_is_bounded_by_remaining_deadline() {
        let (listener, _seen, server) = finite_worker(2, |i, socket, _req| {
            if i == 0 {
                write_answer(
                    socket,
                    "200 OK",
                    "",
                    &json!({"version": CLI_VERSION, "envelope": "h.e.1"}),
                );
            } else {
                let body = serde_json::to_vec(&json!({"ok": true})).unwrap();
                let _ = write!(
                    socket,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(&body[..1]);
                let _ = socket.flush();
                thread::sleep(Duration::from_secs(3));
                let _ = socket.write_all(&body[1..]);
            }
        });
        let (target, _root, dir) = credentialed(&listener);
        let start = Instant::now();
        let out = call_verb_in(&target, "status", Map::new(), 1, &dir, |_| {}, |_| {});
        let elapsed = start.elapsed();
        assert!(out.is_err());
        assert!(
            elapsed < Duration::from_secs(2),
            "a 1s budget let the body read run {elapsed:?}"
        );
        server.join().unwrap();
    }

    #[test]
    fn a_zero_budget_never_sends_a_byte() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let (target, _root, dir) = credentialed(&listener);
        let out = call_verb_in(&target, "status", Map::new(), 0, &dir, |_| {}, |_| {});
        assert!(out.is_err());
        assert!(
            matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "a dead budget opened a socket"
        );
    }

    #[test]
    fn a_shared_expired_deadline_cannot_restart_the_wake_budget() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let (target, _root, dir) = credentialed(&listener);
        let deadline = Instant::now() - Duration::from_secs(1);
        let error = call_verb_until(
            &target,
            "read",
            Map::new(),
            deadline,
            60,
            &dir,
            |_| {},
            |_| {},
        )
        .unwrap_err();
        assert_eq!(error.kind(), "busy");
        assert_eq!(error.code(), Some("waking"));
        assert!(
            matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "a continuation restarted its expired budget"
        );
    }

    #[test]
    fn a_wake_sleep_that_would_pass_the_deadline_is_the_retryable_busy() {
        let (out, _requests) = run(
            vec![
                (503, "", json!({"state": "waking", "retry_after_s": 5})),
                (503, "", json!({"state": "waking", "retry_after_s": 5})),
            ],
            "status",
            Map::new(),
            2,
        );
        let err = out.unwrap_err();
        assert_eq!(err.kind(), "busy");
        assert_eq!(err.code(), Some("waking"));
    }

    #[test]
    fn transfer_ops_stay_on_write_or_job_status_and_refuse_authority() {
        let begin = json!({
            "path": "projects/example",
            "transfer": {
                "op": "begin",
                "manifest": {"version": 1, "root": "projects/example", "entries": []}
            }
        });
        assert!(wiki_transfer_shape("write", begin.as_object().unwrap()).is_ok());
        let mut mixed = begin.as_object().unwrap().clone();
        mixed.insert("text".into(), json!("no"));
        let err = wiki_transfer_shape("write", &mixed).unwrap_err();
        assert!(err.to_string().contains("never sent"), "{err}");
        let export = json!({
            "path": "projects/example/a.md",
            "transfer": {"op": "export", "path": "projects/example/a.md"}
        });
        assert!(wiki_transfer_shape("job_status", export.as_object().unwrap()).is_ok());
        let outside = json!({
            "path": "projects/example",
            "transfer": {"op": "export", "path": "projects/example/a.md"}
        });
        let err = wiki_transfer_shape("job_status", outside.as_object().unwrap()).unwrap_err();
        assert!(err.to_string().contains("outer path"), "{err}");
        let binding = json!({
            "path": "projects/example",
            "transfer": {"op": "status", "root": "projects/example", "_transfer_binding": {}}
        });
        let err = wiki_transfer_shape("job_status", binding.as_object().unwrap()).unwrap_err();
        assert!(err.to_string().contains("never sent"), "{err}");
        let forged = json!({
            "path": "projects/example",
            "transfer": {"op": "status", "actor": "operator"}
        });
        let err = wiki_transfer_shape("job_status", forged.as_object().unwrap()).unwrap_err();
        assert!(err.to_string().contains("never sent"), "{err}");
        let err = wiki_transfer_shape(
            "job_status",
            json!({"path": "projects/example", "transfer": {"op": "begin"}})
                .as_object()
                .unwrap(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("not allowed"), "{err}");
    }
}

#[cfg(test)]
#[path = "remote_cli_renewal_acceptance.rs"]
mod renewal_acceptance;

#[cfg(test)]
#[path = "remote_cli/cad1459_account_image_acceptance.rs"]
pub(crate) mod cad1459_account_image_acceptance;
