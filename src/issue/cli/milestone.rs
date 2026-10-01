//! `cadence milestone ls|show`: read-only milestone listing and detail.

use clap::Subcommand;
use serde_json::{json, Value};

use crate::error::Result;
use crate::issue::{board, model, project, work};

use super::{open_pm, print_json, print_table};

/// `cadence milestone` — milestones from PROJECT.md and the issues'
/// `milestone` field or `m<n>-…` tag, with rolled-up progress. Read-only.
#[derive(Subcommand)]
pub enum MilestoneAction {
    /// Every milestone: configured first, then any an issue names.
    /// Value flags repeat and comma-join and match ANY of their values;
    /// different flags AND.
    #[command(after_long_help = crate::filter::GRAMMAR)]
    #[command(visible_alias = "list")]
    Ls {
        /// Project key; repeatable — milestones in any of them.
        #[arg(long, value_delimiter = ',')]
        project: Vec<String>,
        /// Milestone id; repeatable — any of them.
        #[arg(long, value_delimiter = ',')]
        milestone: Vec<String>,
        /// Only milestones with at least one epic in this stage;
        /// repeatable.
        #[arg(long, value_delimiter = ',')]
        stage: Vec<String>,
        /// Health (on_track at_risk stalled); repeatable.
        #[arg(long, value_delimiter = ',')]
        health: Vec<String>,
        /// Sort by id project title configured progress health;
        /// `-KEY` descending.
        #[arg(long, allow_hyphen_values = true)]
        sort: Option<String>,
        /// Keep only the first N rows.
        #[arg(long)]
        limit: Option<usize>,
        /// Keep only these keys in each --json row (comma-joined).
        #[arg(long, value_delimiter = ',', requires = "json")]
        fields: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// One milestone: exit test, epics (stage, progress, health) and
    /// its loose issues.
    Show {
        /// Milestone id, e.g. `m2`.
        id: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

/// `cadence milestone ls|show` — read-only.
pub fn run_milestone(action: &MilestoneAction, state_dir: &std::path::Path) -> Result<i32> {
    let pm = open_pm()?;
    let wanted: Vec<String> = match action {
        MilestoneAction::Ls { project, .. } => project.clone(),
        MilestoneAction::Show { project, .. } => project.iter().cloned().collect(),
    };
    for want in &wanted {
        model::check_key(want)?;
        if !project::list(&pm.dir)?.iter().any(|p| &p.key == want) {
            return Err(project::unknown_project(want, &pm.dir));
        }
    }
    let issues = board::load_all(&pm.dir, None)?;
    let jobs = board::fetch_job_outcomes(state_dir);
    let views = board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs);
    let by_id: std::collections::HashMap<String, &board::View> = views
        .iter()
        .map(|v| (v.issue.front.id.clone(), v))
        .collect();
    let ctx = work::Ctx::new(
        &pm.dir,
        &by_id,
        crate::issue::time::now_epoch(),
        &work::fetch_approvals(state_dir),
    );
    let one = wanted.first().map(String::as_str);
    match action {
        MilestoneAction::Ls {
            milestone,
            stage,
            health,
            sort,
            limit,
            fields,
            json,
            ..
        } => {
            let json = &(*json || !crate::output::stdout_table());
            crate::filter::fields_need_json(fields, *json)?;
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
            let mut rows: Vec<Value> = work::milestones_json(&ctx, &views, None)
                .into_iter()
                .filter(|r| crate::filter::any_of(&wanted, r["project"].as_str()))
                .filter(|r| crate::filter::any_of(milestone, r["id"].as_str()))
                .filter(|r| crate::filter::any_of(health, r["health"]["state"].as_str()))
                .filter(|r| {
                    stage.is_empty()
                        || r["epics"].as_array().is_some_and(|epics| {
                            epics.iter().any(|e| {
                                stage
                                    .iter()
                                    .any(|s| e["stage"].as_str() == Some(s.as_str()))
                            })
                        })
                })
                .collect();
            const MILESTONE_SORTS: &[(&str, &str)] = &[
                ("id", "id"),
                ("project", "project"),
                ("title", "title"),
                ("configured", "configured"),
                ("status", "status"),
                ("owner", "owner"),
                ("target_date", "target_date"),
                ("progress", "progress.ratio"),
                ("health", "health.state"),
            ];
            if let Some(spec) = sort {
                crate::filter::sort_rows(&mut rows, spec, MILESTONE_SORTS, "id")?;
            }
            crate::filter::apply_limit(&mut rows, *limit);
            crate::filter::apply_fields(&mut rows, fields)?;
            if *json {
                print_json(&json!({"milestones": rows}));
            } else {
                print_milestones_table(&rows);
            }
        }
        MilestoneAction::Show { id, json, .. } => {
            let row = work::milestone_show(&ctx, &views, id, one)?;
            let json = &(*json || !crate::output::stdout_table());
            if *json {
                print_json(&row);
            } else {
                print_milestones_table(std::slice::from_ref(&row));
                if let Some(exit) = row["exit"].as_str() {
                    println!("exit: {exit}");
                }
                for field in [
                    "description",
                    "start_date",
                    "completed_date",
                    "config_error",
                ] {
                    if let Some(value) = row[field].as_str() {
                        println!("{field}: {value}");
                    }
                }
                for field in ["depends_on", "evidence"] {
                    for value in row[field].as_array().into_iter().flatten() {
                        println!("{field}: {}", value.as_str().unwrap_or(""));
                    }
                }
                for r in row["health"]["reasons"].as_array().into_iter().flatten() {
                    println!(
                        "  {} — {} (next: {})",
                        r["cause"].as_str().unwrap_or("?"),
                        r["detail"].as_str().unwrap_or(""),
                        r["next"].as_str().unwrap_or("")
                    );
                }
                let mut rows = vec![
                    ["ID", "KIND", "STAGE/STATUS", "PROGRESS", "HEALTH", "TITLE"]
                        .map(str::to_string)
                        .to_vec(),
                ];
                for e in row["epics"].as_array().into_iter().flatten() {
                    rows.push(vec![
                        e["id"].as_str().unwrap_or_default().to_string(),
                        "epic".to_string(),
                        e["stage"].as_str().unwrap_or("-").to_string(),
                        format!("{:.0}%", e["progress"].as_f64().unwrap_or(0.0) * 100.0),
                        e["health"].as_str().unwrap_or("-").replace('_', " "),
                        e["title"].as_str().unwrap_or_default().to_string(),
                    ]);
                }
                for i in row["issues"].as_array().into_iter().flatten() {
                    rows.push(vec![
                        i["id"].as_str().unwrap_or_default().to_string(),
                        i["type"].as_str().unwrap_or_default().to_string(),
                        i["status"].as_str().unwrap_or_default().to_string(),
                        String::new(),
                        if i["blocked"].as_bool() == Some(true) {
                            "blocked".to_string()
                        } else {
                            String::new()
                        },
                        i["title"].as_str().unwrap_or_default().to_string(),
                    ]);
                }
                println!();
                print_table(&rows);
            }
        }
    }
    Ok(0)
}

fn print_milestones_table(rows: &[Value]) {
    if rows.is_empty() {
        eprintln!(
            "no milestones — declare them in <pm>/<project>/PROJECT.md or set \
             `cadence issue set <ID> milestone=m1`"
        );
        return;
    }
    let mut table = vec![[
        "PROJECT",
        "MILESTONE",
        "STATUS",
        "OWNER",
        "TARGET",
        "PROGRESS",
        "EPICS",
        "ISSUES",
        "HEALTH",
        "TITLE",
    ]
    .map(str::to_string)
    .to_vec()];
    for r in rows {
        let p = &r["progress"];
        table.push(vec![
            r["project"].as_str().unwrap_or_default().to_string(),
            r["id"].as_str().unwrap_or_default().to_string(),
            r["status"].as_str().unwrap_or("undefined").to_string(),
            r["owner"].as_str().unwrap_or("-").to_string(),
            r["target_date"].as_str().unwrap_or("-").to_string(),
            format!(
                "{}/{} {:.0}%",
                p["done_weight"].as_u64().unwrap_or(0),
                p["total_weight"].as_u64().unwrap_or(0),
                p["ratio"].as_f64().unwrap_or(0.0) * 100.0
            ),
            r["epics"].as_array().map_or(0, Vec::len).to_string(),
            r["issues"].as_array().map_or(0, Vec::len).to_string(),
            r["health"]["state"]
                .as_str()
                .unwrap_or("-")
                .replace('_', " "),
            r["title"].as_str().unwrap_or("-").to_string(),
        ]);
    }
    print_table(&table);
}
