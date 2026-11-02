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
//! the local command's shape, so `scripts/enqueue-reviewed` reads them as
//! before. `issue project delivery` records the project's repos and
//! `delivery:` section through `delivery_policy_set`.
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

/// The same check for one row of an answer's list, so a refusal names the row
/// field rather than the whole answer.
fn row_text<'a>(row: &'a Value, key: &str, verb: &str) -> Result<&'a str> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| refuse(format!("`{verb}` row has no {key}")))
}

// ---- delivery requirements ------------------------------------------------

/// What `delivery_policy` answered for one project: the stored
/// `delivery:` section text (`None` = absent, the native "no section"),
/// the approval of it in `approved_from`'s payload shape (or `None`) and
/// the declared repos as `github.com/<owner>/<name>` remotes.
#[derive(Debug, PartialEq)]
pub(crate) struct ProjectPolicy {
    pub delivery: Option<String>,
    pub approval: Option<Value>,
    pub remotes: Vec<String>,
}

/// A declared repo: `owner/name`, each part 1-100 of `[A-Za-z0-9._-]`.
pub fn valid_repo(repo: &str) -> bool {
    let part = |p: &str| {
        (1..=100).contains(&p.len())
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    repo.split_once('/')
        .is_some_and(|(owner, name)| part(owner) && part(name))
}

/// A PROJECT.md text whose frontmatter carries exactly the stored
/// `delivery:` section, so the native [`delivery_policy::parse`] reads it.
/// The text is the section's YAML value, or the section with its
/// `delivery:` key.
pub fn delivery_front(section: &str) -> String {
    let section = section.trim_end();
    if section.trim_start().starts_with("delivery:") {
        format!("---\n{section}\n---\n")
    } else {
        let body: String = section.lines().map(|l| format!("  {l}\n")).collect();
        format!("---\ndelivery:\n{body}---\n")
    }
}

/// The tenant's `{state: …}` marker, if `value` is one.
fn marker_state(value: Option<&Value>) -> Option<&str> {
    value?.get("state")?.as_str()
}

/// `{project, revision, repos, delivery, delivery_approval}`, echoing
/// `project`:
/// - `repos` `{state:"set", repos:[owner/name…]}` (non-empty); `not_stored`
///   refuses, because nothing then proves the PR's repository belongs to
///   the project.
/// - `delivery` `{state:"set", text}` is the section; `{state:"absent"}` is
///   the native "no section" (the default lean policy); anything else
///   refuses.
/// - `delivery_approval` `{state:"approved", digest, recorded_via:
///   "hosted-owner-login"}` is offered to the native `approved_from` as the
///   approval of the parsed section, so it counts only when `digest` is the
///   native policy digest; any other state is no approval (strict, as an
///   unapproved custom section is natively).
///
/// Any other key or shape refuses, so no policy input is ever ignored.
pub(crate) fn delivery_policy_answer(body: &Value, project: &str) -> Result<ProjectPolicy> {
    let fields = body
        .as_object()
        .ok_or_else(|| refuse("`delivery_policy` answer is not an object"))?;
    const KEYS: [&str; 5] = [
        "project",
        "revision",
        "repos",
        "delivery",
        "delivery_approval",
    ];
    if fields.len() != KEYS.len() || !KEYS.iter().all(|k| fields.contains_key(*k)) {
        return Err(refuse(
            "`delivery_policy` answer must be exactly {project, revision, repos, delivery, delivery_approval}",
        ));
    }
    if fields["project"].as_str() != Some(project) {
        return Err(refuse(format!(
            "`delivery_policy` answered for another project than {project}"
        )));
    }
    if !(fields["revision"].is_null() || fields["revision"].is_u64()) {
        return Err(refuse("`delivery_policy` revision is malformed"));
    }
    let repos = match marker_state(fields.get("repos")) {
        Some("set") => fields["repos"]["repos"]
            .as_array()
            .ok_or_else(|| refuse("`delivery_policy` repos has no list"))?,
        Some("not_stored") => {
            return Err(refuse(format!(
                "the hosted org stores no repos for project {project}; nothing proves the \
                 PR's repository belongs to it — the owner records them with \
                 `cadence issue project delivery {project} --repo owner/name`"
            )))
        }
        _ => return Err(refuse("`delivery_policy` repos is malformed")),
    };
    let remotes = repos
        .iter()
        .map(|r| {
            r.as_str()
                .filter(|r| valid_repo(r))
                .map(|r| format!("github.com/{r}"))
                .ok_or_else(|| {
                    refuse("`delivery_policy` repos holds an entry that is not owner/name")
                })
        })
        .collect::<Result<Vec<_>>>()?;
    if remotes.is_empty() {
        return Err(refuse(format!(
            "`delivery_policy` declares no repos for project {project}"
        )));
    }
    let delivery = match marker_state(fields.get("delivery")) {
        Some("set") => Some(
            fields["delivery"]["text"]
                .as_str()
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| refuse("`delivery_policy` delivery has no text"))?
                .to_string(),
        ),
        Some("absent") => None,
        _ => {
            return Err(refuse(format!(
                "`delivery_policy` delivery for project {project} is not stored or malformed"
            )))
        }
    };
    let approval = match marker_state(fields.get("delivery_approval")) {
        Some("approved") => {
            let record = &fields["delivery_approval"];
            if record["recorded_via"].as_str() != Some("hosted-owner-login") {
                return Err(refuse(
                    "`delivery_policy` approval was not recorded by the hosted owner",
                ));
            }
            let digest = record["digest"]
                .as_str()
                .ok_or_else(|| refuse("`delivery_policy` approval has no digest"))?;
            // The native approval payload: the recorded digest plus the
            // policy it approves. `approved_from` keeps it only when the
            // digest is the native digest of exactly that policy.
            let policy = delivery
                .as_deref()
                .map(|text| delivery_policy::parse(&delivery_front(text)))
                .transpose()
                .ok()
                .flatten()
                .flatten();
            policy.map(|p| json!({"delivery_digest": digest, "delivery": p}))
        }
        Some(_) => None,
        None => return Err(refuse("`delivery_policy` delivery_approval is malformed")),
    };
    Ok(ProjectPolicy {
        delivery,
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
        delivery_policy::parse_project(
            project,
            answer.delivery.as_deref().map(delivery_front).as_deref(),
        )
        .map_err(|e| e.to_string()),
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

/// A PR reference as the classifier expects it: a URL when the project's one
/// remote can name it, unchanged when the caller already passed one.
///
/// `--pr 1462` is the documented form, and the classifier parses a URL later
/// (to read the PR through `gh`), so the number has to become a URL here —
/// before anything downstream sees it. Several remotes cannot name it: the
/// caller is asked for the URL rather than a repo being guessed.
pub fn pr_url_from(remotes: &[String], pr: &str) -> Result<String> {
    let bare = pr.trim().trim_start_matches('#');
    if bare.is_empty() || !bare.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(pr.to_string());
    }
    match remotes {
        [only] => {
            let slug = only
                .trim()
                .trim_start_matches("github.com/")
                .trim_end_matches('/');
            Ok(format!("https://github.com/{slug}/pull/{bare}"))
        }
        [] => Err(refuse(format!(
            "`pr: {pr}` names no repo, and the project lists no remote to match it against"
        ))),
        _ => Err(refuse(format!(
            "`pr: {pr}` names no repo, and the project has several remotes — pass the full pull request URL"
        ))),
    }
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
    // The classifier parses the PR as a URL, so a bare number is resolved
    // against the project's remote first — never guessed past that.
    let pr = if pr
        .trim()
        .trim_start_matches('#')
        .bytes()
        .all(|b| b.is_ascii_digit())
    {
        let project = inputs.issue_project(issue)?;
        pr_url_from(&inputs.delivery_policy(&project)?.remotes, pr)?
    } else {
        pr.to_string()
    };
    let (mut answer, _) = classify_from(&inputs, &json!({"issue": issue, "pr": pr, "head": head}))?;
    answer["readiness"] = Value::Null;
    Ok(answer)
}

/// `cadence issue project delivery` for a hosted org: record the project's
/// repos and optional `delivery:` section through `delivery_policy_set`.
/// Each repo must be `owner/name` and the section must parse with the
/// native parser before anything is sent; the receipt must echo exactly
/// the project and repos recorded.
pub fn set_delivery(
    target: &RemoteTarget,
    wake_timeout: u64,
    project: &str,
    repos: &[String],
    delivery: Option<&str>,
) -> Result<Value> {
    let repos = set_delivery_repos(repos)?;
    if let Some(section) = delivery {
        if delivery_policy::parse(&delivery_front(section))?.is_none() {
            return Err(refuse("the delivery file holds no delivery section"));
        }
    }
    let mut call_args = args(&[("project", json!(project)), ("repos", json!(repos))]);
    if let Some(section) = delivery {
        call_args.insert("delivery".into(), json!(section));
    }
    let body = call(target, wake_timeout, "delivery_policy_set", call_args)?;
    set_delivery_answer(&body, project, &repos, delivery)
}

fn set_delivery_repos(repos: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for repo in repos {
        let repo = repo.trim();
        if !valid_repo(repo) {
            return Err(refuse(format!("--repo {repo:?} is not owner/name")));
        }
        if !out.iter().any(|r| r.eq_ignore_ascii_case(repo)) {
            out.push(repo.to_string());
        }
    }
    if out.is_empty() {
        return Err(refuse("at least one --repo is required"));
    }
    Ok(out)
}

/// The receipt `{state:"recorded", project, repos, delivery, revision}`
/// must echo exactly what was sent.
pub(crate) fn set_delivery_answer(
    body: &Value,
    project: &str,
    repos: &[String],
    delivery: Option<&str>,
) -> Result<Value> {
    let echoed = body["repos"].as_array().map(|list| {
        list.iter()
            .map(|r| r.as_str().unwrap_or_default().to_ascii_lowercase())
            .collect::<Vec<_>>()
    });
    let asked: Vec<String> = repos.iter().map(|r| r.to_ascii_lowercase()).collect();
    if body["state"].as_str() != Some("recorded")
        || body["project"].as_str() != Some(project)
        || echoed.as_ref() != Some(&asked)
        || body["delivery"].as_str() != delivery
        || (delivery.is_none() && !body["delivery"].is_null())
        || !body["revision"].is_u64()
    {
        return Err(refuse(format!(
            "`delivery_policy_set` receipt does not name exactly project {project} and its repos: {}",
            short(body)
        )));
    }
    Ok(body.clone())
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
/// an answer that also names them must name exactly those.
///
/// Keys outside the contract are ignored, not refused: a newer service may add
/// an informational field (CAD-1987: `note` on a recorded answer), and the
/// gate's meaning comes only from the fields checked here. Nothing an extra key
/// says is echoed — the printed `verified` and `note` are this client's.
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

/// `cadence audit verdict` (CAD-1542, AOS-473) for a hosted org: record one
/// reviewer's verdict for exactly this repo, issue, PR and head. The board
/// takes the reviewer from the authenticated caller, never from an argument,
/// and binds the row to the owner's attestation of that caller for `repo`
/// (AOS-473: a repeat of the same kind, head and findings returns the latest
/// row with `duplicate: true`; a changed one appends a correction).
#[allow(clippy::too_many_arguments)]
pub fn verdict(
    target: &RemoteTarget,
    wake_timeout: u64,
    repo: &str,
    issue: &str,
    pr: u64,
    head: &str,
    kind: &str,
    outcome: &str,
    title_outcome: &str,
    section_outcome: &str,
    risk: &str,
    body: &str,
) -> Result<Value> {
    call(
        target,
        wake_timeout,
        "audit_verdict",
        args(&[
            ("repo", json!(repo)),
            ("issue", json!(issue)),
            ("pr", json!(pr)),
            ("head", json!(head)),
            ("kind", json!(kind)),
            ("outcome", json!(outcome)),
            ("title_outcome", json!(title_outcome)),
            ("section_outcome", json!(section_outcome)),
            ("risk", json!(risk)),
            ("body", json!(body)),
        ]),
    )
}

/// `cadence audit verdicts` (CAD-1946, AOS-473) for a hosted org: the org's
/// own read of the verdicts naming this repo, issue, PR and full head. A
/// refused or unreadable answer is exit 1 — never the notes folder on this
/// machine, which would answer an empty list and read as "nothing was filed".
pub fn verdicts(
    target: &RemoteTarget,
    wake_timeout: u64,
    repo: &str,
    issue: &str,
    pr: u64,
    head: &str,
) -> Result<(Value, i32)> {
    let body = call(
        target,
        wake_timeout,
        "audit_verdicts",
        args(&[
            ("repo", json!(repo)),
            ("issue", json!(issue)),
            ("pr", json!(pr)),
            ("head", json!(head)),
        ]),
    )?;
    verdicts_answer(&body, repo, issue, pr, head)
}

/// The kinds a hosted row may name. `spec` is retired by AOS-473 but a row
/// stored before it keeps its kind on read-back; the gate counts only the
/// recognised kinds.
const VERDICT_KINDS: [&str; 6] = [
    "standards",
    "spec-security",
    "spec",
    "combined",
    "browser-qa",
    "other",
];

/// One hosted verdict row in the contract's entry shape, so the gate reads
/// one document shape from both origins. The binding is part of the evidence:
/// a row whose repo is not the requested one, or whose `reviewer_id` or
/// `github_login` is empty, proves nothing and refuses the whole answer.
fn verdict_entry(row: &Value, repo: &str, verb: &str) -> Result<Value> {
    let kind = row_text(row, "kind", verb)?;
    if !VERDICT_KINDS.contains(&kind) {
        return Err(refuse(format!("`{verb}` row names kind {kind:?}")));
    }
    if !row["repo"]
        .as_str()
        .is_some_and(|r| r.eq_ignore_ascii_case(repo))
    {
        return Err(refuse(format!(
            "`{verb}` row names another repo than {repo}: {}",
            short(row)
        )));
    }
    let risk = row["risk"]
        .as_str()
        .ok_or_else(|| refuse(format!("`{verb}` row has no risk: {}", short(row))))?;
    Ok(json!({
        "name": row_text(row, "name", verb)?,
        "stamp": row_text(row, "stamp", verb)?,
        "reviewer": row_text(row, "reviewer", verb)?,
        "reviewer_id": row_text(row, "reviewer_id", verb)?,
        "github_login": row_text(row, "github_login", verb)?,
        "repo": repo,
        "kind": kind,
        "outcome": row_text(row, "outcome", verb)?,
        "title_outcome": row_text(row, "title_outcome", verb)?,
        "section_outcome": row_text(row, "section_outcome", verb)?,
        "risk": risk,
    }))
}

/// The answer must be a hosted document about exactly this repo, issue, PR
/// and head. `origin` is what makes the gate apply the attested rule: an
/// answer without it, or one claiming another origin, is refused here rather
/// than passed on as local notes.
pub(crate) fn verdicts_answer(
    body: &Value,
    repo: &str,
    issue: &str,
    pr: u64,
    head: &str,
) -> Result<(Value, i32)> {
    let verb = "audit_verdicts";
    let state = text(body, "state", verb)?;
    if state != "ok" {
        let reason = body["reason"].as_str().unwrap_or(state);
        return Ok((json!({"state": state, "reason": reason}), 1));
    }
    match body["origin"].as_str() {
        Some("hosted") => {}
        Some(origin) => {
            return Err(refuse(format!(
                "`{verb}` answered origin {origin:?}, not \"hosted\""
            )))
        }
        None => return Err(refuse(format!("`{verb}` answer has no origin marker"))),
    }
    if !body["repo"]
        .as_str()
        .is_some_and(|r| r.eq_ignore_ascii_case(repo))
    {
        return Err(refuse(format!(
            "`{verb}` answered for another repo than {repo}"
        )));
    }
    if !body["issue"]
        .as_str()
        .is_some_and(|i| i.eq_ignore_ascii_case(issue))
    {
        return Err(refuse(format!(
            "`{verb}` answered for another issue than {issue}"
        )));
    }
    if body["pr"].as_u64() != Some(pr) {
        return Err(refuse(format!(
            "`{verb}` answered for another PR than {pr}"
        )));
    }
    if !body["head"]
        .as_str()
        .is_some_and(|h| h.eq_ignore_ascii_case(head))
    {
        return Err(refuse(format!(
            "`{verb}` answered for another head than {head}"
        )));
    }
    let verdicts = body["verdicts"]
        .as_array()
        .ok_or_else(|| refuse(format!("`{verb}` answer has no verdicts list")))?
        .iter()
        .map(|row| verdict_entry(row, repo, verb))
        .collect::<Result<Vec<_>>>()?;
    let skipped = body["skipped"]
        .as_array()
        .ok_or_else(|| refuse(format!("`{verb}` answer has no skipped list")))?
        .clone();
    Ok((
        json!({
            "state": "ok",
            "origin": "hosted",
            "repo": repo,
            "issue": issue,
            "pr": pr,
            "head": head,
            "verdicts": verdicts,
            "skipped": skipped,
        }),
        0,
    ))
}

// ---- reviewer attestations (AOS-473) ---------------------------------------

/// `cadence audit reviewers` for a hosted org: the workspace owner's
/// attestations, limited to `repo`. `audit_reviewers` takes no arguments and
/// reads the whole workspace, so the selection is made here.
pub fn reviewers(target: &RemoteTarget, wake_timeout: u64, repo: &str) -> Result<(Value, i32)> {
    let body = call(target, wake_timeout, "audit_reviewers", Map::new())?;
    reviewers_answer(&body, repo)
}

/// One attestation row: a lowercase `owner/name` repo, the GitHub login the
/// owner attested, the workspace principal it is, and when it was recorded.
fn reviewer_row(row: &Value, verb: &str) -> Result<Value> {
    let repo = row_text(row, "repo", verb)?;
    if !valid_repo(repo) {
        return Err(refuse(format!(
            "`{verb}` row names repo {repo:?}, not owner/name"
        )));
    }
    Ok(json!({
        "repo": repo.to_ascii_lowercase(),
        "github_login": row_text(row, "github_login", verb)?,
        "subject_id": row_text(row, "subject_id", verb)?,
        "recorded_at": row_text(row, "recorded_at", verb)?,
    }))
}

pub(crate) fn reviewers_answer(body: &Value, repo: &str) -> Result<(Value, i32)> {
    let verb = "audit_reviewers";
    let state = text(body, "state", verb)?;
    if state != "ok" {
        let reason = body["reason"].as_str().unwrap_or(state);
        return Ok((json!({"state": state, "reason": reason}), 1));
    }
    let mut rows = body["reviewers"]
        .as_array()
        .ok_or_else(|| refuse(format!("`{verb}` answer has no reviewers list")))?
        .iter()
        .map(|row| reviewer_row(row, verb))
        .collect::<Result<Vec<_>>>()?;
    rows.retain(|row| {
        row["repo"]
            .as_str()
            .is_some_and(|r| r.eq_ignore_ascii_case(repo))
    });
    Ok((json!({"state": "ok", "repo": repo, "reviewers": rows}), 0))
}

/// `cadence audit reviewers set`: replace `repo`'s whole attestation map.
/// The tenant checks the live owner and the `audit.approve` scope; the
/// receipt must name exactly the repo and rows sent, or the command refuses
/// — an attestation that was not recorded must never read as recorded.
pub fn set_reviewers(
    target: &RemoteTarget,
    wake_timeout: u64,
    repo: &str,
    reviewers: &[(String, String)],
) -> Result<Value> {
    let rows: Vec<Value> = reviewers
        .iter()
        .map(|(login, subject)| json!({"github_login": login, "subject_id": subject}))
        .collect();
    let body = call(
        target,
        wake_timeout,
        "audit_reviewers_set",
        args(&[("repo", json!(repo)), ("reviewers", json!(rows))]),
    )?;
    set_reviewers_answer(&body, repo, reviewers)
}

pub(crate) fn set_reviewers_answer(
    body: &Value,
    repo: &str,
    reviewers: &[(String, String)],
) -> Result<Value> {
    let verb = "audit_reviewers_set";
    if body["state"].as_str() != Some("recorded") {
        return Err(refuse(format!(
            "`{verb}` did not record the attestations: {}",
            short(body)
        )));
    }
    if !body["repo"]
        .as_str()
        .is_some_and(|r| r.eq_ignore_ascii_case(repo))
    {
        return Err(refuse(format!(
            "`{verb}` receipt names another repo than {repo}"
        )));
    }
    let rows = body["reviewers"]
        .as_array()
        .ok_or_else(|| refuse(format!("`{verb}` receipt has no reviewers list")))?
        .iter()
        .map(|row| reviewer_row(row, verb))
        .collect::<Result<Vec<_>>>()?;
    // The receipt proves the map that now stands: it must be exactly the rows
    // sent, matched by login and principal (the tenant lowercases the login).
    let key = |row: &Value| {
        (
            row["github_login"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase(),
            row["subject_id"].as_str().unwrap_or_default().to_string(),
        )
    };
    let mut recorded: Vec<(String, String)> = rows.iter().map(key).collect();
    let mut sent: Vec<(String, String)> = reviewers
        .iter()
        .map(|(login, subject)| (login.to_ascii_lowercase(), subject.clone()))
        .collect();
    recorded.sort();
    sent.sort();
    if recorded != sent {
        return Err(refuse(format!(
            "`{verb}` receipt does not echo the {} attestation(s) sent",
            reviewers.len()
        )));
    }
    Ok(json!({"state": "recorded", "repo": repo, "reviewers": rows}))
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
    fn approval_ignores_an_informational_key_without_echoing_it() {
        // CAD-1987: a recorded hosted answer carries `note`; refusing the answer
        // made a correctly recorded approval unreadable by the gate.
        let mut body = in_force();
        body["note"] = json!("the tenant records artifact reviews");
        body["verified"] = json!(true);
        let (doc, code) = approval_answer(&body, "favcrm/cadence", 7, HEAD).unwrap();
        assert_eq!(code, 0);
        assert_eq!(doc["state"], "in-force");
        // An answer's claim never becomes this client's claim.
        assert_eq!(doc["verified"], false);
        assert_ne!(doc["note"], json!("the tenant records artifact reviews"));
    }

    #[test]
    fn approval_refuses_a_malformed_answer() {
        let mut not_merge = in_force();
        not_merge["action"] = json!("deploy");
        for body in [
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
    fn a_bare_pr_number_becomes_the_project_remote_url() {
        let one = vec!["github.com/favcrm/cadence".to_string()];
        assert_eq!(
            pr_url_from(&one, "1462").unwrap(),
            "https://github.com/favcrm/cadence/pull/1462"
        );
        assert_eq!(
            pr_url_from(&one, "#1462").unwrap(),
            "https://github.com/favcrm/cadence/pull/1462"
        );
        // A URL is already what the classifier wants.
        let url = "https://github.com/other/repo/pull/7";
        assert_eq!(pr_url_from(&one, url).unwrap(), url);
        // Nothing to match, or more than one candidate: refuse rather than guess.
        assert!(pr_url_from(&[], "1462").is_err());
        assert!(pr_url_from(
            &[
                "github.com/favcrm/cadence".to_string(),
                "github.com/favcrm/pm".to_string()
            ],
            "1462"
        )
        .is_err());
    }

    fn entry(repo: &str, kind: &str, reviewer_id: &str, login: &str, stamp: &str) -> Value {
        json!({"name": format!("{stamp}-{kind}-verdict.md"), "stamp": stamp,
               "reviewer": login, "reviewer_id": reviewer_id, "github_login": login,
               "repo": repo, "kind": kind, "outcome": "pass",
               "title_outcome": "pass", "section_outcome": "pass", "risk": "auto"})
    }

    fn hosted_doc() -> Value {
        json!({"state": "ok", "origin": "hosted", "repo": "favcrm/cadence",
               "issue": "CAD-1", "pr": 7, "head": HEAD, "skipped": [],
               "verdicts": [entry("favcrm/cadence", "standards", "user_a", "alice",
                                  "20261101120000")]})
    }

    #[test]
    fn verdicts_answer_binds_the_issue_pr_and_head() {
        let ok = hosted_doc();
        let (doc, code) = verdicts_answer(&ok, "favcrm/cadence", "CAD-1", 7, HEAD).unwrap();
        assert_eq!(code, 0);
        assert_eq!(doc["origin"], "hosted");
        assert_eq!(doc["repo"], "favcrm/cadence");
        assert_eq!(doc["skipped"], json!([]));
        assert_eq!(doc["verdicts"][0]["reviewer_id"], "user_a");
        assert_eq!(doc["verdicts"][0]["github_login"], "alice");
        // Another issue, PR or head is a refusal, never a silent empty answer.
        assert!(verdicts_answer(&ok, "favcrm/cadence", "CAD-2", 7, HEAD).is_err());
        assert!(verdicts_answer(&ok, "favcrm/cadence", "CAD-1", 8, HEAD).is_err());
        assert!(verdicts_answer(&ok, "favcrm/cadence", "CAD-1", 7, OTHER).is_err());
        // A refused read exits 1 with its reason; it does not become "empty".
        let unknown = json!({"state": "unknown", "reason": "store unreadable"});
        let (_, code) = verdicts_answer(&unknown, "favcrm/cadence", "CAD-1", 7, HEAD).unwrap();
        assert_eq!(code, 1);
        // An answer without a list is malformed, not empty.
        for body in [
            json!({"state": "ok", "origin": "hosted", "repo": "favcrm/cadence",
                   "issue": "CAD-1", "pr": 7, "head": HEAD, "skipped": []}),
            json!({"state": "ok", "origin": "hosted", "repo": "favcrm/cadence",
                   "issue": "CAD-1", "pr": 7, "head": HEAD, "verdicts": []}),
            json!({}),
        ] {
            assert!(
                verdicts_answer(&body, "favcrm/cadence", "CAD-1", 7, HEAD).is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn a_hosted_read_must_name_this_repo_and_carry_the_hosted_origin() {
        let ok = hosted_doc();
        // The repo the read asked for is part of the binding: an answer about
        // another repository proves nothing about this one.
        let err = verdicts_answer(&ok, "favcrm/pm", "CAD-1", 7, HEAD).unwrap_err();
        assert!(err.to_string().contains("another repo"), "{err}");
        // Case is not evidence, so it is not a difference either.
        assert!(verdicts_answer(&ok, "FavCRM/Cadence", "CAD-1", 7, HEAD).is_ok());
        // No marker is not "local": the client never decides the origin.
        for origin in [json!(null), json!("local"), json!("Hosted"), json!("")] {
            let mut body = ok.clone();
            if origin.is_null() {
                body.as_object_mut().unwrap().remove("origin");
            } else {
                body["origin"] = origin.clone();
            }
            let err = verdicts_answer(&body, "favcrm/cadence", "CAD-1", 7, HEAD).unwrap_err();
            assert!(err.to_string().contains("origin"), "{origin}: {err}");
        }
        let mut missing_repo = ok.clone();
        missing_repo.as_object_mut().unwrap().remove("repo");
        assert!(verdicts_answer(&missing_repo, "favcrm/cadence", "CAD-1", 7, HEAD).is_err());
    }

    #[test]
    fn a_hosted_row_without_its_binding_refuses_the_answer() {
        let ok = hosted_doc();
        // Every entry must carry the attestation the gate counts by.
        for (key, value) in [
            ("reviewer_id", json!("")),
            ("github_login", json!("")),
            ("reviewer", json!("  ")),
            ("repo", json!("favcrm/other")),
            ("kind", json!("inspection")),
            ("name", json!(null)),
            ("stamp", json!("")),
            ("outcome", json!("")),
            ("risk", json!(null)),
        ] {
            let mut body = ok.clone();
            body["verdicts"][0][key] = value.clone();
            assert!(
                verdicts_answer(&body, "favcrm/cadence", "CAD-1", 7, HEAD).is_err(),
                "{key} = {value}"
            );
        }
        // A row stored before AOS-473 keeps its retired kind on read-back.
        let mut legacy = ok.clone();
        legacy["verdicts"][0]["kind"] = json!("spec");
        assert!(verdicts_answer(&legacy, "favcrm/cadence", "CAD-1", 7, HEAD).is_ok());
    }

    #[test]
    fn reviewers_answer_selects_the_requested_repo_and_checks_every_row() {
        let body = json!({"state": "ok", "reviewers": [
            {"repo": "favcrm/pm", "github_login": "bob", "subject_id": "user_b",
             "recorded_at": "2026-11-01T12:00:00.000Z"},
            {"repo": "favcrm/cadence", "github_login": "alice", "subject_id": "user_a",
             "recorded_at": "2026-11-01T11:00:00.000Z"},
        ]});
        let (doc, code) = reviewers_answer(&body, "favcrm/cadence").unwrap();
        assert_eq!(code, 0);
        assert_eq!(doc["repo"], "favcrm/cadence");
        assert_eq!(doc["reviewers"].as_array().unwrap().len(), 1);
        assert_eq!(doc["reviewers"][0]["github_login"], "alice");
        assert_eq!(doc["reviewers"][0]["subject_id"], "user_a");
        // A repo with no attestation is an empty list, not another repo's rows.
        assert_eq!(
            reviewers_answer(&body, "favcrm/other").unwrap().0["reviewers"],
            json!([])
        );
        let (refused, code) = reviewers_answer(
            &json!({"state": "unknown", "reason": "store unreadable"}),
            "favcrm/cadence",
        )
        .unwrap();
        assert_eq!((refused["state"].as_str(), code), (Some("unknown"), 1));
        for row in [
            json!({"repo": "favcrm/cadence", "github_login": "alice"}),
            json!({"repo": "favcrm/cadence", "github_login": "", "subject_id": "user_a",
                   "recorded_at": "t"}),
            json!({"repo": "github.com/favcrm/cadence", "github_login": "alice",
                   "subject_id": "user_a", "recorded_at": "t"}),
        ] {
            let body = json!({"state": "ok", "reviewers": [row]});
            assert!(reviewers_answer(&body, "favcrm/cadence").is_err(), "{body}");
        }
        assert!(reviewers_answer(&json!({"state": "ok"}), "favcrm/cadence").is_err());
    }

    #[test]
    fn reviewers_set_receipt_must_echo_the_rows_sent() {
        let sent = vec![
            ("Alice".to_string(), "user_a".to_string()),
            ("bob".to_string(), "user_b".to_string()),
        ];
        let receipt = json!({"state": "recorded", "repo": "favcrm/cadence", "reviewers": [
            {"repo": "favcrm/cadence", "github_login": "alice", "subject_id": "user_a",
             "recorded_at": "2026-11-01T11:00:00.000Z"},
            {"repo": "favcrm/cadence", "github_login": "bob", "subject_id": "user_b",
             "recorded_at": "2026-11-01T11:00:00.000Z"},
        ]});
        let doc = set_reviewers_answer(&receipt, "favcrm/cadence", &sent).unwrap();
        assert_eq!(doc["state"], "recorded");
        assert_eq!(doc["reviewers"].as_array().unwrap().len(), 2);
        // Another repo, a shorter set, a swapped principal or a refusal: the
        // attestation that now stands is not the one asked for.
        assert!(set_reviewers_answer(&receipt, "favcrm/pm", &sent).is_err());
        assert!(set_reviewers_answer(&receipt, "favcrm/cadence", &sent[..1]).is_err());
        let swapped = vec![
            ("alice".to_string(), "user_b".to_string()),
            ("bob".to_string(), "user_a".to_string()),
        ];
        assert!(set_reviewers_answer(&receipt, "favcrm/cadence", &swapped).is_err());
        let mut refused = receipt.clone();
        refused["state"] = json!("refused");
        assert!(set_reviewers_answer(&refused, "favcrm/cadence", &sent).is_err());
        let mut no_rows = receipt.clone();
        no_rows.as_object_mut().unwrap().remove("reviewers");
        assert!(set_reviewers_answer(&no_rows, "favcrm/cadence", &sent).is_err());
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

    fn answer(repos: Value, delivery: Value, approval: Value) -> Value {
        json!({"project": "cadence", "revision": 3, "repos": repos,
               "delivery": delivery, "delivery_approval": approval})
    }

    fn not_stored() -> Value {
        json!({"state": "not_stored", "reason": "x"})
    }

    #[test]
    fn delivery_policy_answer_needs_stored_repos_and_known_keys() {
        let set = json!({"state": "set", "repos": ["favcrm/cadence"]});
        let ok = answer(set.clone(), json!({"state": "absent"}), not_stored());
        let read = delivery_policy_answer(&ok, "cadence").unwrap();
        assert_eq!(read.remotes, vec!["github.com/favcrm/cadence".to_string()]);
        assert_eq!((read.delivery, read.approval), (None, None));
        let unset = answer(not_stored(), not_stored(), not_stored());
        let err = delivery_policy_answer(&unset, "cadence").unwrap_err();
        assert!(err.to_string().contains("nothing proves"), "{err}");
        let mut extra = ok.clone();
        extra["policy"] = json!("routine");
        let mut other_project = ok.clone();
        other_project["project"] = json!("pm");
        for body in [
            answer(
                json!({"state": "set", "repos": []}),
                json!({"state": "absent"}),
                not_stored(),
            ),
            answer(
                json!({"state": "set", "repos": ["github.com/favcrm/cadence"]}),
                json!({"state": "absent"}),
                not_stored(),
            ),
            answer(
                json!(["favcrm/cadence"]),
                json!({"state": "absent"}),
                not_stored(),
            ),
            answer(set.clone(), not_stored(), not_stored()),
            answer(set.clone(), json!(null), not_stored()),
            answer(set.clone(), json!({"state": "set"}), not_stored()),
            answer(set.clone(), json!({"state": "absent"}), json!(null)),
            answer(
                set.clone(),
                json!({"state": "absent"}),
                json!({"state": "approved", "digest": "d", "recorded_via": "operator-connection"}),
            ),
            extra,
            other_project,
            json!({"project": "cadence", "visibility": "private", "grants": []}),
        ] {
            assert!(delivery_policy_answer(&body, "cadence").is_err(), "{body}");
        }
    }

    #[test]
    fn hosted_policy_resolves_through_the_native_functions() {
        // Absent delivery and no approval: exactly the native "no section".
        let none = ProjectPolicy {
            delivery: None,
            approval: None,
            remotes: vec![],
        };
        let local = delivery_policy::effective("cadence", Ok(None), None);
        assert_eq!(resolve("cadence", &none).0, local);
        // An inconsistent approval reads as none, as natively.
        let forged = ProjectPolicy {
            approval: Some(json!({"delivery_digest": "sha256:00", "delivery": null})),
            ..none
        };
        assert_eq!(resolve("cadence", &forged), (local, None));
        // A stored section is parsed by the native parser, with or without
        // its `delivery:` key, and is unapproved until an approval matches.
        let section =
            "reviews: {r: {kind: agent, focus: general}}\nrisk: [{require: [r]}]\nmax_revise: 4\n";
        let text = "---\ndelivery:\n  reviews: {r: {kind: agent, focus: general}}\n  risk: [{require: [r]}]\n  max_revise: 4\n---\n";
        let native = delivery_policy::effective(
            "cadence",
            delivery_policy::parse_project("cadence", Some(text)).map_err(|e| e.to_string()),
            None,
        );
        for stored in [
            section.to_string(),
            format!("delivery:\n  {}", section.replace('\n', "\n  ")),
        ] {
            let custom = ProjectPolicy {
                delivery: Some(stored),
                approval: None,
                remotes: vec![],
            };
            let (resolved, _) = resolve("cadence", &custom);
            assert_eq!(resolved, native);
            assert!(resolved
                .note
                .as_deref()
                .unwrap_or("")
                .starts_with("delivery_unapproved"));
        }
    }

    #[test]
    fn a_hosted_approval_counts_only_with_the_native_policy_digest() {
        let text =
            "reviews: {r: {kind: agent, focus: general}}\nrisk: [{require: [r]}]\nmax_revise: 4\n";
        let policy = delivery_policy::parse(&delivery_front(text))
            .unwrap()
            .unwrap();
        let repos = json!({"state": "set", "repos": ["favcrm/cadence"]});
        let delivery = json!({"state": "set", "text": text});
        let approved = |digest: &str| {
            let body = answer(
                repos.clone(),
                delivery.clone(),
                json!({"state": "approved", "digest": digest,
                       "recorded_via": "hosted-owner-login"}),
            );
            resolve(
                "cadence",
                &delivery_policy_answer(&body, "cadence").unwrap(),
            )
        };
        let (native, approval) = approved(&delivery_policy::digest(&policy));
        assert_eq!((native.source, native.note.as_deref()), ("approved", None));
        assert!(approval.is_some());
        // A digest of the raw text is not the native digest: unapproved.
        use sha2::{Digest, Sha256};
        let raw: String = Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        for digest in [raw.clone(), format!("sha256:{raw}")] {
            let (resolved, approval) = approved(&digest);
            assert_eq!(approval, None);
            assert!(resolved
                .note
                .as_deref()
                .unwrap_or("")
                .starts_with("delivery_unapproved"));
        }
    }

    #[test]
    fn delivery_set_validates_repos_and_its_receipt() {
        assert!(set_delivery_repos(&["favcrm/cadence".into(), "evil".into()]).is_err());
        assert!(set_delivery_repos(&[]).is_err());
        let repos =
            set_delivery_repos(&["favcrm/cadence".into(), "FavCRM/Cadence".into()]).unwrap();
        assert_eq!(repos, vec!["favcrm/cadence".to_string()]);
        let ok = json!({"state": "recorded", "project": "cadence", "repos": ["favcrm/cadence"],
                        "delivery": null, "revision": 2});
        assert!(set_delivery_answer(&ok, "cadence", &repos, None).is_ok());
        assert!(set_delivery_answer(&ok, "pm", &repos, None).is_err());
        assert!(set_delivery_answer(&ok, "cadence", &repos, Some("max_revise: 4")).is_err());
        let mut other = ok.clone();
        other["repos"] = json!(["favcrm/pm"]);
        assert!(set_delivery_answer(&other, "cadence", &repos, None).is_err());
        let mut refused = ok.clone();
        refused["state"] = json!("refused");
        assert!(set_delivery_answer(&refused, "cadence", &repos, None).is_err());
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
