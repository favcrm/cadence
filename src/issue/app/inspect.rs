//! Read-only inspection, listing, board rows, linting, and doctor reporting for `issue::app`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::issue::{model, project, workflow, Pm};

use super::validate::validate;
use super::{
    app_dir, approval_binds, approval_key, approval_state, approved, bundle_files,
    current_install_id, digest, digest_over, fetch_approvals, parse_manifest, read_record,
    read_workflow, workflow_slots, Manifest, DIR, LOCAL_CONNECTION, MANIFEST,
};

/// The connection names the daemon registers, best-effort — `local` is
/// always in the set (the built-in contract, CAD-546 or not). `None`
/// when the daemon is unreachable; callers then report "not verified"
/// rather than "unknown".
pub(super) fn known_connections(state_dir: &Path) -> Option<HashSet<String>> {
    let v = crate::client::rpc(state_dir, "daemon_info", json!({})).ok()?;
    let mut set: HashSet<String> = v["connections"]
        .as_array()?
        .iter()
        .filter_map(|c| c.as_str().map(str::to_string))
        .collect();
    set.insert(LOCAL_CONNECTION.to_string());
    Some(set)
}

/// The manifest of an installed app (`apps/<name>/app.md`).
pub fn read_manifest(pm_dir: &Path, project: &str, name: &str) -> Result<Manifest> {
    let file = app_dir(pm_dir, project, name)?.join(MANIFEST);
    if file.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{} is a symlink — the tracker never follows links",
            file.display()
        )));
    }
    let text = std::fs::read_to_string(&file)
        .map_err(|e| Error::rejected(format!("cannot read {}: {e}", file.display())))?;
    parse_manifest(&text)
}

/// The daemon's registered agent aliases, best-effort — install/update
/// resolve `agent:` against them like `workflow add` does.
pub(super) fn daemon_aliases(state_dir: &Path) -> Vec<String> {
    crate::client::rpc(state_dir, "agent_list", json!({}))
        .ok()
        .and_then(|v| v["agents"].as_array().cloned())
        .map(|a| {
            a.iter()
                .filter_map(|a| a["alias"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Every installed app of `project_key` (or all projects), one row per
/// `apps/<name>/` dir — plus one `error` row per stray entry (a symlink,
/// an orphan `<name>.yaml` with no folder, a folder with no record).
pub fn ls(pm: &Pm, project_key: Option<&str>, state_dir: &Path) -> Result<Value> {
    let projects = project::list(&pm.dir)?;
    let projects: Vec<&project::Project> = match project_key {
        Some(key) => {
            let found: Vec<&project::Project> = projects.iter().filter(|p| p.key == key).collect();
            if found.is_empty() {
                return Err(project::unknown_project(key, &pm.dir));
            }
            found
        }
        None => projects.iter().collect(),
    };
    let approvals = fetch_approvals(state_dir);
    let mut rows = Vec::new();
    for p in projects {
        let dir = pm.dir.join(&p.key).join(DIR);
        if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            rows.push(json!({
                "project": p.key, "name": null,
                "error": "apps/ is a symlink — the tracker never follows links",
            }));
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut seen_records: HashSet<String> = HashSet::new();
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Ok(ft) = e.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                rows.push(json!({
                    "project": p.key, "name": name,
                    "error": "symlink — the tracker never follows links",
                }));
                continue;
            }
            if ft.is_dir() {
                rows.push(describe(&pm.dir, &p.key, &name, approvals.as_ref()));
                continue;
            }
            if let Some(stem) = name.strip_suffix(".yaml") {
                seen_records.insert(stem.to_string());
                continue; // the record's own row is the app dir's
            }
            rows.push(json!({
                "project": p.key, "name": name,
                "error": "not an app — apps/ holds <name>/ folders and <name>.yaml records",
            }));
        }
        // A record without its folder — a half-removed app.
        for stem in seen_records {
            if !dir.join(&stem).is_dir() {
                rows.push(json!({
                    "project": p.key, "name": stem,
                    "error": "install record without its app folder — remove \
                              the stray file or reinstall",
                }));
            }
        }
    }
    rows.sort_by(|a, b| {
        (
            a["project"].as_str().unwrap_or(""),
            a["name"].as_str().unwrap_or(""),
        )
            .cmp(&(
                b["project"].as_str().unwrap_or(""),
                b["name"].as_str().unwrap_or(""),
            ))
    });
    Ok(json!({"apps": rows, "count": rows.len()}))
}

/// One installed app's summary row — used by `ls` and the board list.
fn describe(
    pm_dir: &Path,
    project: &str,
    name: &str,
    approvals: Option<&Map<String, Value>>,
) -> Value {
    let mut row = json!({"project": project, "name": name});
    let record = match read_record(pm_dir, project, name) {
        Ok(record) => record,
        Err(e) => {
            row["error"] = json!(e.to_string());
            return row;
        }
    };
    let manifest = match read_manifest(pm_dir, project, name) {
        Ok(manifest) => manifest,
        Err(e) => {
            row["error"] = json!(e.to_string());
            return row;
        }
    };
    let workflows: Vec<String> = match bundle_files(&app_dir_or(pm_dir, project, name)) {
        Ok(files) => files
            .iter()
            .filter_map(|(rel, _)| {
                rel.strip_prefix("workflows/")
                    .and_then(|n| n.strip_suffix(".md"))
                    .map(str::to_string)
            })
            .collect(),
        Err(e) => {
            row["error"] = json!(e.to_string());
            return row;
        }
    };
    row["title"] = json!(manifest.title);
    row["version"] = json!(manifest.version);
    row["summary"] = json!(manifest.summary);
    row["workflows"] = json!(workflows);
    // The card's primary action (CAD-563 r2): the app's first workflow
    // (sorted, so the choice is deterministic) and the human label the
    // board names the action with.
    let mut names = workflows.clone();
    names.sort();
    if let Some(first) = names.first() {
        let label = std::fs::read_to_string(
            app_dir_or(pm_dir, project, name)
                .join("workflows")
                .join(format!("{first}.md")),
        )
        .ok()
        .and_then(|text| workflow::parse_template(&text).ok())
        .and_then(|tpl| tpl.label);
        row["primary"] = json!({"workflow": first, "label": label});
    }
    row["connections"] = json!(manifest
        .connections
        .iter()
        .map(|slot| json!({
            "slot": slot,
            "bound": record.binding(slot),
        }))
        .collect::<Vec<_>>());
    match digest(pm_dir, project, name) {
        Ok(d) => {
            row["digest"] = json!(d);
            row["approved"] = match approvals {
                Some(a) => json!(approved(project, name, &d, &record.install_id, Some(a))),
                None => json!("unknown — daemon unreachable"),
            };
            row["approval"] = json!(approval_state(
                approvals,
                &approval_key(project, name),
                &d,
                &record.install_id,
            ));
        }
        Err(e) => row["error"] = json!(e.to_string()),
    }
    row["source"] = serde_json::to_value(&record.source).unwrap_or(Value::Null);
    row["team"] = json!(record.team);
    row["installed_at"] = json!(record.installed_at);
    row["installed_by"] = json!(record.installed_by);
    row["updated_at"] = json!(record.updated_at);
    row
}

/// `apps/<name>/` without the "must be installed" wording — for
/// internal walkers that already know it exists.
fn app_dir_or(pm_dir: &Path, project: &str, name: &str) -> PathBuf {
    pm_dir.join(project).join(DIR).join(name)
}

/// `cadence app show <app> --project <key>` — the manifest, the guide,
/// each workflow's summary (the same detail `workflow show` gives),
/// declared slots with their effective bindings, the install record,
/// the digest and the approval state.
pub fn show(pm: &Pm, project_key: &str, name: &str, state_dir: &Path) -> Result<Value> {
    model::check_key(project_key)?;
    if !project::list(&pm.dir)?.iter().any(|p| p.key == project_key) {
        return Err(project::unknown_project(project_key, &pm.dir));
    }
    let mut out = describe(
        &pm.dir,
        project_key,
        name,
        fetch_approvals(state_dir).as_ref(),
    );
    if out.get("error").is_some() {
        return Ok(out);
    }
    let dir = app_dir(&pm.dir, project_key, name)?;
    let manifest = read_manifest(&pm.dir, project_key, name)?;
    let daemon_agents = daemon_aliases(state_dir);
    let (agents, agent_sources) =
        workflow::known_agents(&pm.dir, Some(project_key), &daemon_agents);
    let mut workflows = Vec::new();
    let mut rubrics = Vec::new();
    for (rel, path) in bundle_files(&dir)? {
        if let Some(rb) = rel
            .strip_prefix("rubrics/")
            .and_then(|n| n.strip_suffix(".md"))
        {
            // A file verified real at scan could still be swapped for a
            // link before the read — re-check the leaf, like
            // `read_workflow` and `board_rows` do.
            if path
                .symlink_metadata()
                .map(|m| !m.is_file())
                .unwrap_or(true)
            {
                continue;
            }
            rubrics.push(json!({
                "name": rb,
                "body": std::fs::read_to_string(&path).unwrap_or_default(),
            }));
            continue;
        }
        let Some(wf) = rel
            .strip_prefix("workflows/")
            .and_then(|n| n.strip_suffix(".md"))
            .map(str::to_string)
        else {
            continue;
        };
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        // The installed bundle's own verdict: a `skill:` tag is admissible
        // here, and `validate` proved it resolves.
        let (errors, notes, doc) = workflow::check_installed_text(&text, &agents, &agent_sources);
        let tpl = workflow::parse_template(&text).ok();
        workflows.push(json!({
            "name": format!("{name}/{wf}"),
            "ok": errors.is_empty(),
            "errors": errors,
            "notes": notes,
            "title": doc.as_ref().map(|d| d.title.clone()),
            // The workflow's human label (`label:`, wording) — the
            // board's primary action names it; the app's title when the
            // workflow never declared one.
            "label": tpl.as_ref().and_then(|t| t.label.clone()),
            "tickets": doc.as_ref().map(|d| d.tickets.len()),
            "inputs": tpl.as_ref().map(workflow::inputs_json),
            // The steps, in order, from the canonical render: each
            // title (input names stand in for values) and the agent
            // expression — an input name when the ticket's `agent:` is
            // exactly `{{input}}`, which is how the board maps a run's
            // owners back to the team inputs.
            "steps": doc.as_ref().map(|d| d.tickets.iter().map(|t| json!({
                "title": t.title,
                "agent": t.agent,
                "size": t.size,
            })).collect::<Vec<_>>()),
            "distinct": tpl.as_ref().map(|t| t.distinct.clone()),
            "uses": workflow_slots(&text).unwrap_or_default(),
        }));
    }
    let record = read_record(&pm.dir, project_key, name)?;
    out["summary"] = json!(manifest.summary);
    out["guide"] = json!(manifest.guide);
    out["workflows"] = json!(workflows);
    out["rubrics"] = json!(rubrics);
    out["record"] = serde_json::to_value(&record).unwrap_or(Value::Null);
    Ok(out)
}

/// `plan propose --workflow <app>/<wf>` resolution — the daemon's read:
/// the app must be installed (record included), its current digest must
/// match the operator's recorded approval (`app_unapproved` otherwise),
/// then the workflow renders exactly as a stored one would. The app's
/// saved default team (CAD-577) fills any team role the proposal left
/// unset — the operator set it, so a fresh install runs with only a
/// topic; an explicit input still wins.
pub fn plan_text(
    pm_dir: &Path,
    app_approvals: &std::collections::HashMap<String, Value>,
    project: &str,
    app: &str,
    wf: &str,
    provided: &BTreeMap<String, String>,
) -> Result<String> {
    let _ = app_dir(pm_dir, project, app)?;
    let record = read_record(pm_dir, project, app)?;
    let text = read_workflow(pm_dir, project, app, wf)?;
    // CAD-1867: a `skill:` directive is resolved only by an app RUN, which
    // freezes the package into its snapshot. A plan proposal renders the
    // workflow into tracker tickets — it freezes nothing — so the directive
    // refuses here instead of being dropped into prose.
    workflow::refuse_skill_directive(&text)?;
    // The digest covers THIS buffer for the rendered workflow — the
    // bytes the operator approved are the bytes rendered, even if a
    // co-host writer flips the file after this one read (N2).
    let rel = format!("workflows/{wf}.md");
    let digest = digest_over(pm_dir, project, app, &[(rel.as_str(), text.as_str())])?;
    let ok = app_approvals
        .get(&approval_key(project, app))
        .is_some_and(|p| approval_binds(p, digest.as_str(), &record.install_id));
    if !ok {
        return Err(Error::invalid(
            "app_unapproved",
            format!(
                "app '{app}' in {project} is not approved for its current structure \
                 ({digest}) — a slot, binding, guide, rubric, template or workflow \
                 gate change resets it; the operator re-approves with `cadence app \
                 approve {app} --project {project}`"
            ),
        ));
    }
    // The saved default team fills any team role THIS workflow declares
    // and the proposal left unset (CAD-577). The team record is the
    // union across the app's workflows; filling a role another workflow
    // declares makes `render` refuse `unknown input`. An explicit input
    // still wins: the caller named that agent.
    let declared = workflow::parse_template(&text).ok();
    let mut inputs = provided.clone();
    for (role, agent) in &record.team {
        if declared
            .as_ref()
            .is_some_and(|tpl| tpl.inputs.contains_key(role))
        {
            inputs.entry(role.clone()).or_insert_with(|| agent.clone());
        }
    }
    workflow::render(&text, &inputs)
}

/// `app approve`'s view of the INSTALLED folder — the same strict scan
/// and checks a fresh source passes, so a hand edit after install
/// cannot smuggle content past the operator's review.
pub fn check_installed(
    pm_dir: &Path,
    project: &str,
    name: &str,
    agents: &HashSet<String>,
    agent_sources: &[String],
) -> Result<Vec<String>> {
    let dir = app_dir(pm_dir, project, name)?;
    let _ = read_record(pm_dir, project, name)?;
    Ok(validate(&dir, agents, agent_sources)?.notes)
}

/// The board's New-run rows for `project`'s app workflows — one per
/// `apps/<app>/workflows/<wf>.md`, named `<app>/<wf>`, carrying the
/// APP's approval state (approval is whole-app; there is no
/// per-workflow approval). An app that fails its installed checks lists
/// its workflows with the error attached — the board shows why it
/// cannot run rather than hiding it.
pub fn board_rows(pm_dir: &Path, project: &str, state_dir: &Path) -> Vec<Value> {
    let mut rows = Vec::new();
    let dir = pm_dir.join(project).join(DIR);
    if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return rows;
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return rows;
    };
    let approvals = fetch_approvals(state_dir);
    let daemon_agents = daemon_aliases(state_dir);
    let (agents, agent_sources) = workflow::known_agents(pm_dir, Some(project), &daemon_agents);
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Ok(ft) = e.file_type() else {
            continue;
        };
        if !ft.is_dir() || !model::valid_tag(&name) {
            continue;
        }
        let checked = check_installed(pm_dir, project, &name, &agents, &agent_sources)
            .map_err(|e| e.to_string());
        let app_digest = digest(pm_dir, project, &name);
        let install_id = current_install_id(pm_dir, project, &name);
        let approved = match &app_digest {
            Ok(d) => match &approvals {
                Some(a) => json!(approved(project, &name, d, &install_id, Some(a))),
                None => json!("unknown — daemon unreachable"),
            },
            Err(_) => Value::Null,
        };
        // The listing goes through bundle_files — the same no-links walk
        // install and approve enforce — so a symlinked workflows/ dir (or
        // any planted link) becomes the app's error row instead of
        // leaking the foreign dir's file names onto the board (N1).
        match bundle_files(&e.path()) {
            Ok(files) => {
                for (rel, path) in &files {
                    let Some(stem) = rel
                        .strip_prefix("workflows/")
                        .and_then(|n| n.strip_suffix(".md"))
                    else {
                        continue;
                    };
                    // A file verified real at scan could still be swapped
                    // for a link before the read — re-check the leaf.
                    if path
                        .symlink_metadata()
                        .map(|m| !m.is_file())
                        .unwrap_or(true)
                    {
                        continue;
                    }
                    let text = std::fs::read_to_string(path).unwrap_or_default();
                    let (_, _, doc) =
                        workflow::check_installed_text(&text, &agents, &agent_sources);
                    let tpl = workflow::parse_template(&text).ok();
                    let mut row = json!({
                        "project": project,
                        "name": format!("{name}/{stem}"),
                        "app": name,
                        "title": doc.as_ref().map(|d| d.title.clone()),
                        "tickets": doc.map(|d| d.tickets.len()),
                        "inputs": tpl.as_ref().map(workflow::inputs_json),
                        "approved": approved,
                        "digest": app_digest.as_ref().ok(),
                    });
                    if let Err(why) = &checked {
                        row["error"] = json!(why);
                    }
                    rows.push(row);
                }
            }
            Err(why) => {
                // One row still names the broken app — the board shows
                // why it cannot run rather than hiding it.
                let mut row = json!({
                    "project": project,
                    "name": name,
                    "app": name,
                    "title": Value::Null,
                    "tickets": Value::Null,
                    "inputs": Value::Null,
                    "approved": approved,
                    "digest": app_digest.as_ref().ok(),
                });
                row["error"] = match &checked {
                    Err(checked_why) => json!(checked_why),
                    Ok(_) => json!(why.to_string()),
                };
                rows.push(row);
            }
        }
    }
    rows
}

/// `issue lint`'s view of `<pm>/<key>/apps/` — each `<name>/` is an app
/// folder with a parseable `app.md`, each `<name>.yaml` a well-formed
/// record that names it; anything else is a warning, a symlink an
/// error, like the workflows dir's own lint.
pub fn lint_dir(
    dir: &Path,
    project_key: &str,
    err: &mut dyn FnMut(String),
    warn: &mut dyn FnMut(String),
) {
    if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        err(format!(
            "{project_key}/{DIR}: symlink — the board never follows links"
        ));
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = Vec::new();
    let mut records: Vec<String> = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if e.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
            err(format!(
                "{project_key}/{DIR}/{name}: symlink — the board never follows links"
            ));
            continue;
        }
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if !model::valid_tag(&name) {
                warn(format!(
                    "{project_key}/{DIR}/{name}: not an app name — [a-z0-9-], ≤32"
                ));
                continue;
            }
            names.push(name);
            continue;
        }
        if let Some(stem) = name.strip_suffix(".yaml") {
            records.push(stem.to_string());
            continue;
        }
        warn(format!(
            "{project_key}/{DIR}/{name}: not an app — apps/ holds <name>/ folders \
             and <name>.yaml records"
        ));
    }
    for name in &names {
        let app_path = dir.join(name);
        let md = app_path.join(MANIFEST);
        if md.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            err(format!(
                "{project_key}/{DIR}/{name}/app.md: symlink — the board never follows links"
            ));
            continue;
        }
        match std::fs::read_to_string(&md) {
            Err(_) => err(format!("{project_key}/{DIR}/{name}: no app.md")),
            Ok(text) => match parse_manifest(&text) {
                Err(e) => err(format!("{project_key}/{DIR}/{name}/app.md: {e}")),
                Ok(m) if m.app != *name => warn(format!(
                    "{project_key}/{DIR}/{name}/app.md: declares app '{}' — the folder \
                     and record name it",
                    m.app
                )),
                Ok(_) => {}
            },
        }
        if !records.iter().any(|r| r == name) {
            warn(format!(
                "{project_key}/{DIR}/{name}: no install record {name}.yaml — it was \
                 not installed by `cadence app install`"
            ));
        }
    }
    for stem in &records {
        if names.iter().any(|n| n == stem) {
            continue;
        }
        warn(format!(
            "{project_key}/{DIR}/{stem}.yaml: install record without its app folder"
        ));
    }
}

/// `cadence doctor`'s app report — per installed app, every declared
/// slot's binding health: `unbound` (explicitly unbound), `unknown` when
/// the connection name isn't registered (`known` is the daemon's
/// connection set — `None` when unreachable, reported as
/// "unavailable", never as "unknown connection"), plus slots bound
/// that the manifest no longer declares.
pub fn doctor(pm_dir: &Path, known: Option<&HashSet<String>>) -> Value {
    let mut rows = Vec::new();
    let Ok(projects) = project::list(pm_dir) else {
        return json!({"apps": [], "note": "no readable PM projects"});
    };
    for p in projects {
        let apps_dir = pm_dir.join(&p.key).join(DIR);
        if apps_dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            rows.push(json!({
                "project": p.key, "app": null,
                "error": "apps/ is a symlink — never followed",
            }));
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&apps_dir) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) || !model::valid_tag(&name) {
                continue;
            }
            let (manifest, record) = match (
                read_manifest(pm_dir, &p.key, &name),
                read_record(pm_dir, &p.key, &name),
            ) {
                (Ok(m), Ok(r)) => (m, r),
                (a, b) => {
                    let why = a
                        .err()
                        .or(b.err())
                        .map(|e| e.to_string())
                        .unwrap_or_default();
                    rows.push(json!({"project": p.key, "app": name, "error": why}));
                    continue;
                }
            };
            let mut unbound = Vec::new();
            let mut unknown = Vec::new();
            let mut ok = Vec::new();
            for slot in &manifest.connections {
                match record.binding(slot) {
                    None => unbound.push(slot.clone()),
                    Some(conn) => match known {
                        None => ok.push(
                            json!({"slot": slot, "connection": conn, "verified": "unavailable"}),
                        ),
                        Some(known) if known.contains(conn) => {
                            ok.push(json!({"slot": slot, "connection": conn}))
                        }
                        Some(_) => unknown.push(json!({"slot": slot, "connection": conn})),
                    },
                }
            }
            let stray: Vec<&String> = record
                .bindings
                .keys()
                .filter(|k| !manifest.connections.iter().any(|s| s == *k))
                .collect();
            let mut row = json!({
                "project": p.key,
                "app": name,
                "slots_ok": ok,
                "unbound": unbound,
                "unknown_connection": unknown,
            });
            if !stray.is_empty() {
                row["stray_bindings"] = json!(stray);
            }
            if known.is_none() {
                row["connection_check"] = json!("unavailable — daemon unreachable");
            }
            rows.push(row);
        }
    }
    json!({"apps": rows, "count": rows.len()})
}
