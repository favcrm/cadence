//! Worktree layout policy (ADR-0003 phase-2 prerequisite, CAD-167) —
//! the one place that knows where cadence puts a lane and what it
//! calls its branch: `<repo>/.cadence/wt/<name>` on `cadence/<name>`,
//! an issue lane named `<id-lower>-<slug>`. Every consumer (lane
//! creation, `issue start`/`finish`, `dispatch`'s kickoff prediction,
//! review checkouts, the `session end` and `doctor` scans, the
//! overview's PR match) asks this module; outside it and tests no code
//! joins `.cadence/wt` or formats `cadence/<name>` itself — the grep
//! test below enforces that. Centralise before varying: a configurable
//! root (CAD-168) changes this module, not its consumers.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::proto;

/// The worktrees dir relative to a repo root, as a literal — so prose
/// that must stay `&'static str` (`doctor`'s orphan reasons) can
/// `concat!` it rather than restating it.
macro_rules! worktrees_rel {
    () => {
        ".cadence/wt"
    };
}
pub(crate) use worktrees_rel;

/// `.cadence/wt` — the worktrees dir relative to a repo root.
pub const WORKTREES_REL: &str = worktrees_rel!();

/// Every cadence lane branch is `cadence/<name>`.
const BRANCH_PREFIX: &str = "cadence/";

/// `<root>/.cadence/wt` — where every lane of `root` lives.
pub fn worktrees_dir(root: &Path) -> PathBuf {
    root.join(".cadence").join("wt")
}

/// `<root>/.cadence/wt/<name>`. An absolute `name` replaces the root,
/// as `Path::join` does (a task scope recorded as a path is honoured).
pub fn worktree_dir(root: &Path, name: impl AsRef<Path>) -> PathBuf {
    worktrees_dir(root).join(name)
}

/// `.cadence/wt/<name>` relative to the repo root, as text — for
/// commands and messages that name a lane relative to its checkout.
pub fn rel_dir(name: &str) -> String {
    format!("{WORKTREES_REL}/{name}")
}

/// `cadence/<name>` — the branch a lane named `name` is on.
pub fn branch(name: &str) -> String {
    format!("{BRANCH_PREFIX}{name}")
}

/// The lane name a `cadence/<name>` branch belongs to; `None` for a
/// branch cadence did not name.
pub fn name_of_branch(branch: &str) -> Option<&str> {
    branch.strip_prefix(BRANCH_PREFIX)
}

/// `<id-lower>-` — the stem every lane name of issue `id` starts with.
pub fn issue_stem(id: &str) -> String {
    format!("{}-", id.to_lowercase())
}

/// `cadence/<id-lower>-` — every branch of issue `id` starts with it.
pub fn issue_branch_prefix(id: &str) -> String {
    branch(&issue_stem(id))
}

/// The slug part of issue `id`'s lane name (`cad-7-login` → `login`);
/// a name without the issue stem is returned whole.
pub fn issue_slug<'a>(id: &str, name: &'a str) -> &'a str {
    name.strip_prefix(&issue_stem(id)).unwrap_or(name)
}

/// ASCII-lower `-`-separated slug, ≤32 chars — `New Login Form` →
/// `new-login-form`. Non-ASCII titles fall back to `work`. (`memory
/// propose` derives lesson slugs from it too.)
pub fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            if dash && !slug.is_empty() {
                slug.push('-');
            }
            dash = false;
            slug.push(c.to_ascii_lowercase());
        } else {
            dash = true;
        }
    }
    let slug = slug.chars().take(32).collect::<String>();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "work".to_string()
    } else {
        slug.to_string()
    }
}

/// A fresh issue lane's `(name, branch)`: `<id-lower>-<slug>` on
/// `cadence/<id-lower>-<slug>`, the slug from `--name` when given,
/// else from the title. Its dir is [`worktree_dir`]`(root, name)`.
/// Deciding whether an issue reuses a lane it already has is
/// `issue::start::resolve_lane`'s job, not this one's.
pub fn issue_names(id: &str, title: &str, name: Option<&str>) -> Result<(String, String)> {
    let slug = match name {
        Some(name) => proto::identifier(name, "--name")?,
        None => slugify(title),
    };
    let wt_name = format!("{}{}", issue_stem(id), slug);
    proto::identifier(&wt_name, "Worktree name")?;
    let branch = branch(&wt_name);
    Ok((wt_name, branch))
}

/// The repo a lane path belongs to by the layout alone, for a lane
/// whose checkout is gone: `<root>/.cadence/wt/<name>` → `<root>`
/// (canonicalized). `None` when the path does not have that shape.
pub fn root_of(lane: &Path) -> Option<PathBuf> {
    let wt = lane.parent()?;
    let cadence = wt.parent()?;
    if wt.file_name()? != "wt" || cadence.file_name()? != ".cadence" {
        return None;
    }
    cadence.parent()?.canonicalize().ok()
}

/// The repo root a lane path would have under the layout, WITHOUT
/// checking that the path has its shape — `issue finish`'s last-resort
/// recovery for a lane whose dir is gone (the three levels
/// `<root>/.cadence/wt/<name>` sits below its root), canonicalized.
/// Prefer [`root_of`]; this exists so finish keeps its exact
/// behaviour for hand-recorded paths.
pub fn assumed_root(lane: &Path) -> Option<PathBuf> {
    lane.parent()?.parent()?.parent()?.canonicalize().ok()
}

/// Does a path (as text, e.g. a `/proc` link target) lie inside some
/// repo's worktrees dir — `…/.cadence/wt/…` or the dir itself?
pub fn in_worktrees_dir(text: &str) -> bool {
    text.contains(concat!("/", worktrees_rel!(), "/"))
        || text.ends_with(concat!("/", worktrees_rel!()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_lane_names_and_paths() {
        let (name, branch) = issue_names("CAD-7", "New Login Form", None).unwrap();
        assert_eq!(name, "cad-7-new-login-form");
        assert_eq!(branch, "cadence/cad-7-new-login-form");
        let (name, _) = issue_names("CAD-7", "ignored", Some("custom")).unwrap();
        assert_eq!(name, "cad-7-custom");
        assert!(issue_names("CAD-7", "t", Some("-x")).is_err());
        assert_eq!(issue_slug("CAD-7", "cad-7-custom"), "custom");
        assert_eq!(issue_slug("CAD-7", "other"), "other");
        assert_eq!(issue_branch_prefix("CAD-7"), "cadence/cad-7-");
        assert_eq!(name_of_branch("cadence/x-1"), Some("x-1"));
        assert_eq!(name_of_branch("main"), None);
        let root = Path::new("/r");
        assert_eq!(worktrees_dir(root), Path::new("/r/.cadence/wt"));
        assert_eq!(worktree_dir(root, "a"), Path::new("/r/.cadence/wt/a"));
        assert_eq!(worktree_dir(root, "/abs/x"), Path::new("/abs/x"));
        assert_eq!(rel_dir("a"), ".cadence/wt/a");
        assert!(in_worktrees_dir("/r/.cadence/wt/a/target"));
        assert!(in_worktrees_dir("/r/.cadence/wt"));
        assert!(!in_worktrees_dir("/r/.cadence/wtbak/x"));
        assert_eq!(slugify("¡¿"), "work");
    }

    #[test]
    fn root_of_checks_the_shape_and_assumed_root_does_not() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let lane = worktree_dir(&root, "gone");
        assert_eq!(root_of(&lane), Some(root.clone()));
        assert_eq!(assumed_root(&lane), Some(root.clone()));
        let odd = root.join("a").join("b").join("c");
        assert_eq!(root_of(&odd), None);
        assert_eq!(assumed_root(&odd), Some(root));
    }

    /// Non-comment lines of a source file, test modules cut off: a
    /// `#[cfg(test)]` directly followed by `mod` ends the scan.
    fn code_lines(text: &str) -> Vec<(usize, &str)> {
        let lines: Vec<&str> = text.lines().collect();
        let mut out = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            if t == "#[cfg(test)]"
                && lines
                    .get(i + 1)
                    .is_some_and(|n| n.trim_start().starts_with("mod "))
            {
                break;
            }
            if t.starts_with("//") {
                continue;
            }
            out.push((i + 1, *line));
        }
        out
    }

    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n != "tests") {
                    rs_files(&p, out);
                }
            } else if p.extension().is_some_and(|x| x == "rs")
                && p.file_name().is_some_and(|n| n != "tests.rs")
            {
                out.push(p);
            }
        }
    }

    /// CAD-167 acceptance 1: outside this module and tests, no code
    /// joins `.cadence/wt` or formats a `cadence/<name>` branch itself.
    /// `cadence/…` strings on a `method` line are JSON-RPC notification
    /// names (`cadence/tool_use`), not branches.
    #[test]
    fn no_consumer_computes_the_layout_itself() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let me = src.join("worktree").join("layout.rs");
        let mut files = Vec::new();
        rs_files(&src, &mut files);
        assert!(files.len() > 20, "scanned {files:?}");
        let mut offenders = Vec::new();
        for file in files.iter().filter(|f| **f != me) {
            let text = std::fs::read_to_string(file).unwrap();
            for (n, line) in code_lines(&text) {
                let path_join = line.contains(worktrees_rel!()) || line.contains("\"wt\"");
                let branch_fmt = (line.contains("\"cadence/{") || line.contains("\"cadence/\")"))
                    && !line.contains("method");
                if path_join || branch_fmt {
                    offenders.push(format!(
                        "{}:{n}: {}",
                        file.strip_prefix(&src).unwrap().display(),
                        line.trim()
                    ));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "call crate::worktree::layout instead:\n{}",
            offenders.join("\n")
        );
    }
}
