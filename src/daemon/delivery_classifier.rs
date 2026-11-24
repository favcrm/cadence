//! Shared read-only delivery requirements classification.

use serde_json::{json, Value};
use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::issue::{self, delivery_policy, delivery_requirements as requirements};

fn reject(why: impl std::fmt::Display) -> Error {
    Error::rejected(format!("delivery requirements: {why}"))
}

/// One `gh` read. The daemon runs the binary it resolved at boot; a
/// hosted-org CLI runs the caller's own `gh` (CAD-1946).
pub trait GhRead {
    fn gh_read(&self, args: &[&str]) -> Result<String>;
}

/// The tracker-side inputs of [`classify_from`]. The daemon reads them
/// from its PM and approval store; a hosted-org CLI reads them from the
/// hosted tracker (CAD-1946). Either way the classification below is the
/// one implementation — only where the inputs come from differs.
pub trait RequirementInputs: GhRead {
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

/// Read-only queue and integration enforcement proof against the pinned base.
pub fn require_queue_authority<G: GhRead + ?Sized>(
    shared: &G,
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
    let raw = content_at_ref(shared, repo, ".github/workflows/ci.yml", base)
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
    for gate in ["fmt", "clippy", "build", "ui"] {
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

/// Exercise the production queue guard with isolated GitHub responses.
#[cfg(all(debug_assertions, feature = "test-seam"))]
pub fn queue_authority_acceptance(
    repo: &str,
    base: &str,
    rules: &Value,
    protection: Option<&Value>,
    read: impl Fn(&[&str]) -> Result<String>,
) -> Result<()> {
    struct Reader<F>(F);
    impl<F: Fn(&[&str]) -> Result<String>> GhRead for Reader<F> {
        fn gh_read(&self, args: &[&str]) -> Result<String> {
            (self.0)(args)
        }
    }
    require_queue_authority(&Reader(read), repo, base, rules, protection)
}

pub fn gh<G: GhRead + ?Sized>(source: &G, args: &[&str]) -> Result<String> {
    source.gh_read(args)
}

pub fn api<G: GhRead + ?Sized>(source: &G, endpoint: &str) -> Result<Value> {
    let text = gh(source, &["api", endpoint])?;
    serde_json::from_str(&text).map_err(|e| reject(format!("unreadable GitHub response: {e}")))
}

pub fn pr_view<G: GhRead + ?Sized>(shared: &G, slug: &str, number: u64) -> Result<Value> {
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

pub fn default_branch<G: GhRead + ?Sized>(shared: &G, repo: &str) -> Result<String> {
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
pub fn branch_segment(branch: &str) -> String {
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

fn required_str<'a>(params: &'a Value, field: &str) -> Result<&'a str> {
    params
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::rejected(format!("Missing required parameter '{field}'")))
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

/// `(mode, type, sha)` of one tree entry.
type TreeEntry = (String, String, String);

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
        let sha = entry["sha"]
            .as_str()
            .filter(|s| valid_sha(s))
            .ok_or_else(|| reject(format!("tree entry {path} has no valid sha")))?;
        if modes
            .insert(
                path.to_string(),
                (mode.to_string(), kind.to_string(), sha.to_string()),
            )
            .is_some()
        {
            return Err(reject(format!("GitHub tree repeats path {path}")));
        }
    }
    Ok(modes)
}

fn changed_mode(tree: &HashMap<String, TreeEntry>, path: &str) -> Result<String> {
    let (mode, kind, _) = tree
        .get(path)
        .ok_or_else(|| reject(format!("tree mode unavailable for {path}")))?;
    if kind == "tree" {
        return Err(reject(format!("changed path {path} resolves to a tree")));
    }
    Ok(mode.clone())
}

/// The tip commit of `branch`, read once per evaluation. An unreadable or
/// malformed answer refuses, so the merge-base anchor is never empty.
fn branch_tip<G: GhRead + ?Sized>(shared: &G, repo: &str, branch: &str) -> Result<String> {
    let name = branch
        .split('/')
        .map(branch_segment)
        .collect::<Vec<_>>()
        .join("/");
    let reference = api(shared, &format!("repos/{repo}/git/ref/heads/{name}"))?;
    reference["object"]["sha"]
        .as_str()
        .filter(|s| valid_sha(s))
        .map(str::to_string)
        .ok_or_else(|| reject(format!("cannot read the live tip of {branch:?}")))
}

/// Proof that a compare file list is the whole change between two trees:
/// the paths whose entry differs (or exists on one side only) must be exactly
/// the listed filenames plus the old names of renames. GitHub's `changedFiles`
/// counts from its own, possibly stale, base and cannot vouch for a set taken
/// from a different merge base.
fn changed_set_matches_trees(
    base: &HashMap<String, TreeEntry>,
    head: &HashMap<String, TreeEntry>,
    files: &[Value],
) -> bool {
    use std::collections::BTreeSet;
    let blobs = |tree: &HashMap<String, TreeEntry>| -> HashMap<String, TreeEntry> {
        tree.iter()
            .filter(|(_, (_, kind, _))| kind != "tree")
            .map(|(path, entry)| (path.clone(), entry.clone()))
            .collect()
    };
    let (base, head) = (blobs(base), blobs(head));
    let differs: BTreeSet<&str> = base
        .keys()
        .chain(head.keys())
        .filter(|path| base.get(*path) != head.get(*path))
        .map(String::as_str)
        .collect();
    let mut listed = BTreeSet::new();
    for file in files {
        let Some(path) = file["filename"].as_str() else {
            return false;
        };
        listed.insert(path);
        if file["status"].as_str() == Some("renamed") {
            match file["previous_filename"].as_str() {
                Some(old) => listed.insert(old),
                None => return false,
            };
        }
    }
    differs == listed
}

pub fn content_at_ref<G: GhRead + ?Sized>(
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

/// The requirements classification over any [`RequirementInputs`]: the
/// request binding, the project/remote check, the live PR and anchor-pinned
/// trusted lists, [`requirements::classify`], and the drift re-read.
pub fn classify_from<I: RequirementInputs + ?Sized>(
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

    // CAD-2100: GitHub's `baseRefOid` can stay older than the real merge base
    // after the default branch is merged into the PR, so a main-based PR is
    // read from the merge base with the default branch's live tip, resolved
    // once here and used for the compare and the path lists alike. Any other
    // base keeps the PR's base exactly.
    let main_based = base_ref == default;
    let anchor = if main_based {
        branch_tip(shared, &repo, &default)?
    } else {
        base.clone()
    };
    let comparison = api(shared, &format!("repos/{repo}/compare/{anchor}...{head}"))?;
    // CAD-2117: GitHub's compare answers ONE merge base. When the two commits
    // have several (a criss-cross merge) that answer is not provably the right
    // one and the API cannot enumerate the rest, so this path cannot detect it.
    // `scripts/enqueue-reviewed` runs `git merge-base --all` and refuses on more
    // than one; the daemon's own landing path (unattended_landing) does not go
    // through that script, so it relies on main staying free of merge commits.
    let merge_base = comparison["merge_base_commit"]["sha"]
        .as_str()
        .filter(|s| valid_sha(s))
        .ok_or_else(|| reject("comparison has no valid full merge-base SHA"))?
        .to_string();
    let files = comparison["files"]
        .as_array()
        .ok_or_else(|| reject("comparison has no complete file list"))?;
    // A main-based PR is not compared with GitHub's count (it counts from its
    // own, possibly stale, base); its list is proven against the trees below.
    if files.len() >= 300 {
        return Err(reject("GitHub comparison file list is truncated"));
    }
    if !main_based && files.len() as u64 != changed_files {
        return Err(reject(
            "GitHub comparison file list differs from the PR file count",
        ));
    }
    let base_modes = tree_modes(shared, &repo, &merge_base)?;
    let head_modes = tree_modes(shared, &repo, head)?;
    if main_based && !changed_set_matches_trees(&base_modes, &head_modes, files) {
        return Err(reject(
            "GitHub comparison file list does not match the trees of the merge base and the head",
        ));
    }
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
        if status == "R" {
            // CAD-2117: a rename is a delete of the old path plus an add of the
            // new one (as `git diff --no-renames` reports it, which the script
            // uses), so the path classes see BOTH ends. Recording only the new
            // path let `scripts/x` renamed out of `scripts/` read as outside it.
            changes.push(requirements::change("D", &old_mode, "000000", old_path));
            changes.push(requirements::change("A", "000000", &new_mode, path));
        } else {
            changes.push(requirements::change(status, &old_mode, &new_mode, path));
        }
    }

    // The path lists move with the diff: read at the same anchor.
    let risk_paths = content_at_ref(shared, &repo, "docs/roles/risk-paths.toml", &anchor);
    let one_review = content_at_ref(shared, &repo, "docs/roles/one-review-paths.toml", &anchor);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(entries: &[(&str, char)]) -> HashMap<String, TreeEntry> {
        entries
            .iter()
            .map(|(path, sha)| {
                (
                    path.to_string(),
                    ("100644".into(), "blob".into(), sha.to_string().repeat(40)),
                )
            })
            .collect()
    }

    // CAD-2100: the file list from the merge base with main's tip is proven
    // complete by the two trees, not by GitHub's count from its own base.
    #[test]
    fn a_file_list_must_be_exactly_what_differs_between_the_trees() {
        let base = tree(&[("a.rs", 'a'), ("b.rs", 'b'), ("old.rs", 'c')]);
        let head = tree(&[("a.rs", 'a'), ("b.rs", 'd'), ("new.rs", 'c')]);
        let modified = json!({"filename": "b.rs", "status": "modified"});
        let renamed =
            json!({"filename": "new.rs", "status": "renamed", "previous_filename": "old.rs"});
        assert!(changed_set_matches_trees(
            &base,
            &head,
            &[modified.clone(), renamed.clone()]
        ));
        // a missing entry (a revert the list hides) and an extra one both refuse
        assert!(!changed_set_matches_trees(
            &base,
            &head,
            std::slice::from_ref(&renamed)
        ));
        let extra = json!({"filename": "a.rs", "status": "modified"});
        assert!(!changed_set_matches_trees(
            &base,
            &head,
            &[modified, renamed, extra]
        ));
    }

    #[test]
    fn a_rename_without_its_old_name_refuses() {
        let base = tree(&[("old.rs", 'c')]);
        let head = tree(&[("new.rs", 'c')]);
        let renamed = json!({"filename": "new.rs", "status": "renamed"});
        assert!(!changed_set_matches_trees(&base, &head, &[renamed]));
    }
}
