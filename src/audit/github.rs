//! Github.

use super::*;

/// The `gh` payload: merged-PR rows plus per-sha commit statuses.
#[derive(Default)]
pub(super) struct Gh {
    pub(super) prs: HashMap<u64, Value>,
    pub(super) statuses: HashMap<String, Value>,
    pub(super) error: Option<String>,
    /// `owner/repo` — resolved once at startup; per-row status calls
    /// reuse it instead of re-running `git remote get-url` per head.
    pub(super) slug: Option<String>,
    /// `--merge-report` set: every `gh` lookup resolves from the
    /// fixture — a miss is `unknown`, never a live call.
    pub(super) fixture: bool,
}

/// `owner/name` of the checkout's github.com `origin` — the scope an
/// approval record binds to (also used by `cadence audit approve`).
pub fn origin_slug(repo: &Path) -> Option<String> {
    let url = git(repo, &["remote".into(), "get-url".into(), "origin".into()]).ok()?;
    let norm = project::normalize_remote(url.trim());
    norm.strip_prefix("github.com/").map(str::to_string)
}

pub(super) fn github(repo: &Path) -> Result<Gh> {
    let Some(slug) = origin_slug(repo) else {
        return Ok(Gh {
            error: Some("no github.com origin remote".into()),
            ..Default::default()
        });
    };
    let mut gh = Gh {
        slug: Some(slug.clone()),
        ..Default::default()
    };
    let prs = gh_text(&[
        "pr".into(),
        "list".into(),
        "--repo".into(),
        slug.clone(),
        "--state".into(),
        "merged".into(),
        // One page covers far more than any sane audit window — a PR
        // absent from this list is a data gap, not a truncation.
        "--limit".into(),
        "1000".into(),
        "--json".into(),
        "number,title,mergedBy,mergeCommit,headRefOid,mergedAt".into(),
    ]);
    match prs.and_then(|t| {
        serde_json::from_str::<Value>(&t).map_err(|e| format!("gh pr list: unreadable ({e})"))
    }) {
        Ok(Value::Array(list)) => {
            for pr in list {
                if let Some(n) = pr["number"].as_u64() {
                    gh.prs.insert(n, pr);
                }
            }
        }
        Ok(_) => gh.error = Some("gh pr list: not a JSON array".into()),
        Err(e) => gh.error = Some(e),
    }
    Ok(gh)
}

/// Load a `--merge-report` fixture: `{"prs":[…], "statuses":{"<sha>":{…}}}`.
/// A bare array is treated as the `prs` list. Errors are hard — a bad
/// fixture must never fall back to live `gh`.
pub(super) fn merge_fixture(path: &Path) -> Result<Gh> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::rejected(format!("--merge-report {}: {e}", path.display())))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| {
        Error::rejected(format!("--merge-report {}: bad JSON ({e})", path.display()))
    })?;
    let mut gh = Gh::default();
    let prs = if v.is_array() { &v } else { &v["prs"] };
    for pr in prs.as_array().cloned().unwrap_or_default() {
        if let Some(n) = pr["number"].as_u64() {
            gh.prs.insert(n, pr);
        }
    }
    for (sha, st) in v["statuses"].as_object().cloned().unwrap_or_default() {
        gh.statuses.insert(sha, st);
    }
    gh.fixture = true;
    Ok(gh)
}

pub(super) fn gh_text(args: &[String]) -> std::result::Result<String, String> {
    let mut cmd = Command::new("gh");
    cmd.args(args);
    let out = crate::proc::run_bounded(&mut cmd, GH_TIMEOUT).map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "gh {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// The `qa-verdict` context on one commit status payload: its state,
/// the GitHub `creator.login` that posted it, and `created_at`. Status
/// contexts report `success`, check runs `SUCCESS` — normalized to the
/// uppercase StatusCheckRollup spelling.
pub(super) fn qa_verdict_state(status: &Value) -> Option<(String, Option<String>, Option<f64>)> {
    for ctx in status["statuses"].as_array().into_iter().flatten() {
        if ctx["context"].as_str() == Some("qa-verdict") {
            let Some(state) = ctx["state"].as_str() else {
                continue;
            };
            let at = ctx["created_at"].as_str().and_then(parse_iso);
            return Some((
                state.to_uppercase(),
                ctx["creator"]["login"].as_str().map(str::to_string),
                at.map(|t| t as f64),
            ));
        }
    }
    // Check runs arrive under `check_runs` on some endpoints.
    for run in status["check_runs"].as_array().into_iter().flatten() {
        if run["name"].as_str() == Some("qa-verdict") {
            let Some(state) = run["conclusion"]
                .as_str()
                .or_else(|| run["status"].as_str())
            else {
                continue;
            };
            let at = run["started_at"]
                .as_str()
                .or_else(|| run["completed_at"].as_str())
                .and_then(parse_iso);
            return Some((
                state.to_uppercase(),
                run["user"]["login"]
                    .as_str()
                    .or_else(|| run["app"]["slug"].as_str())
                    .map(str::to_string),
                at.map(|t| t as f64),
            ));
        }
    }
    None
}

/// PR metadata from the one `gh pr list` call — merger, landed head,
/// mergedAt, title. No network here; the per-sha status fetch happens
/// post-filter in `enrich_status`.
pub(super) fn enrich_gh_pr(gh: &Gh, row: &mut Row) {
    let Some(n) = row.pr else { return };
    let Some(pr) = gh.prs.get(&n) else {
        let reason = gh
            .error
            .as_ref()
            .map(|e| format!("gh unavailable: {e}"))
            .unwrap_or_else(|| format!("no merged PR #{n} in gh pr list"));
        row.unknowns.push(("merged_by".into(), reason.clone()));
        row.evidence_gaps.push(reason);
        return;
    };
    row.title = pr["title"].as_str().unwrap_or(&row.title).to_string();
    row.merger = pr["mergedBy"]["login"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    row.landed_head = pr["headRefOid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    // gh's mergeCommit must be the commit git log shows — a mismatch
    // means the PR number in the subject points at a different merge.
    if let Some(mc) = pr["mergeCommit"]["oid"].as_str() {
        if mc != row.merge_sha {
            row.unknowns.push((
                "merge_sha".into(),
                format!("gh mergeCommit {mc} != git log {}", row.merge_sha),
            ));
        }
    }
    if let Some(m) = pr["mergedAt"].as_str() {
        row.merged_at = parse_iso(m).map(|t| t as f64).or(row.merged_at);
    }
    if row.merger.is_none() {
        row.unknowns
            .push(("merged_by".into(), "gh reports no mergedBy".into()));
    }
    if row.landed_head.is_none() {
        row.unknowns
            .push(("landed_head".into(), "gh reports no headRefOid".into()));
        // No head → the status channel cannot be queried.
        row.evidence_gaps.push("gh reports no headRefOid".into());
    }
}

/// The per-row network call — `qa-verdict` status on the exact landed
/// head. Runs only for rows that survive the filters, so a narrow
/// `--since`/`--class`/`--limit` audit spends one API call per shown
/// merge, not per in-window commit. A fetched answer is kept in
/// `gh.statuses`, the evidence `fleet_logins` decides from.
pub(super) fn enrich_status(gh: &mut Gh, row: &mut Row) {
    // Direct pushes have no PR head — but the merge commit itself can
    // still carry a `qa-verdict` status, so it is queried too.
    let head = row
        .landed_head
        .clone()
        .unwrap_or_else(|| row.merge_sha.clone());
    if row.pr.is_some() && row.landed_head.is_none() {
        return; // gap already recorded by enrich_gh_pr
    }
    let status = match gh.statuses.get(head.as_str()) {
        Some(s) => Some(Ok(s.clone())),
        // Fixture mode never falls back to a live call; a missing key
        // is the fixture saying "no statuses on this head".
        None if gh.fixture => None,
        None => gh.slug.clone().map(|s| {
            let fetched = gh_status(&s, head.as_str());
            if let Ok(v) = &fetched {
                gh.statuses.insert(head.clone(), v.clone());
            }
            fetched
        }),
    };
    match status {
        Some(Err(e)) => {
            row.evidence_gaps.push(format!("status fetch failed: {e}"));
            row.unknowns.push((
                "qa_verdict_status".into(),
                format!("status fetch failed on {head}: {e}"),
            ));
        }
        Some(Ok(v)) => match qa_verdict_state(&v) {
            Some((state, creator, at)) => {
                row.qa_verdict_status = Some(state);
                row.qa_verdict_creator = creator;
                row.status_post_hoc = match (at, row.merged_at) {
                    (Some(at), Some(m)) => at > m,
                    _ => false,
                };
            }
            None => row.unknowns.push((
                "qa_verdict_status".into(),
                format!("no qa-verdict status on {head}"),
            )),
        },
        None => {}
    }
}

/// The `qa-verdict` evidence for one head. Two endpoints, in order:
/// `statuses/{ref}` is the only one that returns `creator.login` (the
/// combined `/status` payload omits it), and the combined endpoint is
/// the only one that reports check runs — the fallback when a
/// qa-verdict was posted as a check run instead of a status.
pub(super) fn gh_status(slug: &str, sha: &str) -> std::result::Result<Value, String> {
    let list = gh_text(&[
        "api".into(),
        format!("repos/{slug}/statuses/{sha}?per_page=100"),
    ])?;
    let list: Value =
        serde_json::from_str(&list).map_err(|e| format!("gh api statuses: unreadable ({e})"))?;
    let wrapped = json!({"statuses": list});
    if qa_verdict_state(&wrapped).is_some() {
        return Ok(wrapped);
    }
    let combined = gh_text(&[
        "api".into(),
        format!("repos/{slug}/commits/{sha}/status?per_page=100"),
    ])?;
    let combined: Value =
        serde_json::from_str(&combined).map_err(|e| format!("gh api status: unreadable ({e})"))?;
    if qa_verdict_state(&combined).is_some() {
        return Ok(combined);
    }
    // Both endpoints answered and neither reports a qa-verdict — that
    // is an answer, not a gap.
    Ok(wrapped)
}
