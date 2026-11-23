//! `issue lint` — the pre-commit-hook check over the whole PM dir.
//! Schema, id/folder match, dangling and cyclic links, depth, oversize
//! artifacts, unknown status/kind. Non-zero exit on any error.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use crate::error::Result;
use crate::issue::{model, parse, project, Pm};

mod checks;

use checks::{check_contradictions, check_cycles, check_issues, check_types, load_link_context};

pub(super) struct Lint {
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl Lint {
    pub(super) fn err(&mut self, msg: impl Into<String>) {
        self.errors.push(msg.into());
    }
    pub(super) fn warn(&mut self, msg: impl Into<String>) {
        self.warnings.push(msg.into());
    }
}

pub fn run(pm: &Pm, only_project: Option<&str>) -> Result<Value> {
    run_with(pm, only_project, None, None)
}

/// [`run`] with the operator's work-gate approvals when the caller has
/// them (`issue lint` asks the daemon). Without them (the commit hook,
/// sync, doctor) custom gate keys are warned about as possibly
/// unapproved and stages are checked against the file's own list.
/// `delivery_approvals` is the same for the CAD-826 `delivery:`
/// policy: the approved policy half of each `project_work_approve`
/// payload.
pub fn run_with(
    pm: &Pm,
    only_project: Option<&str>,
    approvals: Option<&crate::issue::work::Approvals>,
    delivery_approvals: Option<&crate::issue::delivery_policy::DeliveryApprovals>,
) -> Result<Value> {
    run_inner(pm, only_project, approvals, delivery_approvals, None, None)
}

/// Issue folders a scoped lint covers: `(project key, folder name)`.
type Scope = Vec<(String, String)>;

pub(super) type Fronts = HashMap<String, (String, model::Front, String)>;

/// CAD-1256: lint only what the index stages (the commit hook's mode).
///
/// Per-issue checks (schema, id/folder, grammar, status/kind, refs,
/// symlinks, task reports, oversize artifacts, depth, parent and
/// blocked_by cycles) and the cross-issue checks a changed issue can
/// break (duplicate id in another project, dangling link targets) run
/// for the staged issues only, resolving link targets and their
/// parent/blocked_by chains on demand. The root-symlink and
/// per-project config checks are cheap and always run. Whole-tracker
/// facts are skipped: other issues' own errors, and the "has children"
/// / "blocked_by still open" warnings of untouched issues.
///
/// Anything but a plain issue-folder change (a deleted `issue.md`, a
/// project file, `memory/`, `workflows/`, `apps/`, a path outside a
/// known project) cannot be bounded to one issue, so it falls back to
/// the whole-tracker lint. Nothing staged is a no-op.
pub fn run_staged(pm: &Pm) -> Result<Value> {
    let staged = staged_paths(pm)?;
    if staged.is_empty() {
        return Ok(json!({
            "ok": true, "errors": [], "warnings": [],
            "scope": "staged", "projects": 0, "issues": 0,
        }));
    }
    let projects = project::list(&pm.dir)?;
    match scope_of(&projects, &staged) {
        Some(scope) => run_inner(pm, None, None, None, Some(scope), None),
        // A memory path cannot be bounded to one issue, so the rest of
        // the tracker is still checked. A missing source or retrieval
        // axis refuses the commit only for a staged memory file; the
        // same gap in an untouched file is a warning. Full `issue lint`
        // still reports every such file as an error.
        None => {
            let strict = staged_memory_paths(&staged);
            run_inner(pm, None, None, None, None, Some(&strict))
        }
    }
}

/// `(status, path)` of every staged change; `-z` keeps odd names
/// (spaces, newlines) intact, `--no-renames` turns a rename into a
/// delete plus an add.
fn staged_paths(pm: &Pm) -> Result<Vec<(String, String)>> {
    let out = crate::reaper::output(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&pm.dir)
            .args(["diff", "--cached", "--name-status", "--no-renames", "-z"]),
    )?;
    if !out.status.success() {
        return Err(crate::error::Error::rejected(format!(
            "git diff --cached failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut parts = text.split('\0').filter(|p| !p.is_empty());
    let mut rows = Vec::new();
    while let (Some(status), Some(path)) = (parts.next(), parts.next()) {
        rows.push((status.to_string(), path.to_string()));
    }
    Ok(rows)
}

/// Repo-relative `memory/*.md` paths in this commit, excluding deletions.
/// A missing source or retrieval axis on any other file must not refuse it.
fn staged_memory_paths(staged: &[(String, String)]) -> HashSet<String> {
    staged
        .iter()
        .filter_map(|(status, path)| {
            if status.starts_with('D') {
                return None;
            }
            let mut parts = path.split('/');
            let (_project, folder, file) = (parts.next()?, parts.next()?, parts.next()?);
            if parts.next().is_some() || folder != "memory" || !file.ends_with(".md") {
                return None;
            }
            Some(path.clone())
        })
        .collect()
}

/// The staged issue folders, or `None` when a staged path is not a plain
/// change under `<project>/<issue>/` (see [`run_staged`]).
fn scope_of(projects: &[project::Project], staged: &[(String, String)]) -> Option<Scope> {
    let mut scope: Scope = Vec::new();
    for (status, path) in staged {
        let mut it = path.splitn(3, '/');
        let (Some(proj), Some(folder), Some(rest)) = (it.next(), it.next(), it.next()) else {
            return None;
        };
        let special = [
            "memory",
            crate::issue::workflow::DIR,
            crate::issue::app::DIR,
        ];
        if !projects.iter().any(|p| p.key == proj)
            || folder.starts_with('.')
            || special.contains(&folder)
            || (status == "D" && rest == "issue.md")
        {
            return None;
        }
        let entry = (proj.to_string(), folder.to_string());
        if !scope.contains(&entry) {
            scope.push(entry);
        }
    }
    Some(scope)
}

fn run_inner(
    pm: &Pm,
    only_project: Option<&str>,
    approvals: Option<&crate::issue::work::Approvals>,
    delivery_approvals: Option<&crate::issue::delivery_policy::DeliveryApprovals>,
    scope: Option<Scope>,
    retrievable_strict: Option<&HashSet<String>>,
) -> Result<Value> {
    let mut lint = Lint {
        errors: vec![],
        warnings: vec![],
    };
    let (projects, work_configs) = collect_projects(pm, approvals, delivery_approvals, &mut lint)?;
    let mut fronts: Fronts = scan_folders(
        pm,
        &projects,
        only_project,
        &scope,
        retrievable_strict,
        &mut lint,
    );
    // CAD-1256: ids whose own checks run. Everything else in `fronts`
    // (scoped lint only) is context loaded to resolve links and chains.
    let checked: HashSet<String> = fronts.keys().cloned().collect();
    if scope.is_some() {
        load_link_context(pm, &projects, &mut fronts, &mut lint);
    }

    check_issues(pm, &projects, &fronts, &work_configs, &checked, &mut lint);
    check_contradictions(&fronts, &checked, &mut lint);
    check_types(&fronts, &checked, &mut lint);
    check_cycles(&fronts, &checked, &mut lint);
    Ok(json!({
        "ok": lint.errors.is_empty(),
        "errors": lint.errors,
        "warnings": lint.warnings,
        "projects": projects.len(),
        "issues": checked.len(),
        "scope": if scope.is_some() { "staged" } else { "all" },
    }))
}

fn collect_projects(
    pm: &Pm,
    approvals: Option<&crate::issue::work::Approvals>,
    delivery_approvals: Option<&crate::issue::delivery_policy::DeliveryApprovals>,
    lint: &mut Lint,
) -> Result<(
    Vec<project::Project>,
    HashMap<String, crate::issue::work::WorkConfig>,
)> {
    let projects = project::list(&pm.dir)?;
    // project::list skips symlinked dirs — flag them here instead of
    // letting them disappear silently.
    if let Ok(entries) = std::fs::read_dir(&pm.dir) {
        for entry in entries.flatten() {
            let ft = entry.file_type().map(|t| t.is_symlink()).unwrap_or(false);
            if ft {
                lint.err(format!(
                    "{}: symlinked entry — the board never follows links",
                    entry.file_name().to_string_lossy()
                ));
            }
        }
    }
    // CAD-405: a malformed PROJECT.md is a warning — readers fall back
    // to the default stages and stage moves refuse until it is fixed.
    let mut work_configs: HashMap<String, crate::issue::work::WorkConfig> = HashMap::new();
    for project in &projects {
        use crate::issue::work;
        let (raw, err) = work::load_config_or_default(&pm.dir, &project.key);
        let file_ok = err.is_none();
        if let Some(err) = err {
            lint.warn(format!(
                "{}/PROJECT.md: {err} — the default stages apply",
                project.key
            ));
        }
        // Gate keys differing from the defaults take effect only once the
        // operator approves them.
        let cfg = match approvals {
            Some(a) => {
                let (cfg, note) =
                    work::effective(&project.key, raw, a.get(&project.key).map(String::as_str));
                if let Some(note) = note {
                    lint.warn(note);
                }
                cfg
            }
            None => {
                if !work::gates_default(&raw) {
                    lint.warn(format!(
                        "{}/PROJECT.md: stages/operator_stages differ from the defaults — they \
                         apply only if the operator approved exactly these ({}); \
                         `cadence issue lint` checks the approval",
                        project.key,
                        work::gate_digest(&raw)
                    ));
                }
                raw
            }
        };
        work_configs.insert(project.key.clone(), cfg);
        // CAD-378: a malformed `areas:` block is a warning — the
        // advisory area checks are skipped until it is fixed; stage
        // gates are unaffected. (An unreadable file is warned once, above.)
        if let (true, (_, Some(err))) = (
            file_ok,
            crate::issue::areas::load_or_error(&pm.dir, &project.key),
        ) {
            lint.warn(format!("{err} — area checks are skipped"));
        }
        // CAD-826: a malformed `delivery:` is a warning — every reader
        // falls back and the section is unenforceable until it parses
        // and the operator approves it. A custom section takes effect
        // only while it matches the approval.
        use crate::issue::delivery_policy as dp;
        let file = dp::load(&pm.dir, &project.key);
        match delivery_approvals {
            Some(ap) => {
                let eff = dp::effective(
                    &project.key,
                    file.map_err(|e| e.to_string()),
                    ap.get(&project.key),
                );
                if let Some(note) = eff.note {
                    lint.warn(note);
                }
            }
            None => match file {
                Err(e) => lint.warn(format!("{e} — the default delivery policy applies")),
                Ok(Some(p)) if dp::digest(&p) != dp::digest(&dp::default_policy()) => {
                    lint.warn(format!(
                        "{}/PROJECT.md: delivery: differs from the defaults — it \
                         applies only if the operator approved exactly this ({}); \
                         `cadence issue lint` checks the approval",
                        project.key,
                        dp::digest(&p)
                    ));
                }
                _ => {}
            },
        }
    }
    // CAD-339: `<pm>/agents/` holds the agent files — never a project.
    if projects.iter().any(|p| p.key == "agents") {
        lint.err(
            "agents/project.yaml: the project key 'agents' is reserved for the agent files \
             (<pm>/agents/<slug>/) — rename the project",
        );
    }
    Ok((projects, work_configs))
}

fn scan_folders(
    pm: &Pm,
    projects: &[project::Project],
    only_project: Option<&str>,
    scope: &Option<Scope>,
    retrievable_strict: Option<&HashSet<String>>,
    lint: &mut Lint,
) -> Fronts {
    let mut fronts: Fronts = HashMap::new();
    for project in projects {
        if let Some(only) = only_project {
            if project.key != only {
                continue;
            }
        }
        let dir = pm.dir.join(&project.key);
        // (name, path, is_symlink, is_dir). A scoped lint stats only its
        // staged folders; the full lint lists the whole project dir.
        let mut candidates: Vec<(String, std::path::PathBuf, bool, bool)> = Vec::new();
        match scope {
            Some(scope) => {
                for (_, name) in scope.iter().filter(|(p, _)| *p == project.key) {
                    let path = dir.join(name);
                    if let Ok(m) = path.symlink_metadata() {
                        candidates.push((name.clone(), path, m.is_symlink(), m.is_dir()));
                    }
                }
            }
            None => {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    // DirEntry::file_type is lstat-style — a symlink is not a dir.
                    let Ok(ft) = entry.file_type() else {
                        continue;
                    };
                    candidates.push((
                        entry.file_name().to_string_lossy().to_string(),
                        entry.path(),
                        ft.is_symlink(),
                        ft.is_dir(),
                    ));
                }
            }
        }
        for (name, path, is_symlink, is_dir) in candidates {
            if is_symlink {
                lint.err(format!(
                    "{}/{name}: symlinked issue folder — the board never follows links",
                    project.key
                ));
                continue;
            }
            if !is_dir || name.starts_with('.') {
                continue;
            }
            // `memory/` holds project-memory files, not an issue —
            // its own lint validates them against the body contract.
            if name == "memory" {
                let (mut merrs, mut mwarns) = (Vec::new(), Vec::new());
                crate::memory::lint_dir_scoped(
                    &path,
                    project,
                    &mut |e| merrs.push(e),
                    &mut |w| mwarns.push(w),
                    retrievable_strict,
                );
                for e in merrs {
                    lint.err(e);
                }
                for w in mwarns {
                    lint.warn(w);
                }
                continue;
            }
            // CAD-487: `workflows/` holds the project's stored
            // workflow files — its own lint validates them as
            // templates.
            if name == crate::issue::workflow::DIR {
                let (mut werrs, mut wwarns) = (Vec::new(), Vec::new());
                crate::issue::workflow::lint_dir(
                    &path,
                    &project.key,
                    &mut |e| werrs.push(e),
                    &mut |w| wwarns.push(w),
                );
                for e in werrs {
                    lint.err(e);
                }
                for w in wwarns {
                    lint.warn(w);
                }
                continue;
            }
            // CAD-547: `apps/` holds installed app folders plus their
            // `<name>.yaml` install records — its own lint checks the
            // manifests.
            if name == crate::issue::app::DIR {
                let (mut aerrs, mut awarns) = (Vec::new(), Vec::new());
                crate::issue::app::lint_dir(
                    &path,
                    &project.key,
                    &mut |e| aerrs.push(e),
                    &mut |w| awarns.push(w),
                );
                for e in aerrs {
                    lint.err(e);
                }
                for w in awarns {
                    lint.warn(w);
                }
                continue;
            }
            let file = path.join("issue.md");
            if file.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
                lint.err(format!(
                    "{}/{name}: issue.md is a symlink — the board never follows links",
                    project.key
                ));
                continue;
            }
            if !file.is_file() {
                lint.err(format!("{}/{}: no issue.md", project.key, name));
                continue;
            }
            for sub in ["comments", "artifacts", crate::issue::task_report::DIR] {
                let sub_dir = path.join(sub);
                if sub_dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
                    lint.err(format!(
                        "{}/{name}: {sub}/ is a symlink — the board never follows links",
                        project.key
                    ));
                    continue;
                }
                if let Ok(files) = std::fs::read_dir(&sub_dir) {
                    for f in files.flatten() {
                        if f.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
                            lint.err(format!(
                                "{}/{name}: {sub}/{} is a symlink",
                                project.key,
                                f.file_name().to_string_lossy()
                            ));
                        }
                    }
                }
            }
            // Task reports (CAD-341): each file must be a valid
            // `cadence.report/2` record filed under this very ticket.
            for r in crate::issue::task_report::list(&path, &name) {
                if let Some(e) = r["error"].as_str() {
                    lint.err(format!(
                        "{}/{}: {e}",
                        project.key,
                        r["path"].as_str().unwrap_or_default()
                    ));
                }
            }
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            match parse::parse_issue(&text) {
                Ok((front, _body)) => {
                    if front.id != name {
                        lint.err(format!(
                            "{}/{}: frontmatter id '{}' does not match folder",
                            project.key, name, front.id
                        ));
                    }
                    if let Some((first_project, _, first_folder)) = fronts.get(&front.id) {
                        lint.err(format!(
                            "{}: duplicated id in {first_project}/{first_folder} and {}/{}",
                            front.id, project.key, name
                        ));
                    } else {
                        fronts.insert(front.id.clone(), (project.key.clone(), front, name));
                    }
                }
                Err(e) => lint.err(format!("{}/{name}: {e}", project.key)),
            }
        }
    }

    fronts
}
