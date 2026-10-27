use super::http::BoardHttp;
use super::*;
use std::borrow::Cow;
use std::os::unix::io::AsRawFd;
use std::sync::Mutex;
use std::time::Instant;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KickoffFile {
    version: u32,
    message_id: String,
    organization_id: String,
    org: String,
    endpoint: String,
    assignment_id: String,
    claim_id: String,
    generation: u64,
    worktree: String,
    text: String,
    #[serde(default)]
    pending_state: Option<String>,
    #[serde(default)]
    pending_text: Option<String>,
    #[serde(default)]
    pending_head: Option<String>,
    #[serde(default)]
    reported_state: Option<String>,
    /// A fence or an already-terminal board settled this kickoff without
    /// accepting the report. Distinct from `reported_state`.
    #[serde(default)]
    settled: bool,
    /// Original directory member. Absent on bindings claimed before the pin.
    #[serde(default)]
    subject_id: Option<String>,
    /// Issuer captured with `subject_id`. Absent means the same as a missing
    /// member: the managed route cannot use this file.
    #[serde(default)]
    issuer: Option<String>,
}

fn pinned_issuer(value: &str) -> bool {
    (1..=300).contains(&value.len())
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        && !value.contains('\\')
}

/// Original identity only. Missing or malformed pins never become authority.
fn pinned_member<'a>(kickoff: &Kickoff<'a>) -> Option<(&'a str, &'a str)> {
    match (kickoff.subject_id, kickoff.issuer) {
        (Some(subject), Some(issuer)) if hosted_id(subject) && pinned_issuer(issuer) => {
            Some((subject, issuer))
        }
        _ => None,
    }
}

fn binding_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("hosted-assignments")
}

fn binding_path(state_dir: &Path, message_id: &str) -> Result<PathBuf> {
    crate::proto::identifier(message_id, "actor message")?;
    Ok(binding_dir(state_dir).join(format!("{message_id}.bind")))
}

fn safe_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && !slug.contains(['/', '\\', '\0'])
        && slug != "."
        && slug != ".."
}

fn read_kickoff_file(path: &Path) -> Result<Option<KickoffFile>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !meta.is_file() || meta.file_type().is_symlink() || meta.mode() & 0o077 != 0 {
        return Err(Error::rejected(
            "hosted kickoff binding must be a private regular file",
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| Error::rejected("hosted kickoff binding is unreadable"))
}

fn unique_temp(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("binding");
    let nonce = Uuid::new_v4().simple();
    path.with_file_name(format!(".{name}.{nonce}.tmp"))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// Replace `path` from a private temp in the same directory. The temp name is
/// unique so two updates cannot truncate one shared `bind.tmp`.
fn write_kickoff_file(path: &Path, binding: &KickoffFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let bytes =
        serde_json::to_vec_pretty(binding).map_err(|error| Error::internal(error.to_string()))?;
    let tmp = unique_temp(path);
    write_private(&tmp, &bytes)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// First binding only. `Ok(false)` means some other handoff already created it.
fn create_kickoff_file(path: &Path, binding: &KickoffFile) -> Result<bool> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let bytes =
        serde_json::to_vec_pretty(binding).map_err(|error| Error::internal(error.to_string()))?;
    let tmp = unique_temp(path);
    write_private(&tmp, &bytes)?;
    match fs::hard_link(&tmp, path) {
        Ok(()) => {
            let _ = fs::remove_file(&tmp);
            if let Some(parent) = path.parent() {
                File::open(parent)?.sync_all()?;
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&tmp);
            Ok(false)
        }
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            Err(error.into())
        }
    }
}

fn kickoff_matches(file: &KickoffFile, kickoff: &Kickoff) -> bool {
    let Some((subject_id, issuer)) = pinned_member(kickoff) else {
        return false;
    };
    file.version == 1
        && file.message_id == kickoff.message_id
        && file.assignment_id == kickoff.assignment_id
        && file.claim_id == kickoff.claim_id
        && file.generation == kickoff.generation
        && file.organization_id == kickoff.organization_id
        && file.subject_id.as_deref() == Some(subject_id)
        && file.issuer.as_deref() == Some(issuer)
}

/// Text already bound to this kickoff id, if a previous attempt stored it.
/// A changed claim does not reuse the body.
pub fn stored_kickoff_text(state_dir: &Path, kickoff: &Kickoff) -> Result<Option<String>> {
    let path = binding_path(state_dir, kickoff.message_id)?;
    let Some(file) = read_kickoff_file(&path)? else {
        return Ok(None);
    };
    if !kickoff_matches(&file, kickoff) {
        return Err(Error::rejected(
            "reserved kickoff identity does not match this claim; the actor was not messaged",
        ));
    }
    Ok(Some(file.text))
}

/// Hosted destination pinned by one immutable kickoff binding.
/// `org`, `endpoint`, and `organization_id` are that file's fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedBindingTarget {
    pub org: String,
    pub endpoint: String,
    pub organization_id: String,
    /// Member and issuer written at claim time. Never filled from a later login.
    pub subject_id: String,
    pub issuer: String,
}

/// Read the live hosted target for `message_id`.
///
/// `Ok(None)` when the binding file is absent, when that binding is
/// already settled or reported, or when it has no original member and
/// issuer. A missing pin is not inferred from the current login. A file
/// that is not a private regular binding, or whose message id does not
/// match, is an error. This does not consult the org registry and does
/// not rewrite the file.
pub fn hosted_binding_target(
    state_dir: &Path,
    message_id: &str,
) -> Result<Option<HostedBindingTarget>> {
    let path = binding_path(state_dir, message_id)?;
    let Some(file) = read_kickoff_file(&path)? else {
        return Ok(None);
    };
    if file.version != 1 || file.message_id != message_id {
        return Err(Error::rejected(
            "hosted kickoff binding does not match this message",
        ));
    }
    if file.settled || file.reported_state.is_some() {
        return Ok(None);
    }
    if file.org.is_empty() || file.endpoint.is_empty() || file.organization_id.is_empty() {
        return Err(Error::rejected("hosted kickoff binding has no destination"));
    }
    let (Some(subject_id), Some(issuer)) = (file.subject_id, file.issuer) else {
        return Ok(None);
    };
    if !hosted_id(&subject_id) || !pinned_issuer(&issuer) {
        return Ok(None);
    }
    Ok(Some(HostedBindingTarget {
        org: file.org,
        endpoint: file.endpoint,
        organization_id: file.organization_id,
        subject_id,
        issuer,
    }))
}

/// Durably bind the reserved message to its originating claim before send.
/// A concurrent handoff of the same claim keeps the first file. A different
/// claim for that message id is refused and does not overwrite it.
pub fn store_kickoff_binding(
    state_dir: &Path,
    org_slug: &str,
    endpoint: &str,
    kickoff: &Kickoff,
    text: &str,
) -> Result<()> {
    if !safe_slug(org_slug)
        || !(endpoint.starts_with("https://") || endpoint.starts_with("http://"))
        || endpoint.chars().any(char::is_control)
        || !prefixed_id(kickoff.assignment_id, "hla_")
        || !prefixed_id(kickoff.claim_id, "hlc_")
        || !hosted_id(kickoff.organization_id)
    {
        return Err(Error::rejected(
            "hosted kickoff binding was refused; the actor was not messaged",
        ));
    }
    let (subject_id, issuer) = pinned_member(kickoff).ok_or_else(|| {
        Error::rejected("hosted kickoff has no original member and issuer; nothing was delivered")
    })?;
    let path = binding_path(state_dir, kickoff.message_id)?;
    if let Some(existing) = read_kickoff_file(&path)? {
        return same_binding(&existing, kickoff, text);
    }
    let created = create_kickoff_file(
        &path,
        &KickoffFile {
            version: 1,
            message_id: kickoff.message_id.to_string(),
            organization_id: kickoff.organization_id.to_string(),
            org: org_slug.to_string(),
            endpoint: endpoint.to_string(),
            assignment_id: kickoff.assignment_id.to_string(),
            claim_id: kickoff.claim_id.to_string(),
            generation: kickoff.generation,
            worktree: kickoff.worktree.to_string(),
            text: text.to_string(),
            pending_state: None,
            pending_text: None,
            pending_head: None,
            reported_state: None,
            settled: false,
            subject_id: Some(subject_id.to_string()),
            issuer: Some(issuer.to_string()),
        },
    )?;
    if created {
        return Ok(());
    }
    let Some(existing) = read_kickoff_file(&path)? else {
        return Err(Error::rejected(
            "hosted kickoff binding disappeared during create; the actor was not messaged",
        ));
    };
    same_binding(&existing, kickoff, text)
}

fn same_binding(existing: &KickoffFile, kickoff: &Kickoff, text: &str) -> Result<()> {
    if !kickoff_matches(existing, kickoff) || existing.text != text {
        return Err(Error::rejected(
            "reserved kickoff identity does not match this claim; the actor was not messaged",
        ));
    }
    Ok(())
}

/// Hosted state owed by a local terminal row.
/// `unknown` and anything still in flight return `None`: unresolved never
/// becomes a result. An explicit accepted SHA is kept; a missing or illegal
/// SHA on completion is a failure, not a guessed commit.
fn completion_outcome<'a>(
    status: &str,
    text: &'a str,
    sha: Option<String>,
) -> Option<(&'static str, Cow<'a, str>, Option<String>)> {
    let explicit = sha.filter(|value| head_sha(value));
    match status {
        "completed" | "result" => {
            let Some(head) = explicit else {
                return Some((
                    "failed",
                    if text.trim().is_empty() {
                        Cow::Borrowed("local actor completed without a reported revision")
                    } else {
                        Cow::Borrowed(text)
                    },
                    None,
                ));
            };
            Some((
                "result",
                if text.trim().is_empty() {
                    Cow::Borrowed("local actor completed")
                } else {
                    Cow::Borrowed(text)
                },
                Some(head),
            ))
        }
        "failed" | "interrupted" | "cancelled" => Some((
            "failed",
            if text.trim().is_empty() {
                Cow::Owned(format!("local actor {status}"))
            } else {
                Cow::Borrowed(text)
            },
            None,
        )),
        _ => None,
    }
}

fn message_text(message: &crate::store::Message) -> &str {
    if let Some(result) = message.result.as_ref() {
        for key in ["text", "note", "reason"] {
            if let Some(text) = result.get(key).and_then(Value::as_str) {
                if !text.is_empty() {
                    return text;
                }
            }
        }
    }
    if let Some(error) = message.error.as_deref() {
        if !error.is_empty() {
            return error;
        }
    }
    message.body.as_str()
}

fn message_sha(message: &crate::store::Message) -> Option<String> {
    let result = message.result.as_ref()?;
    if let Some(sha) = result.get("sha").filter(|value| !value.is_null()) {
        return sha.as_str().filter(|sha| head_sha(sha)).map(str::to_string);
    }
    ["text", "note"].iter().find_map(|key| {
        result
            .get(key)
            .and_then(Value::as_str)
            .and_then(crate::store::last_sha_line)
    })
}

const ATTEMPT_BUDGET: Duration = Duration::from_secs(5);

/// One background reporter for every hosted kickoff in a state dir.
/// Obligations come from the immutable binding plus the durable message row.
pub struct ReportDriver {
    state_dir: PathBuf,
    cursor: Mutex<String>,
    deferred: std::sync::atomic::AtomicBool,
}

impl ReportDriver {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            state_dir: state_dir.to_path_buf(),
            cursor: Mutex::new(String::new()),
            deferred: std::sync::atomic::AtomicBool::new(false),
        }
    }
    /// Retry timers exist only for actual durable terminal report obligations.
    pub fn has_pending(&self) -> Result<bool> {
        if self.deferred.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(true);
        }
        for path in self.pass_paths()? {
            if let Some(binding) = read_kickoff_file(&path)? {
                if binding.pending_state.is_some()
                    && binding.reported_state.is_none()
                    && !binding.settled
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// One fair pass. Local pending state is derived before any request.
    /// Each request's renewal and HTTP share one deadline, and that deadline
    /// is also clipped to `deadline`. Local recovery continues after the HTTP
    /// budget is gone; the fair cursor advances only for actual HTTP attempts.
    pub fn tick(&self, store: &crate::store::Store, deadline: Instant) -> Result<()> {
        self.deferred
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let paths = self.pass_paths()?;
        for path in paths {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(message_id) = name.strip_suffix(".bind") else {
                continue;
            };
            if self.attempt(store, &path, message_id, deadline)? {
                *self.cursor.lock().unwrap_or_else(|err| err.into_inner()) = name.to_string();
            }
        }
        Ok(())
    }

    /// Native outcome/config notifications trigger a complete local recovery
    /// pass. Only pending reports schedule another timer.
    fn pass_paths(&self) -> Result<Vec<PathBuf>> {
        let dir = binding_dir(&self.state_dir);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_string();
                name.strip_suffix(".bind")?;
                Some(name)
            })
            .collect();
        names.sort();
        let start = self
            .cursor
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone();
        let rotate = names
            .iter()
            .position(|name| name.as_str() > start.as_str())
            .unwrap_or(0);
        names.rotate_left(rotate);
        Ok(names.into_iter().map(|name| dir.join(name)).collect())
    }

    fn attempt(
        &self,
        store: &crate::store::Store,
        path: &Path,
        message_id: &str,
        deadline: Instant,
    ) -> Result<bool> {
        let (visited, prepared) = prepare_pending(path, store, message_id)?;
        if !visited {
            self.deferred
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let Some(prepared) = prepared else {
            return Ok(false);
        };
        let attempt = Instant::now() + ATTEMPT_BUDGET;
        let attempt = if attempt < deadline {
            attempt
        } else {
            deadline
        };
        if Instant::now() >= attempt {
            return Ok(false);
        }
        match post_report(&prepared, attempt) {
            Ok(ReportAck::Accepted) => mark_accepted(path, &prepared.pending_state)?,
            Ok(ReportAck::Fenced) => mark_fenced(path)?,
            Err(_) => {}
        }
        Ok(true)
    }
}

enum ReportAck {
    Accepted,
    Fenced,
}

struct Prepared {
    org: String,
    endpoint: String,
    organization_id: String,
    assignment_id: String,
    claim_id: String,
    generation: u64,
    pending_state: String,
    pending_head: Option<String>,
    body: Value,
    /// Original member, when the binding recorded both pins.
    subject_id: Option<String>,
    issuer: Option<String>,
}

/// Derive pending hosted state from the message row when the binding has
/// none. Return whether the file was visited and its ready report, if any.
/// Already reported or fenced rows are idle. The lock never spans a request.
fn prepare_pending(
    path: &Path,
    store: &crate::store::Store,
    message_id: &str,
) -> Result<(bool, Option<Prepared>)> {
    let _lock = match try_lock(path)? {
        Some(lock) => lock,
        None => return Ok((false, None)),
    };
    let Some(mut binding) = read_kickoff_file(path)? else {
        return Ok((true, None));
    };
    if binding.reported_state.is_some() || binding.settled {
        return Ok((true, None));
    }
    if binding.pending_state.is_none() {
        let Some(message) = store.message(message_id)? else {
            return Ok((true, None));
        };
        let Some((state, text, head)) = completion_outcome(
            &message.state,
            message_text(&message),
            message_sha(&message),
        ) else {
            return Ok((true, None));
        };
        let identity = super::wire::ReportIdentity {
            organization_id: &binding.organization_id,
            claim_id: &binding.claim_id,
            generation: binding.generation,
        };
        let bounded = super::wire::bounded_report_text(
            &identity,
            state,
            head.as_deref(),
            message_id,
            text.as_ref(),
        )?;
        binding.pending_state = Some(state.to_string());
        binding.pending_text = Some(bounded);
        binding.pending_head = head;
        write_kickoff_file(path, &binding)?;
    }
    let state = binding
        .pending_state
        .clone()
        .ok_or_else(|| Error::rejected("hosted completion has no pending report"))?;
    let identity = super::wire::ReportIdentity {
        organization_id: &binding.organization_id,
        claim_id: &binding.claim_id,
        generation: binding.generation,
    };
    let body = super::wire::report_body(
        &identity,
        &state,
        binding.pending_text.as_deref(),
        binding.pending_head.as_deref(),
    )?;
    Ok((
        true,
        Some(Prepared {
            org: binding.org,
            endpoint: binding.endpoint,
            organization_id: binding.organization_id,
            assignment_id: binding.assignment_id,
            claim_id: binding.claim_id,
            generation: binding.generation,
            pending_state: state,
            pending_head: binding.pending_head.clone(),
            body,
            subject_id: binding.subject_id,
            issuer: binding.issuer,
        }),
    ))
}

fn post_report(prepared: &Prepared, deadline: Instant) -> Result<ReportAck> {
    let (Some(subject_id), Some(issuer)) =
        (prepared.subject_id.as_deref(), prepared.issuer.as_deref())
    else {
        return Err(Error::rejected(
            "hosted kickoff binding has no original member and issuer; nothing was sent",
        ));
    };
    if !hosted_id(subject_id) || !pinned_issuer(issuer) {
        return Err(Error::rejected(
            "hosted kickoff identity is invalid; nothing was sent",
        ));
    }
    let target = RemoteTarget {
        identity: Some(remote_cli::CliIdentityPin {
            subject_id: subject_id.to_string(),
            issuer: issuer.to_string(),
        }),
        org: prepared.org.clone(),
        endpoint: prepared.endpoint.clone(),
        org_id: prepared.organization_id.clone(),
    };
    let mut http = BoardHttp::open_until(&target, deadline)?;
    let path = format!(
        "{}/{}/report",
        assignment_prefix(&prepared.organization_id),
        prepared.assignment_id
    );
    let reply = http.send_until("POST", &path, Some(&prepared.body), deadline)?;
    classify_reply(prepared, reply.status, &reply.body)
}

/// `200`/`201` count only when the assignment receipt names this claim,
/// generation, and hosted state, and a result carries the same SHA.
/// `409 fenced`/`terminal` settles without becoming an accepted report.
fn classify_reply(prepared: &Prepared, status: u16, body: &Value) -> Result<ReportAck> {
    if matches!(status, 200 | 201) {
        let assignment = parse_assignment(body)?;
        let claim = assignment.claim_id.as_deref().unwrap_or("");
        if assignment.organization_id != prepared.organization_id
            || assignment.assignment_id != prepared.assignment_id
            || claim != prepared.claim_id
            || assignment.generation != prepared.generation
            || assignment.state != prepared.pending_state
        {
            return Err(Error::rejected(
                "hosted report receipt did not match this claim",
            ));
        }
        if prepared.pending_state == "result"
            && assignment.reported_head_sha.as_deref() != prepared.pending_head.as_deref()
        {
            return Err(Error::rejected(
                "hosted report receipt did not carry the reported commit",
            ));
        }
        if prepared.pending_state != "result" && assignment.reported_head_sha.is_some() {
            return Err(Error::rejected("hosted failure receipt carried a commit"));
        }
        return Ok(ReportAck::Accepted);
    }
    if status == 409 {
        let code = body.get("code").and_then(Value::as_str);
        if matches!(code, Some("fenced" | "terminal")) {
            return Ok(ReportAck::Fenced);
        }
    }
    Err(Error::invalid(
        "unavailable",
        "hosted assignment report was not accepted",
    ))
}

fn mark_accepted(path: &Path, state: &str) -> Result<()> {
    let _lock = match try_lock(path)? {
        Some(lock) => lock,
        None => return Ok(()),
    };
    let Some(mut binding) = read_kickoff_file(path)? else {
        return Ok(());
    };
    if binding.reported_state.is_some() {
        return Ok(());
    }
    binding.reported_state = Some(state.to_string());
    write_kickoff_file(path, &binding)
}

fn mark_fenced(path: &Path) -> Result<()> {
    let _lock = match try_lock(path)? {
        Some(lock) => lock,
        None => return Ok(()),
    };
    let Some(mut binding) = read_kickoff_file(path)? else {
        return Ok(());
    };
    if binding.reported_state.is_some() {
        return Ok(());
    }
    binding.settled = true;
    write_kickoff_file(path, &binding)
}

struct FileLock {
    _file: File,
}

fn try_lock(binding: &Path) -> Result<Option<FileLock>> {
    let lock_path = binding.with_extension("lock");
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&lock_path)?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Ok(None);
    }
    Ok(Some(FileLock { _file: file }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{NewAgent, Store};
    use serde_json::json;
    use std::thread;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn kickoff<'a>(message: &'a str, claim: &'a str) -> Kickoff<'a> {
        Kickoff {
            alias: "worker",
            issue_id: "CAD1460",
            assignment_id: "hla_00000000000000000000000000000001",
            worktree: "/absent",
            message_id: message,
            claim_id: claim,
            generation: 3,
            organization_id: "ws_report_driver",
            audience: "https://hosted-report.invalid",
            subject_id: Some("user_member"),
            issuer: Some("https://hosted-report.invalid"),
        }
    }

    fn store_message(dir: &Path) -> Store {
        Store::open(&dir.join("cadence.sqlite3")).unwrap()
    }

    fn register(store: &Store, dir: &Path) {
        store
            .register_agent(&NewAgent {
                alias: "worker",
                provider: "fake",
                endpoint_kind: "managed",
                role: "worker",
                cwd: dir.to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
    }

    fn bind(dir: &Path, message: &str) {
        store_kickoff_binding(
            dir,
            "acme",
            "https://hosted-report.invalid",
            &kickoff(message, "hlc_00000000000000000000000000000001"),
            "do the work",
        )
        .unwrap();
    }

    fn file(dir: &Path, message: &str) -> Value {
        let bytes = fs::read(binding_path(dir, message).unwrap()).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn kickoff_without_original_identity_is_not_persisted() {
        let dir = tempfile::tempdir().unwrap();
        for (subject, issuer) in [
            (None, None),
            (Some("user_member"), None),
            (None, Some("https://hosted-report.invalid")),
            (
                Some("invalid/subject"),
                Some("https://hosted-report.invalid"),
            ),
            (Some("user_member"), Some("invalid\nissuer")),
        ] {
            let mut binding = kickoff("missing-1", "hlc_00000000000000000000000000000001");
            binding.subject_id = subject;
            binding.issuer = issuer;
            assert!(store_kickoff_binding(
                dir.path(),
                "acme",
                "https://hosted-report.invalid",
                &binding,
                "do the work",
            )
            .is_err());
            assert!(!binding_path(dir.path(), "missing-1").unwrap().exists());
        }
    }

    #[test]
    fn crash_gap_recovers_from_the_message_row_without_transport() {
        let dir = tempfile::tempdir().unwrap();
        bind(dir.path(), "gap-1");
        let store = store_message(dir.path());
        register(&store, dir.path());
        store
            .enqueue("worker", "do the work", None, "gap-1", "user")
            .unwrap();
        let message = match store.take_queued("worker").unwrap() {
            crate::store::Take::Message(message) => message,
            _ => panic!("expected a claimed message"),
        };
        store.mark_running("gap-1", "turn-1").unwrap();
        store
            .finish(
                &message,
                "completed",
                &json!({"status": "completed", "text": "landed", "sha": SHA}),
                None,
            )
            .unwrap();
        assert!(file(dir.path(), "gap-1")["pending_state"].is_null());
        let started = Instant::now();
        ReportDriver::new(dir.path())
            .tick(&store, Instant::now())
            .unwrap();
        assert!(started.elapsed() < Duration::from_millis(500));
        let saved = file(dir.path(), "gap-1");
        assert_eq!(saved["pending_state"], "result");
        assert_eq!(saved["pending_head"], SHA);
        assert!(saved["reported_state"].is_null());
        assert_eq!(saved["text"], "do the work");
        assert_eq!(store.message("gap-1").unwrap().unwrap().state, "completed");
    }

    #[test]
    fn recovery_uses_the_durable_reported_revision_not_checkout_head() {
        for mode in ["missing", "invalid", "trailer", "explicit"] {
            let dir = tempfile::tempdir().unwrap();
            let repo = dir.path().join("checkout");
            fs::create_dir(&repo).unwrap();
            let git = |args: &[&str]| {
                let output = Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args([
                        "-c",
                        "user.name=Synthetic revision proof",
                        "-c",
                        "user.email=revision@example.test",
                        "-c",
                        "commit.gpgsign=false",
                    ])
                    .args(args)
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{:?}", output.stderr);
                String::from_utf8(output.stdout).unwrap().trim().to_string()
            };
            git(&["init", "--initial-branch=revision-proof"]);
            git(&["commit", "--allow-empty", "-m", "Original revision"]);
            let original = git(&["rev-parse", "HEAD"]);
            let mut binding = kickoff("revision-1", "hlc_00000000000000000000000000000001");
            binding.worktree = repo.to_str().unwrap();
            store_kickoff_binding(
                dir.path(),
                "acme",
                "https://hosted-report.invalid",
                &binding,
                "do the work",
            )
            .unwrap();
            let store = store_message(dir.path());
            register(&store, &repo);
            store
                .enqueue("worker", "do the work", None, "revision-1", "user")
                .unwrap();
            let message = match store.take_queued("worker").unwrap() {
                crate::store::Take::Message(message) => message,
                _ => panic!("expected a claimed message"),
            };
            store.mark_running("revision-1", "turn-1").unwrap();
            let result = match mode {
                "missing" => json!({"text": "Completed without reporting a revision"}),
                "invalid" => json!({"sha": "not-a-revision", "text": format!("SHA: {original}")}),
                "trailer" => json!({"text": format!("Completed\nSHA: {original}")}),
                "explicit" => json!({"sha": original, "text": format!("SHA: {}", "f".repeat(40))}),
                _ => unreachable!(),
            };
            store.finish(&message, "completed", &result, None).unwrap();
            git(&["commit", "--allow-empty", "-m", "Later unrelated revision"]);
            assert_ne!(git(&["rev-parse", "HEAD"]), original);
            ReportDriver::new(dir.path())
                .tick(&store, Instant::now())
                .unwrap();
            let pending = file(dir.path(), "revision-1");
            if matches!(mode, "missing" | "invalid") {
                assert_eq!(pending["pending_state"], "failed", "{mode}");
                assert!(pending["pending_head"].is_null(), "{mode}");
            } else {
                assert_eq!(pending["pending_state"], "result", "{mode}");
                assert_eq!(pending["pending_head"], original, "{mode}");
            }
            assert_eq!(
                store.message("revision-1").unwrap().unwrap().result,
                Some(result)
            );
        }
    }

    #[test]
    fn cancelled_and_reconciled_rows_report_and_unknown_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_message(dir.path());
        register(&store, dir.path());
        bind(dir.path(), "cancel-1");
        store
            .enqueue("worker", "queued work", None, "cancel-1", "user")
            .unwrap();
        store
            .cancel("cancel-1", "operator", Some("dropped"))
            .unwrap();
        bind(dir.path(), "unknown-1");
        store
            .enqueue("worker", "lost", None, "unknown-1", "user")
            .unwrap();
        let lost = match store.take_queued("worker").unwrap() {
            crate::store::Take::Message(message) => message,
            _ => panic!("expected a claimed message"),
        };
        store.mark_running("unknown-1", "turn-2").unwrap();
        store
            .finish(
                &lost,
                "unknown",
                &json!({"status": "unknown", "text": ""}),
                Some("lost"),
            )
            .unwrap();
        ReportDriver::new(dir.path())
            .tick(&store, Instant::now())
            .unwrap();
        assert_eq!(file(dir.path(), "cancel-1")["pending_state"], "failed");
        assert!(file(dir.path(), "cancel-1")["pending_head"].is_null());
        assert!(file(dir.path(), "unknown-1")["pending_state"].is_null());
        store
            .reconcile(
                "unknown-1",
                "completed",
                Some("checked"),
                "operator",
                Some(SHA),
            )
            .unwrap();
        ReportDriver::new(dir.path())
            .tick(&store, Instant::now())
            .unwrap();
        let reconciled = file(dir.path(), "unknown-1");
        assert_eq!(reconciled["pending_state"], "result");
        assert_eq!(reconciled["pending_head"], SHA);
        let row = store.message("unknown-1").unwrap().unwrap();
        assert_eq!(row.state, "completed");
        assert_eq!(row.result.as_ref().unwrap()["sha"], SHA);
    }

    #[test]
    fn a_matching_receipt_is_accepted_and_a_fence_is_not() {
        let prepared = Prepared {
            org: "acme".to_string(),
            endpoint: "https://hosted-report.invalid".to_string(),
            organization_id: "ws_report_driver".to_string(),
            assignment_id: "hla_00000000000000000000000000000001".to_string(),
            claim_id: "hlc_00000000000000000000000000000001".to_string(),
            generation: 3,
            pending_state: "result".to_string(),
            pending_head: Some(SHA.to_string()),
            body: json!({}),
            subject_id: Some("user_member".to_string()),
            issuer: Some("https://hosted-report.invalid".to_string()),
        };
        let receipt = |sha: Value, state: &str| {
            json!({
                "version": VERSION,
                "assignment_id": "hla_00000000000000000000000000000001",
                "organization_id": "ws_report_driver",
                "issue_id": "CAD1460",
                "action": "dispatch",
                "execution": "local",
                "state": state,
                "generation": 3,
                "claim_id": "hlc_00000000000000000000000000000001",
                "target": Value::Null,
                "created_by_subject_id": "user_member",
                "client_agent_id": Value::Null,
                "guidance": Value::Null,
                "reported_head_sha": sha,
                "text": Value::Null,
            })
        };
        assert!(matches!(
            classify_reply(&prepared, 200, &receipt(json!(SHA), "result")).unwrap(),
            ReportAck::Accepted
        ));
        assert!(classify_reply(
            &prepared,
            200,
            &receipt(json!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), "result")
        )
        .is_err());
        assert!(matches!(
            classify_reply(&prepared, 409, &json!({"code": "fenced"})).unwrap(),
            ReportAck::Fenced
        ));
        let dir = tempfile::tempdir().unwrap();
        bind(dir.path(), "fence-1");
        let path = binding_path(dir.path(), "fence-1").unwrap();
        let mut binding = read_kickoff_file(&path).unwrap().unwrap();
        binding.pending_state = Some("result".to_string());
        binding.pending_head = Some(SHA.to_string());
        write_kickoff_file(&path, &binding).unwrap();
        mark_fenced(&path).unwrap();
        let saved = file(dir.path(), "fence-1");
        assert!(saved["reported_state"].is_null());
        assert_eq!(saved["settled"], true);
        assert_eq!(saved["pending_head"], SHA);
    }

    #[test]
    fn concurrent_different_claims_cannot_overwrite_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().to_path_buf();
        let left = "hlc_00000000000000000000000000000001";
        let right = "hlc_00000000000000000000000000000002";
        let left_ok = std::sync::atomic::AtomicUsize::new(0);
        let right_ok = std::sync::atomic::AtomicUsize::new(0);
        thread::scope(|scope| {
            for index in 0..8 {
                let state = state.clone();
                let left_ok = &left_ok;
                let right_ok = &right_ok;
                scope.spawn(move || {
                    let claim = if index % 2 == 0 { left } else { right };
                    let saved = store_kickoff_binding(
                        &state,
                        "acme",
                        "https://hosted-report.invalid",
                        &kickoff("race-1", claim),
                        "do the work",
                    );
                    if saved.is_ok() {
                        let counter = if claim == left { left_ok } else { right_ok };
                        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                });
            }
        });
        let saved = file(dir.path(), "race-1");
        let claim = saved["claim_id"].as_str().unwrap();
        assert!(claim == left || claim == right);
        assert_eq!(saved["text"], "do the work");
        assert_eq!(saved["generation"], 3);
        if claim == left {
            assert!(left_ok.load(std::sync::atomic::Ordering::SeqCst) > 0);
            assert_eq!(right_ok.load(std::sync::atomic::Ordering::SeqCst), 0);
        } else {
            assert!(right_ok.load(std::sync::atomic::Ordering::SeqCst) > 0);
            assert_eq!(left_ok.load(std::sync::atomic::Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn hosted_binding_target_reads_only_a_live_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(hosted_binding_target(dir.path(), "target-1")
            .unwrap()
            .is_none());
        bind(dir.path(), "target-1");
        let target = hosted_binding_target(dir.path(), "target-1")
            .unwrap()
            .unwrap();
        assert_eq!(target.org, "acme");
        assert_eq!(target.endpoint, "https://hosted-report.invalid");
        assert_eq!(target.organization_id, "ws_report_driver");
        let path = binding_path(dir.path(), "target-1").unwrap();
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["reported_state"] = json!("result");
        fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        assert!(hosted_binding_target(dir.path(), "target-1")
            .unwrap()
            .is_none());
        value["reported_state"] = Value::Null;
        value["settled"] = json!(true);
        fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        assert!(hosted_binding_target(dir.path(), "target-1")
            .unwrap()
            .is_none());
    }
}
