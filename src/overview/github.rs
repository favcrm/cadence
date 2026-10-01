//! The GitHub/provider seam behind the overview: `git`/`gh` shell-outs
//! ([`git_text`], [`gh_text`]), deploy-drift derivation
//! ([`default_ref`], [`compute_drift`]), the cached per-repo fetch
//! ([`gh_repo`]), and the build-repo match ([`build_repo_match`]) —
//! moved verbatim out of `src/overview.rs` (CAD-953).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use crate::issue::project;
use crate::proc::run_bounded;

use super::main_ci::gh_main_ci;
use super::{pr_number, BUILD_REMOTE, BUILD_ROOT};

/// `gh` calls are bounded by this; the caller's own wait comes after.
pub(super) const GH_TIMEOUT: Duration = Duration::from_secs(20);
const GIT_TIMEOUT: Duration = Duration::from_secs(15);

/// Drift subjects carried in the view per repo.
const DRIFT_SUBJECTS: usize = 20;

pub(super) fn git_text(repo: &Path, args: &[String]) -> Result<String, String> {
    let out = run_bounded(
        Command::new("git").arg("-C").arg(repo).args(args),
        GIT_TIMEOUT,
    )
    .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub(super) fn gh_text(args: &[String]) -> Result<String, String> {
    let out =
        run_bounded(Command::new("gh").args(args), GH_TIMEOUT).map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("gh {}: {}", args.join(" "), err));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// The default-branch ref to count drift against: `origin/HEAD`'s
/// target when the symref exists, else origin/main, origin/master,
/// then the local names.
pub(super) fn default_ref(repo: &Path) -> Result<String, String> {
    if let Ok(sym) = git_text(
        repo,
        &[
            "rev-parse".into(),
            "--abbrev-ref".into(),
            "origin/HEAD".into(),
        ],
    ) {
        let sym = sym.trim();
        if sym.starts_with("origin/") {
            return Ok(sym.to_string());
        }
    }
    for cand in ["origin/main", "origin/master", "main", "master"] {
        if git_text(repo, &["rev-parse".into(), "--verify".into(), cand.into()])
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            return Ok(cand.to_string());
        }
    }
    Err("no default branch ref".to_string())
}

/// Commits on the repo's default branch after `build_commit`: the
/// count plus subjects bounded to [`DRIFT_SUBJECTS`], each with the
/// squash-merged `(#n)` parsed out. `build_commit` "unknown" — or one
/// git cannot place — is `known:false`, "cannot tell", never zero.
pub(super) fn compute_drift(repo: &Path, build_commit: &str) -> Value {
    let mut base = json!({"known": false, "repo": repo});
    if build_commit == "unknown" || build_commit.is_empty() {
        base["reason"] = json!("build commit unknown — cannot tell");
        return base;
    }
    let dref = match default_ref(repo) {
        Ok(r) => r,
        Err(e) => {
            base["reason"] = json!(format!("cannot tell — {e}"));
            return base;
        }
    };
    let range = format!("{build_commit}..{dref}");
    let count = match git_text(repo, &["rev-list".into(), "--count".into(), range.clone()]) {
        Ok(c) => match c.trim().parse::<i64>() {
            Ok(n) => n,
            Err(_) => {
                base["reason"] = json!("cannot tell — unreadable count");
                return base;
            }
        },
        Err(e) => {
            base["reason"] = json!(format!("cannot tell — {e}"));
            return base;
        }
    };
    let subjects = git_text(
        repo,
        &[
            "log".into(),
            format!("-{DRIFT_SUBJECTS}"),
            "--format=%s".into(),
            range,
        ],
    )
    .unwrap_or_default();
    let commits: Vec<Value> = subjects
        .lines()
        .map(|s| json!({"subject": s, "pr": pr_number(s)}))
        .collect();
    json!({
        "known": true, "repo": repo, "ref": dref,
        "build_commit": build_commit,
        "count": count, "commits": commits,
    })
}

/// `gh pr list` plus the default branch's `ci.yml` push runs for one
/// repo slug, fetched concurrently into one cache entry (CAD-267). The
/// legacy commit-status API is not read here: Actions reports check
/// runs, so `commits/HEAD/status` stays `pending, total_count: 0` on a
/// red main. `qa-verdict` still rides the PR rollup. A failing runs
/// fetch never costs the PR rows — it lands as `main_ci.error`.
pub(super) fn gh_repo(slug: &str) -> Result<Value, String> {
    let (prs, main_ci) = std::thread::scope(|s| {
        let ci = s.spawn(|| gh_main_ci(slug));
        let prs = gh_prs(slug);
        let main_ci = ci
            .join()
            .unwrap_or_else(|_| json!({"error": "ci runs fetch panicked"}));
        (prs, main_ci)
    });
    Ok(json!({"prs": prs?, "main_ci": main_ci}))
}

pub(super) fn gh_prs(slug: &str) -> Result<Value, String> {
    let prs = gh_text(&[
        "pr".into(),
        "list".into(),
        "--repo".into(),
        slug.into(),
        "--state".into(),
        "open".into(),
        "--limit".into(),
        "50".into(),
        "--json".into(),
        "number,title,url,headRefOid,headRefName,updatedAt,statusCheckRollup".into(),
    ])?;
    serde_json::from_str::<Value>(&prs).map_err(|e| format!("gh pr list: unreadable ({e})"))
}

/// A branch name safe to put in a query string and a git revision
/// unquoted.
pub(super) fn is_plain_ref(b: &str) -> bool {
    !b.is_empty()
        && !b.starts_with('-')
        && b.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
}

// ---------- default-branch CI (CAD-267) ----------

/// The tracker project matching the repo this binary was built from:
/// remote first (normalised both sides), then declared path against
/// the build checkout. Returns the project key and the local clone to
/// walk — the declared `path`, else the build root itself.
pub(super) fn build_repo_match(projects: &[project::Project]) -> Option<(String, PathBuf)> {
    let build_remote = if BUILD_REMOTE == "unknown" {
        None
    } else {
        Some(project::normalize_remote(BUILD_REMOTE))
    };
    let build_root = if BUILD_ROOT == "unknown" {
        None
    } else {
        Some(
            PathBuf::from(BUILD_ROOT)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(BUILD_ROOT)),
        )
    };
    for p in projects {
        for r in &p.repos {
            let remote_hit = match (&build_remote, &r.remote) {
                (Some(want), Some(have)) => project::normalize_remote(have) == *want,
                _ => false,
            };
            let declared = r.path.as_deref().map(project::expand_home);
            let path_hit = match (&build_root, &declared) {
                (Some(want), Some(have)) => {
                    have.canonicalize().unwrap_or_else(|_| have.clone()) == *want
                }
                _ => false,
            };
            if remote_hit || path_hit {
                let repo = declared
                    .clone()
                    .or_else(|| build_root.clone())
                    .unwrap_or_else(|| PathBuf::from("."));
                return Some((p.key.clone(), repo));
            }
        }
    }
    None
}
