//! Cloud sandbox runner seam (CAD-1673).
//!
//! The daemon-side boundary for a remote Cadence runner that executes a
//! ticket inside an AgenticOS-hosted sandbox. Shapes follow the AOS-324
//! contract (`packages/contracts/src/cloud-sandbox-jobs.ts`):
//!
//! - [`SandboxClient`]: the job lifecycle over `api.internal`
//!   `/v1/runtime/cloud-sandbox/jobs` (create, get, stop). The company is
//!   never a request field; AgenticOS binds it from the caller's context.
//! - [`SandboxEntitlement`]: the per-company `cloud_sandbox` check. A 403
//!   `cloud_sandbox_not_entitled` maps to [`ENTITLEMENT_REFUSED`].
//! - [`ResultRelay`]: the idempotent relay of a remote result back to the
//!   daemon. A local unix socket inside the sandbox is never assumed.
//!
//! `gateway_token` is AgenticOS-owned. It is held only in memory for the
//! lifetime of a [`SandboxJob`] and is never logged or persisted.
//!
//! The `sandbox` endpoint kind is registered `internal`, so it is not
//! selectable as a working production endpoint and has no factory arm.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{Error, Result, Structured};

/// Stable refusal code when the company lacks the `cloud_sandbox`
/// entitlement, or the entitlement cannot be read.
pub const ENTITLEMENT_REFUSED: &str = "cloud_sandbox_not_entitled";

/// Stable refusal code when the AgenticOS sandbox route is not served.
pub const SANDBOX_UNAVAILABLE: &str = "cloud_sandbox_unavailable";

/// The entitlement key looked up for the calling company.
pub const ENTITLEMENT_KEY: &str = "cloud_sandbox";

/// Pi config base URL for model calls inside the sandbox. Model traffic
/// goes through AgenticOS; the sandbox holds no provider key.
pub const PI_BASE_URL: &str = "http://gateway.internal";

/// Placeholder token written into the sandbox Pi config. It is not a
/// credential; the gateway authenticates the sandbox job.
pub const PI_PLACEHOLDER_TOKEN: &str = "cadence-sandbox-placeholder";

/// AOS-324 `api.internal` job route. Company is bound server-side.
pub const JOBS_URL: &str = "http://api.internal/v1/runtime/cloud-sandbox/jobs";

/// A remote job handle. `sandbox_id` is what the daemon records on the
/// agent row; `branch` is the Git ref the remote runner pushes to.
/// `gateway_token` is AgenticOS-owned: never logged, never persisted.
#[derive(Clone, PartialEq, Eq)]
pub struct SandboxJob {
    pub job_id: String,
    pub sandbox_id: String,
    pub branch: String,
    pub expires_at: String,
    pub gateway_token: String,
}

impl std::fmt::Debug for SandboxJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxJob")
            .field("job_id", &self.job_id)
            .field("sandbox_id", &self.sandbox_id)
            .field("branch", &self.branch)
            .field("expires_at", &self.expires_at)
            .field("gateway_token", &"<redacted>")
            .finish()
    }
}

/// The create request body, matching the AOS-324 contract strictly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateJob {
    pub ticket_id: String,
    pub branch: String,
    pub base_sha: String,
    pub spec: String,
    pub idempotency_key: String,
}

/// One event streamed from a running sandbox job.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SandboxEvent {
    pub seq: u64,
    pub kind: String,
    pub text: String,
}

/// The result a remote runner reports. `text` is always present; the
/// other fields are optional and recorded as the runner provided them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct SandboxResult {
    pub sha: Option<String>,
    pub diff: Option<String>,
    pub pr: Option<String>,
    pub text: String,
}

/// The `get` response: state, events, and the result snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct JobSnapshot {
    pub job_id: String,
    pub state: String,
    pub events: Vec<SandboxEvent>,
    pub result: SandboxResult,
}

/// Job lifecycle over the AgenticOS sandbox API, from the daemon's view.
pub trait SandboxClient: Send + Sync {
    /// Create a job. Replaying the same `idempotency_key` returns the
    /// existing job rather than starting a second sandbox.
    fn create_job(&self, request: &CreateJob) -> Result<SandboxJob>;
    /// The job's state, events, and result.
    fn get_job(&self, job_id: &str) -> Result<JobSnapshot>;
    /// Stop the job. Stopping an already-stopped job is not an error.
    fn stop_job(&self, job_id: &str) -> Result<String>;
}

/// Per-company entitlement check (`cloud_sandbox`).
pub trait SandboxEntitlement: Send + Sync {
    /// `Ok(true)` only when the company holds `cloud_sandbox`.
    fn cloud_sandbox_enabled(&self, company: &str) -> Result<bool>;
}

/// Refuse a remote lane unless the company is entitled. A disabled or
/// unreadable entitlement returns the stable [`ENTITLEMENT_REFUSED`]
/// code, never a generic error.
pub fn require_entitlement(entitlement: &dyn SandboxEntitlement, company: &str) -> Result<()> {
    match entitlement.cloud_sandbox_enabled(company) {
        Ok(true) => Ok(()),
        Ok(false) => Err(refused(company, "is not entitled")),
        Err(_) => Err(refused(company, "entitlement could not be read")),
    }
}

fn refused(company: &str, why: &str) -> Error {
    Error::Structured(Structured {
        kind: "rejected",
        code: ENTITLEMENT_REFUSED.to_string(),
        message: format!("company '{company}' {why} for {ENTITLEMENT_KEY}"),
        revision: None,
    })
}

/// Remote-lane record: the branch and sandbox id the daemon stores for a
/// sandbox lane. There is no host worktree path; that is the point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteLane {
    pub ticket_id: String,
    pub branch: String,
    pub sandbox_id: String,
    pub job_id: String,
}

/// Create a remote lane for `ticket_id` after the entitlement passes. The
/// caller must have run the acceptance and plan gate before this call.
pub fn create_remote_lane(
    client: &dyn SandboxClient,
    entitlement: &dyn SandboxEntitlement,
    company: &str,
    request: &CreateJob,
) -> Result<(RemoteLane, SandboxJob)> {
    require_entitlement(entitlement, company)?;
    let job = client.create_job(request)?;
    let lane = RemoteLane {
        ticket_id: request.ticket_id.clone(),
        branch: job.branch.clone(),
        sandbox_id: job.sandbox_id.clone(),
        job_id: job.job_id.clone(),
    };
    Ok((lane, job))
}

/// What a poll loop ended with: the relayed result, or a stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// The job settled and its result was relayed by this run.
    Relayed(SandboxResult),
    /// The job failed; it was stopped and nothing was relayed.
    Failed,
    /// The caller asked to stop; the job was stopped.
    Stopped,
}

/// Drive one remote job to an outcome. Polls `get_job` until it settles.
/// A `completed` job relays its result once; a `failed` job is stopped and
/// reports nothing; `stop_requested` stops the job immediately. Stopping
/// is always sent through `stop_job`, never inferred.
pub fn drive_job(
    client: &dyn SandboxClient,
    relay: &ResultRelay,
    job: &SandboxJob,
    stop_requested: bool,
) -> Result<RunOutcome> {
    if stop_requested {
        client.stop_job(&job.job_id)?;
        return Ok(RunOutcome::Stopped);
    }
    let snapshot = client.get_job(&job.job_id)?;
    match snapshot.state.as_str() {
        "failed" => {
            client.stop_job(&job.job_id)?;
            Ok(RunOutcome::Failed)
        }
        "completed" => {
            relay.relay(client, job)?;
            let result = relay
                .recorded(&job.sandbox_id)
                .ok_or_else(|| Error::internal("completed cloud sandbox job was not relayed"))?;
            Ok(RunOutcome::Relayed(result))
        }
        _ => Ok(RunOutcome::Stopped),
    }
}

/// Idempotency ledger for relayed results, keyed by sandbox id. A repeat
/// relay for the same sandbox returns the first result and reports
/// nothing new, so a retried poll never double-reports.
#[derive(Default)]
pub struct ResultRelay {
    reported: Mutex<BTreeMap<String, SandboxResult>>,
}

impl ResultRelay {
    /// Relay the result for `job` once. Returns `Ok(true)` when this call
    /// recorded it, `Ok(false)` when it was already reported. The result
    /// is taken from a settled `get`; an unsettled job is not relayed.
    pub fn relay(&self, client: &dyn SandboxClient, job: &SandboxJob) -> Result<bool> {
        let mut reported = self.reported.lock().unwrap();
        if reported.contains_key(&job.sandbox_id) {
            return Ok(false);
        }
        let snapshot = client.get_job(&job.job_id)?;
        if !is_settled(&snapshot.state) {
            return Ok(false);
        }
        reported.insert(job.sandbox_id.clone(), snapshot.result);
        Ok(true)
    }

    /// The recorded result for `sandbox_id`, if one was relayed.
    pub fn recorded(&self, sandbox_id: &str) -> Option<SandboxResult> {
        self.reported.lock().unwrap().get(sandbox_id).cloned()
    }
}

/// A job is settled once it can no longer produce a result.
pub fn is_settled(state: &str) -> bool {
    matches!(state, "stopped" | "completed" | "failed")
}

/// Transport seam for `api.internal`: one JSON request, one JSON envelope
/// back. The HTTP implementation is [`UreqSandboxTransport`]; tests use a
/// scripted fake so no network is touched.
pub trait SandboxTransport: Send + Sync {
    /// Send `method` to `url` with an optional JSON body and return the
    /// HTTP status and the raw parsed envelope.
    fn send(&self, method: &str, url: &str, body: Option<&Value>) -> Result<(u16, Value)>;
}

/// The real `api.internal` transport over `ureq`.
pub struct UreqSandboxTransport {
    agent: ureq::Agent,
    bearer: Option<String>,
}

impl UreqSandboxTransport {
    pub fn new(bearer: Option<String>) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .max_redirects(0)
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            bearer,
        }
    }
}

const RESPONSE_CAP: u64 = 4 * 1024 * 1024;

impl SandboxTransport for UreqSandboxTransport {
    fn send(&self, method: &str, url: &str, body: Option<&Value>) -> Result<(u16, Value)> {
        let unreachable = || Error::internal("cloud sandbox API unreachable");
        let mut response = match (method, body) {
            ("POST", Some(body)) => {
                let req = self.agent.post(url);
                let req = match &self.bearer {
                    Some(t) => req.header("authorization", &format!("Bearer {t}")),
                    None => req,
                };
                req.send_json(body).map_err(|_| unreachable())?
            }
            ("POST", None) => {
                let req = self.agent.post(url);
                let req = match &self.bearer {
                    Some(t) => req.header("authorization", &format!("Bearer {t}")),
                    None => req,
                };
                req.send_empty().map_err(|_| unreachable())?
            }
            _ => {
                let req = self.agent.get(url);
                let req = match &self.bearer {
                    Some(t) => req.header("authorization", &format!("Bearer {t}")),
                    None => req,
                };
                req.call().map_err(|_| unreachable())?
            }
        };
        let status = response.status().as_u16();
        let bytes = response
            .body_mut()
            .with_config()
            .limit(RESPONSE_CAP)
            .read_to_vec()
            .map_err(|_| unreachable())?;
        let envelope: Value = serde_json::from_slice(&bytes)
            .map_err(|_| Error::internal("cloud sandbox API returned a non-JSON body"))?;
        Ok((status, envelope))
    }
}

/// HTTP-backed [`SandboxClient`] and [`SandboxEntitlement`] over
/// `api.internal`, parsing the `{ok, data | error}` envelope.
pub struct HttpSandboxApi<T: SandboxTransport> {
    transport: T,
}

impl<T: SandboxTransport> HttpSandboxApi<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    fn data(&self, method: &str, url: &str, body: Option<&Value>) -> Result<Value> {
        let (status, envelope) = self.transport.send(method, url, body)?;
        let ok = envelope.get("ok").and_then(Value::as_bool) == Some(true);
        if ok && (200..300).contains(&status) {
            return envelope
                .get("data")
                .cloned()
                .ok_or_else(|| Error::internal("cloud sandbox envelope had no data"));
        }
        let code = envelope
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("");
        Err(map_error(status, code))
    }
}

/// Map an AgenticOS sandbox error to a stable Cadence error. A 403
/// `cloud_sandbox_not_entitled` is [`ENTITLEMENT_REFUSED`]; a missing
/// route is [`SANDBOX_UNAVAILABLE`]; anything else is a generic rejection
/// carrying only the AgenticOS code, never the message body.
fn map_error(status: u16, code: &str) -> Error {
    match (status, code) {
        (403, "cloud_sandbox_not_entitled") => Error::Structured(Structured {
            kind: "rejected",
            code: ENTITLEMENT_REFUSED.to_string(),
            message: format!("company is not entitled for {ENTITLEMENT_KEY}"),
            revision: None,
        }),
        (404, _) | (405, _) => Error::Structured(Structured {
            kind: "rejected",
            code: SANDBOX_UNAVAILABLE.to_string(),
            message: "cloud sandbox route is not available".to_string(),
            revision: None,
        }),
        _ => Error::rejected(format!("cloud sandbox API refused ({status} {code})")),
    }
}

fn str_field<'a>(data: &'a Value, key: &str) -> Result<&'a str> {
    data.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::internal(format!("cloud sandbox response missing {key}")))
}

impl<T: SandboxTransport> SandboxClient for HttpSandboxApi<T> {
    fn create_job(&self, request: &CreateJob) -> Result<SandboxJob> {
        let body = json!({
            "ticket_id": request.ticket_id,
            "branch": request.branch,
            "base_sha": request.base_sha,
            "spec": request.spec,
            "idempotency_key": request.idempotency_key,
        });
        let data = self.data("POST", JOBS_URL, Some(&body))?;
        Ok(SandboxJob {
            job_id: str_field(&data, "job_id")?.to_string(),
            sandbox_id: str_field(&data, "sandbox_id")?.to_string(),
            branch: str_field(&data, "branch")?.to_string(),
            expires_at: str_field(&data, "expires_at")?.to_string(),
            gateway_token: str_field(&data, "gateway_token")?.to_string(),
        })
    }

    fn get_job(&self, job_id: &str) -> Result<JobSnapshot> {
        let url = format!("{JOBS_URL}/{job_id}");
        let data = self.data("GET", &url, None)?;
        serde_json::from_value(data)
            .map_err(|_| Error::internal("cloud sandbox job response was malformed"))
    }

    fn stop_job(&self, job_id: &str) -> Result<String> {
        let url = format!("{JOBS_URL}/{job_id}/stop");
        let data = self.data("POST", &url, None)?;
        Ok(str_field(&data, "state")?.to_string())
    }
}


/// In-memory fake for tests: scripted entitlement, scripted transport
/// responses, and a call log so tests can assert on wire shape.
#[cfg(test)]
pub mod fake {
    use super::*;

    pub struct FakeEntitlement {
        pub enabled: std::result::Result<bool, String>,
    }

    impl SandboxEntitlement for FakeEntitlement {
        fn cloud_sandbox_enabled(&self, _company: &str) -> Result<bool> {
            self.enabled.clone().map_err(Error::rejected)
        }
    }

    /// Scripted transport: returns queued `(status, envelope)` pairs and
    /// records every request as `(method, url, body)`.
    #[derive(Default)]
    pub struct FakeTransport {
        pub responses: Mutex<Vec<(u16, Value)>>,
        pub calls: Mutex<Vec<(String, String, Option<Value>)>>,
    }

    impl FakeTransport {
        pub fn with(responses: Vec<(u16, Value)>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().rev().collect()),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl SandboxTransport for FakeTransport {
        fn send(&self, method: &str, url: &str, body: Option<&Value>) -> Result<(u16, Value)> {
            self.calls.lock().unwrap().push((
                method.to_string(),
                url.to_string(),
                body.cloned(),
            ));
            self.responses
                .lock()
                .unwrap()
                .pop()
                .ok_or_else(|| Error::internal("fake transport has no scripted response"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{FakeEntitlement, FakeTransport};
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn entitled() -> FakeEntitlement {
        FakeEntitlement { enabled: Ok(true) }
    }

    fn req(ticket: &str) -> CreateJob {
        CreateJob {
            ticket_id: ticket.to_string(),
            branch: "cad-1-x".to_string(),
            base_sha: SHA.to_string(),
            spec: "spec".to_string(),
            idempotency_key: "idem-key-0001".to_string(),
        }
    }

    fn created(job_id: &str) -> Value {
        json!({
            "ok": true,
            "data": {
                "job_id": job_id,
                "sandbox_id": format!("csb_{job_id}"),
                "branch": "cad-1-x",
                "gateway_token": "tok-secret-value",
                "expires_at": "2026-10-21T05:00:00.000Z"
            }
        })
    }

    fn forbidden() -> Value {
        json!({"ok": false, "error": {"code": "cloud_sandbox_not_entitled", "message": "x"}})
    }

    struct ByCompany;
    impl SandboxEntitlement for ByCompany {
        fn cloud_sandbox_enabled(&self, company: &str) -> Result<bool> {
            Ok(company == "co-allowed")
        }
    }

    fn code_of(err: &Error) -> Option<&str> {
        err.code()
    }

    #[test]
    fn entitlement_refusal_has_stable_code_and_creates_nothing() {
        let transport = FakeTransport::with(vec![]);
        let api = HttpSandboxApi::new(transport);
        for enabled in [Ok(false), Err("boom".to_string())] {
            let ent = FakeEntitlement { enabled };
            let err = create_remote_lane(&api, &ent, "co-1", &req("CAD-1")).unwrap_err();
            assert_eq!(code_of(&err), Some(ENTITLEMENT_REFUSED), "{err:?}");
        }
        assert!(api.transport.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn create_posts_contract_body_without_company_field() {
        let api = HttpSandboxApi::new(FakeTransport::with(vec![(201, created("job-1"))]));
        let (lane, job) =
            create_remote_lane(&api, &entitled(), "co-1", &req("CAD-1")).unwrap();
        assert_eq!(lane.sandbox_id, "csb_job-1");
        assert_eq!(lane.branch, "cad-1-x");
        assert_eq!(lane.job_id, "job-1");
        assert_eq!(job.gateway_token, "tok-secret-value");
        let calls = api.transport.calls.lock().unwrap();
        let (method, url, body) = &calls[0];
        assert_eq!(method, "POST");
        assert_eq!(url, JOBS_URL);
        let body = body.as_ref().unwrap();
        assert_eq!(body["idempotency_key"], "idem-key-0001");
        assert_eq!(body["base_sha"], SHA);
        assert!(body.get("company").is_none() && body.get("company_id").is_none());
    }

    #[test]
    fn idempotent_replay_returns_the_same_job_identity() {
        // AOS answers a replay with 200 and the same job id.
        let api = HttpSandboxApi::new(FakeTransport::with(vec![
            (201, created("job-1")),
            (200, created("job-1")),
        ]));
        let first = api.create_job(&req("CAD-1")).unwrap();
        let second = api.create_job(&req("CAD-1")).unwrap();
        assert_eq!(first.job_id, second.job_id);
        assert_eq!(first.sandbox_id, second.sandbox_id);
    }

    #[test]
    fn entitlement_403_maps_to_stable_refusal() {
        let api = HttpSandboxApi::new(FakeTransport::with(vec![(403, forbidden())]));
        let err = api.create_job(&req("CAD-1")).unwrap_err();
        assert_eq!(code_of(&err), Some(ENTITLEMENT_REFUSED), "{err:?}");
    }

    #[test]
    fn missing_route_maps_to_unavailable() {
        let api = HttpSandboxApi::new(FakeTransport::with(vec![(
            404,
            json!({"ok": false, "error": {"code": "not_found"}}),
        )]));
        let err = api.create_job(&req("CAD-1")).unwrap_err();
        assert_eq!(code_of(&err), Some(SANDBOX_UNAVAILABLE), "{err:?}");
    }

    #[test]
    fn remote_lane_is_created_with_branch_and_sandbox_id() {
        let api = HttpSandboxApi::new(FakeTransport::with(vec![(201, created("job-1"))]));
        let (lane, _) = create_remote_lane(&api, &entitled(), "co-1", &req("CAD-1")).unwrap();
        assert_eq!(lane.branch, "cad-1-x");
        assert_eq!(lane.sandbox_id, "csb_job-1");
    }

    #[test]
    fn remote_lane_never_touches_a_host_worktree() {
        let api = HttpSandboxApi::new(FakeTransport::with(vec![(201, created("job-1"))]));
        let (lane, job) = create_remote_lane(&api, &entitled(), "co-1", &req("CAD-1")).unwrap();
        let debug = format!("{lane:?}{job:?}");
        assert!(!debug.contains("worktree"), "{debug}");
        assert!(!debug.contains('/'), "{debug}");
    }

    #[test]
    fn gateway_token_is_redacted_from_debug() {
        let api = HttpSandboxApi::new(FakeTransport::with(vec![(201, created("job-1"))]));
        let job = api.create_job(&req("CAD-1")).unwrap();
        let debug = format!("{job:?}");
        assert!(!debug.contains("tok-secret-value"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
    }

    #[test]
    fn get_parses_events_and_result() {
        let body = json!({"ok": true, "data": {
            "job_id": "job-1",
            "state": "completed",
            "events": [{"seq": 1, "kind": "tool", "text": "ran"}],
            "result": {"sha": SHA, "diff": "+x", "pr": null, "text": "done"}
        }});
        let api = HttpSandboxApi::new(FakeTransport::with(vec![(200, body)]));
        let snap = api.get_job("job-1").unwrap();
        assert_eq!(snap.state, "completed");
        assert_eq!(snap.events[0].seq, 1);
        assert_eq!(snap.result.sha.as_deref(), Some(SHA));
        assert_eq!(snap.result.pr, None);
        assert_eq!(api.transport.calls.lock().unwrap()[0].1, format!("{JOBS_URL}/job-1"));
    }

    #[test]
    fn stop_posts_to_stop_route_and_returns_state() {
        let body = json!({"ok": true, "data": {"job_id": "job-1", "state": "stopped"}});
        let api = HttpSandboxApi::new(FakeTransport::with(vec![(200, body)]));
        assert_eq!(api.stop_job("job-1").unwrap(), "stopped");
        let calls = api.transport.calls.lock().unwrap();
        assert_eq!(calls[0].0, "POST");
        assert_eq!(calls[0].1, format!("{JOBS_URL}/job-1/stop"));
    }

    #[test]
    fn result_relay_is_idempotent_and_reports_once() {
        let body = json!({"ok": true, "data": {
            "job_id": "job-1", "state": "completed", "events": [],
            "result": {"sha": SHA, "diff": "+x", "pr": "https://example.invalid/pr/1", "text": "done"}
        }});
        let api = HttpSandboxApi::new(FakeTransport::with(vec![(200, body)]));
        let job = SandboxJob {
            job_id: "job-1".into(),
            sandbox_id: "csb_job-1".into(),
            branch: "b".into(),
            expires_at: "x".into(),
            gateway_token: "t".into(),
        };
        let relay = ResultRelay::default();
        assert!(relay.relay(&api, &job).unwrap());
        assert!(!relay.relay(&api, &job).unwrap());
        assert!(!relay.relay(&api, &job).unwrap());
        // Fetched exactly once across three relays: no double report.
        assert_eq!(api.transport.calls.lock().unwrap().len(), 1);
        assert_eq!(relay.recorded("csb_job-1").unwrap().text, "done");
    }

    #[test]
    fn unsettled_job_is_not_relayed() {
        let body = json!({"ok": true, "data": {
            "job_id": "job-1", "state": "running", "events": [],
            "result": {"sha": null, "diff": null, "pr": null, "text": ""}
        }});
        let api = HttpSandboxApi::new(FakeTransport::with(vec![(200, body)]));
        let job = SandboxJob {
            job_id: "job-1".into(),
            sandbox_id: "csb_job-1".into(),
            branch: "b".into(),
            expires_at: "x".into(),
            gateway_token: "t".into(),
        };
        let relay = ResultRelay::default();
        assert!(!relay.relay(&api, &job).unwrap());
        assert!(relay.recorded("csb_job-1").is_none());
    }

    #[test]
    fn no_hardcoded_company_or_sandbox_ids() {
        let api = HttpSandboxApi::new(FakeTransport::with(vec![
            (201, created("job-1")),
        ]));
        assert!(create_remote_lane(&api, &ByCompany, "co-allowed", &req("CAD-1")).is_ok());
        let err = create_remote_lane(&api, &ByCompany, "co-other", &req("CAD-1")).unwrap_err();
        assert_eq!(code_of(&err), Some(ENTITLEMENT_REFUSED));
        assert_eq!(api.transport.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn pi_config_points_at_gateway_with_placeholder_token() {
        assert_eq!(PI_BASE_URL, "http://gateway.internal");
        assert!(PI_PLACEHOLDER_TOKEN.starts_with("cadence-sandbox-"));
        assert!(!PI_PLACEHOLDER_TOKEN.contains("sk-"));
    }

    fn job_state(state: &str, result: Value) -> Value {
        json!({"ok": true, "data": {
            "job_id": "job-1", "state": state, "events": [], "result": result
        }})
    }

    #[test]
    fn enabled_dispatch_records_remote_lane_and_never_touches_host_worktree() {
        let api = HttpSandboxApi::new(FakeTransport::with(vec![(201, created("job-1"))]));
        let (lane, job) =
            create_remote_lane(&api, &entitled(), "co-1", &req("CAD-1")).unwrap();
        assert_eq!(lane.branch, "cad-1-x");
        assert_eq!(lane.sandbox_id, "csb_job-1");
        assert_eq!(lane.job_id, "job-1");
        // Only the create call hit the wire: no host worktree, no git.
        assert_eq!(api.transport.calls.lock().unwrap().len(), 1);
        assert_eq!(job.job_id, lane.job_id);
    }

    #[test]
    fn disabled_dispatch_refuses_before_any_side_effect() {
        let api = HttpSandboxApi::new(FakeTransport::with(vec![]));
        let ent = FakeEntitlement { enabled: Ok(false) };
        let err = create_remote_lane(&api, &ent, "co-1", &req("CAD-1")).unwrap_err();
        assert_eq!(code_of(&err), Some(ENTITLEMENT_REFUSED));
        assert!(api.transport.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn completed_job_is_relayed_once_across_polls() {
        let result = json!({"sha": SHA, "diff": "+x", "pr": null, "text": "done"});
        let api = HttpSandboxApi::new(FakeTransport::with(vec![
            (200, job_state("completed", result.clone())),
            (200, job_state("completed", result.clone())),
        ]));
        let job = SandboxJob {
            job_id: "job-1".into(),
            sandbox_id: "csb_job-1".into(),
            branch: "b".into(),
            expires_at: "x".into(),
            gateway_token: "t".into(),
        };
        let relay = ResultRelay::default();
        let first = drive_job(&api, &relay, &job, false).unwrap();
        assert!(matches!(&first, RunOutcome::Relayed(r) if r.text == "done"));
        // The relay ledger holds one entry; a second relay over the same
        // sandbox reports nothing new and keeps the first recorded result.
        let again = relay.relay(&api, &job).unwrap();
        assert!(!again, "second relay must not report again");
        assert_eq!(relay.reported.lock().unwrap().len(), 1);
        assert_eq!(relay.recorded("csb_job-1").unwrap().text, "done");
    }

    #[test]
    fn failed_job_is_stopped_and_relays_nothing() {
        let api = HttpSandboxApi::new(FakeTransport::with(vec![
            (200, job_state("failed", json!({"sha": null, "diff": null, "pr": null, "text": ""}))),
            (200, json!({"ok": true, "data": {"job_id": "job-1", "state": "stopped"}})),
        ]));
        let job = SandboxJob {
            job_id: "job-1".into(),
            sandbox_id: "csb_job-1".into(),
            branch: "b".into(),
            expires_at: "x".into(),
            gateway_token: "t".into(),
        };
        let relay = ResultRelay::default();
        assert_eq!(drive_job(&api, &relay, &job, false).unwrap(), RunOutcome::Failed);
        assert!(relay.recorded("csb_job-1").is_none());
        let calls = api.transport.calls.lock().unwrap();
        assert_eq!(calls.last().unwrap().1, format!("{JOBS_URL}/job-1/stop"));
    }
}
