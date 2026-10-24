//! Daemon store (read-only).

use super::*;

#[derive(Default)]
pub(super) struct StoreEvidence {
    /// `(sha, verdict, reviewer)` rows from the `verdicts` table.
    pub(super) verdicts: Vec<(String, String, String)>,
    /// `(kind, at)` events.
    pub(super) events: Vec<(String, f64)>,
    pub(super) opened: bool,
    /// `approval_recorded` events on the approval stream, oldest first.
    pub(super) approvals: Vec<ApprovalRec>,
    /// CAD-1106: ticket scope pre-approvals (`action` scope), oldest first.
    pub(super) scopes: Vec<ScopeRec>,
    /// The first `approval_revoked` event per approval id.
    pub(super) revocations: HashMap<String, Revocation>,
    /// Why the approval stream could not be read — approval state is
    /// then `unknown`, never `missing`. `None` when it answered.
    pub(super) approvals_gap: Option<String>,
    pub(super) workflow_intents: Vec<(Value, f64)>,
    pub(super) workflow_enqueues: Vec<(Value, f64)>,
    pub(super) native_reviews: Vec<(Value, f64)>,
    pub(super) workflow_gap: Option<String>,
}

/// One operator approval record (CAD-217) — `store::NewApproval` as
/// persisted, plus the event time.
#[derive(Debug, Clone, Default)]
pub(super) struct ApprovalRec {
    pub(super) id: String,
    pub(super) source: String,
    pub(super) action: String,
    pub(super) head_sha: String,
    pub(super) repo: String,
    pub(super) pr: u64,
    pub(super) recorded_via: Option<String>,
    /// CAD-918: `delegated:<alias>`, its two verdict notes, their
    /// sha256 at approval time, and the reviewer aliases they name.
    pub(super) approver: Option<String>,
    pub(super) verdicts: Vec<String>,
    pub(super) verdict_sha256: Vec<String>,
    pub(super) reviewers: Vec<String>,
    pub(super) at: f64,
}

/// One ticket scope pre-approval record (CAD-918): the ticket and the
/// digest of its body at approval time.
#[derive(Debug, Clone, Default)]
pub(super) struct ScopeRec {
    pub(super) id: String,
    pub(super) issue: String,
    pub(super) digest: String,
    pub(super) source: String,
    pub(super) recorded_via: Option<String>,
}

/// One operator revocation of an approval id.
#[derive(Debug, Clone, Default)]
pub(super) struct Revocation {
    pub(super) source: String,
    pub(super) reason: String,
    pub(super) at: f64,
}

/// A row's approval binding: `operator-claimed` (a record for the exact
/// landed head, recorded before the merge and not revoked before it —
/// a claim, not proof, until CAD-280: see [`APPROVAL_UNVERIFIED`]),
/// `revoked` (every such record was revoked before the merge),
/// `missing` (the store answered and no record was in force at merge
/// time), `unknown` (the evidence could not be read), or `not-required`
/// (a non-human class with no record).
#[derive(Debug, Default)]
pub(super) struct ApprovalView {
    pub(super) state: String,
    pub(super) reason: Option<String>,
    /// The record the state rests on (for `missing`: a post-merge one).
    pub(super) record: Option<ApprovalRec>,
    pub(super) revocation: Option<Revocation>,
    /// Records for this PR that name a different head — shown, never
    /// counted: an approval does not carry over to a later head.
    pub(super) other_heads: Vec<ApprovalRec>,
}

/// Why no approval record is authoritative yet (CAD-217 review): the
/// operator gate inherits CAD-276's residual — a same-uid process that
/// detaches from every pane (`setsid -f`, scrubbed env and stdio) passes
/// it — and any same-uid process can write rows into the store file
/// directly. Until operator-by-positive-proof lands, a bound approval is
/// reported as `operator-claimed`, never as a verified approval.
pub(super) const APPROVAL_UNVERIFIED: &str = "unverified until CAD-280";

pub(super) fn store_evidence(path: &Path, workflow: bool) -> StoreEvidence {
    let mut ev = StoreEvidence::default();
    if !path.exists() {
        // A host that never ran the daemon has no store — the table
        // is empty, not unreadable. No evidence gap for verdicts; but
        // approval records live nowhere else, so this host cannot say
        // whether one exists.
        ev.approvals_gap = Some(format!(
            "no daemon store at {} — approval records live there",
            path.display()
        ));
        return ev;
    }
    let Ok(conn) = crate::store::open_read_only(path) else {
        ev.approvals_gap = Some(format!("store {} unreadable", path.display()));
        return ev;
    };
    ev.opened = true;
    // Bounded reads — an audit must not pull a whole event history
    // into memory; only recent verdicts and restart-kind events are
    // evidence anyway.
    if let Ok(mut st) =
        conn.prepare("SELECT sha, verdict, reviewer FROM verdicts ORDER BY seq DESC LIMIT 5000")
    {
        ev.verdicts = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
    }
    if let Ok(mut st) = conn.prepare(
        "SELECT kind, at FROM events WHERE kind LIKE '%restart%' ORDER BY seq DESC LIMIT 5000",
    ) {
        ev.events = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
    }
    read_approvals(&conn, &mut ev, workflow);
    if workflow {
        let native: rusqlite::Result<Vec<(String, f64)>> = conn.prepare(
        "SELECT payload,at FROM events WHERE alias=?1 AND kind=?2 ORDER BY seq DESC LIMIT 5000",
    ).and_then(|mut statement| {
        statement.query_map(
            [crate::store::VERDICT_STREAM, crate::store::VERDICT_RECORDED_EVENT],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?.collect()
    });
        match native {
            Ok(rows) => {
                for (raw, at) in rows {
                    match serde_json::from_str(&raw) {
                        Ok(value) => ev.native_reviews.push((value, at)),
                        Err(_) => {
                            ev.workflow_gap =
                                Some("native review evidence is malformed".to_string())
                        }
                    }
                }
            }
            Err(error) => {
                ev.workflow_gap = Some(format!("native review evidence is unreadable: {error}"))
            }
        }
    }
    ev
}

/// The approval stream, read-only. Malformed payloads are skipped —
/// only the store's validated writer puts rows on this stream, so a
/// row that does not parse proves nothing either way.
pub(super) fn read_approvals(conn: &rusqlite::Connection, ev: &mut StoreEvidence, workflow: bool) {
    use crate::store::{APPROVAL_RECORDED_EVENT, APPROVAL_REVOKED_EVENT, APPROVAL_STREAM};
    let rows: rusqlite::Result<Vec<(String, String, f64)>> = conn
        .prepare(
            "SELECT kind,payload,at FROM events WHERE alias=?1 \
             AND (kind IN (?2,?3) OR (?4 AND kind IN (?5,?6))) ORDER BY seq DESC LIMIT 100000",
        )
        .and_then(|mut st| {
            st.query_map(
                rusqlite::params![
                    APPROVAL_STREAM,
                    APPROVAL_RECORDED_EVENT,
                    APPROVAL_REVOKED_EVENT,
                    workflow,
                    crate::store::UNATTENDED_MERGE_INTENT_EVENT,
                    crate::store::UNATTENDED_MERGE_ENQUEUED_EVENT,
                ],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?
            .collect()
        });
    // Newest-first so the row cap can only drop the oldest history, never
    // a recent revocation; replayed oldest-first below.
    let rows = match rows {
        Ok(mut rows) => {
            rows.reverse();
            rows
        }
        Err(e) => {
            ev.approvals_gap = Some(format!("approval stream unreadable: {e}"));
            return;
        }
    };
    for (kind, raw, at) in rows {
        let Ok(p) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if kind == crate::store::UNATTENDED_MERGE_INTENT_EVENT {
            ev.workflow_intents.push((p, at));
            continue;
        }
        if kind == crate::store::UNATTENDED_MERGE_ENQUEUED_EVENT {
            ev.workflow_enqueues.push((p, at));
            continue;
        }
        let text = |k: &str| p[k].as_str().map(str::to_string);
        let Some(id) = text("approval_id") else {
            continue;
        };
        if kind == APPROVAL_RECORDED_EVENT && text("action").as_deref() == Some("scope") {
            if let (Some(source), Some(issue), Some(digest)) = (
                text("source"),
                p["scope"]["issue"].as_str().map(str::to_string),
                p["scope"]["digest"].as_str().map(str::to_string),
            ) {
                ev.scopes.push(ScopeRec {
                    id,
                    issue,
                    digest,
                    source,
                    recorded_via: text("recorded_via"),
                });
            }
        } else if kind == APPROVAL_RECORDED_EVENT {
            let (Some(source), Some(action), Some(head_sha), Some(repo), Some(pr)) = (
                text("source"),
                text("action"),
                text("head_sha"),
                p["scope"]["repo"].as_str().map(str::to_string),
                p["scope"]["pr"].as_u64(),
            ) else {
                continue;
            };
            ev.approvals.push(ApprovalRec {
                id,
                source,
                action,
                head_sha,
                repo,
                pr,
                recorded_via: text("recorded_via"),
                approver: text("approver"),
                verdicts: serde_json::from_value(p["verdicts"].clone()).unwrap_or_default(),
                verdict_sha256: serde_json::from_value(p["verdict_sha256"].clone())
                    .unwrap_or_default(),
                reviewers: serde_json::from_value(p["reviewers"].clone()).unwrap_or_default(),
                at,
            });
        } else if kind == APPROVAL_REVOKED_EVENT {
            let (Some(source), Some(reason)) = (text("source"), text("reason")) else {
                continue;
            };
            ev.revocations
                .entry(id)
                .or_insert(Revocation { source, reason, at });
        }
    }
}

/// Reconstruct native scope consumption without inventing an operator
/// per-head approval or a GitHub status. The same local-store trust limit
/// as the existing verdict table and approval records remains explicit.
pub(super) fn enrich_workflow_authorizations(
    ev: &StoreEvidence,
    slug: Option<&str>,
    rows: &mut [Row],
) {
    let intents: HashMap<_, _> = ev
        .workflow_intents
        .iter()
        .filter_map(|(value, at)| {
            Some((
                (value["issue"].as_str()?, value["head"].as_str()?),
                (value, *at),
            ))
        })
        .collect();
    let enqueued: HashMap<_, _> = ev
        .workflow_enqueues
        .iter()
        .filter_map(|(value, at)| {
            Some((
                (
                    value["issue"].as_str()?,
                    value["head"].as_str()?,
                    value["grant_id"].as_str()?,
                ),
                *at,
            ))
        })
        .collect();
    let mut reviews: HashMap<&str, Vec<(&Value, f64)>> = HashMap::new();
    for (value, at) in &ev.native_reviews {
        if let Some(issue) = value["issue"].as_str() {
            reviews.entry(issue).or_default().push((value, *at));
        }
    }
    for row in rows {
        let (Some(head), Some(number), Some(merged_at)) =
            (row.landed_head.as_deref(), row.pr, row.merged_at)
        else {
            continue;
        };
        let Some((intent, at)) = row
            .issues
            .iter()
            .find_map(|issue| intents.get(&(issue.as_str(), head)).copied())
        else {
            continue;
        };
        let issue = intent["issue"].as_str().unwrap_or_default();
        let grant = intent["grant_id"].as_str().unwrap_or_default();
        let enqueue_at = enqueued.get(&(issue, head, grant)).copied();
        let mut evidence = json!({
            "state": if enqueue_at.is_some() { "enqueue-recorded" } else { "intent-recorded" },
            "grant_id": grant, "issue": issue, "head": head,
            "project": intent["project"], "repo": intent["repo"], "pr": intent["pr"],
            "worker": intent["worker"], "reviewer": intent["reviewer"], "report": intent["report"],
            "policy_digest": intent["policy_digest"], "classification": intent["class"],
            "authorized_at": at, "grant_expires_at": intent["grant_expires_at"],
            "enqueue_recorded_at": enqueue_at, "native_review_at_merge": false,
            "per_head_operator_approval": false, "verified": false,
            "note": "native workflow evidence, not a per-head operator approval or a GitHub status",
        });
        let consistent = slug.is_some_and(|slug| {
            intent["repo"]
                .as_str()
                .is_some_and(|repo| repo.eq_ignore_ascii_case(slug))
        }) && intent["pr"].as_u64() == Some(number)
            && !issue.is_empty()
            && grant
                .strip_prefix("grant-")
                .is_some_and(|id| id.len() == 32 && id.bytes().all(|c| c.is_ascii_hexdigit()))
            && intent["grant_expires_at"]
                .as_f64()
                .is_some_and(|expiry| expiry.is_finite() && expiry > at)
            && matches!(intent["class"].as_str(), Some("routine" | "consequential"))
            && intent["risk"] == "auto"
            && row
                .project
                .as_deref()
                .is_none_or(|project| intent["project"] == project);
        if !consistent {
            evidence["state"] = json!("unknown");
            evidence["reason"] =
                json!("native workflow evidence does not bind this repository, head and scope");
            row.workflow_authorization = Some(evidence);
            continue;
        }
        if let Some(gap) = &ev.workflow_gap {
            evidence["state"] = json!("unknown");
            evidence["reason"] = json!(gap);
            row.evidence_gaps.push(gap.clone());
            row.workflow_authorization = Some(evidence);
            continue;
        }
        let scope_matches = |review: &Value| {
            review["issue"] == intent["issue"]
                && review["sha"].as_str() == Some(head)
                && review["verdict"] == "pass"
                && review["risk"] == "auto"
                && review["reviewer"] == intent["reviewer"]
                && review["report"] == intent["report"]
                && review["project"] == intent["project"]
                && review["worker"] == intent["worker"]
                && review["reviewer"].as_str().is_some_and(|reviewer| {
                    !reviewer.is_empty()
                        && review["worker"]
                            .as_str()
                            .is_some_and(|worker| !worker.is_empty() && worker != reviewer)
                })
                && review["pr"]
                    .as_str()
                    .and_then(|pr| crate::issue::task_report::parse_pr_url(pr).ok())
                    .is_some_and(|(repo, pr)| {
                        pr == number && slug.is_some_and(|slug| repo.eq_ignore_ascii_case(slug))
                    })
        };
        let history = reviews.get(issue);
        let latest = history.and_then(|history| {
            history
                .iter()
                .find(|(_, review_at)| *review_at <= merged_at)
        });
        let independent = latest
            .is_some_and(|(review, review_at)| *review_at <= at && scope_matches(review))
            && !matches!(row.class.as_deref(), Some("human" | "delegated"));
        evidence["native_review_at_merge"] = json!(independent);
        evidence["authorization_before_merge"] = if at <= merged_at {
            json!(true)
        } else if at.floor() == merged_at.floor() {
            Value::Null
        } else {
            json!(false)
        };
        if independent {
            row.native_workflow_review = true;
            if row.class.is_none() {
                row.class = Some("auto".to_string());
            }
        } else if history.is_some_and(|history| {
            history.iter().any(|(review, review_at)| {
                review_at.floor() == merged_at.floor()
                    && *review_at > merged_at
                    && scope_matches(review)
            })
        }) {
            let reason = "native review and merge share a timestamp second; their order is unknown";
            row.evidence_gaps.push(reason.to_string());
            evidence["native_review_at_merge"] = Value::Null;
            evidence["reason"] = json!(reason);
        }
        row.workflow_authorization = Some(evidence);
    }
}

pub(super) fn enrich_store(ev: &StoreEvidence, path: &Path, row: &mut Row) {
    if path.exists() && !ev.opened {
        let reason = format!("store {} unreadable", path.display());
        row.evidence_gaps.push(reason.clone());
        row.unknowns.push(("verdict".into(), reason));
    }
    for (sha, verdict, reviewer) in &ev.verdicts {
        let hit = row
            .landed_head
            .as_deref()
            .or(row.reviewed_head.as_deref())
            .is_some_and(|h| h.len() >= 7 && (h.starts_with(sha) || sha.starts_with(h)));
        if hit {
            row.verdict = row.verdict.clone().or(Some(verdict.clone()));
            row.reviewer = row.reviewer.clone().or(Some(reviewer.clone()));
        }
    }
}
