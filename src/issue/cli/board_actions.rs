//! The board actions of the issue CLI.

use super::*;
use std::collections::HashMap;
use std::path::Path;

struct LsArgs<'a> {
    project: &'a [String],
    status: &'a [String],
    tags: &'a [String],
    epic: &'a [String],
    owner: &'a [String],
    component: &'a [String],
    priority: &'a [String],
    types: &'a [String],
    milestone: &'a [String],
    stage: &'a [String],
    health: &'a [String],
    plan: &'a [String],
    since: Option<&'a str>,
    until: Option<&'a str>,
    open: bool,
    ready: bool,
    blocked: bool,
    at: Option<&'a str>,
    sort: Option<&'a str>,
    limit: Option<usize>,
    fields: &'a [String],
    summary: bool,
    json: bool,
}

struct LsSort {
    key: String,
    desc: bool,
}

struct LsLoaded {
    views: Vec<board::View>,
    at_meta: Option<serde_json::Value>,
    job_state: Option<board::JobState>,
}

struct LsOutputCtx<'a> {
    args: &'a LsArgs<'a>,
    json_flag: bool,
    ctx: Option<&'a work::Ctx<'a>>,
    updated: &'a HashMap<String, i64>,
    at_meta: Option<serde_json::Value>,
    job_state: Option<&'a board::JobState>,
}

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
            json,
        } => run_ls(
            &LsArgs {
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
                since: since.as_deref(),
                until: until.as_deref(),
                open: *open,
                ready: *ready,
                blocked: *blocked,
                at: at.as_deref(),
                sort: sort.as_deref(),
                limit: *limit,
                fields,
                summary: *summary,
                json: *json,
            },
            state_dir,
        ),
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
            crate::output::print_format_line(format_args!("Issue: {}", issue.front.id));
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
        IssueAction::Show { id, json } => run_show(id, *json, state_dir),
        IssueAction::Acceptance { id, from, if_rev } => {
            if if_rev.is_some() {
                return Err(Error::usage("issue acceptance --if-rev is hosted-only"));
            }
            let pm = open_pm()?;
            print_json(&write::set_acceptance(&pm, id, from, "")?);
            Ok(0)
        }
        IssueAction::Set {
            args,
            if_rev,
            force,
        } => run_set(args, if_rev.as_deref(), force.as_deref()),
        IssueAction::Tag { args } => run_tag(args),
        _ => unreachable!("board"),
    }
}

fn updated_of(updated: &HashMap<String, i64>, v: &board::View) -> Option<i64> {
    updated
        .get(&v.issue.front.id)
        .copied()
        .or_else(|| crate::issue::time::parse_iso(&v.issue.front.created))
}

fn validate_ls_projects(pm: &crate::issue::Pm, project: &[String], at: Option<&str>) -> Result<()> {
    // An unknown key must not read as an empty project that
    // invites `issue new` into the wrong place. `--at` may name
    // a project that existed only in history, so it is exempt.
    if at.is_none() && !project.is_empty() {
        let keys: Vec<String> = project::list(&pm.dir)?.into_iter().map(|p| p.key).collect();
        for want in project {
            if !keys.iter().any(|k| k == want) {
                return Err(Error::rejected(format!(
                    "unknown project '{want}' — known: {}",
                    keys.join(", ")
                )));
            }
        }
    }
    Ok(())
}

fn parse_ls_sort(sort: Option<&str>) -> Result<LsSort> {
    let sort_spec = sort.unwrap_or("id");
    const LS_SORTS: &[&str] = &[
        "id", "created", "updated", "priority", "status", "owner", "title", "project",
    ];
    let (key, desc) = match sort_spec.strip_prefix('-') {
        Some(k) => (k.to_string(), true),
        None => (sort_spec.to_string(), false),
    };
    if !LS_SORTS.contains(&key.as_str()) {
        return Err(crate::filter::unknown("sort", sort_spec, LS_SORTS));
    }
    Ok(LsSort { key, desc })
}

fn load_ls_views(
    pm: &crate::issue::Pm,
    args: &LsArgs<'_>,
    state_dir: &Path,
    json_flag: bool,
) -> Result<LsLoaded> {
    match args.at {
        Some(rev) => {
            let (meta, views) = history::ls_at(&pm.dir, rev, None)?;
            Ok(LsLoaded {
                views,
                at_meta: Some(meta),
                job_state: None,
            })
        }
        None => {
            let state = board::fetch_job_state(state_dir);
            // A readiness/blocked filter is a decision, not a
            // display: unknown job state cannot decide it, and
            // an empty page would read as "nothing is ready".
            if args.ready || args.blocked {
                state.require(if args.ready {
                    "issue ls --ready"
                } else {
                    "issue ls --blocked"
                })?;
            }
            // Comment bodies and artifact sizes are only read
            // for a document that can emit `counts`; every
            // other list reads fronts and bodies for derivation.
            let needs_counts = json_flag
                && !args.summary
                && (args.fields.is_empty() || args.fields.iter().any(|f| f == "counts"));
            let issues = if needs_counts {
                board::load_all(&pm.dir, None)?
            } else {
                board::load_all_for_detail(&pm.dir, None, None)?
            };
            let views = board::views_with_job_state(&pm.config.notes_dir(), issues, &state);
            Ok(LsLoaded {
                views,
                at_meta: None,
                job_state: Some(state),
            })
        }
    }
}

fn apply_work_filters(
    views: &mut Vec<&board::View>,
    ctx: &work::Ctx<'_>,
    stage: &[String],
    health: &[String],
) -> Result<()> {
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
    Ok(())
}

fn sort_ls_views(views: &mut [&board::View], sort: &LsSort, updated: &HashMap<String, i64>) {
    // `--sort` orders the surviving rows; the id key ties break
    // on so the order is always total.
    views.sort_by(|a, b| {
        let (fa, fb) = (&a.issue.front, &b.issue.front);
        let ord = match sort.key.as_str() {
            "created" => fa.created.cmp(&fb.created),
            "updated" => updated_of(updated, a).cmp(&updated_of(updated, b)),
            "priority" => fa.priority.cmp(&fb.priority),
            "status" => a.status.cmp(&fb.status),
            "owner" => fa.owner.cmp(&fb.owner),
            "title" => fa.title.cmp(&fb.title),
            "project" => a.issue.project.cmp(&b.issue.project),
            _ => board::natural_key(&fa.id).cmp(&board::natural_key(&fb.id)),
        };
        let ord = ord.then_with(|| board::natural_key(&fa.id).cmp(&board::natural_key(&fb.id)));
        if sort.desc {
            ord.reverse()
        } else {
            ord
        }
    });
}

fn emit_ls_output(out_ctx: LsOutputCtx<'_>, views: &mut Vec<&board::View>) -> Result<i32> {
    if out_ctx.args.summary {
        // The rollup reads the whole filtered set — `--limit`
        // is a row-emitter flag and does not truncate counts.
        let mut out = ls_summary(views);
        if let Some(meta) = out_ctx.at_meta {
            out["at"] = meta;
        }
        if let Some(state) = out_ctx.job_state {
            out["job_state"] = state.json();
        }
        print_json(&out);
        return Ok(0);
    }
    crate::filter::apply_limit(views, out_ctx.args.limit);
    if out_ctx.json_flag {
        let mut rows: Vec<serde_json::Value> = match out_ctx.ctx {
            Some(ctx) => views
                .iter()
                .map(|v| {
                    let mut c = work::card_json(ctx, v);
                    c["updated"] = updated_of(out_ctx.updated, v)
                        .map(crate::issue::time::iso)
                        .into();
                    c
                })
                .collect(),
            None => views
                .iter()
                .map(|v| {
                    let mut c = board::card_json(v);
                    c["updated"] = updated_of(out_ctx.updated, v)
                        .map(crate::issue::time::iso)
                        .into();
                    c
                })
                .collect(),
        };
        crate::filter::apply_fields(&mut rows, out_ctx.args.fields)?;
        let mut out = json!({ "issues": rows });
        if let Some(meta) = out_ctx.at_meta {
            out["at"] = meta;
        }
        if let Some(state) = out_ctx.job_state {
            out["job_state"] = state.json();
        }
        print_json(&out);
    } else {
        if let Some(meta) = &out_ctx.at_meta {
            eprintln!(
                "at {} {}",
                meta["sha"].as_str().unwrap_or("?"),
                meta["time"].as_str().unwrap_or("?")
            );
        }
        if let Some(state) = out_ctx.job_state {
            if let Some(reason) = state.reason() {
                eprint_line(&format!(
                    "job state {} ({reason}); readiness cannot be established",
                    state.state_name()
                ));
            }
        }
        print_ls_table(views);
    }
    Ok(0)
}

fn run_ls(args: &LsArgs<'_>, state_dir: &Path) -> Result<i32> {
    // CAD-877: a table only on a TTY; piped output is JSON.
    let json_flag = args.json || !crate::output::stdout_table();
    crate::filter::fields_need_json(args.fields, json_flag)?;
    if args.summary && !json_flag {
        return Err(Error::rejected(
            "--summary applies to JSON output — add --json",
        ));
    }
    if args.summary && !args.fields.is_empty() {
        return Err(Error::rejected(
            "--fields selects row keys — there are no rows under --summary",
        ));
    }
    // `--plan` is the one flag without a comma delimiter
    // (its value is optional), so comma-join it by hand.
    let plans: Vec<String> = args
        .plan
        .iter()
        .flat_map(|v| v.split(',').map(str::to_string))
        .filter(|v| !v.is_empty())
        .collect();
    let filter = board::Filter {
        tags: args.tags.to_vec(),
        epics: args.epic.to_vec(),
        owners: args.owner.to_vec(),
        statuses: args.status.to_vec(),
        components: args.component.to_vec(),
        priorities: args.priority.to_vec(),
        types: args.types.to_vec(),
        milestones: args.milestone.to_vec(),
        plans,
        open: args.open,
    };
    filter.validate()?;
    let pm = open_pm()?;
    validate_ls_projects(&pm, args.project, args.at)?;
    let now = crate::issue::time::now_epoch();
    let since = args
        .since
        .map(|s| crate::filter::parse_time("since", s, now))
        .transpose()?;
    let until = args
        .until
        .map(|s| crate::filter::parse_time("until", s, now))
        .transpose()?;
    // Stages and health are computed from the live PROJECT.md —
    // a `--at` export has neither its config nor its clock.
    if args.at.is_some() && (!args.stage.is_empty() || !args.health.is_empty()) {
        return Err(Error::rejected(
            "--stage/--health describe the live board — not under --at",
        ));
    }
    let sort = parse_ls_sort(args.sort)?;
    // `--updated` is a full-history `git log`; pay for it only
    // when the output reads it — a since/until window, an
    // `updated` sort, or a projection that still carries the
    // field. No cutoff: the walk is skipped, never truncated.
    let need_updated = since.is_some()
        || until.is_some()
        || sort.key == "updated"
        || (json_flag
            && !args.summary
            && (args.fields.is_empty() || args.fields.iter().any(|f| f == "updated")));
    let loaded = load_ls_views(&pm, args, state_dir, json_flag)?;
    let updated = if need_updated {
        board::updated_map(&pm.dir, args.at)
    } else {
        Default::default()
    };
    let all = loaded.views;
    let mut views: Vec<&board::View> = all.iter().collect();
    views.retain(|v| filter.matches(v));
    views.retain(|v| args.project.is_empty() || args.project.contains(&v.issue.project));
    if let Some(s) = since {
        views.retain(|v| updated_of(&updated, v).is_some_and(|t| t >= s));
    }
    if let Some(u) = until {
        views.retain(|v| updated_of(&updated, v).is_some_and(|t| t <= u));
    }
    if args.ready {
        views.retain(|v| v.ready);
    }
    if args.blocked {
        views.retain(|v| v.blocked);
    }
    // Stage/health filters run on the work block — one ctx. A
    // JSON projection that drops `work` needs none of it, so
    // the per-row health/progress build is skipped with the
    // approvals read it would have made.
    let by_id: HashMap<String, &board::View> =
        all.iter().map(|v| (v.issue.front.id.clone(), v)).collect();
    let want_work = !args.stage.is_empty()
        || !args.health.is_empty()
        || (json_flag
            && !args.summary
            && loaded.at_meta.is_none()
            && (args.fields.is_empty() || args.fields.iter().any(|f| f == "work")));
    let ctx =
        want_work.then(|| work::Ctx::new(&pm.dir, &by_id, now, &work::fetch_approvals(state_dir)));
    if let Some(ctx) = &ctx {
        apply_work_filters(&mut views, ctx, args.stage, args.health)?;
    }
    sort_ls_views(&mut views, &sort, &updated);
    emit_ls_output(
        LsOutputCtx {
            args,
            json_flag,
            ctx: ctx.as_ref(),
            updated: &updated,
            at_meta: loaded.at_meta,
            job_state: loaded.job_state.as_ref(),
        },
        &mut views,
    )
}

fn run_show(id: &str, json: bool, state_dir: &Path) -> Result<i32> {
    let pm = open_pm()?;
    model::check_id(id)?;
    let state = board::fetch_job_state(state_dir);
    // Cross-project links still need every issue; only this
    // drawer's comments and artifacts are read.
    let issues = board::load_all_for_detail(&pm.dir, None, Some(id))?;
    let views = board::views_with_job_state(&pm.config.notes_dir(), issues, &state);
    let by_id: HashMap<String, &board::View> = views
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
    if json {
        let mut out = work::detail_json(&pm.dir, &ctx, view);
        out["job_state"] = state.json();
        print_json(&out);
    } else {
        if let Some(reason) = state.reason() {
            eprint_line(&format!(
                "job state {} ({reason}); readiness cannot be established",
                state.state_name()
            ));
        }
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

fn run_set(args: &[String], if_rev: Option<&str>, force: Option<&str>) -> Result<i32> {
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
    let out = write::set_fields_if_rev(&pm, ids, pairs, "", force, if_rev)?;
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

fn run_tag(args: &[String]) -> Result<i32> {
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
