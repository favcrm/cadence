//! CAD-120: live operator export of native review evidence. This is not
//! a signed attestation and must never be trusted after leaving the
//! authenticated socket. GitHub publishing is a separate trust boundary.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::{json, Value};

use super::Shared;
use crate::delivery::{self, State};
use crate::error::{Error, Result};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Batch {
    requests: Vec<Request>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    issue: String,
    pr: String,
    sha: String,
}

impl Shared {
    pub(super) fn rpc_delivery_review_evidence(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("delivery review evidence", params, peer_pid)?;
        let batch: Batch = serde_json::from_value(params.clone())
            .map_err(|e| Error::rejected(format!("invalid review evidence request: {e}")))?;
        if batch.requests.is_empty() || batch.requests.len() > 100 {
            return Err(Error::rejected(
                "request between 1 and 100 reviewed PR heads",
            ));
        }
        let _guard = self.delivery_lock.lock().unwrap_or_else(|e| e.into_inner());
        let all = delivery::load(&self.state_dir)?;
        let pm = self.pm()?;
        let mut issues = BTreeSet::new();
        let mut prs = BTreeSet::new();
        let mut reviews = Vec::new();
        for request in batch.requests {
            if request.sha.len() != 40
                || !request
                    .sha
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(Error::rejected(
                    "review evidence needs a full lowercase 40-hex head",
                ));
            }
            let (repository, number) = crate::issue::task_report::parse_pr_url(&request.pr)?;
            let repository = repository.to_ascii_lowercase();
            if !issues.insert(request.issue.clone()) || !prs.insert((repository.clone(), number)) {
                return Err(Error::rejected(
                    "duplicate issue or PR in review evidence request",
                ));
            }
            let rec = all.get(&request.issue).ok_or_else(|| {
                Error::rejected(format!(
                    "{} is not in the native delivery loop",
                    request.issue
                ))
            })?;
            if !matches!(rec.state, State::Passed | State::Enqueued)
                || rec.head.as_deref() != Some(request.sha.as_str())
                || rec.pr.as_deref() != Some(request.pr.as_str())
            {
                return Err(Error::rejected(
                    "no standing native PASS for the requested PR and head",
                ));
            }
            if let Some(why) = delivery::project_pr_refusal(&pm.dir, &rec.project, &request.pr)? {
                return Err(Error::rejected(format!("review evidence PR {why}")));
            }
            let verdict = rec
                .verdict
                .as_ref()
                .ok_or_else(|| Error::rejected("no native verdict"))?;
            if verdict.verdict != "pass"
                || verdict.sha != request.sha
                || rec.reviewer.as_deref() != Some(verdict.reviewer.as_str())
                || verdict.reviewer == rec.worker
                || rec
                    .observed
                    .as_ref()
                    .is_some_and(|o| o.head != request.sha || o.pr_state != "OPEN")
                || !self.store.review_export_recorded(rec)?
            {
                return Err(Error::rejected(
                    "native independent PASS receipt is missing, stale or unbound",
                ));
            }
            reviews.push(json!({
                "issue": rec.issue, "project": rec.project,
                "repository": repository, "number": number, "pr": rec.pr,
                "sha": verdict.sha, "worker": rec.worker,
                "reviewer": verdict.reviewer, "report": verdict.report,
                "reviewed_at": verdict.at,
            }));
        }
        Ok(json!({
            "schema": "cadence.review-evidence/1",
            "transport_only": true,
            "checked_at": crate::issue::time::now_epoch(),
            "reviews": reviews,
        }))
    }
}
