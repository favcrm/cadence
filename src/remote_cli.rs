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
/// `team` command exists to map to it) and `message_send` (an agent-to-agent
/// channel the remote milestone does not expose): a command not listed here
/// is refused *locally* and its bytes never leave the process.
///
/// The cli.write half (`issue_new`/`issue_comment`/`issue_set`) rides the
/// same envelope — the caller's authority never travels as a request field,
/// only inside the verified, single-use envelope. A failed or ambiguous
/// call is never retried by the client: the wake retry is the only loop,
/// and it fires only on a pre-forward `waking` verdict. Anything else — a
/// timeout, a dropped connection, a 5xx after forward — stays explicitly
/// unresolved: the mutation may or may not have landed, so reconcile with
/// `issue show`/`issue log` first rather than blindly re-issuing.
pub const REMOTE_VERBS: &[&str] = &[
    // cli.read
    "status",
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
];

/// The wiki actions this build sends over `/__platform/wiki/call` (CAD-1401):
/// `enumerate` is `wiki ls` or `wiki search`, `read` is `wiki cat`, `write` is
/// a text `wiki put`. No delete/job_status/history and no blob upload. Any
/// other verb is refused locally, before a request.
pub const WIKI_ACTIONS: &[&str] = &["enumerate", "read", "write"];

/// The argument keys each wiki action may send (CAD-1401). Any other key —
/// an authority-shaped one such as `actor` or `as` included — is refused
/// locally, before a request. Unknown actions get no keys (fail closed).
fn wiki_argument_keys(action: &str) -> &'static [&'static str] {
    match action {
        "enumerate" => &["path", "q"],
        "read" => &["path"],
        "write" => &["path", "text", "if_rev"],
        _ => &[],
    }
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

/// A resolved remote org destination — pinned once per invocation from the
/// registry (`cli::org::resolve`). `org` is the registry name (the
/// issuer-verified slug); `endpoint`/`org_id` came only from the verified
/// grant at login. Nothing here is caller-mutable mid-command.
#[derive(Clone, Debug)]
pub struct RemoteTarget {
    pub org: String,
    pub endpoint: String,
    pub org_id: String,
}

const SESSION_VERSION: &str = "hosted-cadence-cli-session.v1";
const CAPABILITIES: &[&str] = &["cli.read", "cli.write", "wiki.read", "wiki.write"];
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
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(reject(
            "remote credential directory must be owned by you, mode 0700, and not a symlink",
        ));
    }
    Ok(())
}

fn load_credential(dir: &Path, target: &RemoteTarget) -> Result<Credential> {
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
    if !valid_token(&cred.access_token, "hct_") {
        return Err(reject("invalid remote credential"));
    }
    if cred.cli_session.is_none() && cred.expires_at <= now()? {
        return Err(reject(
            "the remote credential has expired — run `cadence login` again",
        ));
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
    let _lock = private_lock(dir, &target.org, deadline)?;
    if Instant::now() >= deadline {
        return Err(Error::busy_coded(
            "timeout",
            "remote command deadline elapsed before session renewal",
        ));
    }
    let mut cred = load_credential(dir, target)?;
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
        || !same_capabilities(&cred.capabilities, CAPABILITIES)
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
    let (status, body) = crate::remote_enrollment::cli_session_post(
        dir,
        issuer,
        "/v1/hosted-cadence/cli-session/renew",
        &session.refresh_token,
        request,
        request_deadline,
    )?;
    if status != 200 {
        return Err(reject(
            "remote CLI session renewal refused — run `cadence login` again",
        ));
    }
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
        || body["principal"]["current_role"] != "owner"
        || body["principal"]["subject_id"].as_str() != prior.subject_id.as_deref()
        || !same_capabilities_json(&body["capabilities"], CAPABILITIES)
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

fn same_capabilities(values: &[String], expected: &[&str]) -> bool {
    values.len() == expected.len()
        && expected
            .iter()
            .all(|item| values.iter().filter(|v| v.as_str() == *item).count() == 1)
}
fn same_capabilities_json(value: &Value, expected: &[&str]) -> bool {
    value.as_array().is_some_and(|values| {
        values.len() == expected.len()
            && expected
                .iter()
                .all(|item| values.iter().filter(|v| v.as_str() == Some(*item)).count() == 1)
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
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .build();
    let response = ureq::Agent::new_with_config(config)
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

/// `POST /__platform/{cli,wiki}/authorize` — exchange the live `hct_` for one
/// actor envelope scoped to `scopes`. The request is the strict AOS-128 /
/// AOS-122 shape; the answer must echo the version and carry a compact-JWS
/// envelope.
fn authorize(
    target: &RemoteTarget,
    cred: &Credential,
    surface: Surface,
    scopes: &[&str],
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
        AUTH_TIMEOUT,
    )?;
    let body = body.ok_or_else(|| reject("remote authorize answered non-JSON"))?;
    match status {
        200 => {
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
        401 | 403 => Err(reject(
            "the remote refused the credential — run `cadence login` again",
        )),
        s if (300..400).contains(&s) => Err(reject("remote redirect refused")),
        _ => Err(reject(format!(
            "remote authorize failed ({})",
            body["code"].as_str().unwrap_or("unavailable")
        ))),
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
) -> Result<(u16, Option<u64>, Option<Value>)> {
    let scopes: &[&str] = match surface {
        Surface::Cli => &["cli.read", "cli.write"],
        Surface::Wiki if verb == "write" => &["wiki.write"],
        Surface::Wiki => &["wiki.read"],
    };
    let envelope = authorize(target, cred, surface, scopes)?;
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
    post(
        &target.endpoint,
        &format!("{relay}/call"),
        &cred.access_token,
        &body,
        CALL_TIMEOUT,
    )
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
                _ => Verdict::Done(Err(reject("remote org is unavailable"))),
            }
        }
        401 | 403 => Verdict::Done(Err(reject(
            "the remote refused the credential — run `cadence login` again",
        ))),
        s if (300..400).contains(&s) => Verdict::Done(Err(reject("remote redirect refused"))),
        409 => {
            // A checked conflict (`{conflict, current_rev, card, error}`
            // — the payload `write_reply` maps a stale `if_rev` to) is a
            // definite "not written" verdict, never a refusal of the
            // call itself and never retried. The body rides up so the
            // CLI can print `current_rev`/`card` for the caller to
            // resync on the spot. A 409 without that shape is the same
            // generic refusal as any other unhandled status.
            match body {
                Some(b) if b.get("conflict").is_some() => Verdict::Conflict(b),
                _ => Verdict::Done(Err(reject("remote call refused"))),
            }
        }
        _ => Verdict::Done(Err(reject("remote call refused"))),
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

pub fn call_verb(
    target: &RemoteTarget,
    verb: &str,
    arguments: Map<String, Value>,
    wake_timeout: u64,
    note: impl Fn(&str),
) -> Result<RemoteAnswer> {
    let dir = auth_dir(None).map_err(|_| {
        reject("remote credential directory is unavailable — set HOME or XDG_CONFIG_HOME")
    })?;
    call_verb_in(target, verb, arguments, wake_timeout, &dir, note, |d| {
        std::thread::sleep(d)
    })
}

/// `call_verb` with the credential dir and the sleep injectable — tests
/// drive the wake loop without wall-clock cost and point the credential
/// dir at a fixture. The verb allowlist is checked *here*, on the shared
/// path, so no caller can bypass it.
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
    }
    let deadline = Instant::now() + Duration::from_secs(wake_timeout);
    loop {
        let cred = renewed_credential(dir, target, deadline)?;
        if Instant::now() >= deadline {
            return Err(Error::busy_coded(
                "timeout",
                "remote command deadline elapsed during session renewal",
            ));
        }
        let (status, retry_after, body) = call_once(target, &cred, surface, verb, &arguments)?;
        match classify(status, retry_after, body) {
            Verdict::Done(result) => return result.map(RemoteAnswer::Ok),
            Verdict::Conflict(body) => return Ok(RemoteAnswer::Conflict(body)),
            Verdict::Waking(wait) => {
                let now = Instant::now();
                if now + wait > deadline {
                    // The remote was still waking when the budget ran out —
                    // retryable by the operator's own choice, so `busy` (75)
                    // with the `waking` code the contract pins.
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
                sleep(wait);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    const ORG: &str = "acme";
    const ORG_ID: &str = "ws_real";
    const BRIDGE: &str = "hct_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

    fn target(listener: &TcpListener) -> RemoteTarget {
        RemoteTarget {
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
        assert!(out.unwrap_err().to_string().contains("remote call refused"));
        assert_eq!(hits(&requests, "/__platform/cli/call"), 1);
    }
}

#[cfg(test)]
#[path = "remote_cli_renewal_acceptance.rs"]
mod renewal_acceptance;
