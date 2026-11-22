//! Read-only tracker inspection queries for `cadence memory`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use serde_json::{json, Value};

use super::MemoryAction;
use crate::error::{Error, Result};
use crate::issue::{board, write, Pm};
use crate::memory::{self, MatchCtx, Scope};

fn open_pm() -> Result<Pm> {
    Pm::open_default()
}

fn scope_of(m: &memory::Memory) -> &Scope {
    &m.front.scope
}

/// Dispatch read-only `cadence memory` tracker queries (`ls`, `show`, `match`, `lint`).
pub(super) fn run(action: &MemoryAction) -> Result<i32> {
    match action {
        MemoryAction::Ls {
            project,
            status,
            kind,
            component,
            path,
            stale,
            days,
            sort,
            limit,
            fields,
            json: as_json,
        } => {
            let pm = open_pm()?;
            let as_json = crate::output::json_selected(*as_json);
            crate::filter::fields_need_json(fields, as_json)?;
            crate::filter::check_set("status", status, memory::STATUSES)?;
            crate::filter::check_set("type", kind, memory::TYPES)?;
            if *stale {
                let (mut hits, errors) = memory::stale(&pm, *days);
                if let Some(line) = memory::load_errors_line(&errors) {
                    eprintln!("{line}");
                }
                // Stale rows are all `accepted` — --status proposed
                // yields nothing. --component/--path don't apply:
                // they pick the retrieval scope, not the stale axis.
                hits.retain(|h| {
                    crate::filter::any_of(project, h["project"].as_str())
                        && crate::filter::any_of(kind, h["type"].as_str())
                        && crate::filter::any_of(status, Some("accepted"))
                });
                if let Some(spec) = sort {
                    crate::filter::sort_rows(
                        &mut hits,
                        spec,
                        &[
                            ("project", "project"),
                            ("slug", "slug"),
                            ("type", "type"),
                            ("reason", "reason"),
                            ("label", "evidence.label"),
                            ("verified_at", "verified_at"),
                        ],
                        "slug",
                    )?;
                }
                crate::filter::apply_limit(&mut hits, *limit);
                if as_json {
                    crate::filter::apply_fields(&mut hits, fields)?;
                    crate::issue::cli::print_json(&json!({"stale": hits, "load_errors": errors}));
                } else if hits.is_empty() {
                    match days {
                        Some(days) => println!("no stale memories (window: {days} days)"),
                        None => println!("no stale memories (window: each project's)"),
                    }
                } else {
                    for h in &hits {
                        let changed = h["changed"]
                            .as_array()
                            .map(|c| {
                                c.iter()
                                    .filter_map(|p| p.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            })
                            .unwrap_or_default();
                        let reason = h["reason"].as_str().unwrap_or_default();
                        println!(
                            "{}/{}\t{}\t{}",
                            h["project"].as_str().unwrap_or_default(),
                            h["slug"].as_str().unwrap_or_default(),
                            h["evidence"]["label"].as_str().unwrap_or("unverified"),
                            if changed.is_empty() {
                                reason.to_string()
                            } else {
                                format!("{reason}: {changed}")
                            },
                        );
                    }
                }
                return Ok(0);
            }
            let ctx = MatchCtx {
                components: component.clone(),
                paths: path.clone(),
                providers: vec![],
                tags: vec![],
                roles: Vec::new(),
            };
            let (all, load_errors) = memory::load_all_report(&pm.dir);
            let projects = crate::issue::project::list(&pm.dir).unwrap_or_default();
            if let Some(line) = memory::load_errors_line(&load_errors) {
                eprintln!("{line}");
            }
            let mut mems = Vec::new();
            for m in all {
                if !crate::filter::any_of(project, Some(m.project.as_str()))
                    || !crate::filter::any_of(status, Some(m.front.status.as_str()))
                    || !crate::filter::any_of(kind, Some(m.front.kind.as_str()))
                {
                    continue;
                }
                // Component and path are retrieval axes — a project-wide
                // memory applies to every component/path — but different
                // flags still AND: a `--component X --path Y` row must
                // apply to both.
                let scope = scope_of(&m);
                if !component.is_empty()
                    && !scope.project
                    && !scope.components.iter().any(|c| component.contains(c))
                {
                    continue;
                }
                if !path.is_empty()
                    && !scope.project
                    && !scope
                        .paths
                        .iter()
                        .any(|g| ctx.paths.iter().any(|p| memory::glob_match(g, p)))
                {
                    continue;
                }
                mems.push(m);
            }
            let mut rows: Vec<Value> = mems
                .iter()
                .map(|m| memory::card_json(m, &memory::Freshness::among(&projects, &m.project)))
                .collect();
            if let Some(spec) = sort {
                crate::filter::sort_rows(
                    &mut rows,
                    spec,
                    &[
                        ("project", "project"),
                        ("slug", "slug"),
                        ("type", "type"),
                        ("status", "status"),
                        ("confidence", "confidence"),
                        ("created", "created"),
                    ],
                    "slug",
                )?;
            }
            crate::filter::apply_limit(&mut rows, *limit);
            if as_json {
                crate::filter::apply_fields(&mut rows, fields)?;
                crate::issue::cli::print_json(&json!({
                    "memories": rows,
                    "load_errors": load_errors,
                }));
            } else if rows.is_empty() {
                println!("no memories");
            } else {
                for r in &rows {
                    println!(
                        "{}/{}\t{}\t{}\t{}\t{}",
                        r["project"].as_str().unwrap_or_default(),
                        r["slug"].as_str().unwrap_or_default(),
                        r["status"].as_str().unwrap_or_default(),
                        r["type"].as_str().unwrap_or_default(),
                        r["confidence"].as_str().unwrap_or_default(),
                        r["fact"].as_str().unwrap_or_default(),
                    );
                }
            }
            Ok(0)
        }
        MemoryAction::Show {
            slug,
            project,
            json: as_json,
        } => {
            let pm = open_pm()?;
            let (proj, m) = memory::find(&pm, project.as_deref(), slug)?;
            if *as_json {
                let fresh = memory::Freshness::for_project(Some(&proj));
                // A local CLI read proves no operator identity, so the
                // anonymous reader is used: no access it did not already have.
                crate::issue::cli::print_json(&memory::detail_json_with_evidence(
                    &m,
                    &fresh,
                    Some(&pm),
                    None,
                ));
            } else {
                let text = std::fs::read_to_string(&m.path)?;
                println!("{text}");
            }
            Ok(0)
        }
        MemoryAction::Match {
            issue,
            project,
            provider,
            role,
            component,
            path,
            tag,
            json: as_json,
        } => {
            let pm = open_pm()?;
            let (mems, ctx, load_errors, project_key, planned_paths) = if let Some(id) = issue {
                let (proj, dir) = write::issue_dir(&pm, id)?;
                let (front, body) = write::load_front(&dir)?;
                let issue_obj = board::Issue {
                    project: proj.key.clone(),
                    dir,
                    front,
                    body,
                    comments: vec![],
                    artifacts: vec![],
                };
                // The issue path is delivery's own selection — planned
                // paths included — with the CLI's extra axes layered on,
                // so this diagnostic can never disagree with what a
                // dispatch delivers. The declared paths are reported
                // separately from the recorded/explicit ones, from the
                // same validated list selection used.
                let (mems, ctx, load_errors) = memory::issue_matching::match_for_issue_ctx(
                    &pm,
                    &issue_obj,
                    provider.as_deref(),
                    role.as_deref(),
                    |ctx| {
                        ctx.components.extend(component.clone());
                        ctx.paths.extend(path.clone());
                        ctx.tags.extend(tag.clone());
                    },
                )?;
                let planned = memory::issue_matching::issue_planned_paths(&issue_obj);
                (mems, ctx, load_errors, issue_obj.project, planned)
            } else {
                let cwd = std::env::current_dir()?;
                let proj = crate::issue::project::resolve(&pm.dir, project.as_deref(), &cwd)?;
                let ctx = MatchCtx {
                    components: component.clone(),
                    paths: path.clone(),
                    providers: provider.clone().into_iter().collect(),
                    tags: tag.clone(),
                    roles: role.clone().into_iter().collect(),
                };
                let (pool, errors) = memory::load_project_report(&pm.dir, &proj.key);
                let fresh = memory::Freshness::for_project(Some(&proj));
                (
                    memory::match_memories(&pool, &ctx, &fresh),
                    ctx,
                    errors,
                    proj.key,
                    Vec::new(),
                )
            };
            if let Some(line) = memory::load_errors_line(&load_errors) {
                eprintln!("{line}");
            }
            if *as_json {
                crate::issue::cli::print_json(&json!({
                    "matched": mems.lessons.iter().map(|m| json!({
                        "project": m.project,
                        "slug": m.front.id,
                        "type": m.front.kind,
                        "confidence": m.front.confidence,
                        "verified_at": memory::last_verified(m),
                        "evidence": mems.label(m),
                        "fact": memory::fact_line(&m.body),
                    })).collect::<Vec<_>>(),
                    "withheld": mems.withheld.iter().map(|(m, reason)| json!({
                        "project": m.project,
                        "slug": m.front.id,
                        "reason": reason,
                    })).collect::<Vec<_>>(),
                    "context": {
                        "project": project_key,
                        "components": ctx.components,
                        "paths": ctx.paths,
                        // The issue's validated planned declarations — a
                        // separate selection input, never mixed into the
                        // recorded/explicit `paths` above. Empty without
                        // `--issue`.
                        "planned_paths": planned_paths,
                        "providers": ctx.providers,
                        "tags": ctx.tags,
                        "roles": ctx.roles,
                    },
                    "load_errors": load_errors,
                }));
            } else {
                for m in &mems.lessons {
                    println!(
                        "{}/{}\t{}\t{}\t{}",
                        m.project,
                        m.front.id,
                        m.front.kind,
                        mems.label(m),
                        memory::fact_line(&m.body),
                    );
                }
                for (m, reason) in &mems.withheld {
                    println!("{}/{}\twithheld\t{reason}", m.project, m.front.id);
                }
            }
            Ok(0)
        }
        MemoryAction::Lint { project } => {
            let pm = open_pm()?;
            let mut errors = Vec::new();
            let mut warnings = Vec::new();
            for p in crate::issue::project::list(&pm.dir)? {
                if let Some(only) = project {
                    if p.key != *only {
                        continue;
                    }
                }
                let dir = memory::memory_dir(&pm, &p.key);
                if !dir.is_dir() {
                    continue;
                }
                memory::lint_dir(&dir, &p, &mut |e| errors.push(e), &mut |w| warnings.push(w));
            }
            let out = json!({
                "ok": errors.is_empty(),
                "errors": errors,
                "warnings": warnings,
            });
            if errors.is_empty() {
                crate::issue::cli::print_json(&out);
                Ok(0)
            } else {
                // The failure report precedes the nonzero status: a closed
                // stderr must not panic it into success.
                let report = serde_json::to_string_pretty(&out).unwrap_or_default();
                crate::output::eprint_line(&report);
                Ok(1)
            }
        }
        _ => Err(Error::internal("expected read memory action")),
    }
}
