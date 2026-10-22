//! Daemon-owned outbound hosted events. HTTPS remains the assignment authority.
use super::http::BoardHttp;
use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tungstenite::Message;

pub const VERSION: &str = "hosted-cadence-local-events.v1";
const DIRECTORY_VERSION: &str = "hosted-cadence-agent-directory.v1";
const INBOX_LIMIT: usize = 512;
const FILE_LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentBinding {
    pub alias: String,
    agent_id: String,
    adapter: String,
    delivery: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceBinding {
    version: u32,
    org: String,
    organization_id: String,
    endpoint: String,
    subject_id: String,
    issuer: String,
    runner_id: String,
    agents: Vec<AgentBinding>,
    connected: bool,
    lane_name: Option<String>,
    default_alias: Option<String>,
    /// Explicit connect may resume blocked work once; background reconnect may not.
    resume_operation: Option<String>,
}

impl WorkspaceBinding {
    pub fn organization_id(&self) -> &str {
        &self.organization_id
    }
    pub fn default_alias(&self) -> Option<&str> {
        self.default_alias.as_deref()
    }
    fn target(&self) -> RemoteTarget {
        RemoteTarget {
            org: self.org.clone(),
            org_id: self.organization_id.clone(),
            endpoint: self.endpoint.clone(),
            identity: Some(remote_cli::CliIdentityPin {
                subject_id: self.subject_id.clone(),
                issuer: self.issuer.clone(),
            }),
        }
    }
    fn path(&self, state: &Path) -> PathBuf {
        folder(state).join(format!("{}.workspace", self.organization_id))
    }
    fn inbox_path(&self, state: &Path) -> PathBuf {
        folder(state).join(format!("{}.inbox", self.organization_id))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Inbox {
    version: u32,
    organization_id: String,
    endpoint: String,
    subject_id: String,
    issuer: String,
    cursor: u64,
    initialized: bool,
    resumed_operation: Option<String>,
    selected_operation: Option<String>,
    recovery_watermark: Option<u64>,
    recovery_notice_after: Option<String>,
    entries: Vec<Value>,
}

fn folder(state: &Path) -> PathBuf {
    state.join("hosted-events")
}

fn private_read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.mode() & 0o077 != 0
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.len() > FILE_LIMIT
    {
        return Err(Error::rejected(
            "hosted event state must be a bounded private owned file",
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let mut bytes = Vec::new();
    file.take(FILE_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > FILE_LIMIT {
        return Err(Error::rejected("hosted event state is too large"));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| Error::rejected("hosted event state is unreadable"))
}

fn atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::rejected("event state directory is missing"))?;
    let existed = match fs::symlink_metadata(parent) {
        Ok(meta)
            if meta.is_dir()
                && !meta.file_type().is_symlink()
                && meta.uid() == unsafe { libc::geteuid() } =>
        {
            true
        }
        Ok(_) => {
            return Err(Error::rejected(
                "event state directory must be owned and not a symlink",
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    if !existed {
        if let Some(grandparent) = parent.parent() {
            File::open(grandparent)?.sync_all()?;
        }
    }
    let bytes = serde_json::to_vec(value).map_err(|error| Error::internal(error.to_string()))?;
    if bytes.len() as u64 > FILE_LIMIT {
        return Err(Error::rejected("hosted event inbox is full"));
    }
    let tmp = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&tmp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn config_lock(state: &Path) -> Result<impl Sized> {
    remote_cli::private_lock(
        state,
        "hosted-event-config",
        Instant::now() + Duration::from_secs(5),
    )
}

fn inbox_lock(state: &Path) -> Result<impl Sized> {
    remote_cli::private_lock(
        state,
        "hosted-event-inbox",
        Instant::now() + Duration::from_secs(5),
    )
}

/// The daemon resolves real endpoint capabilities. Caller declarations do not
/// grant push, an actor, or another member's identity.
pub fn connect(
    state: &Path,
    target: &RemoteTarget,
    agent: &crate::store::Agent,
    fyi_only: bool,
    lane_name: Option<&str>,
    repo: Option<&Path>,
) -> Result<Value> {
    let alias = agent.alias.as_str();
    let provider = agent.provider.as_str();
    let endpoint_kind = agent.endpoint_kind.as_str();
    alias_name(alias)?;
    let http = BoardHttp::open(target)?;
    let pinned = http.pinned_target()?;
    let identity = pinned
        .identity
        .as_ref()
        .ok_or_else(|| Error::rejected("member pin is missing"))?;
    let _lock = config_lock(state)?;
    let path = folder(state).join(format!("{}.workspace", pinned.org_id));
    let mut binding = if let Some(saved) = private_read::<WorkspaceBinding>(&path)? {
        if saved.endpoint != pinned.endpoint
            || saved.subject_id != identity.subject_id
            || saved.issuer != identity.issuer
        {
            return Err(Error::rejected("workspace identity is pinned; use a separate private native state for another member or issuer"));
        }
        saved
    } else {
        let runner_path = folder(state).join("runner.json");
        let runner_id = match private_read::<String>(&runner_path)? {
            Some(id) if prefixed_id(&id, "lri_") => id,
            Some(_) => return Err(Error::rejected("installed runner identity is invalid")),
            None => {
                let id = format!("lri_{}", Uuid::new_v4());
                atomic(&runner_path, &id)?;
                id
            }
        };
        WorkspaceBinding {
            version: 1,
            org: pinned.org.clone(),
            organization_id: pinned.org_id.clone(),
            endpoint: pinned.endpoint.clone(),
            subject_id: identity.subject_id.clone(),
            issuer: identity.issuer.clone(),
            runner_id,
            agents: Vec::new(),
            connected: true,
            lane_name: None,
            default_alias: None,
            resume_operation: None,
        }
    };
    let capabilities = crate::adapter::registry::capabilities_json(provider, endpoint_kind);
    let delivery = if capabilities["has_actor"] == true {
        "turn"
    } else if capabilities["has_actor"] == false {
        "pull"
    } else {
        return Err(Error::rejected(
            "endpoint has no declared delivery capability",
        ));
    };
    let adapter = format!("{provider}/{endpoint_kind}");
    if adapter.len() > 80 {
        return Err(Error::rejected("adapter name is too long"));
    }
    if let Some(agent) = binding.agents.iter_mut().find(|agent| agent.alias == alias) {
        agent.adapter = adapter;
        agent.delivery = delivery.to_string();
    } else {
        if binding.agents.len() >= 32 {
            return Err(Error::rejected(
                "at most 32 explicit agents may share a workspace runner",
            ));
        }
        binding.agents.push(AgentBinding {
            alias: alias.to_string(),
            agent_id: format!("lag_{}", Uuid::new_v4()),
            adapter,
            delivery: delivery.to_string(),
        });
    }
    if !fyi_only {
        http.into_coordinator()?
            .configure(repo, lane_name, Some(alias))?;
        binding.lane_name = lane_name.map(str::to_string);
        binding.default_alias = Some(alias.to_string());
        binding.resume_operation = Some(operation_id());
    }
    binding.connected = true;
    atomic(&path, &binding)?;
    Ok(
        json!({"configured": true, "organization_id": binding.organization_id, "runner_id": binding.runner_id, "alias": alias, "default_alias": binding.default_alias, "agents": binding.agents}),
    )
}

pub fn disconnect(state: &Path, org: &str) -> Result<Value> {
    if !hosted_id(org) {
        return Err(Error::rejected("invalid workspace id"));
    }
    let _lock = config_lock(state)?;
    let path = folder(state).join(format!("{org}.workspace"));
    if let Some(mut binding) = private_read::<WorkspaceBinding>(&path)? {
        binding.connected = false;
        binding.default_alias = None;
        atomic(&path, &binding)?;
        // Stable workspace agent IDs are retained across disconnect/reconnect.
        // A failed hosted unregister never re-enables local delivery.
        let unregister: Result<()> = (|| {
            let mut http = BoardHttp::open(&binding.target())?;
            let body = json!({"version": DIRECTORY_VERSION, "runner_id": binding.runner_id, "agents": [], "default_agent_id": null});
            let reply = http.send(
                "POST",
                &format!("/__platform/hosted-cadence/{org}/local-agents"),
                Some(&body),
            )?;
            if !(200..300).contains(&reply.status) {
                return Err(Error::rejected("directory unregister was refused"));
            }
            Ok(())
        })();
        return Ok(
            json!({"configured": false, "stopped": false, "reported": false, "directory_unregistered": unregister.is_ok()}),
        );
    }
    Ok(json!({"configured": false, "stopped": false, "reported": false}))
}

pub fn bindings(state: &Path) -> Result<Vec<WorkspaceBinding>> {
    let entries = match fs::read_dir(folder(state)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut result = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry
            .path()
            .extension()
            .is_some_and(|ext| ext == "workspace")
        {
            let binding = private_read::<WorkspaceBinding>(&entry.path())?
                .ok_or_else(|| Error::rejected("workspace binding disappeared"))?;
            if binding.version != 1
                || !hosted_id(&binding.organization_id)
                || binding.agents.len() > 32
                || entry.path() != binding.path(state)
            {
                return Err(Error::rejected("workspace event binding is invalid"));
            }
            if binding.connected {
                result.push(binding);
            }
        }
    }
    Ok(result)
}

fn load_inbox(state: &Path, binding: &WorkspaceBinding) -> Result<Inbox> {
    let inbox = private_read::<Inbox>(&binding.inbox_path(state))?.unwrap_or_else(|| Inbox {
        version: 1,
        organization_id: binding.organization_id.clone(),
        endpoint: binding.endpoint.clone(),
        subject_id: binding.subject_id.clone(),
        issuer: binding.issuer.clone(),
        cursor: 0,
        initialized: false,
        resumed_operation: None,
        selected_operation: None,
        recovery_watermark: None,
        recovery_notice_after: None,
        entries: Vec::new(),
    });
    if inbox.version != 1
        || inbox.organization_id != binding.organization_id
        || inbox.endpoint != binding.endpoint
        || inbox.subject_id != binding.subject_id
        || inbox.issuer != binding.issuer
        || inbox.entries.len() > INBOX_LIMIT
    {
        return Err(Error::rejected(
            "event inbox belongs to another workspace member or issuer",
        ));
    }
    Ok(inbox)
}

pub fn observe(state: &Path, org: &str, clear: bool) -> Result<Value> {
    if !hosted_id(org) {
        return Err(Error::rejected("invalid workspace id"));
    }
    let _lock = inbox_lock(state)?;
    let path = folder(state).join(format!("{org}.inbox"));
    let Some(mut inbox) = private_read::<Inbox>(&path)? else {
        return Ok(json!({"cursor": 0, "entries": []}));
    };
    let value = json!({"cursor": inbox.cursor, "entries": inbox.entries, "recovering": !inbox.initialized, "recovery_watermark": inbox.recovery_watermark, "recovery_notice_after": inbox.recovery_notice_after, "capacity": INBOX_LIMIT});
    if clear {
        // Pending invalidations must survive observation/clear and restart.
        inbox
            .entries
            .retain(|entry| entry["type"] != "agent_notified" || entry.get("notice").is_none());
        atomic(&path, &inbox)?;
    }
    Ok(value)
}

/// Notice recovery shares the assignment snapshot's watermark. Progress is
/// durable per page: a full private inbox backpressures without skipping cloud
/// history, and an explicit inbox --clear lets recovery continue from that page.
fn recover_notices(
    state: &Path,
    binding: &WorkspaceBinding,
    http: &mut BoardHttp,
    stop: &AtomicBool,
    notify: &dyn Fn(),
) -> Result<Inbox> {
    loop {
        if stop.load(Ordering::SeqCst) {
            return Err(Error::rejected("workspace recovery stopped"));
        }
        let prior = {
            let _lock = inbox_lock(state)?;
            load_inbox(state, binding)?
        };
        let watermark = prior
            .recovery_watermark
            .ok_or_else(|| Error::rejected("notice recovery watermark is missing"))?;
        let mut path = format!(
            "/__platform/hosted-cadence/{}/local-agents/notices?watermark={watermark}",
            binding.organization_id
        );
        if let Some(after) = &prior.recovery_notice_after {
            path.push_str(&format!("&after_id={after}"));
        }
        let reply = http.send("GET", &path, None)?;
        if reply.status != 200
            || reply.body["version"] != DIRECTORY_VERSION
            || reply.body["watermark"].as_u64() != Some(watermark)
        {
            return Err(Error::rejected("notice recovery contract changed"));
        }
        let notices = reply.body["notices"]
            .as_array()
            .filter(|rows| rows.len() <= 50)
            .ok_or_else(|| Error::rejected("notice recovery page is invalid"))?;
        let mut after = prior.recovery_notice_after.clone();
        let mut recovered = Vec::new();
        for notice in notices {
            let id = notice["notice_id"]
                .as_str()
                .filter(|id| prefixed_id(id, "lan_"))
                .ok_or_else(|| Error::rejected("notice recovery identity is invalid"))?;
            let seq = notice["created_seq"]
                .as_u64()
                .filter(|seq| *seq > 0 && *seq <= watermark)
                .ok_or_else(|| Error::rejected("notice recovery sequence is invalid"))?;
            if after.as_ref().is_some_and(|after| id <= after.as_str())
                || notice["version"] != DIRECTORY_VERSION
                || notice["kind"] != "fyi"
                || !notice["agent_id"]
                    .as_str()
                    .is_some_and(|id| prefixed_id(id, "lag_"))
                || notice["text"]
                    .as_str()
                    .is_none_or(|text| text.len() > 16000)
            {
                return Err(Error::rejected("notice recovery did not advance safely"));
            }
            after = Some(id.to_string());
            recovered.push(
                json!({"version": VERSION, "type": "agent_notified", "seq": seq,
                "agent_id": notice["agent_id"], "notice_id": id, "kind": "fyi", "notice": notice}),
            );
        }
        let complete = match &reply.body["after_id"] {
            Value::Null => true,
            Value::String(next)
                if after.as_ref() == Some(next)
                    && prior.recovery_notice_after.as_ref() != Some(next) =>
            {
                false
            }
            _ => return Err(Error::rejected("notice recovery continuation is invalid")),
        };
        let _lock = inbox_lock(state)?;
        let mut inbox = load_inbox(state, binding)?;
        for event in recovered {
            if inbox
                .entries
                .iter()
                .any(|prior| prior["notice_id"] == event["notice_id"])
            {
                continue;
            }
            if inbox.entries.len() >= INBOX_LIMIT {
                return Err(Error::rejected(
                    "hosted inbox is full; display and clear FYIs to continue recovery",
                ));
            }
            inbox.entries.push(event);
        }
        inbox.recovery_notice_after = after;
        if complete {
            inbox.cursor = watermark;
            inbox.initialized = true;
            inbox.recovery_watermark = None;
            inbox.recovery_notice_after = None;
            inbox.resumed_operation = binding.resume_operation.clone();
        }
        atomic(&binding.inbox_path(state), &inbox)?;
        drop(_lock);
        notify();
        if complete {
            return Ok(inbox);
        }
    }
}

fn register(
    http: &mut BoardHttp,
    binding: &WorkspaceBinding,
    sessions: &[String],
    select_default: bool,
    client_agent_id: &str,
) -> Result<()> {
    let agents: Vec<Value> = binding.agents.iter().zip(sessions).map(|(agent, session)| json!({
        "agent_id": agent.agent_id, "label": agent.alias, "adapter": agent.adapter, "delivery": agent.delivery,
        "session_id": session, "client_agent_id": client_agent_id,
    })).collect();
    let mut body =
        json!({"version": DIRECTORY_VERSION, "runner_id": binding.runner_id, "agents": agents});
    if select_default {
        body["default_agent_id"] = json!(binding
            .default_alias
            .as_ref()
            .and_then(|alias| binding.agents.iter().find(|agent| &agent.alias == alias))
            .map(|agent| &agent.agent_id));
    }
    let reply = http.send(
        "POST",
        &format!(
            "/__platform/hosted-cadence/{}/local-agents",
            binding.organization_id
        ),
        Some(&body),
    )?;
    if !(200..300).contains(&reply.status) || reply.body["version"] != DIRECTORY_VERSION {
        return Err(Error::rejected(
            "workspace refused explicit local agent registration",
        ));
    }
    Ok(())
}

fn is_assignment_default(http: &mut BoardHttp, binding: &WorkspaceBinding) -> Result<bool> {
    let Some(default) = binding
        .default_alias
        .as_ref()
        .and_then(|alias| binding.agents.iter().find(|agent| &agent.alias == alias))
    else {
        return Ok(false);
    };
    let mut after: Option<String> = None;
    loop {
        let mut path = format!(
            "/__platform/hosted-cadence/{}/local-agents",
            binding.organization_id
        );
        if let Some(after) = &after {
            path.push_str(&format!("?after_id={after}"));
        }
        let reply = http.send("GET", &path, None)?;
        if reply.status != 200 || reply.body["version"] != DIRECTORY_VERSION {
            return Err(Error::rejected("workspace directory is unavailable"));
        }
        let agents = reply.body["agents"]
            .as_array()
            .filter(|agents| agents.len() <= 50)
            .ok_or_else(|| Error::rejected("workspace directory page is invalid"))?;
        for agent in agents {
            if agent["owner_subject_id"] == binding.subject_id
                && agent["default_assignment_alias"] == true
            {
                if agent["agent_id"] != default.agent_id || agent["runner_id"] != binding.runner_id
                {
                    return Ok(false);
                }
                if agent["registration_state"] != "current" {
                    return Err(Error::busy_coded("registration_pending", "the selected local agent needs its current credential registration refreshed"));
                }
                return Ok(true);
            }
        }
        match &reply.body["after_id"] {
            Value::Null => return Ok(false),
            Value::String(next)
                if prefixed_id(next, "lag_") && after.as_ref().is_none_or(|prior| next > prior) =>
            {
                after = Some(next.clone())
            }
            _ => return Err(Error::rejected("workspace directory did not advance")),
        }
    }
}

fn guarded_http(
    target: &RemoteTarget,
    lifetime: &Arc<dyn Fn() -> bool + Send + Sync>,
) -> Result<BoardHttp> {
    if !lifetime() {
        return Err(Error::busy_coded(
            "binding_cancelled",
            "workspace binding stopped or changed",
        ));
    }
    let mut http = BoardHttp::open(target)?;
    http.set_request_guard(lifetime.clone());
    Ok(http)
}

/// An activation can return Ok with per-row refusals. Only an authoritative
/// terminal/block or transfer to another claimant settles such an obligation;
/// a queued/running row after a transient failure remains durable pending work.
fn pickup_reconciled(coord: &mut Coordinator<BoardHttp>, value: &Value) -> Result<()> {
    coord.ensure_active()?;
    let refused = value["refused"].as_array();
    let Some(refused) = refused.filter(|rows| !rows.is_empty()) else {
        return Ok(());
    };
    let client = coord.client_agent_id()?;
    for refusal in refused {
        let id = refusal["assignment_id"]
            .as_str()
            .filter(|id| prefixed_id(id, "hla_"))
            .ok_or_else(|| Error::rejected("pickup refusal has no assignment identity"))?;
        let path = format!("{}/{id}", coord.base());
        let reply = coord.http.send("GET", &path, None)?;
        coord.ensure_active()?;
        match reply.status {
            403 | 404 => continue,
            200 => {
                let row = coord.parse_owned(&reply.body)?;
                if row.assignment_id != id {
                    return Err(Error::rejected(
                        "pickup reconciliation named another assignment",
                    ));
                }
                coord.remember_discovered(&row)?;
                if attempt_ended(&row.state)
                    || row.target.as_ref() != coord.subject_id.as_ref()
                    || row.state == "blocked" && refusal["code"] == "assignment_blocked"
                    || matches!(row.state.as_str(), "running" | "blocked")
                        && row
                            .client_agent_id
                            .as_ref()
                            .is_some_and(|owner| owner != &client)
                {
                    continue;
                }
            }
            _ => {
                return Err(Error::busy_coded(
                    "pickup_pending",
                    "the assignment pickup is awaiting authoritative reconciliation",
                ))
            }
        }
        return Err(Error::busy_coded(
            "pickup_pending",
            "the assignment remains queued or running after an unsuccessful local pickup",
        ));
    }
    Ok(())
}

/// Each thread owns exactly one workspace socket. A stop flag is checked at
/// bounded socket deadlines; no inbound listener or session discovery exists.
pub fn run(
    state: PathBuf,
    binding: WorkspaceBinding,
    stop: Arc<AtomicBool>,
    send: Arc<dyn Fn(Value) -> Result<Value> + Send + Sync>,
    notify: Arc<dyn Fn() + Send + Sync>,
) {
    let sessions: Vec<String> = binding
        .agents
        .iter()
        .map(|_| format!("las_{}", Uuid::new_v4()))
        .collect();
    // A replacement configuration cannot overlap the old workspace socket.
    let name = format!("hosted-workspace-{}", binding.organization_id);
    let _owner = loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if let Ok(lock) =
            remote_cli::private_lock(&state, &name, Instant::now() + Duration::from_millis(250))
        {
            break lock;
        }
    };
    let lifetime: Arc<dyn Fn() -> bool + Send + Sync> = {
        let state = state.clone();
        let binding = binding.clone();
        let stop = stop.clone();
        Arc::new(move || {
            !stop.load(Ordering::SeqCst)
                && private_read::<WorkspaceBinding>(&binding.path(&state))
                    .ok()
                    .flatten()
                    .as_ref()
                    == Some(&binding)
        })
    };
    let (pending, receive) = std::sync::mpsc::sync_channel::<()>(1);
    let processor = {
        let state = state.clone();
        let binding = binding.clone();
        let stop = stop.clone();
        let send = send.clone();
        let notify = notify.clone();
        let lifetime = lifetime.clone();
        std::thread::spawn(move || {
            let mut retry = false;
            while !stop.load(Ordering::SeqCst) {
                let wake = if retry {
                    receive.recv_timeout(Duration::from_secs(5)).map_err(|_| ())
                } else {
                    receive.recv().map_err(|_| ())
                };
                if wake.is_err() && !retry {
                    break;
                }
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let result = (|| {
                    let target = binding.target();
                    let mut http = guarded_http(&target, &lifetime)?;
                    let mut coord = guarded_http(&target, &lifetime)?.into_coordinator()?;
                    coord.delivery_guard = Some(lifetime.clone());
                    let mut deliver = |kickoff: &Kickoff| {
                        super::delivery::deliver_kickoff_with(
                            &target,
                            &state,
                            kickoff,
                            30,
                            &|params| {
                                if !lifetime() {
                                    return Err(Error::busy_coded(
                                        "binding_cancelled",
                                        "workspace binding stopped or changed",
                                    ));
                                }
                                send(params)
                            },
                        )
                    };
                    process(&state, &binding, &mut coord, &mut http, &mut deliver, &stop)
                })();
                notify();
                retry = result.is_err();
            }
        })
    };
    let mut retry = Duration::from_secs(1);
    while !stop.load(Ordering::SeqCst) {
        let result = session(
            &state,
            SessionRegistration {
                binding: &binding,
                sessions: &sessions,
            },
            &stop,
            send.as_ref(),
            &pending,
            notify.as_ref(),
            &lifetime,
        );
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if result.is_ok() {
            retry = Duration::from_secs(1);
        }
        let deadline = Instant::now() + retry;
        while Instant::now() < deadline && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(50));
        }
        retry = (retry * 2).min(Duration::from_secs(30));
    }
    drop(pending);
    // No network join on daemon shutdown. The stop-checked send closure fences
    // any context read still finishing; process exit bounds the detached read.
    drop(processor);
}

struct SessionRegistration<'a> {
    binding: &'a WorkspaceBinding,
    sessions: &'a [String],
}

fn session(
    state: &Path,
    registration: SessionRegistration<'_>,
    stop: &AtomicBool,
    send: &dyn Fn(Value) -> Result<Value>,
    pending: &std::sync::mpsc::SyncSender<()>,
    notify: &dyn Fn(),
    lifetime: &Arc<dyn Fn() -> bool + Send + Sync>,
) -> Result<()> {
    let SessionRegistration { binding, sessions } = registration;
    let target = binding.target();
    let mut http = guarded_http(&target, lifetime)?;
    let mut inbox = {
        let _lock = inbox_lock(state)?;
        load_inbox(state, binding)?
    };
    let mut coord = guarded_http(&target, lifetime)?.into_coordinator()?;
    coord.delivery_guard = Some(lifetime.clone());
    coord.ensure_active()?;
    // A new credential for this same pinned member must refresh the directory
    // credential fence while retaining stable IDs/session and selected default.
    register(
        &mut http,
        binding,
        sessions,
        binding.resume_operation.is_some() && inbox.selected_operation != binding.resume_operation,
        &coord.client_agent_id()?,
    )?;
    {
        let _lock = inbox_lock(state)?;
        inbox = load_inbox(state, binding)?;
        inbox.selected_operation = binding.resume_operation.clone();
        atomic(&binding.inbox_path(state), &inbox)?;
    }
    let mut deliver = |kickoff: &Kickoff| {
        if !lifetime() {
            return Err(Error::busy_coded(
                "binding_cancelled",
                "workspace binding stopped or changed",
            ));
        }
        super::delivery::deliver_kickoff_with(&target, state, kickoff, 30, &|params| {
            if !lifetime() {
                return Err(Error::busy_coded(
                    "binding_cancelled",
                    "workspace binding stopped or changed",
                ));
            }
            send(params)
        })
    };
    if !inbox.initialized {
        if inbox.recovery_watermark.is_none() {
            let (watermark, rows) = coord.snapshot()?;
            if watermark < inbox.cursor {
                return Err(Error::rejected("snapshot watermark regressed"));
            }
            if is_assignment_default(&mut http, binding)? && !stop.load(Ordering::SeqCst) {
                let resume = binding.resume_operation.is_some()
                    && inbox.resumed_operation != binding.resume_operation;
                let picked = coord.pickup_rows(
                    rows,
                    binding.lane_name.as_deref(),
                    resume,
                    binding.default_alias.as_deref(),
                    &mut deliver,
                )?;
                pickup_reconciled(&mut coord, &picked)?;
            }
            let _lock = inbox_lock(state)?;
            inbox = load_inbox(state, binding)?;
            inbox.recovery_watermark = Some(watermark);
            inbox.recovery_notice_after = None;
            atomic(&binding.inbox_path(state), &inbox)?;
        }
        inbox = recover_notices(state, binding, &mut http, stop, notify)?;
    } else if binding.resume_operation.is_some()
        && inbox.resumed_operation != binding.resume_operation
    {
        let (_, rows) = coord.snapshot()?;
        if is_assignment_default(&mut http, binding)? && !stop.load(Ordering::SeqCst) {
            let picked = coord.pickup_rows(
                rows,
                binding.lane_name.as_deref(),
                true,
                binding.default_alias.as_deref(),
                &mut deliver,
            )?;
            pickup_reconciled(&mut coord, &picked)?;
        }
        let _lock = inbox_lock(state)?;
        inbox = load_inbox(state, binding)?;
        inbox.resumed_operation = binding.resume_operation.clone();
        atomic(&binding.inbox_path(state), &inbox)?;
    }
    let mut socket = http.events_socket(inbox.cursor, &binding.runner_id)?;
    let _ = pending.try_send(());
    let renewed = Instant::now() + Duration::from_secs(60);
    while !stop.load(Ordering::SeqCst) && Instant::now() < renewed {
        let message = match socket.read() {
            Ok(message) => message,
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(_) => return Err(Error::rejected("hosted event stream disconnected")),
        };
        let text = match message {
            Message::Text(text) => text,
            Message::Ping(_) | Message::Pong(_) => {
                let _ = socket.flush();
                continue;
            }
            Message::Close(_) => break,
            _ => return Err(Error::rejected("hosted event frame is not text")),
        };
        let event: Value = serde_json::from_str(text.as_ref())
            .map_err(|_| Error::rejected("hosted event frame is invalid"))?;
        if event["version"] != VERSION {
            return Err(Error::rejected("hosted event version is invalid"));
        }
        match event["type"].as_str() {
            Some("ready") => continue,
            Some("error") => {
                if matches!(
                    event["code"].as_str(),
                    Some("replay_gap" | "resync_required")
                ) {
                    let _lock = inbox_lock(state)?;
                    inbox = load_inbox(state, binding)?;
                    inbox.initialized = false;
                    atomic(&binding.inbox_path(state), &inbox)?;
                }
                return Err(Error::rejected(
                    "hosted event stream requires reconnect or reconciliation",
                ));
            }
            Some("assignment_changed") => {
                if !event["assignment_id"]
                    .as_str()
                    .is_some_and(|id| prefixed_id(id, "hla_"))
                    || event["generation"]
                        .as_u64()
                        .is_none_or(|generation| generation == 0)
                    || !matches!(
                        event["state"].as_str(),
                        Some(
                            "waiting"
                                | "queued"
                                | "running"
                                | "blocked"
                                | "failed"
                                | "result"
                                | "cancelled"
                        )
                    )
                {
                    return Err(Error::rejected("assignment event is invalid"));
                }
            }
            Some("agent_notified") => {
                if event["kind"] != "fyi"
                    || !event["notice_id"]
                        .as_str()
                        .is_some_and(|id| prefixed_id(id, "lan_"))
                    || !event["agent_id"]
                        .as_str()
                        .is_some_and(|id| prefixed_id(id, "lag_"))
                {
                    return Err(Error::rejected("agent notice event is invalid"));
                }
            }
            _ => return Err(Error::rejected("hosted event type is invalid")),
        }
        let seq = event["seq"]
            .as_u64()
            .filter(|seq| *seq > 0)
            .ok_or_else(|| Error::rejected("event sequence is invalid"))?;
        {
            let _lock = inbox_lock(state)?;
            inbox = load_inbox(state, binding)?;
            if seq > inbox.cursor {
                if inbox.entries.len() >= INBOX_LIMIT {
                    return Err(Error::rejected(
                        "hosted event inbox is full; observe and clear FYI notices",
                    ));
                }
                inbox.entries.push(event);
                inbox.cursor = seq; // Recipient sequences are sparse, not contiguous.
                atomic(&binding.inbox_path(state), &inbox)?;
            }
        }
        socket
            .send(Message::Text(
                json!({"version": VERSION, "type": "ack", "seq": seq})
                    .to_string()
                    .into(),
            ))
            .map_err(|_| Error::rejected("hosted event ACK failed"))?;
        let _ = pending.try_send(());
        notify();
    }
    let _ = socket.close(None);
    Ok(())
}

fn process<F: FnMut(&Kickoff) -> Result<Value>>(
    state: &Path,
    binding: &WorkspaceBinding,
    coord: &mut Coordinator<BoardHttp>,
    http: &mut BoardHttp,
    deliver: &mut F,
    stop: &AtomicBool,
) -> Result<()> {
    let entries = {
        let _lock = inbox_lock(state)?;
        load_inbox(state, binding)?.entries
    };
    for event in entries {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let seq = event["seq"].as_u64();
        if event["type"] == "assignment_changed" {
            let id = event["assignment_id"]
                .as_str()
                .ok_or_else(|| Error::rejected("stored assignment event is invalid"))?;
            // A stale old-recipient invalidation still fences local custody,
            // even when its row is no longer visible over HTTPS.
            let generation = event["generation"].as_u64().unwrap_or(0);
            coord.with_ledger(|ledger| {
                for attempt in ledger
                    .attempts
                    .values_mut()
                    .filter(|attempt| attempt.assignment_id.as_deref() == Some(id))
                {
                    if attempt.generation.is_none_or(|prior| generation > prior) {
                        retire_current_claim(attempt);
                        attempt.generation = Some(generation);
                        attempt.fenced = true;
                    }
                    if attempt.generation == Some(generation)
                        && matches!(
                            event["state"].as_str(),
                            Some("result" | "failed" | "cancelled")
                        )
                    {
                        attempt.state = event["state"].as_str().unwrap_or("cancelled").to_string();
                        attempt.fenced = true;
                    }
                }
                Ok(())
            })?;
            let path = format!("{}/{id}", coord.base());
            let reply = coord.http.send("GET", &path, None)?;
            match reply.status {
                200 => {
                    let row = coord.parse_owned(&reply.body)?;
                    if row.assignment_id != id {
                        return Err(Error::rejected("event read named another assignment"));
                    }
                    coord.remember_discovered(&row)?;
                    if is_assignment_default(http, binding)? {
                        let picked = coord.pickup_rows(
                            vec![row],
                            binding.lane_name.as_deref(),
                            false,
                            binding.default_alias.as_deref(),
                            deliver,
                        )?;
                        pickup_reconciled(coord, &picked)?;
                    }
                }
                403 | 404 => {}
                _ => return Err(Error::rejected("event assignment read was refused")),
            }
            let _lock = inbox_lock(state)?;
            let mut inbox = load_inbox(state, binding)?;
            inbox.entries.retain(|entry| entry["seq"].as_u64() != seq);
            atomic(&binding.inbox_path(state), &inbox)?;
        } else if event["type"] == "agent_notified" && event.get("notice").is_none() {
            let agent = event["agent_id"]
                .as_str()
                .ok_or_else(|| Error::rejected("stored agent notice is invalid"))?;
            let notice = event["notice_id"]
                .as_str()
                .ok_or_else(|| Error::rejected("stored notice identity is invalid"))?;
            let reply = http.send(
                "GET",
                &format!(
                    "/__platform/hosted-cadence/{}/local-agents/{agent}/notices/{notice}",
                    binding.organization_id
                ),
                None,
            )?;
            if reply.status != 200
                || reply.body["version"] != DIRECTORY_VERSION
                || reply.body["kind"] != "fyi"
                || reply.body["agent_id"] != agent
                || reply.body["notice_id"] != notice
                || reply.body["text"]
                    .as_str()
                    .is_none_or(|text| text.len() > 16000)
            {
                return Err(Error::rejected(
                    "workspace refused the informational notice",
                ));
            }
            let _lock = inbox_lock(state)?;
            let mut inbox = load_inbox(state, binding)?;
            if let Some(entry) = inbox
                .entries
                .iter_mut()
                .find(|entry| entry["seq"].as_u64() == seq)
            {
                entry["notice"] = reply.body;
            }
            atomic(&binding.inbox_path(state), &inbox)?;
            // Deliberately no agent_send, actor wake, or model call for FYI.
        }
    }
    Ok(())
}
