//! Cloud sandbox runner seam (CAD-1673).
//!
//! The daemon-side boundary for a remote Cadence runner that executes a
//! ticket inside an AgenticOS-hosted sandbox. This module owns three
//! things and nothing else:
//!
//! - [`SandboxClient`]: the job lifecycle (create, collect events, fetch
//!   result, stop) as seen from the daemon. The transport is injected.
//! - [`SandboxEntitlement`]: the per-company `cloud_sandbox` check. There
//!   is no environment flag and no hardcoded company id; the company is
//!   whatever the AgenticOS API answers for the caller.
//! - [`ResultRelay`]: the idempotent report of a remote result (sha,
//!   diff, PR, result text) back to the daemon through the client
//!   boundary. A local unix socket inside the sandbox is never assumed.
//!
//! TODO(CAD-1673 / AOS-324): no real transport is wired. Before this
//! kind can run, wire these AOS-324 contract artifacts: the sandbox
//! route names (create, events, result, stop) and the entitlement route
//! for `cloud_sandbox`; their request and response schemas; and the auth
//! header the daemon presents. Until then only the test fakes implement
//! [`SandboxClient`] and [`SandboxEntitlement`].
//!
//! The `sandbox` endpoint kind is registered `internal`, so it is not
//! selectable as a working production endpoint and has no factory arm.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::error::{Error, Result, Structured};

/// Stable refusal code when the company lacks the `cloud_sandbox`
/// entitlement, or the entitlement cannot be read.
pub const ENTITLEMENT_REFUSED: &str = "cloud_sandbox_not_entitled";

/// The entitlement key looked up for the calling company.
pub const ENTITLEMENT_KEY: &str = "cloud_sandbox";

/// Pi config base URL for model calls inside the sandbox. Model traffic
/// goes through AgenticOS; the sandbox holds no provider key.
pub const PI_BASE_URL: &str = "http://gateway.internal";

/// Placeholder token written into the sandbox Pi config. It is not a
/// credential; the gateway authenticates the sandbox job.
pub const PI_PLACEHOLDER_TOKEN: &str = "cadence-sandbox-placeholder";

/// A remote job handle. `sandbox_id` is what the daemon records on the
/// agent row; `branch` is the Git ref the remote runner pushes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxJob {
    pub sandbox_id: String,
    pub branch: String,
}

/// One event streamed from a running sandbox job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxEvent {
    pub seq: u64,
    pub kind: String,
    pub text: String,
}

/// The result a remote runner reports. Every field is optional except
/// `text`; the daemon records what the runner provided.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SandboxResult {
    pub sha: Option<String>,
    pub diff: Option<String>,
    pub pr: Option<String>,
    pub text: String,
}

/// Job lifecycle over the AgenticOS sandbox API, from the daemon's view.
///
/// TODO(CAD-1673 / AOS-324): wire the real transport once AOS-324 lands
/// its contract artifacts: the sandbox route names for create, events,
/// result, and stop; their request and response schemas; and the auth
/// header the daemon presents. Until then only [`FakeSandboxClient`]
/// implements this trait.
pub trait SandboxClient: Send + Sync {
    /// Create a job for `ticket_id` on `branch` with the inlined `spec`.
    fn create_job(&self, ticket_id: &str, branch: &str, spec: &str) -> Result<SandboxJob>;
    /// Events recorded for the job after `after_seq`, in order.
    fn collect_events(&self, job: &SandboxJob, after_seq: u64) -> Result<Vec<SandboxEvent>>;
    /// The job's result, once it has settled.
    fn fetch_result(&self, job: &SandboxJob) -> Result<SandboxResult>;
    /// Stop the job. Stopping an already-stopped job is not an error.
    fn stop_job(&self, job: &SandboxJob) -> Result<()>;
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
}

/// Create a remote lane for `ticket_id` after the entitlement passes. The
/// caller must have run the acceptance and plan gate before this call.
pub fn create_remote_lane(
    client: &dyn SandboxClient,
    entitlement: &dyn SandboxEntitlement,
    company: &str,
    ticket_id: &str,
    branch: &str,
    spec: &str,
) -> Result<RemoteLane> {
    require_entitlement(entitlement, company)?;
    let job = client.create_job(ticket_id, branch, spec)?;
    Ok(RemoteLane {
        ticket_id: ticket_id.to_string(),
        branch: job.branch,
        sandbox_id: job.sandbox_id,
    })
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
    /// recorded it, `Ok(false)` when it was already reported.
    pub fn relay(&self, client: &dyn SandboxClient, job: &SandboxJob) -> Result<bool> {
        let mut reported = self.reported.lock().unwrap();
        if reported.contains_key(&job.sandbox_id) {
            return Ok(false);
        }
        let result = client.fetch_result(job)?;
        reported.insert(job.sandbox_id.clone(), result);
        Ok(true)
    }

    /// The recorded result for `sandbox_id`, if one was relayed.
    pub fn recorded(&self, sandbox_id: &str) -> Option<SandboxResult> {
        self.reported.lock().unwrap().get(sandbox_id).cloned()
    }
}

/// In-memory fake for tests: scripted entitlement, scripted job results,
/// and a call log for asserting that no host worktree was requested.
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

    #[derive(Default)]
    pub struct FakeSandboxClient {
        pub created: Mutex<Vec<(String, String, String)>>,
        pub fetches: Mutex<usize>,
        pub stops: Mutex<usize>,
        pub result: SandboxResult,
    }

    impl SandboxClient for FakeSandboxClient {
        fn create_job(&self, ticket_id: &str, branch: &str, spec: &str) -> Result<SandboxJob> {
            self.created.lock().unwrap().push((
                ticket_id.to_string(),
                branch.to_string(),
                spec.to_string(),
            ));
            Ok(SandboxJob {
                sandbox_id: format!("sbx-{}", self.created.lock().unwrap().len()),
                branch: branch.to_string(),
            })
        }

        fn collect_events(&self, _job: &SandboxJob, _after: u64) -> Result<Vec<SandboxEvent>> {
            Ok(Vec::new())
        }

        fn fetch_result(&self, _job: &SandboxJob) -> Result<SandboxResult> {
            *self.fetches.lock().unwrap() += 1;
            Ok(self.result.clone())
        }

        fn stop_job(&self, _job: &SandboxJob) -> Result<()> {
            *self.stops.lock().unwrap() += 1;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{FakeEntitlement, FakeSandboxClient};
    use super::*;

    fn entitled() -> FakeEntitlement {
        FakeEntitlement { enabled: Ok(true) }
    }

    fn code_of(err: &Error) -> Option<&str> {
        err.code()
    }

    #[test]
    fn entitlement_refusal_has_stable_code_and_creates_nothing() {
        let client = FakeSandboxClient::default();
        for enabled in [Ok(false), Err("boom".to_string())] {
            let ent = FakeEntitlement { enabled };
            let err =
                create_remote_lane(&client, &ent, "co-1", "CAD-1", "b", "spec").unwrap_err();
            assert_eq!(code_of(&err), Some(ENTITLEMENT_REFUSED), "{err:?}");
        }
        assert!(client.created.lock().unwrap().is_empty());
    }

    #[test]
    fn remote_lane_is_created_with_branch_and_sandbox_id() {
        let client = FakeSandboxClient::default();
        let lane = create_remote_lane(&client, &entitled(), "co-1", "CAD-1", "cad-1-x", "spec")
            .unwrap();
        assert_eq!(lane.branch, "cad-1-x");
        assert_eq!(lane.sandbox_id, "sbx-1");
        // The spec is inlined into the create call, not fetched later.
        assert_eq!(client.created.lock().unwrap()[0].2, "spec");
    }

    #[test]
    fn remote_lane_never_touches_a_host_worktree() {
        // The client surface has no worktree method; the lane record holds
        // only ticket, branch, and sandbox id. Nothing here names a path.
        let client = FakeSandboxClient::default();
        let lane =
            create_remote_lane(&client, &entitled(), "co-1", "CAD-1", "b", "spec").unwrap();
        let debug = format!("{lane:?}");
        assert!(!debug.contains("worktree"), "{debug}");
        assert!(!debug.contains('/'), "{debug}");
    }

    #[test]
    fn result_relay_is_idempotent_and_reports_once() {
        let client = FakeSandboxClient {
            result: SandboxResult {
                sha: Some("a".repeat(40)),
                diff: Some("+x".to_string()),
                pr: Some("https://example.invalid/pr/1".to_string()),
                text: "done".to_string(),
            },
            ..Default::default()
        };
        let job = SandboxJob {
            sandbox_id: "sbx-1".to_string(),
            branch: "b".to_string(),
        };
        let relay = ResultRelay::default();
        assert!(relay.relay(&client, &job).unwrap());
        assert!(!relay.relay(&client, &job).unwrap());
        assert!(!relay.relay(&client, &job).unwrap());
        // Fetched exactly once across three relays: no double report.
        assert_eq!(*client.fetches.lock().unwrap(), 1);
        assert_eq!(relay.recorded("sbx-1").unwrap().text, "done");
    }

    #[test]
    fn no_hardcoded_company_or_sandbox_ids() {
        // The entitlement is looked up for whichever company the caller
        // passes; a different company must not reuse another's answer.
        struct ByCompany;
        impl SandboxEntitlement for ByCompany {
            fn cloud_sandbox_enabled(&self, company: &str) -> Result<bool> {
                Ok(company == "co-allowed")
            }
        }
        let client = FakeSandboxClient::default();
        assert!(create_remote_lane(&client, &ByCompany, "co-allowed", "CAD-1", "b", "s").is_ok());
        let err = create_remote_lane(&client, &ByCompany, "co-other", "CAD-1", "b", "s")
            .unwrap_err();
        assert_eq!(code_of(&err), Some(ENTITLEMENT_REFUSED));
        assert_eq!(client.created.lock().unwrap().len(), 1);
    }

    #[test]
    fn pi_config_points_at_gateway_with_placeholder_token() {
        assert_eq!(PI_BASE_URL, "http://gateway.internal");
        assert!(PI_PLACEHOLDER_TOKEN.starts_with("cadence-sandbox-"));
        assert!(!PI_PLACEHOLDER_TOKEN.contains("sk-"));
    }
}
