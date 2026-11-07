//! CAD-2149 (client half of AOS-502): a managed local session sends its hosted
//! CLI verbs on a credential of its own, bound to its kickoff, instead of its
//! member's login.
//!
//! This is attribution and server-side refusal. It is NOT a security boundary:
//! the session runs as the same OS user as its member and can still read the
//! member's login file until CAD-2148 confines it.
//!
//! What moves and what does not:
//! - Every hosted CLI verb the managed session sends rides the credential,
//!   except [`stays_on_login`] (wiki and the audit verdicts). `issue review`
//!   rides the credential: its author check compares the reviewer with an
//!   artifact's author, and `issue attach` is recorded as the agent. That is an allowlist of what
//!   keeps the login, so a verb added later defaults to the restricted bearer.
//! - Claim, daemon reports and `agent report` never come through here; they
//!   stay on the member's login.
//! - The credential is minted on the member's login, lazily at first use, and
//!   cached 0600 beside the kickoff's `.bind` file. It lives at most 900 s and
//!   has no refresh token: renewal is a fresh mint.
//!
//! When it is not used, and what that costs:
//! - For `platform_call`, an unavailable or refused agent credential stops the
//!   action. Retrying as the member could give the call a person's authority.
//! - Other verbs keep the login when the board has no mint route (404), the
//!   login has no renewable session (409 `renewable_login_required`), or the
//!   tenant answers `human_session_required`. One stderr line names this.
//! - A 401, a 403 `forbidden` and every other refusal is never retried on the
//!   login, or the refusal would be undone.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub(super) const HUMAN_SESSION_REQUIRED: &str = "human_session_required";
const VERSION: &str = "hosted-cadence-local-assignment.v1";
const FILE_VERSION: &str = "cadence-agent-credential.v1";
/// The longest the board lets a credential live, plus clock skew.
const MAX_LIFETIME: u64 = 900 + SKEW;

/// One managed kickoff's credential: where it is cached and which claim it
/// serves. Built by the CLI from the live kickoff binding.
#[derive(Debug)]
pub struct AgentSlot {
    pub path: PathBuf,
    pub org: String,
    pub endpoint: String,
    pub org_id: String,
    pub assignment_id: String,
    pub claim_id: String,
    pub generation: u64,
    /// Set once the login is kept, so the line is said once and the next verb
    /// of the same invocation does not ask the board again.
    declined: AtomicBool,
}

impl AgentSlot {
    pub fn new(
        path: PathBuf,
        target: &RemoteTarget,
        assignment_id: String,
        claim_id: String,
        generation: u64,
    ) -> Self {
        Self {
            path,
            org: target.org.clone(),
            endpoint: target.endpoint.clone(),
            org_id: target.org_id.clone(),
            assignment_id,
            claim_id,
            generation,
            declined: AtomicBool::new(false),
        }
    }

    fn serves(&self, target: &RemoteTarget) -> bool {
        self.org == target.org && self.endpoint == target.endpoint && self.org_id == target.org_id
    }

    /// Keep the member's login for the rest of this invocation. Says so once.
    pub(super) fn keep_login(&self, note: &impl Fn(&str), why: &str) {
        if !self.declined.swap(true, Ordering::SeqCst) {
            note(&format!(
                "cadence: using your login for hosted verbs, not an agent credential \
                 ({why}); the board will attribute them to you"
            ));
        }
    }

    pub(super) fn declined(&self) -> bool {
        self.declined.load(Ordering::SeqCst)
    }
}

static BOUND: Mutex<Option<Arc<AgentSlot>>> = Mutex::new(None);

/// Called once by the CLI when it settles a managed caller's destination. A
/// human shell never calls it, so it never reads the cached credential.
pub fn bind_managed_session(slot: AgentSlot) {
    *BOUND.lock().unwrap_or_else(|error| error.into_inner()) = Some(Arc::new(slot));
}

/// Test seam: forget the bound session, so a test can prove a path binds none.
#[doc(hidden)]
pub fn unbind_managed_session() {
    *BOUND.lock().unwrap_or_else(|error| error.into_inner()) = None;
}

/// The session `bind_managed_session` recorded, if this is a managed caller.
pub fn bound_session() -> Option<Arc<AgentSlot>> {
    BOUND
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

/// Wiki traffic and verdicts keep the member's login (operator decision,
/// AOS-502). An allowlist: anything not named here uses the agent credential.
pub(super) fn stays_on_login(surface: Surface, verb: &str) -> bool {
    matches!(surface, Surface::Wiki) || matches!(verb, "audit_verdict" | "audit_verdict_project")
}

/// Whether this call is sent on the agent credential at all.
pub(super) fn applies<'a>(
    slot: Option<&'a AgentSlot>,
    target: &RemoteTarget,
    surface: Surface,
    verb: &str,
) -> Option<&'a AgentSlot> {
    slot.filter(|slot| slot.serves(target) && !stays_on_login(surface, verb) && !slot.declined())
}

pub(super) fn platform_agent_required() -> Error {
    Error::invalid(
        "agent_credential_required",
        "A managed platform action needs an agent credential; the member login was not used for the action.",
    )
}

/// The credential to send now: the cached one while it has time left, else a
/// fresh mint on the member's login. `Ok(None)` means the login is kept (and
/// the one line was said); `Err` means nothing was sent.
pub(super) fn bearer(
    slot: &AgentSlot,
    target: &RemoteTarget,
    dir: &Path,
    deadline: Instant,
    allow_login_fallback: bool,
    note: &impl Fn(&str),
) -> Result<Option<Credential>> {
    if slot.declined() {
        return if allow_login_fallback {
            Ok(None)
        } else {
            Err(platform_agent_required())
        };
    }
    let at = now()?;
    if let Some(cached) = read_cached(slot, target, at) {
        return Ok(Some(cached));
    }
    match mint(slot, target, dir, deadline)? {
        Minted::Credential(cred) => {
            // A cache that cannot be written only costs the next process a mint.
            let _ = write_cache(slot, &cred);
            Ok(Some(*cred))
        }
        Minted::Keep(_) if !allow_login_fallback => Err(platform_agent_required()),
        Minted::Keep(why) => {
            slot.keep_login(note, why);
            Ok(None)
        }
    }
}

enum Minted {
    Credential(Box<Credential>),
    Keep(&'static str),
}

fn credential_from(
    target: &RemoteTarget,
    token: String,
    capabilities: Vec<String>,
    expires_at: u64,
) -> Credential {
    Credential {
        organization_id: target.org_id.clone(),
        organization_slug: Some(target.org.clone()),
        audience: target.endpoint.clone(),
        issuer: target.identity.as_ref().map(|pin| pin.issuer.clone()),
        // `sub` stays the member on the board, so receipts that name the
        // sender still correlate.
        subject_id: target.identity.as_ref().map(|pin| pin.subject_id.clone()),
        capabilities,
        access_token: token,
        expires_at,
        cli_session: None,
        pending: None,
    }
}

fn mint(slot: &AgentSlot, target: &RemoteTarget, dir: &Path, deadline: Instant) -> Result<Minted> {
    let login = renewed_credential(dir, target, deadline)?;
    let path = format!(
        "/__platform/hosted-cadence/{}/local-assignments/{}/agent-credential",
        slot.org_id, slot.assignment_id
    );
    let body = json!({
        "version": VERSION,
        "organization_id": slot.org_id,
        "expected_generation": slot.generation,
        "claim_id": slot.claim_id,
    });
    let (status, _, answer) = post(
        &slot.endpoint,
        &path,
        &login.access_token,
        &body,
        request_timeout(deadline, AUTH_TIMEOUT)?,
    )
    .map_err(|error| {
        reject(format!(
            "could not ask for this session's agent credential ({error}); nothing was \
             sent, and your login was not used in its place — retry later"
        ))
    })?;
    // After the wire call, as login renewal does: a slow board must not age a
    // good answer out of its own lifetime.
    let at = now()?;
    let code = refusal_fields(answer.as_ref()).0;
    match (status, code) {
        (200, _) => validated(slot, target, answer.as_ref(), at)
            .map(|cred| Minted::Credential(Box::new(cred))),
        (404, _) => Ok(Minted::Keep("this board does not offer one")),
        (409, Some("renewable_login_required")) => Ok(Minted::Keep(
            "your login has no renewable session to bind it to",
        )),
        (500..=599, _) => Err(reject(format!(
            "the board could not issue this session's agent credential (HTTP {status}); \
             nothing was sent, and your login was not used in its place — retry later"
        ))),
        _ => Err(reject(format!(
            "the board refused this session's agent credential (HTTP {status}{}); \
             nothing was sent, and your login was not used in its place",
            code.map_or(String::new(), |code| format!(", {code}"))
        ))),
    }
}

/// A 200 only counts when it names this claim and carries a usable bearer.
/// `at` is read after the answer arrived; the lifetime is bounded from the
/// board's own `issued_at`, never from when this client started asking.
pub(super) fn validated(
    slot: &AgentSlot,
    target: &RemoteTarget,
    answer: Option<&Value>,
    at: u64,
) -> Result<Credential> {
    let unusable =
        || reject("the board's agent credential answer was not usable; nothing was sent");
    let answer = answer.ok_or_else(unusable)?;
    let text = |pointer: &str| answer.pointer(pointer).and_then(Value::as_str);
    let credential = answer.get("credential").ok_or_else(unusable)?;
    let token = credential.get("access_token").and_then(Value::as_str);
    let expires_at = credential.get("expires_at").and_then(Value::as_u64);
    let issued_at = credential.get("issued_at").and_then(Value::as_u64);
    let capabilities: Option<Vec<String>> = answer
        .get("capabilities")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        });
    let (Some(token), Some(expires_at), Some(issued_at), Some(capabilities)) =
        (token, expires_at, issued_at, capabilities)
    else {
        return Err(unusable());
    };
    if text("/version") != Some(VERSION)
        || text("/assignment_id") != Some(slot.assignment_id.as_str())
        || text("/claim_id") != Some(slot.claim_id.as_str())
        || answer.get("generation").and_then(Value::as_u64) != Some(slot.generation)
        || text("/principal/kind") != Some("agent")
        || credential.get("token_type").and_then(Value::as_str) != Some("Bearer")
        || credential.get("renewal").and_then(Value::as_str) != Some("remint")
        || !valid_token(token, "hct_")
        || issued_at > at.saturating_add(SKEW)
        || expires_at <= at.saturating_add(SKEW)
        || expires_at > issued_at.saturating_add(MAX_LIFETIME)
        || capabilities.is_empty()
    {
        return Err(unusable());
    }
    Ok(credential_from(
        target,
        token.to_owned(),
        capabilities,
        expires_at,
    ))
}

fn read_cached(slot: &AgentSlot, target: &RemoteTarget, at: u64) -> Option<Credential> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&slot.path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.len() > 8192
    {
        return None;
    }
    let mut raw = Vec::new();
    file.take(8193).read_to_end(&mut raw).ok()?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    let token = text("access_token")?;
    let expires_at = value.get("expires_at").and_then(Value::as_u64)?;
    let capabilities: Vec<String> = value
        .get("capabilities")?
        .as_array()?
        .iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect();
    // The file must still be for exactly this claim and destination, and have
    // time left. Anything else is as if there were no file.
    if text("version") != Some(FILE_VERSION)
        || text("assignment_id") != Some(slot.assignment_id.as_str())
        || text("claim_id") != Some(slot.claim_id.as_str())
        || value.get("generation").and_then(Value::as_u64) != Some(slot.generation)
        || text("organization_id") != Some(slot.org_id.as_str())
        || text("audience") != Some(slot.endpoint.as_str())
        || !valid_token(token, "hct_")
        || expires_at <= at.saturating_add(RENEW_BEFORE)
        || expires_at > at.saturating_add(MAX_LIFETIME)
        || capabilities.is_empty()
    {
        return None;
    }
    Some(credential_from(
        target,
        token.to_owned(),
        capabilities,
        expires_at,
    ))
}

fn write_cache(slot: &AgentSlot, cred: &Credential) -> Result<()> {
    let body = json!({
        "version": FILE_VERSION,
        "assignment_id": slot.assignment_id,
        "claim_id": slot.claim_id,
        "generation": slot.generation,
        "organization_id": slot.org_id,
        "audience": slot.endpoint,
        "capabilities": cred.capabilities,
        "access_token": cred.access_token,
        "expires_at": cred.expires_at,
    });
    let nonce = uuid::Uuid::new_v4().simple();
    let name = slot
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("agent");
    let tmp = slot.path.with_file_name(format!(".{name}.{nonce}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&tmp)?;
    let result = (|| {
        file.write_all(body.to_string().as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, &slot.path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}
