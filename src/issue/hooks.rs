//! Git hooks installed by `issue init`: `pre-commit` refuses a commit
//! lint rejects, `post-commit` background-pushes the private remote.
//! A marker comment identifies a cadence-owned hook — ours are written
//! and refreshed, a foreign hook is never overwritten.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{Error, Result};

/// Marker substring identifying a cadence-owned hook.
const MARKER: &str = "cadence board tracker";

const PRE_COMMIT: &str = "#!/bin/sh\n\
# cadence board tracker: refuse a commit that lint rejects (staged issues only).\n\
set -e\n\
if command -v cadence >/dev/null 2>&1; then\n\
  mode=\n\
  cadence issue lint --help 2>/dev/null | grep -q -- --staged && mode=--staged\n\
  out=$(CADENCE_PM_DIR=\"$(git rev-parse --show-toplevel)\" cadence issue lint $mode 2>&1) || {\n\
    echo \"cadence issue lint failed; commit refused:\" >&2\n\
    echo \"$out\" | sed -n '1,20p' >&2\n\
    exit 1\n\
  }\n\
fi\n";

const POST_COMMIT: &str = "#!/bin/sh\n\
# cadence board tracker: keep the private remote current after every write.\n\
# No-op until an `origin` remote exists; runs in the background so writers never wait.\n\
# The subshell drops git's stdout/stderr (a captured commit would otherwise read them to EOF).\n\
git remote get-url origin >/dev/null 2>&1 || exit 0\n\
gd=$(git rev-parse --git-dir 2>/dev/null) || exit 0\n\
# A rebase, merge or cherry-pick is mid-sequence — whoever drives it\n\
# (e.g. `cadence issue sync`) owns the push; replayed commits are not\n\
# settled state and pushing them leaks work that may still be undone.\n\
if [ -d \"$gd/rebase-merge\" ] || [ -d \"$gd/rebase-apply\" ] || \\\n\
   [ -f \"$gd/MERGE_HEAD\" ] || [ -f \"$gd/CHERRY_PICK_HEAD\" ]; then\n\
  exit 0\n\
fi\n\
# A detached HEAD has no branch to push — leave it alone.\n\
branch=$(git symbolic-ref --quiet --short HEAD) || exit 0\n\
( git push -q origin \"$branch\" >/dev/null 2>&1 || echo \"$(date -u +%FT%TZ) push failed\" >> \"$gd/push-failures.log\" ) >/dev/null 2>&1 </dev/null &\n";

/// The hooks init manages: name → content.
const HOOKS: [(&str, &str); 2] = [("pre-commit", PRE_COMMIT), ("post-commit", POST_COMMIT)];

/// The repo's real git dir (`git rev-parse --git-dir`, resolved
/// absolute) — `.git` may be a file when the tracker is a worktree.
/// `None` when the tracker has no repo.
pub fn git_dir(pm_dir: &Path) -> Option<PathBuf> {
    let out = crate::reaper::output(
        std::process::Command::new("git")
            .arg("-C")
            .arg(pm_dir)
            .args(["rev-parse", "--git-dir"]),
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    Some(if raw.is_absolute() {
        raw
    } else {
        pm_dir.join(raw)
    })
}

fn hooks_dir(pm_dir: &Path) -> Result<PathBuf> {
    git_dir(pm_dir).map(|d| d.join("hooks")).ok_or_else(|| {
        Error::rejected(format!(
            "{} is not a git repo — `cadence issue init` sets one up",
            pm_dir.display()
        ))
    })
}

/// One hook's observed state — shared by `install`'s report and
/// `issue doctor`.
fn describe(path: &Path) -> Value {
    let meta = std::fs::symlink_metadata(path);
    let (present, executable, ours) = match &meta {
        Ok(m) => {
            let exec = m.permissions().mode() & 0o111 != 0;
            let owned = std::fs::read_to_string(path)
                .map(|text| text.contains(MARKER))
                .unwrap_or(false);
            (true, exec, owned)
        }
        Err(_) => (false, false, false),
    };
    json!({
        "present": present,
        "executable": executable,
        "owner": if !present { Value::Null } else if ours { "cadence".into() } else { "foreign".into() },
        "path": path,
    })
}

/// Every hook's current state: `{"pre-commit": {...}, "post-commit": {...}}`.
/// Missing repo → each reports `present: false`.
pub fn report(pm_dir: &Path) -> Value {
    let dir = git_dir(pm_dir).map(|d| d.join("hooks"));
    let mut map = serde_json::Map::new();
    for (name, _) in HOOKS {
        let state = match &dir {
            Some(d) => describe(&d.join(name)),
            None => json!({"present": false, "executable": false,
                           "owner": Value::Null, "path": Value::Null}),
        };
        map.insert(name.to_string(), state);
    }
    Value::Object(map)
}

/// `issue init`'s hook step — idempotent. Missing hooks are written
/// 0755; a cadence-owned hook whose content drifted is refreshed; a
/// foreign hook is reported `kept_foreign` and left alone.
pub fn install(pm_dir: &Path) -> Result<Value> {
    let dir = hooks_dir(pm_dir)?;
    std::fs::create_dir_all(&dir)?;
    let mut map = serde_json::Map::new();
    for (name, content) in HOOKS {
        let path = dir.join(name);
        let existing = std::fs::read_to_string(&path).ok();
        let action = match &existing {
            None => {
                std::fs::write(&path, content)?;
                "installed"
            }
            Some(text) if !text.contains(MARKER) => "kept_foreign",
            Some(text) if text != content => {
                std::fs::write(&path, content)?;
                "updated"
            }
            Some(_) => "present",
        };
        if action != "kept_foreign" {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
        let mut state = describe(&path);
        state["action"] = action.into();
        map.insert(name.to_string(), state);
    }
    Ok(Value::Object(map))
}

#[cfg(test)]
mod cad1255_acceptance;
#[cfg(test)]
mod cad1256_staged_lint;

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// CAD-1255: the post-commit push must not hold git's captured
    /// stdout/stderr, or `reaper::output` waits for the whole push.
    #[test]
    fn commit_does_not_wait_for_slow_background_push() {
        let root = PathBuf::from(format!("/tmp/c1255-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (origin, pm) = (root.join("o.git"), root.join("pm"));
        std::fs::create_dir_all(&pm).unwrap();
        git(&root, &["init", "-q", "--bare", "o.git"]);
        let pre = origin.join("hooks/pre-receive");
        std::fs::write(&pre, "#!/bin/sh\nsleep 3\n").unwrap();
        std::fs::set_permissions(&pre, std::fs::Permissions::from_mode(0o755)).unwrap();
        git(&pm, &["init", "-q", "-b", "main"]);
        git(&pm, &["remote", "add", "origin", origin.to_str().unwrap()]);
        install(&pm).unwrap();
        // pre-commit shells out to whatever `cadence` is on PATH; only the
        // post-commit push is under test.
        std::fs::remove_file(pm.join(".git/hooks/pre-commit")).unwrap();
        std::fs::write(pm.join("f"), "x").unwrap();
        git(&pm, &["add", "f"]);

        let start = Instant::now();
        let out = crate::reaper::output(
            Command::new("git")
                .arg("-C")
                .arg(&pm)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["commit", "-q", "-m", "x"]),
        )
        .unwrap();
        let took = start.elapsed();
        assert!(out.status.success(), "{out:?}");
        assert!(
            took < Duration::from_millis(1500),
            "commit waited for the push: {took:?}"
        );
        // The background push finishes on its own; let it before cleanup.
        std::thread::sleep(Duration::from_secs(4));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn install_rewrites_stale_managed_post_commit() {
        let root = PathBuf::from(format!("/tmp/c1255h-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q"]);
        let hook = root.join(".git/hooks/post-commit");
        std::fs::write(&hook, format!("#!/bin/sh\n# {MARKER}: old\n")).unwrap();
        let rep = install(&root).unwrap();
        assert_eq!(rep["post-commit"]["action"], "updated");
        assert_eq!(std::fs::read_to_string(&hook).unwrap(), POST_COMMIT);
        let _ = std::fs::remove_dir_all(&root);
    }
}
