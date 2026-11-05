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

/// Why `tip` counts as merged into `into`, checked in order: plain
/// ancestry; every commit up to `tip` already applied upstream
/// (`git cherry` — the rebase/cherry-pick case); the combined diff
/// up to `tip` reverse-applying onto `into` (a squash merge); or a
/// merged GitHub PR whose recorded head covers `tip`. All four are
/// pinned to the SHA, never re-resolved by name — the evidence and
/// the commit a `branch -D` deletes cannot disagree. `pr_head` is
/// the head branch name the `pr` arm filters on (a remote-tracking
/// tip still names the local head it was pushed from). The first
/// match wins and is reported as `merged_by`.
pub(crate) fn merge_rule(
    root: &Path,
    pr_head: &str,
    tip: &str,
    into: &str,
) -> Option<&'static str> {
    if ancestor(root, tip, into) {
        return Some("ancestry");
    }
    // `+` = a commit with no patch-equivalent upstream; a listing
    // with none means everything up to `tip` already landed.
    if let Ok(marks) = git(root, &["cherry", into, tip]) {
        if !marks.lines().any(|l| l.starts_with('+')) {
            return Some("cherry");
        }
    }
    if patch_applied(root, tip, into) {
        return Some("patch");
    }
    if pr_merged(root, pr_head, tip, into) {
        return Some("pr");
    }
    None
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

/// A merged GitHub PR counts as merged only when its recorded head
/// commit covers `tip` — the name alone is not evidence: a branch
/// name reused after its PR merged would otherwise report new
/// unmerged commits as merged and the sweep would `branch -D` them
/// away. The head SHA must equal the tip, or the tip must be an
/// ancestor of it (a local branch behind the merged head). When the
/// PR's head commit is not a local object only equality can prove
/// coverage — anything else falls through to the other evidence.
/// The PR's `baseRefName` must be the default ref — a PR merged into
/// a stacked base is not merged into the default branch. `gh` needs
/// a GitHub origin and a successful answer; any failure falls
/// through: gh is a hint, never the only authority.
pub(crate) fn pr_merged(root: &Path, branch: &str, tip: &str, into: &str) -> bool {
    let Ok(url) = git(root, &["remote", "get-url", "origin"]) else {
        return false;
    };
    if !url.contains("github.com") {
        return false;
    }
    // `into` may be a remote-tracking name ("origin/main") — the PR
    // base is a bare branch name.
    let base_name = into.strip_prefix("origin/").unwrap_or(into);
    let mut cmd = Command::new("gh");
    cmd.args([
        "pr",
        "list",
        "--head",
        branch,
        "--state",
        "merged",
        "--json",
        "number,headRefOid,baseRefName",
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
        prs.iter()
            .filter(|pr| pr["baseRefName"].as_str() == Some(base_name))
            .filter_map(|pr| pr["headRefOid"].as_str())
            .any(|oid| oid == tip || object_has(root, oid) && ancestor(root, tip, oid))
    })
}

/// Is `oid` present in the local object store? Ancestry against a
/// commit we do not have cannot be checked.
pub(crate) fn object_has(root: &Path, oid: &str) -> bool {
    git_out(root, &["cat-file", "-e", oid], &[])
        .map(|o| o.status.success())
        .unwrap_or(false)
}
