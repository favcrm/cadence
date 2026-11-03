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
//! CAD-2001: the review evidence is project-scoped. Every gate read and the
//! operator's approval carry the exact [`GateBinding`] — project, issue,
//! repo, PR and full head — through `audit_verdicts_project`,
//! `audit_approval_project` and `audit_approve_project`, so an answer can
//! only ever be that project's evidence for that issue. The project is never
//! inferred from a repo or the cwd: a caller that cannot name it is refused
//! before a request exists. The older repo-scoped verbs stay served by the
//! tenant for old clients only; this build never sends them and never falls
//! back to them. The release order is the tenant server first, then this
//! client: an older tenant refuses the project verbs, and
//! [`crate::remote_cli::PROJECT_SCOPED_GATE_VERBS`] reports that as the
//! release-order problem it is.
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

/// GitHub's login grammar, as the attestation writer validates it: 1-39
/// characters of ASCII alphanumerics and single hyphens, never starting or
/// ending with a hyphen and never doubled. A hosted row's login is compared
/// against the PR author, so a value that is not a login must refuse the read
/// rather than be compared as a name.
pub fn valid_login(login: &str) -> bool {
    (1..=39).contains(&login.len())
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !login.starts_with('-')
        && !login.ends_with('-')
        && !login.contains("--")
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
    // CAD-2001: the marker the merge gate reads to know which evidence scope
    // its later calls must carry. The daemon's own answer has no `origin`, so
    // a gate that sees it knows the org is local; a hosted answer says so, and
    // `scripts/enqueue-reviewed` then passes the project to every
    // project-scoped read. It is the origin of this answer, never a claim
    // about the local record.
    answer["origin"] = json!("hosted");
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

/// The exact binding every project-scoped gate call carries: the live project
/// scope plus the issue, the PR's repository, its number and the full head.
///
/// CAD-2001: each field is required and travels on every call. The tenant
/// answers only that project's evidence and re-checks the live
/// issue→project relation and the project's declared repo, so a caller that
/// cannot name the project is refused *here* — never answered from a repo's
/// first project, and never from the cwd checkout's origin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateBinding {
    pub project: String,
    pub repo: String,
    pub issue: String,
    pub pr: u64,
    pub head: String,
}

impl GateBinding {
    /// Refuse an incomplete binding before a request exists. `head` must
    /// already be the full 40-hex SHA (the CLI's `full_head`), because a
    /// shorter one would let a read bind to another revision.
    pub fn new(project: &str, repo: &str, issue: &str, pr: u64, head: &str) -> Result<Self> {
        let field = |value: &str, name: &str| -> Result<String> {
            let value = value.trim();
            if value.is_empty() || value.chars().any(char::is_control) {
                return Err(refuse(format!(
                    "the hosted gate needs an explicit nonempty {name}: nothing is inferred \
                     from a repo or the cwd — nothing was sent"
                )));
            }
            Ok(value.to_string())
        };
        let project = field(project, "--project")?;
        if !valid_project_key(&project) {
            return Err(refuse(format!(
                "--project {project:?} is not a project key (lowercase letters, digits and \
                 hyphens, starting with a letter or digit) — nothing was sent"
            )));
        }
        let issue = field(issue, "--issue")?;
        let repo = field(repo, "--repo")?;
        if !valid_repo(&repo) {
            return Err(refuse(format!("--repo {repo:?} is not owner/name")));
        }
        let head = head.trim().to_ascii_lowercase();
        if head.len() != 40 || !head.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(refuse("--head must be the full 40-hex SHA"));
        }
        if pr == 0 {
            return Err(refuse("--pr must be a PR number"));
        }
        Ok(Self {
            project,
            repo,
            issue,
            pr,
            head,
        })
    }

    /// The `{project, repo, issue, pr, head}` tuple every project-scoped verb
    /// takes, and nothing else — no authority-shaped field ever rides here.
    fn arguments(&self) -> Map<String, Value> {
        args(&[
            ("project", json!(self.project)),
            ("repo", json!(self.repo)),
            ("issue", json!(self.issue)),
            ("pr", json!(self.pr)),
            ("head", json!(self.head)),
        ])
    }

    /// Refuse an answer that names another project, issue, repo, PR or head.
    /// The project is always required — it is the scope. With `require_all`,
    /// every binding field is required (the answers the tenant documents echo
    /// all five); otherwise a field the answer does not carry is left to the
    /// caller's own checks.
    fn echoes(&self, body: &Value, verb: &str, require_all: bool) -> Result<()> {
        let same = |key: &str, want: &str, required: bool, fold: bool| -> Result<()> {
            let equal = |got: &str| {
                if fold {
                    got.eq_ignore_ascii_case(want)
                } else {
                    got == want
                }
            };
            match body.get(key).and_then(Value::as_str) {
                Some(got) if equal(got) => Ok(()),
                Some(got) => Err(refuse(format!(
                    "`{verb}` answered {key} {got:?}, not {want:?}"
                ))),
                None if required => Err(refuse(format!("`{verb}` answer names no {key}"))),
                None => Ok(()),
            }
        };
        // A project key is an exact scope, never a case-folded match.
        same("project", &self.project, true, false)?;
        same("repo", &self.repo, require_all, true)?;
        same("issue", &self.issue, require_all, true)?;
        same("head", &self.head, require_all, true)?;
        if let Some(pr) = body.get("pr") {
            if pr.as_u64() != Some(self.pr) {
                return Err(refuse(format!(
                    "`{verb}` answered for another PR than {}",
                    self.pr
                )));
            }
        }
        Ok(())
    }

    /// A refusal carries no evidence, so it need not name the project — but a
    /// refusal that names *another* project is not this call's answer.
    fn refusal_scope(&self, body: &Value, verb: &str) -> Result<()> {
        match body.get("project").and_then(Value::as_str) {
            Some(got) if got == self.project => Ok(()),
            Some(got) => Err(refuse(format!(
                "`{verb}` answered for another project than {}: {got:?}",
                self.project
            ))),
            None => Ok(()),
        }
    }
}

/// `cadence audit approval` for a hosted org: `(document, exit code)` in the
/// local command's shape (see [`approval_answer`]). `audit_approval_project`
/// binds the project and the issue the approval authorizes, so an approval
/// recorded for another issue or project can never answer this read.
pub fn approval(
    target: &RemoteTarget,
    wake_timeout: u64,
    binding: &GateBinding,
) -> Result<(Value, i32)> {
    let body = call(
        target,
        wake_timeout,
        "audit_approval_project",
        binding.arguments(),
    )?;
    approval_answer(&body, binding)
}

/// The contract answer is `{state:"in-force", project, issue, approval_id,
/// source, action, recorded_via}` or `{state:"missing", project, reason}`.
/// The verb binds the exact `{project, issue, repo, pr, head}` it was asked;
/// an answer that also names them must name exactly those.
///
/// Keys outside the contract are ignored, not refused: a newer service may add
/// an informational field (CAD-1987: `note` on a recorded answer), and the
/// gate's meaning comes only from the fields checked here. Nothing an extra key
/// says is echoed — the printed `verified` and `note` are this client's.
pub(crate) fn approval_answer(body: &Value, binding: &GateBinding) -> Result<(Value, i32)> {
    let verb = "audit_approval_project";
    body.as_object()
        .ok_or_else(|| refuse(format!("`{verb}` answer is not an object")))?;
    binding.echoes(body, verb, false)?;
    if body
        .get("action")
        .is_some_and(|a| a.as_str() != Some("merge"))
    {
        return Err(refuse(format!("`{verb}` answer is not a merge approval")));
    }
    let state = text(body, "state", verb)?;
    if state == "in-force" {
        if body.get("reason").is_some() {
            return Err(refuse(format!("`{verb}` in-force answer carries a reason")));
        }
        // The issue is part of the approval's identity: an in-force answer
        // that does not name it cannot prove which ticket it authorizes.
        if body.get("issue").and_then(Value::as_str) != Some(binding.issue.as_str()) {
            return Err(refuse(format!(
                "`{verb}` in-force answer does not name issue {}",
                binding.issue
            )));
        }
        if body.get("action").and_then(Value::as_str) != Some("merge") {
            return Err(refuse(format!(
                "`{verb}` in-force answer names no merge action"
            )));
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
    if body.get("approval_id").is_some() {
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

/// `cadence audit verdict` (CAD-1542, AOS-473, CAD-2001) for a hosted org:
/// record one reviewer's verdict for exactly this project, repo, issue, PR and
/// head. The board takes the reviewer from the authenticated caller, never
/// from an argument, and binds the row to the owner's project eligibility and
/// workspace identity attestation for that caller (AOS-473: a repeat of the
/// same kind, head and findings returns the latest row with
/// `duplicate: true`; a changed one appends a correction).
#[allow(clippy::too_many_arguments)]
pub fn verdict(
    target: &RemoteTarget,
    wake_timeout: u64,
    binding: &GateBinding,
    kind: &str,
    outcome: &str,
    title_outcome: &str,
    section_outcome: &str,
    risk: &str,
    body: &str,
) -> Result<Value> {
    let mut call_args = binding.arguments();
    call_args.insert("kind".into(), json!(kind));
    call_args.insert("outcome".into(), json!(outcome));
    call_args.insert("title_outcome".into(), json!(title_outcome));
    call_args.insert("section_outcome".into(), json!(section_outcome));
    call_args.insert("risk".into(), json!(risk));
    call_args.insert("body".into(), json!(body));
    let answer = call(target, wake_timeout, "audit_verdict_project", call_args)?;
    verdict_answer(
        &answer,
        binding,
        kind,
        outcome,
        title_outcome,
        section_outcome,
    )
}

/// The receipt `{state:"recorded", duplicate, project, repo, issue, pr, head,
/// reviewer_id, github_login, identity_source, verdict}` must name exactly the
/// binding and verdict that was sent. A receipt that does not is not evidence
/// this verdict landed, so it refuses instead of printing a success.
fn verdict_answer(
    body: &Value,
    binding: &GateBinding,
    kind: &str,
    outcome: &str,
    title_outcome: &str,
    section_outcome: &str,
) -> Result<Value> {
    let verb = "audit_verdict_project";
    if body["state"].as_str() != Some("recorded") {
        return Err(refuse(format!(
            "`{verb}` did not record the verdict: {}",
            short(body)
        )));
    }
    binding.echoes(body, verb, true)?;
    if !body["duplicate"].is_boolean() {
        return Err(refuse(format!("`{verb}` receipt has no duplicate flag")));
    }
    if body["identity_source"].as_str() != Some("owner-attested") {
        return Err(refuse(format!(
            "`{verb}` receipt does not name an owner-attested identity"
        )));
    }
    let login = text(body, "github_login", verb)?;
    if !valid_login(login) {
        return Err(refuse(format!(
            "`{verb}` receipt names github_login {login:?}, which is not a GitHub login"
        )));
    }
    let reviewer = text(body, "reviewer_id", verb)?;
    let note = body
        .get("verdict")
        .filter(|note| note.is_object())
        .ok_or_else(|| refuse(format!("`{verb}` receipt has no verdict row")))?;
    // The note row is the tenant's verdict entry: it carries the project and
    // repo, not the issue/PR/head tuple (those are on the receipt), so only
    // the fields it does carry are checked.
    binding.echoes(note, verb, false)?;
    if note["kind"].as_str() != Some(kind)
        || note["outcome"].as_str() != Some(outcome)
        || note["title_outcome"].as_str() != Some(title_outcome)
        || note["section_outcome"].as_str() != Some(section_outcome)
        || note["reviewer_id"].as_str() != Some(reviewer)
    {
        return Err(refuse(format!(
            "`{verb}` receipt does not echo the verdict sent: {}",
            short(note)
        )));
    }
    Ok(body.clone())
}

/// `cadence audit verdicts` (CAD-1946, AOS-473, CAD-2001) for a hosted org:
/// the org's own read of this project's verdicts naming this issue, repo, PR
/// and full head. The tenant re-checks the live issue→project relation and the
/// project's declared repo, so a moved or removed issue answers a refusal, not
/// countable evidence. A refused or unreadable answer is exit 1 — never the
/// notes folder on this machine, which would answer an empty list and read as
/// "nothing was filed".
pub fn verdicts(
    target: &RemoteTarget,
    wake_timeout: u64,
    binding: &GateBinding,
) -> Result<(Value, i32)> {
    let body = call(
        target,
        wake_timeout,
        "audit_verdicts_project",
        binding.arguments(),
    )?;
    verdicts_answer(&body, binding)
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
/// a row whose repo is not the requested one, whose `reviewer_id` or
/// `github_login` is empty, or whose login is not a GitHub login, proves
/// nothing and refuses the whole answer. A `workflow-oidc` row (CAD-2081) is
/// the one shape with no login; it carries its run fields instead. Whether its
/// principal may count is the gate's allowlist, not this reader's.
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
    // CAD-2081: where the reviewer identity came from. The row contract is the
    // tenant's `projectVerdictEntry`; this is its one reader. A row without the
    // field is an owner-attested one, as before.
    let identity_source = match row.get("identity_source") {
        None | Some(Value::Null) => "owner-attested",
        Some(Value::String(s)) if s == "owner-attested" || s == "workflow-oidc" => s.as_str(),
        Some(other) => {
            return Err(refuse(format!(
                "`{verb}` row names identity_source {}",
                short(other)
            )))
        }
    };
    let workflow = identity_source == "workflow-oidc";
    let login = if workflow {
        // A workflow principal is not a person: it has no login, and a row
        // that names one is contradicting itself.
        if row.get("github_login").is_some_and(|v| !v.is_null()) {
            return Err(refuse(format!(
                "`{verb}` workflow row names a github_login: {}",
                short(row)
            )));
        }
        None
    } else {
        let login = row_text(row, "github_login", verb)?;
        if !valid_login(login) {
            return Err(refuse(format!(
                "`{verb}` row names github_login {login:?}, which is not a GitHub login"
            )));
        }
        Some(login)
    };
    let risk = row["risk"]
        .as_str()
        .ok_or_else(|| refuse(format!("`{verb}` row has no risk: {}", short(row))))?;
    let mut entry = json!({
        "name": row_text(row, "name", verb)?,
        "stamp": row_text(row, "stamp", verb)?,
        "reviewer": row_text(row, "reviewer", verb)?,
        "reviewer_id": row_text(row, "reviewer_id", verb)?,
        "repo": repo,
        "kind": kind,
        "outcome": row_text(row, "outcome", verb)?,
        "title_outcome": row_text(row, "title_outcome", verb)?,
        "section_outcome": row_text(row, "section_outcome", verb)?,
        "risk": risk,
        "identity_source": identity_source,
    });
    if let Some(login) = login {
        entry["github_login"] = json!(login);
    }
    if workflow {
        // The run that filed it, so the evidence names a job, not just a name.
        for field in ["run_id", "run_attempt", "workflow_sha", "jti"] {
            entry[field] = json!(row_text(row, field, verb)?);
        }
    }
    Ok(entry)
}

/// The answer must be a hosted document about exactly this project, issue,
/// repo, PR and head. `origin` is what makes the gate apply the attested rule:
/// an answer without it, or one claiming another origin, is refused here
/// rather than passed on as local notes. The tenant echoes all five binding
/// fields, so all five are required here.
pub(crate) fn verdicts_answer(body: &Value, binding: &GateBinding) -> Result<(Value, i32)> {
    let verb = "audit_verdicts_project";
    let state = text(body, "state", verb)?;
    if state != "ok" {
        // A refusal carries no evidence; it must still never be another
        // project's answer.
        binding.refusal_scope(body, verb)?;
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
    binding.echoes(body, verb, true)?;
    let verdicts = body["verdicts"]
        .as_array()
        .ok_or_else(|| refuse(format!("`{verb}` answer has no verdicts list")))?
        .iter()
        .map(|row| verdict_entry(row, &binding.repo, verb))
        .collect::<Result<Vec<_>>>()?;
    let skipped = body["skipped"]
        .as_array()
        .ok_or_else(|| refuse(format!("`{verb}` answer has no skipped list")))?
        .clone();
    Ok((
        json!({
            "state": "ok",
            "origin": "hosted",
            "project": binding.project,
            "repo": binding.repo,
            "issue": binding.issue,
            "pr": binding.pr,
            "head": binding.head,
            "verdicts": verdicts,
            "skipped": skipped,
        }),
        0,
    ))
}

// ---- project eligibility and identity attestations (AOS-473, CAD-2001) -----

/// A project key: the tenant's `projectKeySchema`. The key is the scope, so it
/// is validated before a request rather than after a refusal.
pub fn valid_project_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes[0] != b'-'
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// A workspace principal id: the tenant's `HOSTED_ID_PATTERN`.
fn valid_subject_id(id: &str) -> bool {
    (1..=200).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// One eligibility row of a project: the principal the owner admitted, the
/// GitHub login the workspace attests for it (null until the owner attests
/// one), and when the eligibility was recorded. The join is the point: an
/// unbound principal stays visible as `github_login: null` instead of
/// disappearing, because its verdicts cannot count yet.
fn eligibility_row(row: &Value, project: &str, verb: &str) -> Result<Value> {
    if let Some(named) = row.get("project").and_then(Value::as_str) {
        if named != project {
            return Err(refuse(format!(
                "`{verb}` row names project {named:?}, not {project:?}"
            )));
        }
    }
    let subject = row_text(row, "subject_id", verb)?;
    if !valid_subject_id(subject) {
        return Err(refuse(format!(
            "`{verb}` row names subject_id {subject:?}, which is not a principal id"
        )));
    }
    let login = match row.get("github_login") {
        None | Some(Value::Null) => Value::Null,
        Some(login) => {
            let login = login.as_str().filter(|l| valid_login(l)).ok_or_else(|| {
                refuse(format!(
                    "`{verb}` row names github_login {login:?}, which is not a GitHub login"
                ))
            })?;
            json!(login.to_ascii_lowercase())
        }
    };
    Ok(json!({
        "project": project,
        "subject_id": subject,
        "github_login": login,
        "recorded_at": row_text(row, "recorded_at", verb)?,
    }))
}

/// `cadence audit reviewers` for a hosted org: one project's eligibility map
/// joined to the workspace identity attestation. `audit_reviewers_project`
/// takes exactly the project, so the caller must name it — a repo never picks
/// one, and a project with no proven policy is refused rather than answered
/// empty.
pub fn reviewers(target: &RemoteTarget, wake_timeout: u64, project: &str) -> Result<(Value, i32)> {
    let body = call(
        target,
        wake_timeout,
        "audit_reviewers_project",
        args(&[("project", json!(project))]),
    )?;
    reviewers_answer(&body, project)
}

pub(crate) fn reviewers_answer(body: &Value, project: &str) -> Result<(Value, i32)> {
    let verb = "audit_reviewers_project";
    let state = text(body, "state", verb)?;
    if state != "ok" {
        // A refusal carries no evidence, but it must not be another project's.
        if let Some(named) = body.get("project").and_then(Value::as_str) {
            if named != project {
                return Err(refuse(format!(
                    "`{verb}` answered for another project than {project}: {named:?}"
                )));
            }
        }
        let reason = body["reason"].as_str().unwrap_or(state);
        return Ok((json!({"state": state, "reason": reason}), 1));
    }
    if body["project"].as_str() != Some(project) {
        return Err(refuse(format!(
            "`{verb}` answered for another project than {project}"
        )));
    }
    let rows = body["reviewers"]
        .as_array()
        .ok_or_else(|| refuse(format!("`{verb}` answer has no reviewers list")))?
        .iter()
        .map(|row| eligibility_row(row, project, verb))
        .collect::<Result<Vec<_>>>()?;
    Ok((
        json!({"state": "ok", "project": project, "reviewers": rows}),
        0,
    ))
}

/// `cadence audit reviewers set` for a hosted org: replace one project's whole
/// eligibility map. The tenant checks the live owner and the `audit.approve`
/// scope; the receipt must name exactly the project and principals sent, or
/// the command refuses — an eligibility change that was not recorded must
/// never read as recorded. Zero principals is a real replacement (it clears
/// the project), not a no-op.
pub fn set_reviewers(
    target: &RemoteTarget,
    wake_timeout: u64,
    project: &str,
    subjects: &[String],
) -> Result<Value> {
    let rows: Vec<Value> = subjects
        .iter()
        .map(|subject| json!({"subject_id": subject}))
        .collect();
    let body = call(
        target,
        wake_timeout,
        "audit_reviewers_project_set",
        args(&[("project", json!(project)), ("reviewers", json!(rows))]),
    )?;
    set_reviewers_answer(&body, project, subjects)
}

pub(crate) fn set_reviewers_answer(
    body: &Value,
    project: &str,
    subjects: &[String],
) -> Result<Value> {
    let verb = "audit_reviewers_project_set";
    if body["state"].as_str() != Some("recorded") {
        return Err(refuse(format!(
            "`{verb}` did not record the eligibility map: {}",
            short(body)
        )));
    }
    if body["project"].as_str() != Some(project) {
        return Err(refuse(format!(
            "`{verb}` receipt names another project than {project}"
        )));
    }
    let rows = body["reviewers"]
        .as_array()
        .ok_or_else(|| refuse(format!("`{verb}` receipt has no reviewers list")))?
        .iter()
        .map(|row| eligibility_row(row, project, verb))
        .collect::<Result<Vec<_>>>()?;
    // The receipt proves the map that now stands: exactly the principals sent.
    let mut recorded: Vec<&str> = rows
        .iter()
        .filter_map(|row| row["subject_id"].as_str())
        .collect();
    let mut sent: Vec<&str> = subjects.iter().map(String::as_str).collect();
    recorded.sort_unstable();
    sent.sort_unstable();
    if recorded != sent {
        return Err(refuse(format!(
            "`{verb}` receipt does not echo the {} principal(s) sent",
            subjects.len()
        )));
    }
    Ok(json!({"state": "recorded", "project": project, "reviewers": rows}))
}

/// One workspace identity attestation: the principal, the GitHub login the
/// owner attested for it, and when. This is an attestation the gate counts
/// against the PR author — never a claim of OAuth verification.
fn identity_row(row: &Value, verb: &str) -> Result<Value> {
    let subject = row_text(row, "subject_id", verb)?;
    if !valid_subject_id(subject) {
        return Err(refuse(format!(
            "`{verb}` row names subject_id {subject:?}, which is not a principal id"
        )));
    }
    let login = row_text(row, "github_login", verb)?;
    if !valid_login(login) {
        return Err(refuse(format!(
            "`{verb}` row names github_login {login:?}, which is not a GitHub login"
        )));
    }
    if row["identity_source"].as_str() != Some("owner-attested") {
        return Err(refuse(format!(
            "`{verb}` row is not an owner-attested identity"
        )));
    }
    Ok(json!({
        "subject_id": subject,
        "github_login": login.to_ascii_lowercase(),
        "attested_by": row.get("attested_by").and_then(Value::as_str),
        "attested_at": row_text(row, "attested_at", verb)?,
    }))
}

/// `cadence audit identities` for a hosted org: the workspace owner's
/// principal→GitHub-login attestation map. The tenant admits it for the live
/// human owner only, so an agent's answer is a refusal, never an empty map.
pub fn identities(target: &RemoteTarget, wake_timeout: u64) -> Result<(Value, i32)> {
    let body = call(target, wake_timeout, "audit_identities", Map::new())?;
    identities_answer(&body)
}

pub(crate) fn identities_answer(body: &Value) -> Result<(Value, i32)> {
    let verb = "audit_identities";
    let state = text(body, "state", verb)?;
    if state != "ok" {
        let reason = body["reason"].as_str().unwrap_or(state);
        return Ok((json!({"state": state, "reason": reason}), 1));
    }
    let rows = body["identities"]
        .as_array()
        .ok_or_else(|| refuse(format!("`{verb}` answer has no identities list")))?
        .iter()
        .map(|row| identity_row(row, verb))
        .collect::<Result<Vec<_>>>()?;
    Ok((json!({"state": "ok", "identities": rows}), 0))
}

/// `cadence audit identities set` for a hosted org: replace the workspace's
/// owner-attested identity map. The receipt must echo exactly the rows sent
/// (the tenant lowercases each login) or the command refuses: an attestation
/// that was not recorded must never read as recorded.
pub fn set_identities(
    target: &RemoteTarget,
    wake_timeout: u64,
    identities: &[(String, String)],
) -> Result<Value> {
    let rows: Vec<Value> = identities
        .iter()
        .map(|(subject, login)| json!({"subject_id": subject, "github_login": login}))
        .collect();
    let body = call(
        target,
        wake_timeout,
        "audit_identities_set",
        args(&[("identities", json!(rows))]),
    )?;
    set_identities_answer(&body, identities)
}

pub(crate) fn set_identities_answer(
    body: &Value,
    identities: &[(String, String)],
) -> Result<Value> {
    let verb = "audit_identities_set";
    if body["state"].as_str() != Some("recorded") {
        return Err(refuse(format!(
            "`{verb}` did not record the identities: {}",
            short(body)
        )));
    }
    let rows = body["identities"]
        .as_array()
        .ok_or_else(|| refuse(format!("`{verb}` receipt has no identities list")))?
        .iter()
        .map(|row| identity_row(row, verb))
        .collect::<Result<Vec<_>>>()?;
    let key = |row: &Value| {
        (
            row["subject_id"].as_str().unwrap_or_default().to_string(),
            row["github_login"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase(),
        )
    };
    let mut recorded: Vec<(String, String)> = rows.iter().map(key).collect();
    let mut sent: Vec<(String, String)> = identities
        .iter()
        .map(|(subject, login)| (subject.clone(), login.to_ascii_lowercase()))
        .collect();
    recorded.sort();
    sent.sort();
    if recorded != sent {
        return Err(refuse(format!(
            "`{verb}` receipt does not echo the {} identity row(s) sent",
            identities.len()
        )));
    }
    Ok(json!({"state": "recorded", "identities": rows}))
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
/// project and the issue the approval authorizes both travel with the exact
/// repo, PR and head, and the hosted org decides who may approve. The receipt
/// must name exactly that binding, or the command refuses: an approval
/// recorded for another issue or project is not this head's approval.
pub fn approve(
    target: &RemoteTarget,
    wake_timeout: u64,
    binding: &GateBinding,
    source: &str,
) -> Result<Value> {
    let mut call_args = binding.arguments();
    call_args.insert("source".into(), json!(source));
    let body = call(target, wake_timeout, "audit_approve_project", call_args)?;
    approve_answer(&body, binding, source)
}

pub(crate) fn approve_answer(body: &Value, binding: &GateBinding, source: &str) -> Result<Value> {
    let verb = "audit_approve_project";
    if body["state"].as_str() != Some("recorded") || body["action"].as_str() != Some("merge") {
        return Err(refuse(format!(
            "`{verb}` did not record a merge approval: {}",
            short(body)
        )));
    }
    binding.echoes(body, verb, false)?;
    // The issue is part of the approval's identity: a receipt that names
    // another ticket (or none) does not prove this merge was approved.
    if body["issue"].as_str() != Some(binding.issue.as_str()) {
        return Err(refuse(format!(
            "`{verb}` receipt does not name issue {}",
            binding.issue
        )));
    }
    if body["head_sha"].as_str() != Some(binding.head.as_str())
        || body["scope"]["pr"].as_u64() != Some(binding.pr)
        || !body["scope"]["repo"]
            .as_str()
            .is_some_and(|r| r.eq_ignore_ascii_case(&binding.repo))
    {
        return Err(refuse(format!(
            "`{verb}` receipt does not name exactly {}#{} at {}",
            binding.repo, binding.pr, binding.head
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

    fn binding() -> GateBinding {
        GateBinding::new("cadence", "favcrm/cadence", "CAD-1", 7, HEAD).unwrap()
    }

    fn in_force() -> Value {
        json!({"state": "in-force", "project": "cadence", "issue": "CAD-1",
               "approval_id": "merge-pr7-0123456789ab", "action": "merge",
               "source": "chris on the board", "recorded_via": "operator-connection"})
    }

    #[test]
    fn a_binding_requires_an_explicit_project_and_the_full_head() {
        // CAD-2001: a missing project is an error, never a repo inference.
        for (project, repo, issue, pr, head) in [
            ("", "favcrm/cadence", "CAD-1", 7, HEAD),
            ("  ", "favcrm/cadence", "CAD-1", 7, HEAD),
            ("Cadence", "favcrm/cadence", "CAD-1", 7, HEAD),
            ("-cadence", "favcrm/cadence", "CAD-1", 7, HEAD),
            ("cadence", "favcrm/cadence", "", 7, HEAD),
            ("cadence", "cadence", "CAD-1", 7, HEAD),
            ("cadence", "favcrm/cadence", "CAD-1", 0, HEAD),
            ("cadence", "favcrm/cadence", "CAD-1", 7, &HEAD[..12]),
        ] {
            assert!(
                GateBinding::new(project, repo, issue, pr, head).is_err(),
                "{project:?} {repo:?} {issue:?} {pr} {head}"
            );
        }
        assert_eq!(binding().project, "cadence");
    }

    #[test]
    fn approval_in_force_prints_the_local_shape() {
        let (doc, code) = approval_answer(&in_force(), &binding()).unwrap();
        assert_eq!(code, 0);
        assert_eq!(doc["state"], "in-force");
        assert_eq!(doc["action"], "merge");
        assert_eq!(doc["recorded_via"], "operator-connection");
        assert_eq!(doc["verified"], false);
    }

    #[test]
    fn approval_naming_another_project_or_issue_refuses() {
        let echoed = in_force();
        assert_eq!(approval_answer(&echoed, &binding()).unwrap().1, 0);
        // The approval answer names its project and the issue it authorizes;
        // either one being another call's is a refusal.
        for (project, issue) in [("other", "CAD-1"), ("cadence", "CAD-2")] {
            let other = GateBinding::new(project, "favcrm/cadence", issue, 7, HEAD).unwrap();
            let err = approval_answer(&echoed, &other).unwrap_err();
            assert!(err.to_string().contains("not "), "{err}");
        }
        // Another project's approval can never be this project's approval,
        // even when every other field agrees.
        let mut foreign = echoed.clone();
        foreign["project"] = json!("other");
        let err = approval_answer(&foreign, &binding()).unwrap_err();
        assert!(err.to_string().contains("other"), "{err}");
    }

    #[test]
    fn approval_ignores_an_informational_key_without_echoing_it() {
        // CAD-1987: a recorded hosted answer carries `note`; refusing the answer
        // made a correctly recorded approval unreadable by the gate.
        let mut body = in_force();
        body["note"] = json!("the tenant records artifact reviews");
        body["verified"] = json!(true);
        let (doc, code) = approval_answer(&body, &binding()).unwrap();
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
        let mut no_issue = in_force();
        no_issue.as_object_mut().unwrap().remove("issue");
        for body in [
            not_merge,
            no_issue,
            json!({"state": "in-force"}),
            json!({"state": "granted", "approval_id": "x"}),
            json!({"state": "missing", "project": "cadence", "reason": "x", "approval_id": "a1"}),
            json!(["in-force"]),
            json!({}),
            json!({"state": "missing"}),
            json!({"state": "missing", "reason": "no head"}),
        ] {
            assert!(approval_answer(&body, &binding()).is_err(), "{body}");
        }
    }

    #[test]
    fn approval_not_in_force_exits_one_with_its_reason() {
        let body = json!({"state": "missing", "project": "cadence", "reason": "head moved"});
        let (doc, code) = approval_answer(&body, &binding()).unwrap();
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
        json!({"state": "ok", "origin": "hosted", "project": "cadence",
               "repo": "favcrm/cadence", "issue": "CAD-1", "pr": 7, "head": HEAD,
               "skipped": [],
               "verdicts": [entry("favcrm/cadence", "standards", "user_a", "alice",
                                  "20261101120000")]})
    }

    #[test]
    fn verdicts_answer_binds_the_project_issue_pr_and_head() {
        let ok = hosted_doc();
        let (doc, code) = verdicts_answer(&ok, &binding()).unwrap();
        assert_eq!(code, 0);
        assert_eq!(doc["origin"], "hosted");
        assert_eq!(doc["project"], "cadence");
        assert_eq!(doc["repo"], "favcrm/cadence");
        assert_eq!(doc["skipped"], json!([]));
        assert_eq!(doc["verdicts"][0]["reviewer_id"], "user_a");
        assert_eq!(doc["verdicts"][0]["github_login"], "alice");
        // Another project, issue, PR or head is a refusal, never a silent
        // empty answer.
        for (project, issue, pr, head) in [
            ("other", "CAD-1", 7, HEAD),
            ("cadence", "CAD-2", 7, HEAD),
            ("cadence", "CAD-1", 8, HEAD),
            ("cadence", "CAD-1", 7, OTHER),
        ] {
            let other = GateBinding::new(project, "favcrm/cadence", issue, pr, head).unwrap();
            assert!(verdicts_answer(&ok, &other).is_err());
        }
        // A refused read exits 1 with its reason; it does not become "empty".
        let unknown = json!({"state": "unknown", "reason": "store unreadable"});
        let (_, code) = verdicts_answer(&unknown, &binding()).unwrap();
        assert_eq!(code, 1);
        // An answer without a list is malformed, not empty.
        for body in [
            json!({"state": "ok", "origin": "hosted", "project": "cadence",
                   "repo": "favcrm/cadence", "issue": "CAD-1", "pr": 7, "head": HEAD,
                   "skipped": []}),
            json!({"state": "ok", "origin": "hosted", "project": "cadence",
                   "repo": "favcrm/cadence", "issue": "CAD-1", "pr": 7, "head": HEAD,
                   "verdicts": []}),
            json!({}),
        ] {
            assert!(verdicts_answer(&body, &binding()).is_err(), "{body}");
        }
    }

    #[test]
    fn a_hosted_read_must_name_this_project_repo_and_carry_the_hosted_origin() {
        let ok = hosted_doc();
        assert!(verdicts_answer(&ok, &binding()).is_ok());
        // The project and the repo the read asked for are part of the binding:
        // an answer about another one proves nothing about this one.
        let other_project = GateBinding::new("other", "favcrm/cadence", "CAD-1", 7, HEAD).unwrap();
        let err = verdicts_answer(&ok, &other_project).unwrap_err();
        assert!(err.to_string().contains("not \"other\""), "{err}");
        let other_repo = GateBinding::new("cadence", "favcrm/pm", "CAD-1", 7, HEAD).unwrap();
        let err = verdicts_answer(&ok, &other_repo).unwrap_err();
        assert!(err.to_string().contains("favcrm/pm"), "{err}");
        // A project key is an exact scope: case-folding it would answer for
        // another project.
        let cased_project = GateBinding::new("Cadence", "favcrm/cadence", "CAD-1", 7, HEAD);
        assert!(cased_project.is_err());
        // Repo and head case are not evidence, so they are not differences.
        let cased = GateBinding::new("cadence", "FavCRM/Cadence", "CAD-1", 7, HEAD).unwrap();
        assert!(verdicts_answer(&ok, &cased).is_ok());
        let upper_head = GateBinding::new(
            "cadence",
            "favcrm/cadence",
            "CAD-1",
            7,
            &HEAD.to_ascii_uppercase(),
        )
        .unwrap();
        assert!(verdicts_answer(&ok, &upper_head).is_ok());
        // No marker is not "local": the client never decides the origin.
        for origin in [json!(null), json!("local"), json!("Hosted"), json!("")] {
            let mut body = ok.clone();
            if origin.is_null() {
                body.as_object_mut().unwrap().remove("origin");
            } else {
                body["origin"] = origin.clone();
            }
            let err = verdicts_answer(&body, &binding()).unwrap_err();
            assert!(err.to_string().contains("origin"), "{origin}: {err}");
        }
        // Every binding field the tenant echoes is required: an answer that
        // drops the project or the repo is not this read's answer.
        for key in ["project", "repo", "issue", "head"] {
            let mut body = ok.clone();
            body.as_object_mut().unwrap().remove(key);
            assert!(verdicts_answer(&body, &binding()).is_err(), "{key}");
        }
    }

    #[test]
    fn a_hosted_login_must_be_a_github_login() {
        // AOS-473: the entry's login is compared against the PR author, so a
        // value that is not a login must refuse the read instead of being
        // compared as a name.
        let ok = hosted_doc();
        let mut prose = ok.clone();
        prose["verdicts"][0]["github_login"] = json!("alice (security reviewer)");
        let err = verdicts_answer(&prose, &binding()).unwrap_err();
        assert!(err.to_string().contains("not a GitHub login"), "{err}");
        // The rest of the grammar, and case, which GitHub itself ignores.
        for login in [
            json!("-alice"),
            json!("alice-"),
            json!("alice--bob"),
            json!("alice_bob"),
            json!("alice bob"),
            json!(""),
            json!("a".repeat(40)),
        ] {
            let mut body = ok.clone();
            body["verdicts"][0]["github_login"] = login.clone();
            assert!(verdicts_answer(&body, &binding()).is_err(), "{login}");
        }
        let longest = "a".repeat(39);
        for login in ["alice", "Alice-Bob9", "a", longest.as_str()] {
            let mut body = ok.clone();
            body["verdicts"][0]["github_login"] = json!(login);
            assert!(verdicts_answer(&body, &binding()).is_ok(), "{login}");
        }
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
                verdicts_answer(&body, &binding()).is_err(),
                "{key} = {value}"
            );
        }
        // A row stored before AOS-473 keeps its retired kind on read-back.
        let mut legacy = ok.clone();
        legacy["verdicts"][0]["kind"] = json!("spec");
        assert!(verdicts_answer(&legacy, &binding()).is_ok());
    }

    #[test]
    fn a_verdict_receipt_must_echo_the_binding_and_the_verdict_sent() {
        let receipt = json!({"state": "recorded", "duplicate": false, "project": "cadence",
                             "repo": "favcrm/cadence", "issue": "CAD-1", "pr": 7, "head": HEAD,
                             "reviewer": "alice", "reviewer_id": "user_a",
                             "github_login": "alice", "identity_source": "owner-attested",
                             "policy_revision": 3,
                             "verdict": {"project": "cadence", "repo": "favcrm/cadence",
                                         "name": "20261101120000-standards-verdict.md",
                                         "stamp": "20261101120000",
                                         "kind": "standards", "outcome": "pass",
                                         "title_outcome": "pass",
                                         "section_outcome": "pass",
                                         "reviewer": "alice",
                                         "github_login": "alice",
                                         "risk": "auto",
                                         "reviewer_id": "user_a"}});
        assert!(verdict_answer(&receipt, &binding(), "standards", "pass", "pass", "pass").is_ok());
        // A receipt for another project, issue or head is not this verdict.
        let other = GateBinding::new("other", "favcrm/cadence", "CAD-1", 7, HEAD).unwrap();
        assert!(verdict_answer(&receipt, &other, "standards", "pass", "pass", "pass").is_err());
        // A receipt that does not echo the verdict sent proves nothing landed.
        assert!(verdict_answer(&receipt, &binding(), "combined", "pass", "pass", "pass").is_err());
        assert!(
            verdict_answer(&receipt, &binding(), "standards", "revise", "pass", "pass").is_err()
        );
        // An unattested identity cannot be the login the gate counts.
        let mut unattested = receipt.clone();
        unattested["identity_source"] = json!("unverified");
        assert!(
            verdict_answer(&unattested, &binding(), "standards", "pass", "pass", "pass").is_err()
        );
        // A refusal is not a recorded verdict.
        let mut refused = receipt.clone();
        refused["state"] = json!("refused");
        assert!(verdict_answer(&refused, &binding(), "standards", "pass", "pass", "pass").is_err());
    }

    #[test]
    fn reviewers_answer_names_the_project_and_keeps_unbound_principals() {
        let body = json!({"state": "ok", "project": "cadence", "reviewers": [
            {"project": "cadence", "subject_id": "user_b", "github_login": "bob",
             "recorded_at": "2026-11-01T12:00:00.000Z"},
            {"project": "cadence", "subject_id": "user_a", "github_login": null,
             "recorded_at": "2026-11-01T11:00:00.000Z"},
        ]});
        let (doc, code) = reviewers_answer(&body, "cadence").unwrap();
        assert_eq!(code, 0);
        assert_eq!(doc["project"], "cadence");
        assert_eq!(doc["reviewers"].as_array().unwrap().len(), 2);
        // A principal with no attested login stays visible: its verdicts
        // cannot count yet, and the operator must see that.
        assert_eq!(doc["reviewers"][1]["github_login"], json!(null));
        // Another project's map is a refusal, never this project's rows.
        assert!(reviewers_answer(&body, "other").is_err());
        let mut foreign_row = body.clone();
        foreign_row["reviewers"][0]["project"] = json!("other");
        assert!(reviewers_answer(&foreign_row, "cadence").is_err());
        let (refused, code) =
            reviewers_answer(&json!({"state": "unknown", "reason": "x"}), "cadence").unwrap();
        assert_eq!((refused["state"].as_str(), code), (Some("unknown"), 1));
        for row in [
            json!({"project": "cadence", "recorded_at": "t"}),
            json!({"project": "cadence", "subject_id": "user_a", "github_login": "",
                   "recorded_at": "t"}),
            json!({"project": "cadence", "subject_id": "user a", "github_login": "alice",
                   "recorded_at": "t"}),
        ] {
            let body = json!({"state": "ok", "project": "cadence", "reviewers": [row]});
            assert!(reviewers_answer(&body, "cadence").is_err(), "{body}");
        }
        assert!(reviewers_answer(&json!({"state": "ok"}), "cadence").is_err());
    }

    #[test]
    fn reviewers_set_receipt_must_echo_the_principals_sent() {
        let sent = vec!["user_a".to_string(), "user_b".to_string()];
        let receipt = json!({"state": "recorded", "project": "cadence", "reviewers": [
            {"project": "cadence", "subject_id": "user_a", "github_login": "alice",
             "recorded_at": "2026-11-01T11:00:00.000Z"},
            {"project": "cadence", "subject_id": "user_b", "github_login": null,
             "recorded_at": "2026-11-01T11:00:00.000Z"},
        ]});
        let doc = set_reviewers_answer(&receipt, "cadence", &sent).unwrap();
        assert_eq!(doc["state"], "recorded");
        assert_eq!(doc["reviewers"].as_array().unwrap().len(), 2);
        // Another project, a shorter set or a refusal: the map that now stands
        // is not the one asked for.
        assert!(set_reviewers_answer(&receipt, "other", &sent).is_err());
        assert!(set_reviewers_answer(&receipt, "cadence", &sent[..1]).is_err());
        // Zero principals is a real replacement, and its receipt must be empty.
        assert!(set_reviewers_answer(&receipt, "cadence", &[]).is_err());
        let cleared = json!({"state": "recorded", "project": "cadence", "reviewers": []});
        assert!(set_reviewers_answer(&cleared, "cadence", &[]).is_ok());
        let mut refused = receipt.clone();
        refused["state"] = json!("refused");
        assert!(set_reviewers_answer(&refused, "cadence", &sent).is_err());
        let mut no_rows = receipt.clone();
        no_rows.as_object_mut().unwrap().remove("reviewers");
        assert!(set_reviewers_answer(&no_rows, "cadence", &sent).is_err());
    }

    #[test]
    fn identities_answer_and_receipt_carry_the_owner_attestation() {
        let row = json!({"subject_id": "user_a", "github_login": "Alice",
                         "identity_source": "owner-attested", "attested_by": "user_owner",
                         "attested_at": "2026-11-01T11:00:00.000Z"});
        let (doc, code) = identities_answer(&json!({"state": "ok", "identities": [row]})).unwrap();
        assert_eq!(code, 0);
        assert_eq!(doc["identities"][0]["github_login"], "alice");
        assert_eq!(doc["identities"][0]["attested_by"], "user_owner");
        // An identity that is not owner-attested is not evidence the gate may
        // count a login against.
        for (key, value) in [
            ("identity_source", json!("oauth-verified")),
            ("github_login", json!("alice (security)")),
            ("subject_id", json!("")),
            ("attested_at", json!("")),
        ] {
            let mut bad = row.clone();
            bad[key] = value.clone();
            assert!(
                identities_answer(&json!({"state": "ok", "identities": [bad]})).is_err(),
                "{key} = {value}"
            );
        }
        let sent = vec![("user_a".to_string(), "Alice".to_string())];
        let receipt = json!({"state": "recorded", "identities": [row]});
        assert!(set_identities_answer(&receipt, &sent).is_ok());
        // A receipt that echoes another principal or login is not the map sent.
        assert!(
            set_identities_answer(&receipt, &[("user_b".to_string(), "alice".into())]).is_err()
        );
        assert!(set_identities_answer(&receipt, &[("user_a".to_string(), "bob".into())]).is_err());
        assert!(set_identities_answer(&receipt, &[]).is_err());
        let mut refused = receipt.clone();
        refused["state"] = json!("refused");
        assert!(set_identities_answer(&refused, &sent).is_err());
    }

    #[test]
    fn approve_receipt_must_name_the_exact_project_issue_head_pr_and_repo() {
        let receipt = json!({"action": "merge", "approval_id": "merge-pr7-0123456789ab",
                             "duplicate": false, "head_sha": HEAD, "project": "cadence",
                             "issue": "CAD-1", "recorded_via": "hosted-owner-login",
                             "scope": {"pr": 7, "repo": "favcrm/cadence"},
                             "source": "chris", "state": "recorded"});
        assert!(approve_answer(&receipt, &binding(), "chris").is_ok());
        // A moved head, another PR, repo, project, issue or source.
        for (project, issue, pr, head) in [
            ("cadence", "CAD-1", 7, OTHER),
            ("cadence", "CAD-1", 8, HEAD),
            ("cadence", "CAD-2", 7, HEAD),
            ("other", "CAD-1", 7, HEAD),
        ] {
            let other = GateBinding::new(project, "favcrm/cadence", issue, pr, head).unwrap();
            assert!(
                approve_answer(&receipt, &other, "chris").is_err(),
                "{project} {issue}"
            );
        }
        let other_repo = GateBinding::new("cadence", "favcrm/pm", "CAD-1", 7, HEAD).unwrap();
        assert!(approve_answer(&receipt, &other_repo, "chris").is_err());
        assert!(approve_answer(&receipt, &binding(), "someone").is_err());
        let mut refused = receipt.clone();
        refused["state"] = json!("refused");
        assert!(approve_answer(&refused, &binding(), "chris").is_err());
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
