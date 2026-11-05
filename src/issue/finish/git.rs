//! Git and merge-evidence probes for `issue finish`.

use super::*;

pub(crate) const GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// `git -C dir <args>`; non-zero exit is a rejected error carrying
/// stderr, like `issue::git`. `pub(crate)`: `issue reconcile` runs
/// the same bounded probes for its merge classification.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = git_out(dir, args, &[])?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Resolve HEAD's branch; symbolic-ref exit 1 is Git's detached-HEAD result.
pub(crate) fn git_branch(dir: &Path) -> Result<Option<String>> {
    let args = ["symbolic-ref", "--quiet", "--short", "HEAD"];
    let out = git_out(dir, &args, &[])?;
    match out.status.code() {
        Some(0) => {
            let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if branch.is_empty() {
                Err(Error::rejected(format!(
                    "git {} returned an empty branch in {}",
                    args.join(" "),
                    dir.display()
                )))
            } else {
                Ok(Some(branch))
            }
        }
        Some(1) => Ok(None),
        code => Err(Error::rejected(format!(
            "git {} failed in {} with status {:?}: {}",
            args.join(" "),
            dir.display(),
            code,
            String::from_utf8_lossy(&out.stderr).trim()
        ))),
    }
}

/// Raw bounded git output for probes that need stdout on any exit.
pub(crate) fn git_out(dir: &Path, args: &[&str], env: &[(&str, &Path)]) -> Result<Output> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    run_bounded(&mut cmd, GIT_TIMEOUT).map_err(|e| match e {
        BoundedError::Spawn(_) => Error::rejected("`git` is required and was not found on PATH"),
        e => Error::rejected(format!("git {} in {}: {e}", args.join(" "), dir.display())),
    })
}

/// The repo's default ref for merge checks: `origin/HEAD` when set,
/// else the main checkout's current branch.
pub(crate) fn default_ref(root: &Path) -> Option<String> {
    if let Ok(origin_head) = git(
        root,
        &["symbolic-ref", "refs/remotes/origin/HEAD", "--short"],
    ) {
        return Some(origin_head);
    }
    git(root, &["symbolic-ref", "--short", "HEAD"]).ok()
}

pub(crate) fn ancestor(root: &Path, branch: &str, into: &str) -> bool {
    git(root, &["merge-base", "--is-ancestor", branch, into]).is_ok()
}

/// Why `tip` survives deletion, checked in order: plain ancestry of
/// the default ref, or a merged GitHub PR on origin's own repository
/// whose recorded head is exactly `tip` (GitHub keeps
/// `refs/pull/N/head` after a squash). Both are pinned to the SHA, so
/// the evidence and the commit a `branch -D` deletes cannot disagree.
/// CAD-2181: content inference (`git cherry`, a reverse-applying
/// diff) is NOT evidence here — repeated lines, mode-only changes,
/// merge commits, net-zero edits and merge drivers each fooled it
/// into deleting the only copy of unpushed work. That guess lives in
/// [`content_hint`], for messages and status only. `pr_head` is the
/// head branch name the `pr` arm filters on. The first match wins and
/// is reported as `merged_by`.
pub(crate) fn merge_rule(
    root: &Path,
    pr_head: &str,
    tip: &str,
    into: &str,
) -> Option<&'static str> {
    if ancestor(root, tip, into) {
        return Some("ancestry");
    }
    if pr_merged(root, pr_head, tip, into) {
        return Some("pr");
    }
    None
}

/// Strict evidence, else the content guess. For callers that only
/// report or change tracker status and never delete: `issue
/// reconcile` classification and the dirty-lane label. Never use it
/// to authorize a delete.
pub(crate) fn merged_claim(
    root: &Path,
    pr_head: &str,
    tip: &str,
    into: &str,
) -> Option<&'static str> {
    merge_rule(root, pr_head, tip, into).or_else(|| content_hint(root, tip, into))
}

/// Content looks landed on `into`: every commit up to `tip` has a
/// patch-equivalent upstream (`git cherry`), or the combined diff
/// reverse-applies (a squash merge). A guess, forgeable by content
/// shape — a message, never a delete.
pub(crate) fn content_hint(root: &Path, tip: &str, into: &str) -> Option<&'static str> {
    // `+` = a commit with no patch-equivalent upstream.
    if let Ok(marks) = git(root, &["cherry", into, tip]) {
        if !marks.lines().any(|l| l.starts_with('+')) {
            return Some("cherry");
        }
    }
    patch_applied(root, tip, into).then_some("patch")
}

/// Squash-merge test: if the branch's combined diff against its
/// merge-base reverse-applies cleanly onto `into`'s tree, `into`
/// already holds that state. Runs against a temporary index — no
/// worktree is ever touched.
pub(crate) fn patch_applied(root: &Path, tip: &str, into: &str) -> bool {
    let Ok(base) = git(root, &["merge-base", into, tip]) else {
        return false;
    };
    // Raw stdout bytes, not `git()`'s trimmed string — `git apply`
    // rejects a patch whose final newline was stripped as corrupt.
    let diff = match git_out(root, &["diff", "--binary", &base, tip], &[]) {
        Ok(o) if o.status.success() => o.stdout,
        _ => return false,
    };
    // An empty diff means the branch contributes nothing — deleting
    // it loses no work.
    if diff.is_empty() {
        return true;
    }
    let Ok(tmp) = TempDir::new() else {
        return false;
    };
    let index = tmp.path().join("index");
    let env = [("GIT_INDEX_FILE", index.as_path())];
    // git_out is Ok on any exit status — the check is the exit code.
    let seeded = git_out(root, &["read-tree", into], &env)
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !seeded {
        return false;
    }
    let patch = tmp.path().join("patch");
    if std::fs::write(&patch, &diff).is_err() {
        return false;
    }
    git_out(
        root,
        &[
            "apply",
            "--check",
            "-R",
            "--cached",
            patch.to_str().unwrap(),
        ],
        &env,
    )
    .map(|o| o.status.success())
    .unwrap_or(false)
}

/// A merged GitHub PR counts only when its recorded head commit is
/// exactly `tip` — the name alone is not evidence (a reused branch
/// name), and a head that merely descends from or precedes `tip` does
/// not prove `tip` itself was merged. The lookup is pinned to
/// origin's own GitHub repository (`-R <slug>`), never gh's default
/// repo, and the PR's `baseRefName` must be the default ref. Any
/// failure falls through: gh is a hint, never the only authority.
pub(crate) fn pr_merged(root: &Path, branch: &str, tip: &str, into: &str) -> bool {
    let Ok(url) = git(root, &["remote", "get-url", "origin"]) else {
        return false;
    };
    let Some(slug) = github_slug(&url) else {
        return false;
    };
    // `into` may be a remote-tracking name ("origin/main") — the PR
    // base is a bare branch name.
    let base_name = into.strip_prefix("origin/").unwrap_or(into);
    let mut cmd = Command::new("gh");
    cmd.args([
        "-R",
        &slug,
        "pr",
        "list",
        "--head",
        branch,
        "--state",
        "merged",
        "--json",
        "number,headRefOid,baseRefName,mergedAt",
        "--limit",
        "10",
    ])
    .current_dir(root);
    let Ok(out) = crate::gh_guard::run(&mut cmd, Duration::from_secs(10)) else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let Ok(prs) = serde_json::from_slice::<Value>(&out.stdout) else {
        return false;
    };
    prs.as_array().is_some_and(|prs| {
        prs.iter().any(|pr| {
            pr["headRefOid"].as_str() == Some(tip)
                && pr["baseRefName"].as_str() == Some(base_name)
                && pr["mergedAt"].as_str().is_some_and(|t| !t.is_empty())
        })
    })
}

/// `owner/repo` of an `origin` url on exactly `github.com`
/// (`git@github.com:o/r`, `ssh://[user@]github.com/o/r`,
/// `https://[user@]github.com/o/r`, with or without `.git`); `None`
/// for any other host, including `github.com.evil.example` and
/// `corp.test/github.com/o/r`. TODO(CAD-2178/#1645): that PR adds
/// `github_slug` in `hosted.rs`; keep one copy once both land.
pub(crate) fn github_slug(url: &str) -> Option<String> {
    let url = url.trim();
    let path = if let Some(rest) = url.strip_prefix("git@github.com:") {
        rest
    } else {
        let (scheme, rest) = url.split_once("://")?;
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = host.split_once(':').map_or(host, |(h, _)| h);
        if !matches!(scheme, "https" | "ssh") || !host.eq_ignore_ascii_case("github.com") {
            return None;
        }
        path
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    let ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    (parts.next().is_none() && ok(owner) && ok(repo)).then(|| format!("{owner}/{repo}"))
}
