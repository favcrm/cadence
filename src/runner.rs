//! Daemon-launched build runners (CAD-230 phase b2).
//!
//! A caller with no pane and no managed endpoint (a subagent, an external
//! worker, a CI-like run) has no slot identity of its own, so it cannot
//! queue for a build slot. It can instead ask the daemon to run one of
//! its project's **recipes**: a fixed `argv`, a repo-relative `cwd` and an
//! environment allowlist declared in the tracker's `project.yaml`
//! (`build: {recipes: …}`). Nothing about the command comes from the
//! request — the caller names a recipe, a project and, optionally, a
//! checkout of one of that project's registered repos; an unknown recipe,
//! a foreign checkout or any command-shaped field is refused.
//!
//! This module is the daemon-independent half: resolving a recipe into a
//! launch [`Intent`] (with its digest), the gated spawn, and the durable
//! [`Receipt`] under `<state>/runners/`. The daemon (`slot_launch`) owns
//! authorization, enrollment, the slot and the wait.
//!
//! The spawn is **gated**: the daemon starts `/bin/sh` running [`GATE`],
//! which blocks on its stdin until the daemon writes `go <runner_id>` —
//! only after the process is enrolled (its exact pid, starttime and uid)
//! and granted a strict slot. The gate then `exec`s the recipe, which
//! keeps the pid and starttime: the enrolled process, the slot holder and
//! the running build are one identity from grant to exit. A gate whose
//! stdin closes without the go line (refusal, queue timeout, a daemon
//! that died) exits 125 without running anything.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::issue::project::{self, Recipe};
use crate::slots::SlotKind;

/// The gate `/bin/sh -c` runs; `$@` is the recipe's argv. It reads one
/// line, requires exactly `go <runner id>`, then execs the recipe with
/// stdin from `/dev/null`. Anything else — EOF included — exits 125
/// with the recipe never started.
pub const GATE: &str = "IFS= read -r go || exit 125; \
     [ \"$go\" = \"go $CADENCE_RUNNER_ID\" ] || exit 125; \
     exec \"$@\" </dev/null";

/// The exit status a gate that never opened reports.
pub const GATE_CLOSED: i32 = 125;

/// Receipt states. `pending` (intent bound, nothing spawned), `queued`
/// (enrolled gate waiting for its slot) and `running` (gate opened) are
/// in flight; the rest are terminal.
pub const TERMINAL: [&str; 5] = ["exited", "timed_out", "refused", "cancelled", "unknown"];

/// Everything a launch is: resolved from project config and the
/// checkout, never from the request. `digest` binds it all, and the
/// daemon binds the digest to `runner_id` before anything is spawned.
#[derive(Clone, Debug)]
pub struct Intent {
    pub runner_id: String,
    pub project: String,
    pub recipe: String,
    pub kind: SlotKind,
    pub argv: Vec<String>,
    /// The checkout's top-level directory.
    pub worktree: PathBuf,
    /// `worktree` + the recipe's repo-relative `cwd`, canonical.
    pub cwd: PathBuf,
    /// The allowlisted environment NAMES; values come from the daemon's
    /// own environment at spawn.
    pub env: Vec<String>,
    /// `HEAD` of the checkout at resolution.
    pub head_sha: String,
    /// The checkout had uncommitted changes at resolution — `head_sha`
    /// then does not describe everything the recipe builds. Recorded,
    /// never hidden; it is not part of the digest.
    pub dirty: bool,
    pub digest: String,
}

/// Who asked for a runner — derived from the connection by the daemon.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Requester {
    /// `pane`, `managed` or `operator`.
    pub kind: String,
    /// The lane the runner's slot is accounted to.
    pub lane: String,
}

/// The durable record of one runner, `<state>/runners/<id>.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub runner_id: String,
    pub project: String,
    pub recipe: String,
    pub kind: String,
    pub digest: String,
    pub head_sha: String,
    /// See [`Intent::dirty`].
    #[serde(default)]
    pub dirty: bool,
    pub worktree: String,
    pub cwd: String,
    pub argv: Vec<String>,
    pub env: Vec<String>,
    pub requester: Requester,
    /// See [`TERMINAL`] and the in-flight states above it.
    pub state: String,
    /// `true` once the outcome is recorded; `false` in flight and for an
    /// `unknown` outcome (a daemon restart lost the runner).
    pub complete: bool,
    /// The state an `unknown` runner was last recorded in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_state: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub starttime: Option<u64>,
    #[serde(default)]
    pub enrollment_id: Option<String>,
    pub created: f64,
    #[serde(default)]
    pub started: Option<f64>,
    #[serde(default)]
    pub ended: Option<f64>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub signal: Option<i32>,
    #[serde(default)]
    pub reason: Option<String>,
    pub log_path: String,
}

impl Receipt {
    /// A fresh `pending` receipt for `intent` — written before spawn.
    pub fn pending(intent: &Intent, requester: Requester, log_path: &Path, now: f64) -> Self {
        Self {
            runner_id: intent.runner_id.clone(),
            project: intent.project.clone(),
            recipe: intent.recipe.clone(),
            kind: intent.kind.as_str().to_string(),
            digest: intent.digest.clone(),
            head_sha: intent.head_sha.clone(),
            dirty: intent.dirty,
            worktree: intent.worktree.display().to_string(),
            cwd: intent.cwd.display().to_string(),
            argv: intent.argv.clone(),
            env: intent.env.clone(),
            requester,
            state: "pending".into(),
            complete: false,
            last_state: None,
            pid: None,
            starttime: None,
            enrollment_id: None,
            created: now,
            started: None,
            ended: None,
            exit_code: None,
            signal: None,
            reason: None,
            log_path: log_path.display().to_string(),
        }
    }

    pub fn is_terminal(&self) -> bool {
        TERMINAL.contains(&self.state.as_str())
    }

    /// Close the receipt with a terminal `state`.
    pub fn finish(&mut self, state: &str, reason: Option<String>, now: f64) {
        self.state = state.to_string();
        self.complete = state != "unknown";
        self.reason = reason;
        self.ended = Some(now);
    }
}

/// `run-<32 hex>` — the only shape a runner id has; anything else is
/// refused before it can name a path.
pub fn valid_runner_id(id: &str) -> bool {
    id.strip_prefix("run-")
        .is_some_and(|h| h.len() == 32 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub fn new_runner_id() -> String {
    format!("run-{}", uuid::Uuid::new_v4().simple())
}

pub fn runners_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("runners")
}

pub fn receipt_path(state_dir: &Path, runner_id: &str) -> PathBuf {
    runners_dir(state_dir).join(format!("{runner_id}.json"))
}

pub fn log_path(state_dir: &Path, runner_id: &str) -> PathBuf {
    runners_dir(state_dir).join(format!("{runner_id}.log"))
}

/// Atomic, fsynced, mode 0600 — a reader never sees a torn receipt.
pub fn write_receipt(state_dir: &Path, r: &Receipt) -> Result<()> {
    let dir = runners_dir(state_dir);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    let path = receipt_path(state_dir, &r.runner_id);
    let tmp = path.with_extension("tmp");
    let body = serde_json::to_vec_pretty(r)
        .map_err(|e| Error::internal(format!("runner receipt: {e}")))?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(&body)?;
    f.sync_all()?;
    std::fs::rename(&tmp, &path)?;
    if let Ok(d) = std::fs::File::open(&dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

pub fn read_receipt(state_dir: &Path, runner_id: &str) -> Result<Receipt> {
    if !valid_runner_id(runner_id) {
        return Err(Error::rejected(format!(
            "'{runner_id}' is not a runner id (run-<32 hex>)"
        )));
    }
    let text = std::fs::read_to_string(receipt_path(state_dir, runner_id))
        .map_err(|_| Error::rejected(format!("No runner '{runner_id}'")))?;
    serde_json::from_str(&text)
        .map_err(|e| Error::internal(format!("runner receipt {runner_id} is unreadable: {e}")))
}

/// Daemon boot (CAD-230b restart semantics): every receipt still in
/// flight belonged to the previous daemon's runner threads, which are
/// gone. Each becomes `unknown` with `complete: false` and the state it
/// was last in — never relaunched, never guessed (CAD-236 owns
/// resumption). Returns the receipts it marked, for events.
pub fn recover(state_dir: &Path, now: f64) -> Vec<Receipt> {
    let Ok(entries) = std::fs::read_dir(runners_dir(state_dir)) else {
        return Vec::new();
    };
    let mut marked = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(id) = name.strip_suffix(".json").filter(|id| valid_runner_id(id)) else {
            continue;
        };
        let Ok(mut r) = read_receipt(state_dir, id) else {
            continue;
        };
        if r.is_terminal() {
            continue;
        }
        let last = r.state.clone();
        r.last_state = Some(last.clone());
        r.finish(
            "unknown",
            Some(format!(
                "the daemon restarted while this runner was {last} — its outcome \
                 is unknown and it is never relaunched automatically"
            )),
            now,
        );
        r.ended = None;
        if write_receipt(state_dir, &r).is_ok() {
            marked.push(r);
        }
    }
    marked
}

/// Read-only git in a checkout the requester controls, run by the
/// daemon: no fsmonitor hook (a checkout's config could name any
/// program), no optional index lock, and only `PATH`/`HOME` from the
/// daemon's environment.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_clear()
        .envs(
            ["PATH", "HOME"]
                .iter()
                .filter_map(|k| Some((k, std::env::var_os(k)?))),
        )
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The checkout's `HEAD` commit now — re-read when the gate is about
/// to open, so a launch never runs a source other than the one its
/// digest names.
pub fn head_of(checkout: &Path) -> Option<String> {
    git(checkout, &["rev-parse", "--verify", "HEAD"])
        .filter(|s| s.len() >= 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Uncommitted changes — tracked edits or untracked (non-ignored)
/// files — in the checkout now; an unreadable status counts as dirty.
/// Re-read when the gate opens: the receipt's `dirty` covers both reads.
pub fn is_dirty(checkout: &Path) -> bool {
    git(checkout, &["status", "--porcelain"]).is_none_or(|out| !out.is_empty())
}

/// `^[a-z0-9][a-z0-9_-]{0,63}$`
fn valid_recipe_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

/// A POSIX environment name the recipe may allowlist. `CADENCE_*` is
/// the daemon's own namespace (the runner id, the gate) and never
/// passes through.
fn valid_env_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
        && !name.starts_with("CADENCE_")
}

/// Validate one recipe as declared. Every refusal names the recipe and
/// the rule, so the operator can fix `project.yaml`.
fn check_recipe(project: &str, name: &str, r: &Recipe) -> Result<SlotKind> {
    let bad = |why: String| {
        Error::rejected(format!(
            "Recipe '{name}' in project '{project}' is invalid: {why} — fix \
             build.recipes.{name} in its project.yaml"
        ))
    };
    if !valid_recipe_name(name) {
        return Err(bad("a recipe name is [a-z0-9][a-z0-9_-]{0,63}".into()));
    }
    if r.argv.is_empty() || r.argv.iter().any(|a| a.is_empty() || a.contains('\0')) {
        return Err(bad(
            "argv must be a non-empty list of non-empty strings".into()
        ));
    }
    if r.argv[0].starts_with('-') {
        return Err(bad("argv[0] must name a program, not an option".into()));
    }
    if let Some(n) = r.env.iter().find(|n| !valid_env_name(n)) {
        return Err(bad(format!(
            "env name '{n}' is not an allowlistable name (POSIX, not CADENCE_*)"
        )));
    }
    SlotKind::parse(r.kind.as_deref().unwrap_or("build")).map_err(|e| bad(e.to_string()))
}

/// The recipe's repo-relative `cwd` under `top`: relative, no `..`,
/// and — once symlinks resolve — still inside the checkout.
fn resolve_cwd(project: &str, name: &str, top: &Path, rel: Option<&str>) -> Result<PathBuf> {
    let rel = rel.unwrap_or(".");
    let bad = |why: &str| {
        Error::rejected(format!(
            "Recipe '{name}' in project '{project}': cwd '{rel}' {why}"
        ))
    };
    let p = Path::new(rel);
    if p.is_absolute()
        || p.components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(bad("must be repo-relative with no '..'"));
    }
    let cwd = top
        .join(p)
        .canonicalize()
        .map_err(|_| bad("does not exist in the checkout"))?;
    if !cwd.starts_with(top) || !cwd.is_dir() {
        return Err(bad("resolves outside the checkout or is not a directory"));
    }
    Ok(cwd)
}

/// The checkout a launch runs in: `worktree` (any path inside a checkout)
/// or, when absent, the project's first registered repo. Either way its
/// git common-dir root must be one of the project's registered repo
/// paths — a checkout of anything else is refused.
fn resolve_checkout(p: &project::Project, worktree: Option<&Path>) -> Result<PathBuf> {
    let registered: Vec<PathBuf> = p
        .repos
        .iter()
        .filter_map(|r| r.path.as_deref())
        .map(|path| {
            let path = project::expand_home(path);
            path.canonicalize().unwrap_or(path)
        })
        .collect();
    if registered.is_empty() {
        return Err(Error::rejected(format!(
            "Project '{}' registers no repo path — a runner needs a checkout \
             of a registered repo",
            p.key
        )));
    }
    let start = match worktree {
        Some(w) => w
            .canonicalize()
            .map_err(|_| Error::rejected(format!("Worktree '{}' does not exist", w.display())))?,
        None => registered[0].clone(),
    };
    let top = git(&start, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .and_then(|t| t.canonicalize().ok())
        .ok_or_else(|| {
            Error::rejected(format!(
                "'{}' is not inside a git checkout",
                start.display()
            ))
        })?;
    let root = project::repo_identity(&top).map(|(root, _)| root);
    if !root.as_ref().is_some_and(|r| registered.contains(r)) {
        return Err(Error::rejected(format!(
            "'{}' is not a checkout of a repo registered to project '{}' \
             (registered: {})",
            top.display(),
            p.key,
            registered
                .iter()
                .map(|r| r.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(top)
}

/// The launch-intent digest: sha256 over the canonical JSON of every
/// resolved input — project, recipe, slot kind, argv, checkout, cwd,
/// allowlisted env names and the checkout's `HEAD`.
fn digest_of(i: &Intent) -> String {
    let doc = json!({
        "v": 1,
        "project": i.project,
        "recipe": i.recipe,
        "kind": i.kind.as_str(),
        "argv": i.argv,
        "worktree": i.worktree.display().to_string(),
        "cwd": i.cwd.display().to_string(),
        "env": i.env,
        "head_sha": i.head_sha,
    });
    format!("{:x}", Sha256::digest(doc.to_string().as_bytes()))
}

/// Resolve `recipe` of `project_key` (from `<pm_dir>/<key>/project.yaml`)
/// against a checkout into a launch [`Intent`] with a fresh runner id.
/// Refuses an unknown project or recipe (naming what exists), an invalid
/// recipe, a checkout outside the project's registered repos and a
/// checkout with no `HEAD`.
pub fn resolve(
    pm_dir: &Path,
    project_key: &str,
    recipe: &str,
    worktree: Option<&Path>,
) -> Result<Intent> {
    let projects = project::list(pm_dir)?;
    let p = projects
        .iter()
        .find(|p| p.key == project_key)
        .ok_or_else(|| project::unknown_project(project_key, pm_dir))?;
    let recipes = p.build.as_ref().map(|b| &b.recipes);
    let Some(r) = recipes.and_then(|rs| rs.get(recipe)) else {
        let known: Vec<&str> = recipes
            .map(|rs| rs.keys().map(String::as_str).collect())
            .unwrap_or_default();
        return Err(Error::rejected(format!(
            "Unknown recipe '{recipe}' for project '{project_key}' — {}. Recipes \
             come only from build.recipes in the project's project.yaml",
            if known.is_empty() {
                "it defines none".to_string()
            } else {
                format!("it defines: {}", known.join(", "))
            }
        )));
    };
    let kind = check_recipe(project_key, recipe, r)?;
    let top = resolve_checkout(p, worktree)?;
    let cwd = resolve_cwd(project_key, recipe, &top, r.cwd.as_deref())?;
    let head_sha = head_of(&top).ok_or_else(|| {
        Error::rejected(format!(
            "Checkout '{}' has no HEAD commit to bind the launch to",
            top.display()
        ))
    })?;
    let dirty = is_dirty(&top);
    let mut intent = Intent {
        runner_id: new_runner_id(),
        project: project_key.to_string(),
        recipe: recipe.to_string(),
        kind,
        argv: r.argv.clone(),
        worktree: top,
        cwd,
        env: r.env.clone(),
        head_sha,
        dirty,
        digest: String::new(),
    };
    intent.digest = digest_of(&intent);
    Ok(intent)
}

/// Spawn the gated runner process: `/bin/sh -c GATE` with the recipe's
/// argv as `$@`, in the recipe's cwd, with ONLY `env` (the allowlisted
/// names the daemon resolved) plus the runner id, stdin a pipe the
/// daemon holds (the gate), stdout and stderr appended to `log`, in its
/// own process group. The recipe does not start until the daemon writes
/// the go line.
pub fn spawn_gated(intent: &Intent, env: &[(String, String)], log: &Path) -> Result<Child> {
    // A fresh file only: a runner id is new, so an existing path at
    // its log name is refused rather than followed.
    let out = std::fs::OpenOptions::new()
        .append(true)
        .create_new(true)
        .mode(0o600)
        .open(log)?;
    let err = out.try_clone()?;
    Command::new("/bin/sh")
        .arg("-c")
        .arg(GATE)
        .arg("cadence-runner")
        .args(&intent.argv)
        .current_dir(&intent.cwd)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .env("CADENCE_RUNNER_ID", &intent.runner_id)
        .env("CADENCE_RUNNER_DIGEST", &intent.digest)
        .stdin(Stdio::piped())
        .stdout(out)
        .stderr(err)
        .process_group(0)
        .spawn()
        .map_err(|e| Error::rejected(format!("cannot spawn runner {}: {e}", intent.runner_id)))
}

/// Open the gate: the one line [`GATE`] accepts.
pub fn go_line(runner_id: &str) -> String {
    format!("go {runner_id}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tracker dir with project `p` whose one repo is a fresh git
    /// checkout with a commit, and `recipes` as its build.recipes YAML.
    fn fixture(recipes: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        std::fs::write(repo.join("sub/f"), "x").unwrap();
        for args in [
            &["init", "-q"][..],
            &["add", "."],
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "i",
            ],
        ] {
            assert!(Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .status()
                .unwrap()
                .success());
        }
        let pm = dir.path().join("pm");
        std::fs::create_dir_all(pm.join("p")).unwrap();
        std::fs::write(
            pm.join("p/project.yaml"),
            format!(
                "key: p\nprefix: P\nrepos:\n- path: {}\nbuild:\n  recipes:\n{recipes}",
                repo.display()
            ),
        )
        .unwrap();
        (dir, pm, repo)
    }

    #[test]
    fn recipe_resolves_to_a_bound_intent() {
        let (_d, pm, repo) = fixture(
            "    ok:\n      argv: [sh, -c, echo ok]\n      cwd: sub\n      env: [PATH]\n      kind: test\n",
        );
        let i = resolve(&pm, "p", "ok", None).unwrap();
        assert!(valid_runner_id(&i.runner_id), "{}", i.runner_id);
        assert_eq!(i.kind, SlotKind::Test);
        assert_eq!(i.argv, vec!["sh", "-c", "echo ok"]);
        let top = repo.canonicalize().unwrap();
        assert_eq!(i.worktree, top);
        assert_eq!(i.cwd, top.join("sub"));
        assert_eq!(i.env, vec!["PATH"]);
        assert_eq!(i.head_sha.len(), 40);
        assert_eq!(i.digest.len(), 64);
        // Same inputs, same digest; the runner id is not an input.
        let j = resolve(&pm, "p", "ok", Some(&repo.join("sub"))).unwrap();
        assert_eq!(i.digest, j.digest);
        assert_ne!(i.runner_id, j.runner_id);
    }

    #[test]
    fn unknown_recipe_or_project_is_refused_naming_what_exists() {
        let (_d, pm, _repo) = fixture("    ok:\n      argv: [true]\n");
        let e = resolve(&pm, "p", "nope", None).unwrap_err().to_string();
        assert!(
            e.contains("Unknown recipe 'nope'") && e.contains("ok"),
            "{e}"
        );
        let e = resolve(&pm, "q", "ok", None).unwrap_err().to_string();
        assert!(e.contains("No project for q"), "{e}");
    }

    #[test]
    fn invalid_recipes_and_foreign_checkouts_are_refused() {
        for (yaml, needle) in [
            ("    bad:\n      argv: []\n", "argv must be"),
            (
                "    bad:\n      argv: [true]\n      env: [CADENCE_ALIAS]\n",
                "CADENCE_ALIAS",
            ),
            ("    bad:\n      argv: [true]\n      cwd: ../x\n", "no '..'"),
            (
                "    bad:\n      argv: [true]\n      cwd: /tmp\n",
                "repo-relative",
            ),
            ("    bad:\n      argv: [true]\n      kind: deploy\n", "kind"),
            ("    bad:\n      argv: [-c, x]\n", "not an option"),
        ] {
            let (_d, pm, _repo) = fixture(yaml);
            let e = resolve(&pm, "p", "bad", None).unwrap_err().to_string();
            assert!(e.contains(needle), "{yaml}: {e}");
        }
        let (_d, pm, _repo) = fixture("    ok:\n      argv: [true]\n");
        let (_o, _opm, other) = fixture("    ok:\n      argv: [true]\n");
        let e = resolve(&pm, "p", "ok", Some(&other))
            .unwrap_err()
            .to_string();
        assert!(e.contains("not a checkout of a repo registered"), "{e}");
    }

    #[test]
    fn restart_marks_in_flight_receipts_unknown_and_incomplete() {
        let (_d, pm, _repo) = fixture("    ok:\n      argv: [true]\n");
        let state = tempfile::tempdir().unwrap();
        let req = Requester {
            kind: "pane".into(),
            lane: "a".into(),
        };
        let mut done = None;
        for st in ["running", "queued", "exited"] {
            let i = resolve(&pm, "p", "ok", None).unwrap();
            let mut r =
                Receipt::pending(&i, req.clone(), &log_path(state.path(), &i.runner_id), 1.0);
            r.state = st.into();
            if st == "exited" {
                r.finish("exited", None, 2.0);
                r.exit_code = Some(0);
                done = Some(r.runner_id.clone());
            }
            write_receipt(state.path(), &r).unwrap();
        }
        let marked = recover(state.path(), 3.0);
        assert_eq!(marked.len(), 2);
        for r in &marked {
            let back = read_receipt(state.path(), &r.runner_id).unwrap();
            assert_eq!(back.state, "unknown");
            assert!(!back.complete);
            assert!(matches!(
                back.last_state.as_deref(),
                Some("running" | "queued")
            ));
        }
        let back = read_receipt(state.path(), &done.unwrap()).unwrap();
        assert_eq!((back.state.as_str(), back.complete), ("exited", true));
        // A second boot finds nothing more to mark.
        assert!(recover(state.path(), 4.0).is_empty());
        assert!(read_receipt(state.path(), "../etc/passwd").is_err());
    }

    #[test]
    fn gate_runs_nothing_without_the_exact_go_line() {
        let (_d, pm, _repo) = fixture("    ok:\n      argv: [sh, -c, echo RAN]\n");
        let state = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(runners_dir(state.path())).unwrap();
        for line in [None, Some("go run-00000000000000000000000000000000\n")] {
            let i = resolve(&pm, "p", "ok", None).unwrap();
            let log = log_path(state.path(), &i.runner_id);
            let mut child = spawn_gated(&i, &[], &log).unwrap();
            let mut stdin = child.stdin.take().unwrap();
            if let Some(line) = line {
                stdin.write_all(line.as_bytes()).unwrap();
            }
            drop(stdin);
            assert_eq!(child.wait().unwrap().code(), Some(GATE_CLOSED));
            assert!(!std::fs::read_to_string(&log).unwrap().contains("RAN"));
        }
        let i = resolve(&pm, "p", "ok", None).unwrap();
        let log = log_path(state.path(), &i.runner_id);
        let mut child = spawn_gated(&i, &[], &log).unwrap();
        // Blocked at the gate, in its own process group — a signal to
        // the daemon's group never reaches it.
        let pid = child.id();
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let pgrp: u32 = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .nth(2)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(pgrp, pid);
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(go_line(&i.runner_id).as_bytes()).unwrap();
        drop(stdin);
        assert_eq!(child.wait().unwrap().code(), Some(0));
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "RAN\n");
    }
}
