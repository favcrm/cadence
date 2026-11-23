//! `cadence issue epic`: listing, detail, the stage gate, and the plan
//! writes (`propose`, `approve`, `reject`).

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::io::Read;
use std::path::Path;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::{board, model, project, work};

use super::{open_pm, print_json, print_ls_table, print_stage_and_health, print_table};

mod args;
pub use args::EpicAction;

/// `cadence issue epic ls|show|stage|propose|approve|reject`.
pub fn run_epic(action: &EpicAction, state_dir: &std::path::Path) -> Result<i32> {
    match action {
        EpicAction::Stage { id, stage, note } => {
            model::check_id(id)?;
            let out = crate::client::rpc(
                state_dir,
                "epic_stage",
                json!({"epic": id, "stage": stage, "note": note}),
            )?;
            print_json(&out);
            return Ok(0);
        }
        EpicAction::Propose {
            project,
            file,
            workflow,
            inputs,
        } => {
            return propose_plan(
                state_dir,
                project,
                file.as_deref(),
                workflow.as_deref(),
                inputs,
                "issue epic",
            );
        }
        EpicAction::Approve { epic } => return decide_plan(state_dir, epic, None),
        EpicAction::Reject { epic, reason } => return decide_plan(state_dir, epic, Some(reason)),
        EpicAction::Ls { .. } | EpicAction::Show { .. } => {}
    }
    let pm = open_pm()?;
    let projects = match action {
        EpicAction::Ls { project, .. } => project.clone(),
        _ => vec![],
    };
    if !projects.is_empty() {
        let keys: Vec<String> = project::list(&pm.dir)?.into_iter().map(|p| p.key).collect();
        for want in &projects {
            model::check_key(want)?;
            if !keys.iter().any(|k| k == want) {
                return Err(project::unknown_project(want, &pm.dir));
            }
        }
    }
    // Every project loads so cross-project children count.
    let issues = board::load_all(&pm.dir, None)?;
    let jobs = board::fetch_job_state(state_dir);
    let views = board::views_with_job_state(&pm.config.notes_dir(), issues, &jobs);
    let now = crate::issue::time::now_epoch();
    let by_id: std::collections::HashMap<String, &board::View> = views
        .iter()
        .map(|v| (v.issue.front.id.clone(), v))
        .collect();
    match action {
        EpicAction::Ls {
            stage,
            health,
            milestone,
            sort,
            limit,
            fields,
            json,
            ..
        } => {
            let json = &(*json || !crate::output::stdout_table());
            crate::filter::fields_need_json(fields, *json)?;
            let ctx = work::Ctx::new(&pm.dir, &by_id, now, &work::fetch_approvals(state_dir));
            if !stage.is_empty() {
                let mut valid: Vec<String> = ctx
                    .configs
                    .values()
                    .flat_map(|w| w.cfg.stage_ids().into_iter().map(str::to_string))
                    .collect();
                valid.push("rejected".to_string());
                valid.sort();
                valid.dedup();
                for s in stage {
                    if !valid.contains(s) {
                        let list = valid.iter().map(String::as_str).collect::<Vec<_>>();
                        return Err(crate::filter::unknown("stage", s, &list));
                    }
                }
            }
            crate::filter::check_set("health", health, work::HEALTH_STATES)?;
            for m in milestone {
                if !model::valid_tag(m) {
                    return Err(Error::rejected(format!(
                        "Invalid milestone filter '{m}' — a milestone id like m1"
                    )));
                }
            }
            let mut rows: Vec<Value> = views
                .iter()
                .filter(|v| model::item_type(&v.issue.front, v.container) == "epic")
                .map(|v| work::epic_row(&ctx, v))
                .filter(|r| {
                    projects.is_empty()
                        || projects
                            .iter()
                            .any(|p| r["project"].as_str() == Some(p.as_str()))
                })
                .filter(|r| crate::filter::any_of(stage, r["work"]["stage"]["id"].as_str()))
                .filter(|r| crate::filter::any_of(health, r["work"]["health"]["state"].as_str()))
                .filter(|r| crate::filter::any_of(milestone, r["work"]["milestone"].as_str()))
                .collect();
            const EPIC_SORTS: &[(&str, &str)] = &[
                ("id", "id"),
                ("project", "project"),
                ("title", "title"),
                ("status", "status"),
                ("priority", "priority"),
                ("owner", "owner"),
                ("total", "total"),
                ("done_ratio", "done_ratio"),
                ("blocked", "blocked"),
                ("stage", "work.stage.id"),
                ("health", "work.health.state"),
                ("progress", "work.progress.ratio"),
            ];
            if let Some(spec) = sort {
                crate::filter::sort_rows(&mut rows, spec, EPIC_SORTS, "id")?;
            }
            crate::filter::apply_limit(&mut rows, *limit);
            crate::filter::apply_fields(&mut rows, fields)?;
            if *json {
                print_json(&json!({"epics": rows}));
            } else {
                print_epics_table(&rows);
            }
        }
        EpicAction::Show { id, json } => {
            let json = &(*json || !crate::output::stdout_table());
            model::check_id(id)?;
            let epic = by_id.get(id).ok_or_else(|| {
                Error::rejected(format!(
                    "Unknown issue '{id}' — `cadence issue epic ls` lists epics"
                ))
            })?;
            if model::item_type(&epic.issue.front, epic.container) != "epic" {
                return Err(Error::rejected(format!(
                    "{id} has no children and is not `type: epic` — \
                     `cadence issue new --epic {id} \"title\"` makes it an epic"
                )));
            }
            let ctx = work::Ctx::new(&pm.dir, &by_id, now, &work::fetch_approvals(state_dir));
            let kids: Vec<&board::View> = epic
                .children
                .iter()
                .filter_map(|k| by_id.get(k).copied())
                .collect();
            if *json {
                let mut out = work::epic_row(&ctx, epic);
                out["issues"] = kids.iter().map(|v| work::card_json(&ctx, v)).collect();
                print_json(&out);
            } else {
                let row = work::epic_row(&ctx, epic);
                print_epics_table(std::slice::from_ref(&row));
                print_stage_and_health(&row["work"]);
                println!();
                print_ls_table(&kids);
            }
        }
        EpicAction::Stage { .. }
        | EpicAction::Propose { .. }
        | EpicAction::Approve { .. }
        | EpicAction::Reject { .. } => unreachable!("handled above"),
    }
    Ok(0)
}

/// `issue epic ls` table — one row per epic from `work::epics_json`:
/// stage, size-weighted progress, the open · doing · review · blocked
/// counts and health.
fn print_epics_table(epics: &[Value]) {
    if epics.is_empty() {
        eprintln!("no epics — `cadence issue new --epic <ID> \"title\"` gives an issue children");
        return;
    }
    let mut rows = vec![[
        "EPIC", "STAGE", "STATUS", "PROGRESS", "OPEN", "DOING", "REVIEW", "BLOCKED", "HEALTH",
        "OWNERS", "TITLE",
    ]
    .map(str::to_string)
    .to_vec()];
    for e in epics {
        let w = &e["work"];
        let p = &w["progress"];
        let n = |k: &str| p["counts"][k].as_u64().unwrap_or(0);
        let owners: Vec<&str> = e["owners"]
            .as_array()
            .map(|o| o.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        rows.push(vec![
            e["id"].as_str().unwrap_or_default().to_string(),
            w["stage"]["id"].as_str().unwrap_or("-").to_string(),
            e["status"].as_str().unwrap_or_default().to_string(),
            format!(
                "{}/{} {:.0}%",
                p["done_weight"].as_u64().unwrap_or(0),
                p["total_weight"].as_u64().unwrap_or(0),
                p["ratio"].as_f64().unwrap_or(0.0) * 100.0
            ),
            n("open").to_string(),
            n("doing").to_string(),
            n("review").to_string(),
            n("blocked").to_string(),
            w["health"]["state"]
                .as_str()
                .unwrap_or("-")
                .replace('_', " "),
            owners.join(","),
            e["title"].as_str().unwrap_or_default().to_string(),
        ]);
    }
    print_table(&rows);
    eprintln!("PROGRESS = done weight / live weight (S=1 M=3 L=8, unsized=M)");
}

/// `plan propose` and `issue epic propose`. `verb` is the command the
/// caller typed, used only in the missing-source error.
pub fn propose_plan(
    state_dir: &Path,
    project: &str,
    file: Option<&Path>,
    workflow: Option<&str>,
    inputs: &[String],
    verb: &str,
) -> Result<i32> {
    let mut params = json!({"project": project});
    match (file, workflow) {
        (Some(file), None) => {
            let cap = crate::issue::plan::MAX_PLAN_BYTES as u64;
            let text = match read_plan_body(state_dir, file, cap) {
                Err(e) if e.to_string().contains(crate::master::NO_STDIN) => return Err(e),
                res => res.map_err(|e| {
                    Error::rejected(format!("Cannot read plan {}: {e}", file.display()))
                })?,
            };
            params["text"] = json!(text);
        }
        (None, Some(name)) => {
            params["workflow"] = json!(name);
            if !inputs.is_empty() {
                let mut map = serde_json::Map::new();
                for pair in inputs {
                    let Some((k, v)) = pair.split_once('=') else {
                        return Err(Error::rejected(format!("--input '{pair}' — expected k=v")));
                    };
                    if k.is_empty() {
                        return Err(Error::rejected("--input needs a name: k=v"));
                    }
                    map.insert(k.to_string(), json!(v));
                }
                params["inputs"] = Value::Object(map);
            }
        }
        _ => {
            if crate::master::caller_is_master() {
                return Err(Error::rejected(crate::master::NO_STDIN));
            }
            return Err(Error::rejected(format!(
                "{verb} propose needs --file <plan.md> or --workflow <name>"
            )));
        }
    }
    let out = crate::client::rpc(state_dir, "plan_propose", params)?;
    print_json(&out);
    Ok(0)
}

/// `plan approve` (`reason` absent) and `plan reject`.
pub fn decide_plan(state_dir: &Path, epic: &str, reason: Option<&str>) -> Result<i32> {
    let out = match reason {
        None => crate::client::rpc(state_dir, "plan_approve", json!({"epic": epic}))?,
        Some(reason) => crate::client::rpc(
            state_dir,
            "plan_reject",
            json!({"epic": epic, "reason": reason}),
        )?,
    };
    print_json(&out);
    Ok(0)
}

/// Same read as the CLI's `read_master_command_file`: a master caller
/// cannot use stdin, and everyone else may pass `-`.
fn read_plan_body(state_dir: &Path, file: &Path, max: u64) -> Result<String> {
    let path = Some(file).filter(|f| f.as_os_str() != "-" && !f.as_os_str().is_empty());
    if crate::master::caller_is_master() {
        let Some(path) = path else {
            return Err(Error::rejected(crate::master::NO_STDIN));
        };
        return crate::master::read_command_file(state_dir, path, max);
    }
    let read_limit = max.saturating_add(1);
    if let Some(path) = path {
        let mut body = String::new();
        std::fs::File::open(path)?
            .take(read_limit)
            .read_to_string(&mut body)?;
        return Ok(body);
    }
    if super::atty_stdin() {
        return Err(Error::rejected("Provide --text or --file"));
    }
    let mut body = String::new();
    std::io::stdin()
        .take(read_limit)
        .read_to_string(&mut body)?;
    Ok(body)
}
