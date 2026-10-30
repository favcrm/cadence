//! Read-only CAD-1298 delivery-requirements bridge. All classification
//! inputs are derived from the daemon's PM, recorded approvals and the
//! current GitHub PR; no caller-supplied policy or evidence is accepted.

use serde_json::{json, Value};
use std::collections::HashMap;

use super::{required_str, Shared};
use crate::error::{Error, Result};
use crate::issue::{self, delivery_policy, delivery_requirements as requirements};

#[cfg(test)]
#[path = "cad1298_requirements_acceptance.rs"]
mod acceptance;

#[cfg(test)]
#[path = "cad1561_integration_branch_acceptance.rs"]
mod cad1561_integration_branch_acceptance;

fn reject(why: impl std::fmt::Display) -> Error {
    Error::rejected(format!("delivery requirements: {why}"))
}

/// One `gh` read. The daemon runs the binary it resolved at boot; a
/// hosted-org CLI runs the caller's own `gh` (CAD-1946).
pub(crate) trait GhRead {
    fn gh_read(&self, args: &[&str]) -> Result<String>;
}

impl GhRead for Shared {
    fn gh_read(&self, args: &[&str]) -> Result<String> {
        crate::delivery::gh(
            self.delivery_gh
                .to_str()
                .unwrap_or("/nonexistent/gh-not-on-path-at-boot"),
            args,
        )
    }
}

/// The tracker-side inputs of [`classify_from`]. The daemon reads them
/// from its PM and approval store; a hosted-org CLI reads them from the
/// hosted tracker (CAD-1946). Either way the classification below is the
/// one implementation — only where the inputs come from differs.
pub(crate) trait RequirementInputs: GhRead {
    /// The key of the project the ticket belongs to.
    fn issue_project(&self, issue: &str) -> Result<String>;
    /// `Some(why)` when the PR's repository is not a declared remote of
    /// `project` ([`crate::delivery::pr_refusal_for_remotes`]).
    fn pr_refusal(&self, project: &str, pr_url: &str) -> Result<Option<String>>;
    /// The effective policy and its recorded approval, read afresh on
    /// every call so the drift re-read compares two real reads.
    fn policy(
        &self,
        project: &str,
    ) -> Result<(delivery_policy::Resolved, Option<delivery_policy::Approved>)>;
}

impl RequirementInputs for Shared {
    fn issue_project(&self, issue: &str) -> Result<String> {
        let pm = self.pm()?;
        let issue = issue::board::find_issue(&pm.dir, issue)?;
        Ok(issue::project::load(&pm.dir.join(&issue.project).join("project.yaml"))?.key)
    }

    fn pr_refusal(&self, project: &str, pr_url: &str) -> Result<Option<String>> {
        crate::delivery::project_pr_refusal(&self.pm()?.dir, project, pr_url)
    }

    fn policy(
        &self,
        project: &str,
    ) -> Result<(delivery_policy::Resolved, Option<delivery_policy::Approved>)> {
        policy_snapshot(self, &self.pm()?.dir, project)
    }
}

fn gh<G: GhRead + ?Sized>(source: &G, args: &[&str]) -> Result<String> {
    source.gh_read(args)
}

fn api<G: GhRead + ?Sized>(source: &G, endpoint: &str) -> Result<Value> {
    let text = gh(source, &["api", endpoint])?;
    serde_json::from_str(&text).map_err(|e| reject(format!("unreadable GitHub response: {e}")))
}

fn pr_view<G: GhRead + ?Sized>(shared: &G, slug: &str, number: u64) -> Result<Value> {
    let text = gh(
        shared,
        &[
            "pr",
            "view",
            &number.to_string(),
            "-R",
            slug,
            "--json",
            "title,headRefOid,baseRefName,baseRefOid,changedFiles,state,statusCheckRollup",
        ],
    )?;
    serde_json::from_str(&text).map_err(|e| reject(format!("unreadable PR view: {e}")))
}

fn default_branch<G: GhRead + ?Sized>(shared: &G, repo: &str) -> Result<String> {
    let text = gh(
        shared,
        &["repo", "view", repo, "--json", "defaultBranchRef"],
    )?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| reject(format!("unreadable repository default branch: {e}")))?;
    value["defaultBranchRef"]["name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .ok_or_else(|| reject("repository has no default branch"))
}

/// Percent-encode a branch name for a single URL path segment —
/// `repos/{repo}/rules/branches/{x}` takes the name inline, so `/`
/// (and every other non-unreserved byte) must be `%`-escaped or a
/// slashed target like `release/staging` reads as a different branch.
/// Policy already restricts these to canonical git names; this encodes
/// rather than revalidates so callers never depend on that.
pub(super) fn branch_segment(branch: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(branch.len());
    for b in branch.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

fn valid_sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn request_fields(params: &Value) -> Result<(&str, &str, &str)> {
    let fields = params
        .as_object()
        .ok_or_else(|| reject("request must be an object"))?;
    if fields
        .keys()
        .any(|key| !matches!(key.as_str(), "issue" | "pr" | "head"))
    {
        return Err(reject("request accepts only issue, pr and head"));
    }
    let issue = required_str(params, "issue")?;
    let pr = required_str(params, "pr")?;
    let head = required_str(params, "head")?;
    if issue.trim().is_empty() || pr.trim().is_empty() {
        return Err(reject("issue and pr must not be empty or whitespace-only"));
    }
    if !valid_sha(head) {
        return Err(reject("head must be a full lowercase 40-hex commit SHA"));
    }
    Ok((issue, pr, head))
}

fn title_names_issue(title: &str, issue: &str) -> bool {
    regex::Regex::new(r"^\s*([A-Z][A-Z0-9]*-\d+)\b")
        .expect("static issue-token pattern")
        .captures(title)
        .and_then(|captures| captures.get(1))
        .is_some_and(|token| token.as_str() == issue)
}

type TreeEntry = (String, String);

fn tree_modes<G: GhRead + ?Sized>(
    shared: &G,
    repo: &str,
    sha: &str,
) -> Result<HashMap<String, TreeEntry>> {
    let tree = api(shared, &format!("repos/{repo}/git/trees/{sha}?recursive=1"))?;
    if tree["truncated"].as_bool() != Some(false) {
        return Err(reject(
            "GitHub tree is truncated or has no truncation proof",
        ));
    }
    let entries = tree["tree"]
        .as_array()
        .ok_or_else(|| reject("GitHub tree has no entries"))?;
    let mut modes = HashMap::with_capacity(entries.len());
    for entry in entries {
        let path = entry["path"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| reject("GitHub tree contains an entry without a path"))?;
        let mode = entry["mode"]
            .as_str()
            .ok_or_else(|| reject(format!("tree entry {path} has no mode")))?;
        let kind = entry["type"]
            .as_str()
            .ok_or_else(|| reject(format!("tree entry {path} has no type")))?;
        let agrees = match kind {
            "blob" => matches!(mode, "100644" | "100755" | "120000"),
            "tree" => mode == "040000",
            "commit" => mode == "160000",
            _ => false,
        };
        if !agrees {
            return Err(reject(format!(
                "tree entry {path} has invalid type/mode {kind}/{mode}"
            )));
        }
        if modes
            .insert(path.to_string(), (mode.to_string(), kind.to_string()))
            .is_some()
        {
            return Err(reject(format!("GitHub tree repeats path {path}")));
        }
    }
    Ok(modes)
}

fn changed_mode(tree: &HashMap<String, TreeEntry>, path: &str) -> Result<String> {
    let (mode, kind) = tree
        .get(path)
        .ok_or_else(|| reject(format!("tree mode unavailable for {path}")))?;
    if kind == "tree" {
        return Err(reject(format!("changed path {path} resolves to a tree")));
    }
    Ok(mode.clone())
}

fn content_at_base<G: GhRead + ?Sized>(
    shared: &G,
    repo: &str,
    path: &str,
    base: &str,
) -> Option<String> {
    let endpoint = format!("repos/{repo}/contents/{path}?ref={base}");
    let value = api(shared, &endpoint).ok()?;
    if value["encoding"].as_str()? != "base64" {
        return None;
    }
    let encoded = value["content"].as_str()?.replace('\n', "");
    use base64::Engine;
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?,
    )
    .ok()
}

/// CAD-1561: a non-default PR target is admitted only when the EXACT
/// current approved policy names it. `source == "approved"` plus
/// `note == None` means the file's section IS the recorded approval
/// verbatim — a last-approved fallback after an edit carries a
/// `delivery_unapproved` note and does not qualify. The approval's own
/// digest/policy equality keeps provenance honest even if `effective`
/// semantics change.
fn approved_integration_target(
    base_ref: &str,
    resolved: &delivery_policy::Resolved,
    approved: Option<&delivery_policy::Approved>,
) -> bool {
    resolved.source == "approved"
        && resolved.note.is_none()
        && resolved.policy.integration_branch.as_deref() == Some(base_ref)
        && approved.is_some_and(|a| {
            a.digest == resolved.digest && a.policy.as_ref() == Some(&resolved.policy)
        })
}

/// Local policy and approval provenance, without remote collection. A caller
/// committing authority holds the tracker lock through its mutation.
pub(super) fn policy_snapshot(
    shared: &Shared,
    pm_dir: &std::path::Path,
    project: &str,
) -> Result<(delivery_policy::Resolved, Option<delivery_policy::Approved>)> {
    let approvals = shared.store.work_approvals()?;
    let approved = approvals
        .get(project)
        .and_then(delivery_policy::approved_from);
    let resolved = delivery_policy::effective(
        project,
        delivery_policy::load(pm_dir, project),
        approved.as_ref(),
    );
    Ok((resolved, approved))
}

pub(super) fn classify(shared: &Shared, params: &Value) -> Result<Value> {
    classify_with_policy(shared, params).map(|(answer, _)| answer)
}

pub(super) fn classify_with_policy(
    shared: &Shared,
    params: &Value,
) -> Result<(Value, delivery_policy::Resolved)> {
    classify_from(shared, params)
}

/// The requirements classification over any [`RequirementInputs`]: the
/// request binding, the project/remote check, the live PR and base-pinned
/// trusted lists, [`requirements::classify`], and the drift re-read.
pub(crate) fn classify_from<I: RequirementInputs + ?Sized>(
    shared: &I,
    params: &Value,
) -> Result<(Value, delivery_policy::Resolved)> {
    let (issue_id, pr_url, requested_head) = request_fields(params)?;

    let project_key = shared.issue_project(issue_id)?;
    if let Some(why) = shared.pr_refusal(&project_key, pr_url)? {
        return Err(reject(why));
    }
    let (slug, number) = issue::task_report::parse_pr_url(pr_url)?;
    let repo = slug.to_ascii_lowercase();
    let default = default_branch(shared, &repo)?;
    let view = pr_view(shared, &repo, number)?;
    if view["state"].as_str() != Some("OPEN") {
        return Err(reject("PR is not open"));
    }
    if view["title"]
        .as_str()
        .is_none_or(|title| !title_names_issue(title, issue_id))
    {
        return Err(reject(
            "PR title's first issue token does not exactly match the requested issue",
        ));
    }
    let head = view["headRefOid"]
        .as_str()
        .filter(|s| valid_sha(s))
        .ok_or_else(|| reject("PR has no valid full head SHA"))?;
    if head != requested_head {
        return Err(reject("requested head does not match current PR head"));
    }
    let base_ref = view["baseRefName"]
        .as_str()
        .ok_or_else(|| reject("PR has no base branch"))?;
    // Resolve the effective policy once — the target gate and the
    // requirements evaluator below share it. The reread at the end
    // still re-resolves for drift detection.
    let (resolved, approved) = shared.policy(&project_key)?;
    if base_ref != default && !approved_integration_target(base_ref, &resolved, approved.as_ref()) {
        return Err(reject(format!(
            "PR base {base_ref:?} is neither the repository default {default:?} \
             nor an approved delivery integration_branch"
        )));
    }
    let base = view["baseRefOid"]
        .as_str()
        .filter(|s| valid_sha(s))
        .ok_or_else(|| reject("PR has no valid full base commit SHA"))?
        .to_string();
    let changed_files = view["changedFiles"]
        .as_u64()
        .ok_or_else(|| reject("PR view has no changed-file count"))?;

    let comparison = api(shared, &format!("repos/{repo}/compare/{base}...{head}"))?;
    let merge_base = comparison["merge_base_commit"]["sha"]
        .as_str()
        .filter(|s| valid_sha(s))
        .ok_or_else(|| reject("comparison has no valid full merge-base SHA"))?
        .to_string();
    let files = comparison["files"]
        .as_array()
        .ok_or_else(|| reject("comparison has no complete file list"))?;
    if files.len() >= 300 || files.len() as u64 != changed_files {
        return Err(reject(
            "GitHub comparison file list is truncated or differs from the PR file count",
        ));
    }
    let base_modes = tree_modes(shared, &repo, &merge_base)?;
    let head_modes = tree_modes(shared, &repo, head)?;
    let mut changes = Vec::with_capacity(files.len());
    let mut lines = 0u64;
    for file in files {
        let path = file["filename"]
            .as_str()
            .ok_or_else(|| reject("comparison file has no path"))?;
        let status = match file["status"].as_str().unwrap_or("") {
            "added" => "A",
            "modified" => "M",
            "removed" => "D",
            "renamed" => "R",
            "changed" => "T",
            "copied" => "C",
            _ => "X",
        };
        let old_path = if status == "R" {
            file["previous_filename"]
                .as_str()
                .ok_or_else(|| reject(format!("renamed path {path} has no previous filename")))?
        } else {
            path
        };
        let old_mode = if status == "A" {
            "000000".to_string()
        } else {
            changed_mode(&base_modes, old_path)?
        };
        let new_mode = if status == "D" {
            "000000".to_string()
        } else {
            changed_mode(&head_modes, path)?
        };
        if file["additions"].as_u64().is_none() || file["deletions"].as_u64().is_none() {
            return Err(reject(format!(
                "changed-line counts unavailable for {path}"
            )));
        }
        lines = lines
            .saturating_add(file["additions"].as_u64().unwrap())
            .saturating_add(file["deletions"].as_u64().unwrap());
        changes.push(requirements::change(status, &old_mode, &new_mode, path));
    }

    let risk_paths = content_at_base(shared, &repo, "docs/roles/risk-paths.toml", &base);
    let one_review = content_at_base(shared, &repo, "docs/roles/one-review-paths.toml", &base);
    let lists = requirements::TrustedLists {
        risk_paths: risk_paths.as_deref(),
        one_review: one_review.as_deref(),
    };
    let result = requirements::classify(&resolved, &changes, lines, &lists);

    // Refuse a mixed-head answer: re-read all external and operator-owned
    // inputs that determine the result before returning it.
    let after = pr_view(shared, &repo, number)?;
    let (after_resolved, _) = shared.policy(&project_key)?;
    if after["state"].as_str() != Some("OPEN")
        || after["title"]
            .as_str()
            .is_none_or(|title| !title_names_issue(title, issue_id))
        || after["headRefOid"].as_str() != Some(head)
        || after["baseRefName"].as_str() != Some(base_ref)
        || after["baseRefOid"].as_str() != Some(base.as_str())
        || after["changedFiles"].as_u64() != Some(changed_files)
        || default_branch(shared, &repo)? != default
        // Whole-Resolved equality: an unapproved/malformed PROJECT.md
        // mid-collection can keep the approved policy in force with the
        // SAME digest but source/note changed — a digest-only check
        // would grant the extra target under stale provenance.
        || after_resolved != resolved
    {
        return Err(reject(
            "PR head/base or approved policy changed during collection",
        ));
    }
    Ok((
        json!({"issue": issue_id, "project": project_key, "repo": repo, "pr": number,
        "head": head, "base": base, "base_ref": base_ref, "merge_base": merge_base,
        "policy_digest": result.policy_digest, "requirements": result, "changes": changes}),
        resolved,
    ))
}

/// Recheck the actual target's required checks and latest canonical PR CI.
/// Automatic landing additionally requires the default branch's enforced
/// native merge queue; prequeue feedback is not current-base integration.
pub(super) fn full_ci_green(
    shared: &Shared,
    repo: &str,
    number: u64,
    head: &str,
    base: &str,
    base_ref: &str,
    require_queue: bool,
) -> Result<bool> {
    let current: Value = serde_json::from_str(&gh(
        shared,
        &[
            "pr",
            "view",
            &number.to_string(),
            "-R",
            repo,
            "--json",
            "headRefOid,baseRefOid,baseRefName,statusCheckRollup",
        ],
    )?)
    .map_err(|e| reject(format!("unreadable current PR checks: {e}")))?;
    if current["headRefOid"].as_str() != Some(head)
        || current["baseRefOid"].as_str() != Some(base)
        || current["baseRefName"].as_str() != Some(base_ref)
    {
        return Ok(false);
    }
    if require_queue && default_branch(shared, repo)? != base_ref {
        return Err(reject(
            "automatic landing requires the repository default branch",
        ));
    }
    let branch = branch_segment(base_ref);
    let rules = api(shared, &format!("repos/{repo}/rules/branches/{branch}"))?;
    let protection = api(
        shared,
        &format!("repos/{repo}/branches/{branch}/protection"),
    );
    let mut required = std::collections::BTreeSet::new();
    for rule in rules
        .as_array()
        .ok_or_else(|| reject("branch rules response is not an array"))?
    {
        if rule["type"] == "required_status_checks" {
            for check in rule["parameters"]["required_status_checks"]
                .as_array()
                .ok_or_else(|| reject("required branch rules have no check list"))?
            {
                required.insert(
                    check["context"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| reject("required check has no context"))?
                        .to_string(),
                );
            }
        }
    }
    match &protection {
        Ok(doc) => {
            let checks = &doc["required_status_checks"];
            if !checks.is_null() && !checks.is_object() {
                return Err(reject(
                    "branch protection required_status_checks is malformed",
                ));
            }
            for (field, context_field) in [("contexts", false), ("checks", true)] {
                if let Some(values) = checks.get(field) {
                    for item in values
                        .as_array()
                        .ok_or_else(|| reject("branch protection checks are malformed"))?
                    {
                        let value = if context_field {
                            item["context"].as_str()
                        } else {
                            item.as_str()
                        }
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| reject("invalid required check"))?;
                        required.insert(value.to_string());
                    }
                }
            }
        }
        Err(error)
            if error.to_string().to_ascii_lowercase().contains("http 404")
                && error
                    .to_string()
                    .to_ascii_lowercase()
                    .contains("not protected")
                && !required.is_empty() => {}
        Err(_) => return Ok(false),
    }
    if required.is_empty() {
        return Ok(false);
    }
    let mut states: HashMap<String, Vec<String>> = HashMap::new();
    for check in current["statusCheckRollup"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let (name, state) = if check.get("context").is_some() {
            (
                check["context"].as_str(),
                check["state"].as_str().unwrap_or("").to_ascii_uppercase(),
            )
        } else {
            let status = check["status"].as_str().unwrap_or("").to_ascii_uppercase();
            let state = if status == "COMPLETED" {
                check["conclusion"]
                    .as_str()
                    .unwrap_or("")
                    .to_ascii_uppercase()
            } else {
                status
            };
            (check["name"].as_str(), state)
        };
        if let Some(name) = name.filter(|s| !s.is_empty()) {
            states.entry(name.to_string()).or_default().push(state);
        }
    }
    if required.iter().any(|name| {
        states
            .get(name)
            .is_none_or(|values| values.iter().any(|state| state != "SUCCESS"))
    }) {
        return Ok(false);
    }
    let runs = api(shared, &format!("repos/{repo}/actions/workflows/ci.yml/runs?head_sha={head}&event=pull_request&per_page=100"))?;
    let entries = runs["workflow_runs"]
        .as_array()
        .ok_or_else(|| reject("canonical CI runs are unreadable"))?;
    if runs["total_count"].as_u64() != Some(entries.len() as u64) {
        return Err(reject(
            "canonical CI history is incomplete; newest attempt cannot be proven",
        ));
    }
    let mut latest: Option<(&Value, (u64, u64))> = None;
    for run in entries {
        if run["head_sha"].as_str() != Some(head) || run["event"].as_str() != Some("pull_request") {
            return Err(reject(
                "canonical CI response contradicts its head/event filter",
            ));
        }
        let order = (
            run["run_number"].as_u64().filter(|n| *n > 0),
            run["run_attempt"].as_u64().filter(|n| *n > 0),
        );
        let (Some(number), Some(attempt)) = order else {
            return Err(reject(
                "canonical CI run has no server-owned ordering evidence",
            ));
        };
        let order = (number, attempt);
        if let Some((previous, key)) = latest {
            if order == key && previous["id"] != run["id"] {
                return Err(reject("canonical CI attempts have ambiguous ordering"));
            }
        }
        if latest.is_none_or(|(_, key)| order > key) {
            latest = Some((run, order));
        }
    }
    let Some((run, _)) = latest else {
        return Ok(false);
    };
    // Select before checking status: an older success cannot hide a new
    // pending/failed attempt. Those are work-in-progress, not human approvals.
    if run["status"].as_str() != Some("completed") || run["conclusion"].as_str() != Some("success")
    {
        return Ok(false);
    }
    let id = run["id"]
        .as_u64()
        .filter(|id| *id > 0)
        .ok_or_else(|| reject("canonical CI run has no id"))?;
    if !run["html_url"]
        .as_str()
        .and_then(|url| url.strip_prefix("https://github.com/"))
        .and_then(|path| path.strip_suffix(&format!("/actions/runs/{id}")))
        .is_some_and(|run_repo| run_repo.eq_ignore_ascii_case(repo))
    {
        return Err(reject("canonical CI run belongs to another repository"));
    }
    if require_queue {
        require_queue_authority(shared, repo, base, &rules, protection.as_ref().ok())?;
    }
    Ok(true)
}

/// Recheck the external enforcement boundary before postmerge settlement.
pub(super) fn unattended_queue_authority(
    shared: &Shared,
    repo: &str,
    base: &str,
    base_ref: &str,
) -> Result<()> {
    if default_branch(shared, repo)? != base_ref {
        return Err(reject(
            "the authorized target is no longer the repository default",
        ));
    }
    let branch = branch_segment(base_ref);
    let rules = api(shared, &format!("repos/{repo}/rules/branches/{branch}"))?;
    let protection = api(
        shared,
        &format!("repos/{repo}/branches/{branch}/protection"),
    )?;
    require_queue_authority(shared, repo, base, &rules, Some(&protection))
}

fn require_queue_authority(
    shared: &Shared,
    repo: &str,
    base: &str,
    rules: &Value,
    protection: Option<&Value>,
) -> Result<()> {
    let mut queues = rules
        .as_array()
        .ok_or_else(|| reject("applicable branch rules are unreadable"))?
        .iter()
        .filter(|rule| rule["type"] == "merge_queue")
        .peekable();
    if queues.peek().is_none() {
        return Err(reject("automatic landing requires an enforced native merge queue on the default branch; operator setup is required"));
    }
    for rule in queues {
        let id = rule["ruleset_id"]
            .as_u64()
            .filter(|id| *id > 0)
            .ok_or_else(|| reject("merge queue has no authenticated ruleset identity"))?;
        let detail = api(shared, &format!("repos/{repo}/rulesets/{id}"))?;
        if detail["id"].as_u64() != Some(id)
            || detail["enforcement"] != "active"
            || detail["target"] != "branch"
            || detail["current_user_can_bypass"] != "never"
            || !detail["bypass_actors"]
                .as_array()
                .is_some_and(Vec::is_empty)
            || !detail["rules"]
                .as_array()
                .is_some_and(|rules| rules.iter().any(|rule| rule["type"] == "merge_queue"))
        {
            return Err(reject("merge queue enforcement or mutation-identity bypass protection is unproven; operator inspection is required"));
        }
    }
    let protection = protection
        .filter(|doc| doc["enforce_admins"]["enabled"] == true)
        .ok_or_else(|| {
            reject(
                "automatic landing requires required-check protection enforced for administrators",
            )
        })?;
    let checks = protection["required_status_checks"]["checks"]
        .as_array()
        .filter(|checks| !checks.is_empty())
        .ok_or_else(|| {
            reject("required integration checks have no authenticated source bindings")
        })?;
    let mut bound = std::collections::BTreeSet::new();
    for check in checks {
        let context = check["context"]
            .as_str()
            .filter(|context| !context.is_empty())
            .ok_or_else(|| reject("a required integration check has no context"))?;
        // github.com GitHub Actions app, not an arbitrary status publisher.
        if check["app_id"].as_u64() != Some(15368) {
            return Err(reject("required integration checks must be bound to GitHub Actions, not an unspecified status publisher"));
        }
        bound.insert(context);
    }
    if !protection["required_status_checks"]["contexts"]
        .as_array()
        .is_some_and(|contexts| {
            contexts.iter().all(|context| {
                context
                    .as_str()
                    .is_some_and(|context| bound.contains(context))
            })
        })
    {
        return Err(reject(
            "required integration contexts contain an unbound check",
        ));
    }
    for rule in rules.as_array().into_iter().flatten() {
        if rule["type"] == "required_status_checks" {
            let checks = rule["parameters"]["required_status_checks"]
                .as_array()
                .ok_or_else(|| reject("required-check rules are unreadable"))?;
            if checks.iter().any(|check| {
                check["integration_id"].as_u64() != Some(15368)
                    || check["context"]
                        .as_str()
                        .is_none_or(|name| !bound.contains(name))
            }) {
                return Err(reject(
                    "ruleset integration checks lack matching trusted protection bindings",
                ));
            }
        }
    }
    let raw = content_at_base(shared, repo, ".github/workflows/ci.yml", base)
        .ok_or_else(|| reject("trusted integration workflow is unavailable"))?;
    let workflow: serde_yaml::Value = serde_yaml::from_str(&raw)
        .map_err(|_| reject("trusted integration workflow is malformed"))?;
    let merge_group = match &workflow["on"] {
        serde_yaml::Value::Mapping(events) => events.contains_key("merge_group"),
        serde_yaml::Value::Sequence(events) => events
            .iter()
            .any(|event| event.as_str() == Some("merge_group")),
        event => event.as_str() == Some("merge_group"),
    };
    if !merge_group {
        return Err(reject(
            "trusted CI does not handle merge_group; automatic landing is unsupported",
        ));
    }
    // These are the canonical workflow's existing full integration floor,
    // not whatever subset happens to remain in mutable repository settings.
    for gate in ["fmt", "clippy", "test", "build", "ui"] {
        let job = workflow["jobs"].get(gate).ok_or_else(|| {
            reject(format!(
                "trusted CI lacks the supported {gate} integration gate"
            ))
        })?;
        let name = match job.get("name") {
            None => gate,
            Some(name) => name
                .as_str()
                .ok_or_else(|| reject("trusted integration gate name is unreadable"))?,
        };
        if !bound.contains(name) {
            return Err(reject(format!("operator must protect the existing {name} integration gate before automatic landing")));
        }
    }
    if let Some(job) = workflow["jobs"].get("cross-build-required") {
        let name = job["name"].as_str().unwrap_or("cross-build-required");
        if !bound.contains(name) {
            return Err(reject(format!("operator must protect the existing {name} integration aggregate before automatic landing")));
        }
    }
    Ok(())
}

/// An enabled auto-merge request is not proof of admission to the queue.
pub(super) fn unattended_queue_entry(
    shared: &Shared,
    repo: &str,
    number: u64,
    head: &str,
    base: &str,
    base_ref: &str,
) -> Result<Option<String>> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| reject("invalid queue repository"))?;
    let query = format!(
        "query {{ repository(owner:{},name:{}) {{ pullRequest(number:{number}) {{ headRefOid baseRefOid baseRefName state mergeQueueEntry {{ id }} }} }} }}",
        json!(owner), json!(name),
    );
    let response: Value = serde_json::from_str(&gh(
        shared,
        &["api", "graphql", "-f", &format!("query={query}")],
    )?)
    .map_err(|_| reject("native merge queue response is unreadable"))?;
    if response
        .get("errors")
        .is_some_and(|errors| !errors.as_array().is_some_and(Vec::is_empty))
    {
        return Err(reject("native merge queue query failed"));
    }
    let pr = &response["data"]["repository"]["pullRequest"];
    if pr["headRefOid"] != head
        || pr["state"] != "OPEN"
        || pr["baseRefOid"] != base
        || pr["baseRefName"] != base_ref
    {
        return Err(reject(
            "queue observation no longer names the open authorized head",
        ));
    }
    if pr.get("mergeQueueEntry").is_none() {
        return Err(reject("native merge queue entry evidence is missing"));
    }
    if pr["mergeQueueEntry"].is_null() {
        return Ok(None);
    }
    pr["mergeQueueEntry"]["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(|id| Some(id.to_owned()))
        .ok_or_else(|| reject("native merge queue entry has no identity"))
}

pub(super) enum RoutineReadiness {
    NotRoutine,
    Waiting,
    Ready(crate::delivery::ReadyEvidence),
}

/// Recompute routine eligibility from server-owned PR/policy/diff inputs and
/// actual GitHub checks. `Waiting` is retryable; no report or CI is fabricated.
pub(super) fn routine_readiness(
    shared: &Shared,
    issue_id: &str,
    pr_url: &str,
    head: &str,
    outcome_report: &str,
) -> Result<RoutineReadiness> {
    routine_readiness_inner(shared, issue_id, pr_url, head, outcome_report, true)
}

fn routine_readiness_inner(
    shared: &Shared,
    issue_id: &str,
    pr_url: &str,
    head: &str,
    outcome_report: &str,
    confirm: bool,
) -> Result<RoutineReadiness> {
    let answer = match classify(shared, &json!({"issue":issue_id,"pr":pr_url,"head":head})) {
        Ok(answer) => answer,
        Err(_) => return Ok(RoutineReadiness::NotRoutine),
    };
    if answer["requirements"]["class"] != "routine" {
        return Ok(RoutineReadiness::NotRoutine);
    }
    if outcome_report.trim().is_empty() {
        return Ok(RoutineReadiness::NotRoutine);
    }
    let record = crate::delivery::load(&shared.state_dir)?
        .remove(issue_id)
        .ok_or_else(|| reject("delivery record is unavailable"))?;
    if record
        .outcome_report
        .as_deref()
        .is_some_and(|accepted| accepted != outcome_report)
    {
        return Ok(RoutineReadiness::NotRoutine);
    }
    let pm = shared.pm()?;
    let report = issue::task_report::list(&pm.dir.join(&record.project).join(issue_id), issue_id)
        .into_iter()
        .find(|row| row["path"].as_str() == Some(outcome_report));
    let Some(report) = report else {
        return Ok(RoutineReadiness::NotRoutine);
    };
    let filed_at = report["at"].as_str().and_then(issue::time::parse_iso);
    if !report["error"].is_null()
        || report["kind"] != "done"
        || report["agent"].as_str() != Some(record.worker.as_str())
        || report["sha"].as_str() != Some(head)
        || report["pr"].as_str() != Some(pr_url)
        || !filed_at.is_some_and(|at| at >= record.dispatched_at)
    {
        return Ok(RoutineReadiness::NotRoutine);
    }
    let repo = answer["repo"]
        .as_str()
        .ok_or_else(|| reject("missing canonical repo"))?;
    let base = answer["base"]
        .as_str()
        .ok_or_else(|| reject("missing base SHA"))?;
    let base_ref = answer["base_ref"]
        .as_str()
        .ok_or_else(|| reject("missing base branch name"))?;
    let number = answer["pr"]
        .as_u64()
        .ok_or_else(|| reject("missing PR number"))?;
    let branch = branch_segment(base_ref);
    let rules = api(shared, &format!("repos/{repo}/rules/branches/{branch}"))?;
    let protection = api(
        shared,
        &format!("repos/{repo}/branches/{branch}/protection"),
    );
    let mut required = std::collections::BTreeSet::new();
    let rules = rules
        .as_array()
        .ok_or_else(|| reject("branch rules response is not an array"))?;
    for rule in rules {
        if rule["type"] == "required_status_checks" {
            let checks = rule["parameters"]["required_status_checks"]
                .as_array()
                .ok_or_else(|| reject("required branch rules have no check list"))?;
            for check in checks {
                required.insert(
                    check["context"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| reject("required check has no context"))?
                        .to_string(),
                );
            }
        }
    }
    let verified_ruleset_checks = !required.is_empty();
    match protection {
        Ok(doc) => {
            let status_checks = &doc["required_status_checks"];
            if !status_checks.is_null() && !status_checks.is_object() {
                return Err(reject(
                    "branch protection required_status_checks is malformed",
                ));
            }
            for (field, what) in [
                ("contexts", "required context"),
                ("checks", "required check"),
            ] {
                let Some(value) = status_checks.get(field) else {
                    continue;
                };
                let values = value
                    .as_array()
                    .ok_or_else(|| reject(format!("branch protection {what}s are malformed")))?;
                for item in values {
                    let name = if field == "contexts" {
                        item.as_str()
                    } else {
                        item["context"].as_str()
                    }
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| reject(format!("invalid {what}")))?;
                    required.insert(name.to_string());
                }
            }
        }
        Err(error) => {
            let message = error.to_string().to_ascii_lowercase();
            if !(message.contains("http 404")
                && message.contains("not protected")
                && verified_ruleset_checks)
            {
                return Ok(RoutineReadiness::NotRoutine);
            }
        }
    }
    if required.is_empty() {
        return Ok(RoutineReadiness::NotRoutine);
    }
    let view = pr_view(shared, repo, number)?;
    if view["headRefOid"].as_str() != Some(head)
        || view["baseRefOid"].as_str() != Some(base)
        || view["baseRefName"].as_str() != Some(base_ref)
    {
        return Ok(RoutineReadiness::NotRoutine);
    }
    let mut states: HashMap<String, Vec<String>> = HashMap::new();
    for check in view["statusCheckRollup"].as_array().into_iter().flatten() {
        let (name, state) = if check.get("context").is_some() {
            (
                check["context"].as_str(),
                check["state"].as_str().unwrap_or("").to_ascii_uppercase(),
            )
        } else {
            let status = check["status"].as_str().unwrap_or("").to_ascii_uppercase();
            let state = if status == "COMPLETED" {
                check["conclusion"]
                    .as_str()
                    .unwrap_or("")
                    .to_ascii_uppercase()
            } else {
                status
            };
            (check["name"].as_str(), state)
        };
        if let Some(name) = name.filter(|s| !s.is_empty()) {
            states.entry(name.to_string()).or_default().push(state);
        }
    }
    let mut waiting = false;
    for name in &required {
        let Some(values) = states.get(name) else {
            waiting = true;
            continue;
        };
        if values.iter().any(|v| v != "SUCCESS") {
            if values.iter().any(|v| {
                matches!(
                    v.as_str(),
                    "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED"
                )
            }) {
                return Ok(RoutineReadiness::NotRoutine);
            }
            waiting = true;
        }
    }
    let runs = api(shared, &format!("repos/{repo}/actions/workflows/ci.yml/runs?head_sha={head}&event=pull_request&status=success&per_page=100"))?;
    let runs = runs["workflow_runs"]
        .as_array()
        .ok_or_else(|| reject("ci.yml runs response has no workflow_runs"))?;
    let run = runs.iter().find(|run| {
        run["head_sha"].as_str() == Some(head)
            && run["event"].as_str() == Some("pull_request")
            && run["status"].as_str() == Some("completed")
            && run["conclusion"].as_str() == Some("success")
            && run["id"].as_u64().is_some_and(|id| id > 0)
    });
    let Some(run) = run else {
        return Ok(RoutineReadiness::Waiting);
    };
    let run_id = run["id"].as_u64().unwrap();
    let ci_url = format!("https://github.com/{repo}/actions/runs/{run_id}");
    if run["html_url"].as_str() != Some(ci_url.as_str()) {
        return Ok(RoutineReadiness::NotRoutine);
    }
    if waiting {
        return Ok(RoutineReadiness::Waiting);
    }
    let final_answer = classify(shared, &json!({"issue":issue_id,"pr":pr_url,"head":head}))?;
    if final_answer["head"] != answer["head"]
        || final_answer["base"] != answer["base"]
        || final_answer["base_ref"] != answer["base_ref"]
        || final_answer["policy_digest"] != answer["policy_digest"]
        || final_answer["requirements"]["class"] != "routine"
    {
        return Ok(RoutineReadiness::NotRoutine);
    }
    let evidence = crate::delivery::ReadyEvidence {
        repo: repo.to_string(),
        pr: number,
        head: head.to_string(),
        base: base.to_string(),
        base_ref: base_ref.to_string(),
        policy_digest: answer["policy_digest"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        ci_run_id: run_id,
        ci_run_url: ci_url,
        outcome_report: outcome_report.to_string(),
    };
    if confirm {
        return match routine_readiness_inner(shared, issue_id, pr_url, head, outcome_report, false)?
        {
            RoutineReadiness::Ready(current) if current == evidence => {
                Ok(RoutineReadiness::Ready(evidence))
            }
            RoutineReadiness::Waiting => Ok(RoutineReadiness::Waiting),
            _ => Ok(RoutineReadiness::NotRoutine),
        };
    }
    Ok(RoutineReadiness::Ready(evidence))
}

impl Shared {
    pub(super) fn rpc_delivery_requirements(&self, params: &Value) -> Result<Value> {
        let mut answer = classify(self, params)?;
        let issue_id = params["issue"].as_str().unwrap_or_default();
        let pr = params["pr"].as_str().unwrap_or_default();
        let head = params["head"].as_str().unwrap_or_default();
        let report = crate::delivery::load(&self.state_dir)?
            .get(issue_id)
            .and_then(|record| record.outcome_report.clone());
        let readiness = match report {
            Some(report) => match routine_readiness(self, issue_id, pr, head, &report)
                .unwrap_or(RoutineReadiness::NotRoutine)
            {
                RoutineReadiness::Ready(evidence) => Some(evidence),
                RoutineReadiness::NotRoutine | RoutineReadiness::Waiting => None,
            },
            None => None,
        };
        answer["readiness"] = json!(readiness);
        Ok(answer)
    }
}
