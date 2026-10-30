//! CAD-1946: the merge gate's reads and the operator approval against a
//! hosted org, with no local daemon.
//!
//! `cadence delivery requirements` runs the daemon's own classifier
//! ([`crate::daemon::classify_from`]) in this process. Only its inputs
//! move: the ticket's project comes from the hosted `issue_show`, the
//! PROJECT.md text, recorded delivery approval and declared remotes from
//! the hosted `delivery_policy`, and the PR from the caller's `gh` exactly
//! as the daemon read it. `audit approve|approval|scope` are hosted verbs;
//! their answers are checked here against the exact request and printed in
//! the local command's shape, so `scripts/enqueue-reviewed` is unchanged.
//!
//! Every answer is checked strictly. A missing, malformed or foreign
//! answer is a refusal, never an approval, a pass or a default.

use serde_json::{json, Map, Value};
use std::time::{Duration, Instant};

use crate::daemon::{classify_from, GhRead, RequirementInputs};
use crate::error::{Error, Result};
use crate::issue::delivery_policy;
use crate::remote_cli::{RemoteAnswer, RemoteTarget};

fn refuse(why: impl std::fmt::Display) -> Error {
    Error::rejected(format!("hosted merge gate: {why}"))
}

/// One hosted verb. A checked conflict is a refusal here: none of these
/// calls has a "not written, resync" answer the caller could act on.
fn call(
    target: &RemoteTarget,
    wake_timeout: u64,
    verb: &str,
    args: Map<String, Value>,
) -> Result<Value> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(wake_timeout))
        .ok_or_else(|| refuse("invalid wake timeout"))?;
    match crate::remote_cli::call_verb(target, verb, args, deadline, wake_timeout, |line| {
        eprintln!("{line}")
    })? {
        RemoteAnswer::Ok(body) => Ok(body),
        RemoteAnswer::Conflict(body) => Err(refuse(format!(
            "`{verb}` answered a conflict: {}",
            short(&body)
        ))),
    }
}

fn short(body: &Value) -> String {
    let text = body.to_string();
    text.chars().take(300).collect()
}

fn args(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

fn text<'a>(body: &'a Value, key: &str, verb: &str) -> Result<&'a str> {
    body.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| refuse(format!("`{verb}` answer has no {key}")))
}

// ---- delivery requirements ------------------------------------------------

/// What `delivery_policy` answered for one project: the PROJECT.md text
/// (`None` = no file), the recorded `project_work_approve` delivery half
/// (`approved_from`'s payload) and the declared `repos[].remote` values.
#[derive(Debug, PartialEq)]
pub(crate) struct ProjectPolicy {
    pub project_md: Option<String>,
    pub approval: Option<Value>,
    pub remotes: Vec<String>,
}

/// Exactly `{project, project_md, approval, remotes}`, echoing `project`,
/// with at least one remote. Any other document — the hosted org's absent
/// marker, the visibility/grants answer of `project_policy`, an extra or
/// missing key — refuses: without the project's declared remotes nothing
/// proves the PR's repository belongs to it, so nothing is defaulted.
pub(crate) fn delivery_policy_answer(body: &Value, project: &str) -> Result<ProjectPolicy> {
    let fields = body
        .as_object()
        .ok_or_else(|| refuse("`delivery_policy` answer is not an object"))?;
    if fields.contains_key("absent")
        || fields.get("state").and_then(Value::as_str) == Some("absent")
    {
        return Err(refuse(format!(
            "the hosted org has no delivery policy or repo remotes for project {project}; \
             nothing proves the PR's repository belongs to it"
        )));
    }
    const KEYS: [&str; 4] = ["project", "project_md", "approval", "remotes"];
    if fields.len() != KEYS.len() || !KEYS.iter().all(|k| fields.contains_key(*k)) {
        return Err(refuse(
            "`delivery_policy` answer must be exactly {project, project_md, approval, remotes}",
        ));
    }
    if fields["project"].as_str() != Some(project) {
        return Err(refuse(format!(
            "`delivery_policy` answered for another project than {project}"
        )));
    }
    let project_md = match &fields["project_md"] {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        _ => return Err(refuse("`delivery_policy` project_md is not text or null")),
    };
    let approval = match &fields["approval"] {
        Value::Null => None,
        v @ Value::Object(_) => Some(v.clone()),
        _ => {
            return Err(refuse(
                "`delivery_policy` approval is not an object or null",
            ))
        }
    };
    let remotes = fields["remotes"]
        .as_array()
        .ok_or_else(|| refuse("`delivery_policy` remotes is not a list"))?
        .iter()
        .map(|r| {
            r.as_str()
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
                .ok_or_else(|| refuse("`delivery_policy` remotes holds a non-text entry"))
        })
        .collect::<Result<Vec<_>>>()?;
    if remotes.is_empty() {
        return Err(refuse(format!(
            "`delivery_policy` declares no repo remotes for project {project}"
        )));
    }
    Ok(ProjectPolicy {
        project_md,
        approval,
        remotes,
    })
}

/// The hosted half of [`RequirementInputs`]. Each method is a fresh read,
/// so the classifier's drift re-read compares two hosted answers.
struct HostedInputs<'a> {
    target: &'a RemoteTarget,
    wake_timeout: u64,
}

impl HostedInputs<'_> {
    fn delivery_policy(&self, project: &str) -> Result<ProjectPolicy> {
        let body = call(
            self.target,
            self.wake_timeout,
            "delivery_policy",
            args(&[("project", json!(project))]),
        )?;
        delivery_policy_answer(&body, project)
    }
}

impl GhRead for HostedInputs<'_> {
    fn gh_read(&self, args: &[&str]) -> Result<String> {
        crate::delivery::gh(crate::delivery::GH, args)
    }
}

impl RequirementInputs for HostedInputs<'_> {
    fn issue_project(&self, issue: &str) -> Result<String> {
        let body = call(
            self.target,
            self.wake_timeout,
            "issue_show",
            args(&[("id", json!(issue))]),
        )?;
        issue_project_answer(&body, issue)
    }

    fn pr_refusal(&self, project: &str, pr_url: &str) -> Result<Option<String>> {
        crate::delivery::pr_refusal_for_remotes(project, pr_url, || {
            Ok(self.delivery_policy(project)?.remotes)
        })
    }

    fn policy(
        &self,
        project: &str,
    ) -> Result<(delivery_policy::Resolved, Option<delivery_policy::Approved>)> {
        let answer = self.delivery_policy(project)?;
        Ok(resolve(project, &answer))
    }
}

/// The daemon's `policy_snapshot` over a hosted answer: the same
/// `approved_from`, `parse_project` and `effective`.
fn resolve(
    project: &str,
    answer: &ProjectPolicy,
) -> (delivery_policy::Resolved, Option<delivery_policy::Approved>) {
    let approved = answer
        .approval
        .as_ref()
        .and_then(delivery_policy::approved_from);
    let resolved = delivery_policy::effective(
        project,
        delivery_policy::parse_project(project, answer.project_md.as_deref()),
        approved.as_ref(),
    );
    (resolved, approved)
}

pub(crate) fn issue_project_answer(body: &Value, issue: &str) -> Result<String> {
    if body.get("id").and_then(Value::as_str) != Some(issue) {
        return Err(refuse(format!(
            "`issue_show` answered for another issue than {issue}"
        )));
    }
    Ok(text(body, "project", "issue_show")?.to_string())
}

/// `cadence delivery requirements` for a hosted org: the daemon's answer
/// shape, with `readiness` null — routine readiness reads the local
/// delivery record, which a hosted org does not have here.
pub fn requirements(
    target: &RemoteTarget,
    wake_timeout: u64,
    issue: &str,
    pr: &str,
    head: &str,
) -> Result<Value> {
    let inputs = HostedInputs {
        target,
        wake_timeout,
    };
    let (mut answer, _) = classify_from(&inputs, &json!({"issue": issue, "pr": pr, "head": head}))?;
    answer["readiness"] = Value::Null;
    Ok(answer)
}

// ---- audit approval / scope / approve ---------------------------------------

/// `cadence audit approval` for a hosted org: `(document, exit code)` in
/// the local command's shape (see [`approval_answer`]).
pub fn approval(
    target: &RemoteTarget,
    wake_timeout: u64,
    repo: &str,
    pr: u64,
    head: &str,
) -> Result<(Value, i32)> {
    let body = call(
        target,
        wake_timeout,
        "audit_approval",
        args(&[
            ("repo", json!(repo)),
            ("pr", json!(pr)),
            ("head", json!(head)),
        ]),
    )?;
    approval_answer(&body, repo, pr, head)
}

/// The contract answer is `{state:"in-force", approval_id, source,
/// recorded_via}`, or another state with a text `reason` and no
/// `approval_id`. The verb binds the exact `{repo, pr, head}` it was asked;
/// an answer that also names them must name exactly those, and any key
/// outside the contract refuses.
pub(crate) fn approval_answer(
    body: &Value,
    repo: &str,
    pr: u64,
    head: &str,
) -> Result<(Value, i32)> {
    let verb = "audit_approval";
    let fields = body
        .as_object()
        .ok_or_else(|| refuse(format!("`{verb}` answer is not an object")))?;
    const KEYS: [&str; 9] = [
        "state",
        "approval_id",
        "source",
        "recorded_via",
        "reason",
        "repo",
        "pr",
        "head",
        "action",
    ];
    if let Some(key) = fields.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(refuse(format!(
            "`{verb}` answer carries unexpected {key:?}"
        )));
    }
    if fields
        .get("repo")
        .is_some_and(|r| !r.as_str().is_some_and(|r| r.eq_ignore_ascii_case(repo)))
        || fields.get("pr").is_some_and(|p| p.as_u64() != Some(pr))
        || fields.get("head").is_some_and(|h| h.as_str() != Some(head))
    {
        return Err(refuse(format!(
            "`{verb}` answer names another approval than {repo}#{pr} at {head}"
        )));
    }
    if fields
        .get("action")
        .is_some_and(|a| a.as_str() != Some("merge"))
    {
        return Err(refuse(format!("`{verb}` answer is not a merge approval")));
    }
    let state = text(body, "state", verb)?;
    if state == "in-force" {
        if fields.contains_key("reason") {
            return Err(refuse(format!("`{verb}` in-force answer carries a reason")));
        }
        let id = text(body, "approval_id", verb)?;
        let source = text(body, "source", verb)?;
        let via = text(body, "recorded_via", verb)?;
        return Ok((
            json!({"state": "in-force", "approval_id": id, "source": source,
                   "action": "merge", "recorded_via": via, "verified": false,
                   "note": crate::audit::APPROVAL_UNVERIFIED}),
            0,
        ));
    }
    if fields.contains_key("approval_id") {
        return Err(refuse(format!(
            "`{verb}` answer is {state:?} but names an approval"
        )));
    }
    let reason = text(body, "reason", verb)?;
    Ok((json!({"state": state, "reason": reason}), 1))
}

/// `cadence audit scope` for a hosted org: `(document, exit code)` in the
/// local command's shape. Exit 0 only for an `ok` answer about this issue.
pub fn scope(target: &RemoteTarget, wake_timeout: u64, issue: &str) -> Result<(Value, i32)> {
    let body = call(
        target,
        wake_timeout,
        "audit_scope",
        args(&[("issue", json!(issue))]),
    )?;
    scope_answer(&body, issue)
}

pub(crate) fn scope_answer(body: &Value, issue: &str) -> Result<(Value, i32)> {
    let verb = "audit_scope";
    let state = text(body, "state", verb)?;
    if state != "ok" {
        let reason = body["reason"].as_str().unwrap_or(state);
        return Ok((json!({"state": state, "reason": reason}), 1));
    }
    if !body["issue"]
        .as_str()
        .is_some_and(|i| i.eq_ignore_ascii_case(issue))
    {
        return Err(refuse(format!(
            "`{verb}` answered for another issue than {issue}"
        )));
    }
    let rows = body["approvals"]
        .as_array()
        .ok_or_else(|| refuse(format!("`{verb}` answer has no approvals list")))?
        .iter()
        .map(|row| {
            let revoked = row["revoked"]
                .as_bool()
                .ok_or_else(|| refuse(format!("`{verb}` row has no revoked flag")))?;
            Ok(json!({
                "approval_id": text(row, "approval_id", verb)?,
                "digest": text(row, "digest", verb)?,
                "source": text(row, "source", verb)?,
                "recorded_via": text(row, "recorded_via", verb)?,
                "revoked": revoked,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((json!({"state": "ok", "issue": issue, "approvals": rows}), 0))
}

/// `cadence audit approve` (merge, by the operator) for a hosted org. The
/// hosted org decides who may approve; the receipt must name exactly the
/// repo, PR and head asked for, or the command refuses.
pub fn approve(
    target: &RemoteTarget,
    wake_timeout: u64,
    repo: &str,
    pr: u64,
    head: &str,
    source: &str,
) -> Result<Value> {
    let body = call(
        target,
        wake_timeout,
        "audit_approve",
        args(&[
            ("repo", json!(repo)),
            ("pr", json!(pr)),
            ("head", json!(head)),
            ("source", json!(source)),
        ]),
    )?;
    approve_answer(&body, repo, pr, head, source)
}

pub(crate) fn approve_answer(
    body: &Value,
    repo: &str,
    pr: u64,
    head: &str,
    source: &str,
) -> Result<Value> {
    let verb = "audit_approve";
    if body["state"].as_str() != Some("recorded") || body["action"].as_str() != Some("merge") {
        return Err(refuse(format!(
            "`{verb}` did not record a merge approval: {}",
            short(body)
        )));
    }
    if body["head_sha"].as_str() != Some(head)
        || body["scope"]["pr"].as_u64() != Some(pr)
        || !body["scope"]["repo"]
            .as_str()
            .is_some_and(|r| r.eq_ignore_ascii_case(repo))
    {
        return Err(refuse(format!(
            "`{verb}` receipt does not name exactly {repo}#{pr} at {head}"
        )));
    }
    if body["source"].as_str() != Some(source) {
        return Err(refuse(format!("`{verb}` receipt names another source")));
    }
    text(body, "approval_id", verb)?;
    text(body, "recorded_via", verb)?;
    if !body["duplicate"].is_boolean() {
        return Err(refuse(format!("`{verb}` receipt has no duplicate flag")));
    }
    Ok(body.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER: &str = "fedcba9876543210fedcba9876543210fedcba98";

    fn in_force() -> Value {
        json!({"state": "in-force", "approval_id": "merge-pr7-0123456789ab",
               "source": "chris on the board", "recorded_via": "operator-connection"})
    }

    #[test]
    fn approval_in_force_prints_the_local_shape() {
        let (doc, code) = approval_answer(&in_force(), "favcrm/cadence", 7, HEAD).unwrap();
        assert_eq!(code, 0);
        assert_eq!(doc["state"], "in-force");
        assert_eq!(doc["action"], "merge");
        assert_eq!(doc["recorded_via"], "operator-connection");
        assert_eq!(doc["verified"], false);
    }

    #[test]
    fn approval_naming_another_repo_pr_or_head_refuses() {
        let mut echoed = in_force();
        echoed["repo"] = json!("favcrm/cadence");
        echoed["pr"] = json!(7);
        echoed["head"] = json!(HEAD);
        assert_eq!(
            approval_answer(&echoed, "favcrm/cadence", 7, HEAD)
                .unwrap()
                .1,
            0
        );
        for (repo, pr, head) in [
            ("favcrm/other", 7, HEAD),
            ("favcrm/cadence", 8, HEAD),
            ("favcrm/cadence", 7, OTHER),
        ] {
            let err = approval_answer(&echoed, repo, pr, head).unwrap_err();
            assert!(err.to_string().contains("another approval"), "{err}");
        }
    }

    #[test]
    fn approval_refuses_a_malformed_answer() {
        let mut extra = in_force();
        extra["verified"] = json!(true);
        let mut not_merge = in_force();
        not_merge["action"] = json!("deploy");
        for body in [
            extra,
            not_merge,
            json!({"state": "in-force"}),
            json!({"state": "granted", "approval_id": "x"}),
            json!({"state": "missing", "reason": "x", "approval_id": "a1"}),
            json!(["in-force"]),
            json!({}),
            json!({"state": "missing"}),
        ] {
            assert!(
                approval_answer(&body, "favcrm/cadence", 7, HEAD).is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn approval_not_in_force_exits_one_with_its_reason() {
        let body = json!({"state": "missing", "reason": "head moved"});
        let (doc, code) = approval_answer(&body, "favcrm/cadence", 7, HEAD).unwrap();
        assert_eq!((doc["state"].as_str(), code), (Some("missing"), 1));
        assert_eq!(doc["reason"], "head moved");
    }

    #[test]
    fn scope_answer_binds_the_issue_and_checks_every_row() {
        let ok = json!({"state": "ok", "issue": "CAD-1", "approvals": []});
        assert_eq!(scope_answer(&ok, "CAD-1").unwrap().1, 0);
        assert!(scope_answer(&ok, "CAD-2").is_err());
        let bad_row = json!({"state": "ok", "issue": "CAD-1",
                             "approvals": [{"approval_id": "s1", "digest": "d"}]});
        assert!(scope_answer(&bad_row, "CAD-1").is_err());
        let unknown = json!({"state": "unknown", "reason": "store unreadable"});
        assert_eq!(scope_answer(&unknown, "CAD-1").unwrap().1, 1);
        assert!(scope_answer(&json!({}), "CAD-1").is_err());
    }

    #[test]
    fn approve_receipt_must_name_the_exact_head_pr_and_repo() {
        let receipt = json!({"action": "merge", "approval_id": "merge-pr7-0123456789ab",
                             "duplicate": false, "head_sha": HEAD,
                             "recorded_via": "operator-connection",
                             "scope": {"pr": 7, "repo": "favcrm/cadence"},
                             "source": "chris", "state": "recorded"});
        assert!(approve_answer(&receipt, "favcrm/cadence", 7, HEAD, "chris").is_ok());
        // A moved head, another PR, another repo, another source.
        assert!(approve_answer(&receipt, "favcrm/cadence", 7, OTHER, "chris").is_err());
        assert!(approve_answer(&receipt, "favcrm/cadence", 8, HEAD, "chris").is_err());
        assert!(approve_answer(&receipt, "favcrm/pm", 7, HEAD, "chris").is_err());
        assert!(approve_answer(&receipt, "favcrm/cadence", 7, HEAD, "someone").is_err());
        let mut refused = receipt.clone();
        refused["state"] = json!("refused");
        assert!(approve_answer(&refused, "favcrm/cadence", 7, HEAD, "chris").is_err());
    }

    #[test]
    fn delivery_policy_answer_is_exact_and_refuses_the_visibility_shape() {
        let ok = json!({"project": "cadence", "project_md": null, "approval": null,
                        "remotes": ["github.com/favcrm/cadence"]});
        for absent in [
            json!({"project": "cadence", "absent": true}),
            json!({"project": "cadence", "state": "absent"}),
        ] {
            let err = delivery_policy_answer(&absent, "cadence").unwrap_err();
            assert!(err.to_string().contains("nothing proves"), "{err}");
        }
        let mut no_remotes = ok.clone();
        no_remotes["remotes"] = json!([]);
        assert!(delivery_policy_answer(&no_remotes, "cadence").is_err());
        let read = delivery_policy_answer(&ok, "cadence").unwrap();
        assert_eq!(read.remotes, vec!["github.com/favcrm/cadence".to_string()]);
        assert!(delivery_policy_answer(&ok, "pm").is_err());
        let visibility = json!({"project": "cadence", "visibility": "private", "grants": []});
        assert!(delivery_policy_answer(&visibility, "cadence").is_err());
        let mut extra = ok.clone();
        extra["policy"] = json!("routine");
        assert!(delivery_policy_answer(&extra, "cadence").is_err());
        let mut bad_remote = ok.clone();
        bad_remote["remotes"] = json!([7]);
        assert!(delivery_policy_answer(&bad_remote, "cadence").is_err());
    }

    #[test]
    fn hosted_policy_resolves_through_the_daemon_functions() {
        // No PROJECT.md, no approval: the daemon's defaults (not an active
        // profile); an inconsistent approval reads as none, as locally.
        let none = ProjectPolicy {
            project_md: None,
            approval: None,
            remotes: vec![],
        };
        let local = delivery_policy::effective("cadence", Ok(None), None);
        assert_eq!(resolve("cadence", &none).0, local);
        let forged = ProjectPolicy {
            approval: Some(json!({"delivery_digest": "sha256:00", "delivery": null})),
            ..none
        };
        assert_eq!(resolve("cadence", &forged), (local, None));
    }

    /// The classifier refuses a PR outside the ticket project's declared
    /// remotes before any GitHub read.
    struct Fixture(Vec<String>);
    impl GhRead for Fixture {
        fn gh_read(&self, args: &[&str]) -> Result<String> {
            panic!("no GitHub read expected, got gh {args:?}")
        }
    }
    impl RequirementInputs for Fixture {
        fn issue_project(&self, _issue: &str) -> Result<String> {
            Ok("cadence".into())
        }
        fn pr_refusal(&self, project: &str, pr_url: &str) -> Result<Option<String>> {
            crate::delivery::pr_refusal_for_remotes(project, pr_url, || Ok(self.0.clone()))
        }
        fn policy(
            &self,
            _project: &str,
        ) -> Result<(delivery_policy::Resolved, Option<delivery_policy::Approved>)> {
            panic!("no policy read expected")
        }
    }

    #[test]
    fn a_foreign_repo_is_refused_before_github_is_read() {
        let inputs = Fixture(vec!["github.com/favcrm/cadence".into()]);
        let err = classify_from(
            &inputs,
            &json!({"issue": "CAD-1", "pr": "https://github.com/evil/cadence/pull/7", "head": HEAD}),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("not a repo of project cadence"),
            "{err}"
        );
    }
}
