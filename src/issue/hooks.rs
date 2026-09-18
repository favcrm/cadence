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
# cadence board tracker: refuse a commit that lint rejects.\n\
set -e\n\
if command -v cadence >/dev/null 2>&1; then\n\
  out=$(CADENCE_PM_DIR=\"$(git rev-parse --show-toplevel)\" cadence issue lint 2>&1) || {\n\
    echo \"cadence issue lint failed; commit refused:\" >&2\n\
    echo \"$out\" | sed -n '1,20p' >&2\n\
    exit 1\n\
  }\n\
fi\n";

const POST_COMMIT: &str = "#!/bin/sh\n\
# cadence board tracker: keep the private remote current after every write.\n\
# No-op until an `origin` remote exists; runs in the background so writers never wait.\n\
git remote get-url origin >/dev/null 2>&1 || exit 0\n\
branch=$(git rev-parse --abbrev-ref HEAD)\n\
( git push -q origin \"$branch\" >/dev/null 2>&1 || echo \"$(date -u +%FT%TZ) push failed\" >> .git/push-failures.log ) &\n";

/// The hooks init manages: name → content.
const HOOKS: [(&str, &str); 2] = [("pre-commit", PRE_COMMIT), ("post-commit", POST_COMMIT)];

/// The repo's real git dir (`git rev-parse --git-dir`, resolved
/// absolute) — `.git` may be a file when the tracker is a worktree.
/// `None` when the tracker has no repo.
pub fn git_dir(pm_dir: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(pm_dir)
        .args(["rev-parse", "--git-dir"])
        .output()
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
