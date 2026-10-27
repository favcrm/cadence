//! The board actions of the issue CLI.

use super::*;
use std::path::Path;

pub(crate) fn run(action: &IssueAction, state_dir: &Path) -> Result<i32> {
    match action {
        IssueAction::Ls {
            project,
            status,
            tags,
            epic,
            owner,
            component,
            priority,
            types,
            milestone,
            stage,
            health,
            plan,
            since,
            until,
            open,
            ready,
            blocked,
            at,
            sort,
            limit,
            fields,
            summary,
            json: json_flag,
        } => {
            // CAD-877: a table only on a TTY; piped output is JSON.
            let json_flag = &(*json_flag || !crate::output::stdout_table());
            crate::filter::fields_need_json(fields, *json_flag)?;
            if *summary && !*json_flag {
                return Err(Error::rejected(
                    "--summary applies to JSON output — add --json",
                ));
            }
            if *summary && !fields.is_empty() {
                return Err(Error::rejected(
                    "--fields selects row keys — there are no rows under --summary",
                ));
            }
            // `--plan` is the one flag without a comma delimiter
            // (its value is optional), so comma-join it by hand.
            let plans: Vec<String> = plan
                .iter()
                .flat_map(|v| v.split(',').map(str::to_string))
                .filter(|v| !v.is_empty())
                .collect();
            let filter = board::Filter {
                tags: tags.clone(),
                epics: epic.clone(),
                owners: owner.clone(),
                statuses: status.clone(),
                components: component.clone(),
                priorities: priority.clone(),
                types: types.clone(),
                milestones: milestone.clone(),
                plans,
                open: *open,
            };
            filter.validate()?;
            let pm = open_pm()?;
            // An unknown key must not read as an empty project that
            // invites `issue new` into the wrong place. `--at` may name
            // a project that existed only in history, so it is exempt.
            if at.is_none() && !project.is_empty() {
                let keys: Vec<String> =
                    project::list(&pm.dir)?.into_iter().map(|p| p.key).collect();
                for want in project {
                    if !keys.iter().any(|k| k == want) {
                        return Err(Error::rejected(format!(
                            "unknown project '{want}' — known: {}",
                            keys.join(", ")
                        )));
                    }
                }
            }
            let now = crate::issue::time::now_epoch();
            let since = since
                .as_deref()
                .map(|s| crate::filter::parse_time("since", s, now))
                .transpose()?;
            let until = until
                .as_deref()
                .map(|s| crate::filter::parse_time("until", s, now))
                .transpose()?;
            // Stages and health are computed from the live PROJECT.md —
            // a `--at` export has neither its config nor its clock.
            if at.is_some() && (!stage.is_empty() || !health.is_empty()) {
                return Err(Error::rejected(
                    "--stage/--health describe the live board — not under --at",
                ));
            }
            let sort_spec = sort.clone().unwrap_or_else(|| "id".to_string());
            const LS_SORTS: &[&str] = &[
                "id", "created", "updated", "priority", "status", "owner", "title", "project",
            ];
            let (sort_key, sort_desc) = match sort_spec.strip_prefix('-') {
                Some(k) => (k.to_string(), true),
                None => (sort_spec.clone(), false),
            };
            if !LS_SORTS.contains(&sort_key.as_str()) {
                return Err(crate::filter::unknown("sort", &sort_spec, LS_SORTS));
            }
            let need_updated =
                since.is_some() || until.is_some() || sort_key == "updated" || *json_flag;
            let (views, at_meta) = match at {
                Some(rev) => {
                    let (meta, views) = history::ls_at(&pm.dir, rev, None)?;
                    (views, Some(meta))
                }
                None => {
                    let issues = board::load_all(&pm.dir, None)?;
                    let jobs = board::fetch_job_outcomes(state_dir);
                    (
                        board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs),
                        None,
                    )
                }
            };
            let updated = if need_updated {
                board::updated_map(&pm.dir, at.as_deref())
            } else {
                Default::default()
            };
            let updated_of = |v: &board::View| -> Option<i64> {
                updated
                    .get(&v.issue.front.id)
                    .copied()
                    .or_else(|| crate::issue::time::parse_iso(&v.issue.front.created))
            };
            let all = views;
            let mut views: Vec<&board::View> = all.iter().collect();
            views.retain(|v| filter.matches(v));
            views.retain(|v| project.is_empty() || project.contains(&v.issue.project));
            if let Some(s) = since {
                views.retain(|v| updated_of(v).is_some_and(|t| t >= s));
            }
            if let Some(u) = until {
                views.retain(|v| updated_of(v).is_some_and(|t| t <= u));
            }
            if *ready {
                views.retain(|v| v.ready);
            }
            if *blocked {
                views.retain(|v| v.blocked);
            }
            // Stage/health filters run on the work block — one ctx.
            let by_id: std::collections::HashMap<String, &board::View> =
                all.iter().map(|v| (v.issue.front.id.clone(), v)).collect();
            let want_work = !stage.is_empty()
                || !health.is_empty()
                || (*json_flag && !*summary && at_meta.is_none());
            let ctx = want_work
                .then(|| work::Ctx::new(&pm.dir, &by_id, now, &work::fetch_approvals(state_dir)));
            if let Some(ctx) = &ctx {
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
                    views.retain(|v| {
                        let w = work::item_json(ctx, v);
                        let id = w["stage"]["id"].as_str().unwrap_or_default();
                        stage.iter().any(|s| s == id)
                    });
                }
                if !health.is_empty() {
                    crate::filter::check_set("health", health, work::HEALTH_STATES)?;
                    views.retain(|v| {
                        let w = work::item_json(ctx, v);
                        let state = w["health"]["state"].as_str().unwrap_or_default();
                        health.iter().any(|h| h == state)
                    });
                }
            }
            // `--sort` orders the surviving rows; the id key ties break
            // on so the order is always total.
            views.sort_by(|a, b| {
                let (fa, fb) = (&a.issue.front, &b.issue.front);
                let ord = match sort_key.as_str() {
                    "created" => fa.created.cmp(&fb.created),
                    "updated" => updated_of(a).cmp(&updated_of(b)),
                    "priority" => fa.priority.cmp(&fb.priority),
                    "status" => a.status.cmp(&b.status),
                    "owner" => fa.owner.cmp(&fb.owner),
                    "title" => fa.title.cmp(&fb.title),
                    "project" => a.issue.project.cmp(&b.issue.project),
                    _ => board::natural_key(&fa.id).cmp(&board::natural_key(&fb.id)),
                };
                let ord =
                    ord.then_with(|| board::natural_key(&fa.id).cmp(&board::natural_key(&fb.id)));
                if sort_desc {
                    ord.reverse()
                } else {
                    ord
                }
            });
            if *summary {
                // The rollup reads the whole filtered set — `--limit`
                // is a row-emitter flag and does not truncate counts.
                let mut out = ls_summary(&views);
                if let Some(meta) = at_meta {
                    out["at"] = meta;
                }
                print_json(&out);
                return Ok(0);
            }
            crate::filter::apply_limit(&mut views, *limit);
            if *json_flag {
                let mut rows: Vec<serde_json::Value> = match &ctx {
                    Some(ctx) => views
                        .iter()
                        .map(|v| {
                            let mut c = work::card_json(ctx, v);
                            c["updated"] = updated_of(v).map(crate::issue::time::iso).into();
                            c
                        })
                        .collect(),
                    None => views
                        .iter()
                        .map(|v| {
                            let mut c = board::card_json(v);
                            c["updated"] = updated_of(v).map(crate::issue::time::iso).into();
                            c
                        })
                        .collect(),
                };
                crate::filter::apply_fields(&mut rows, fields)?;
                let mut out = json!({ "issues": rows });
                if let Some(meta) = at_meta {
                    out["at"] = meta;
                }
                print_json(&out);
            } else {
                if let Some(meta) = &at_meta {
                    eprintln!(
                        "at {} {}",
                        meta["sha"].as_str().unwrap_or("?"),
                        meta["time"].as_str().unwrap_or("?")
                    );
                }
                print_ls_table(&views);
            }
            Ok(0)
        }
        IssueAction::Log { id, limit } => {
            let pm = open_pm()?;
            let issue = board::find_issue(&pm.dir, id)?;
            print_json(&json!({
                "id": issue.front.id,
                "history": history::log(&pm.dir, &issue, *limit)?,
            }));
            Ok(0)
        }
        IssueAction::Diff { id, rev, to } => {
            let pm = open_pm()?;
            let issue = board::find_issue(&pm.dir, id)?;
            print_json(&history::diff(
                &pm.dir,
                &issue,
                rev.as_deref(),
                to.as_deref(),
            )?);
            Ok(0)
        }
        IssueAction::Blame { id } => {
            let pm = open_pm()?;
            let issue = board::find_issue(&pm.dir, id)?;
            print_json(&history::blame(&pm.dir, &issue)?);
            Ok(0)
        }
        IssueAction::Trailer { id } => {
            let pm = open_pm()?;
            let issue = board::find_issue(&pm.dir, id)?;
            println!("Issue: {}", issue.front.id);
            Ok(0)
        }
        IssueAction::Retro { id, json } => {
            let pm = open_pm()?;
            let v = retro::run(&pm.dir, &pm.config.notes_dir(), state_dir, id)?;
            if *json {
                print_json(&v);
            } else {
                print!("{}", retro::render(&v));
            }
            Ok(0)
        }
        IssueAction::Show { id, json } => {
            let pm = open_pm()?;
            model::check_id(id)?;
            let issues = board::load_all(&pm.dir, None)?;
            let jobs = board::fetch_job_outcomes(state_dir);
            let views = board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs);
            let by_id: std::collections::HashMap<String, &board::View> = views
                .iter()
                .map(|v| (v.issue.front.id.clone(), v))
                .collect();
            let view = by_id.get(id).ok_or_else(|| {
                Error::rejected(format!(
                    "Unknown issue '{id}' — `cadence issue ls` lists what exists"
                ))
            })?;
            let ctx = work::Ctx::new(
                &pm.dir,
                &by_id,
                crate::issue::time::now_epoch(),
                &work::fetch_approvals(state_dir),
            );
            if *json {
                print_json(&work::detail_json(&pm.dir, &ctx, view));
            } else {
                print_show(view, &by_id);
                let w = work::item_json(&ctx, view);
                let mut line = format!("type: {}", w["type"].as_str().unwrap_or("?"));
                if let Some(m) = w["milestone"].as_str() {
                    line.push_str(&format!("  milestone: {m}"));
                }
                if let Some(size) = w["size"].as_str() {
                    line.push_str(&format!("  size: {size}"));
                }
                println!("\n{line}");
                print_stage_and_health(&w);
            }
            Ok(0)
        }
        IssueAction::Acceptance { id, from } => {
            let pm = open_pm()?;
            print_json(&write::set_acceptance(&pm, id, from, "")?);
            Ok(0)
        }
        IssueAction::Set {
            args,
            if_rev,
            force,
        } => {
            // Ids never contain `=`, pairs always do.
            let split = args
                .iter()
                .position(|a| a.contains('='))
                .unwrap_or(args.len());
            let (ids, pairs) = args.split_at(split);
            if let Some(stray) = pairs.iter().find(|p| !p.contains('=')) {
                return Err(Error::rejected(format!(
                    "'{stray}' after a key=value pair — ids come first: \
                     `cadence issue set CAD-16 CAD-17 status=ready`"
                )));
            }
            let pm = open_pm()?;
            let out =
                write::set_fields_if_rev(&pm, ids, pairs, "", force.as_deref(), if_rev.as_deref())?;
            print_json(&out);
            // A checked conflict is "not written", not a failure — but
            // the caller needs a nonzero exit to branch on, the same
            // `conflict` code the daemon write API uses.
            if out.get("conflict").is_some() {
                return Ok(5);
            }
            // The post-merge reminder: a done issue with an open
            // worktree ref still holds the tree — finish it (or sweep
            // every merged one with `issue finish --merged`).
            if let Some(ids) = out["worktree_open"].as_array() {
                for id in ids.iter().filter_map(|i| i.as_str()) {
                    eprintln!("{id}: worktree open: run cadence issue finish {id}");
                }
            }
            Ok(0)
        }
        IssueAction::Tag { args } => {
            let Some(split) = args.iter().position(|a| a == "add" || a == "rm") else {
                return Err(Error::rejected(
                    "tag needs add or rm — `cadence issue tag CAD-16 CAD-17 add ui`",
                ));
            };
            let (ids, rest) = args.split_at(split);
            let pm = open_pm()?;
            print_json(&write::tag_edit(
                &pm,
                ids,
                rest[0] == "add",
                &rest[1..],
                "",
            )?);
            Ok(0)
        }

        _ => unreachable!("board"),
    }
}
