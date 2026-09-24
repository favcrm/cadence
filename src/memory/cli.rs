//! `cadence memory` reads the tracker directly, but all authority-bearing
//! writes are daemon RPCs. The daemon derives the native endpoint identity;
//! request aliases and operator fallbacks are never accepted.

use std::path::PathBuf;

use clap::Subcommand;
use serde_json::{json, Value};

use crate::client;
use crate::error::{Error, Result};
use crate::issue::{board, write, Pm};
use crate::memory::{self, MatchCtx, Scope};

#[derive(Subcommand)]
pub enum MemoryAction {
    /// Propose a memory through the authenticated native daemon endpoint.
    Propose {
        /// Project key (required).
        #[arg(long)]
        project: String,
        /// rule | gotcha | decision | recipe.
        #[arg(long = "type")]
        kind: String,
        /// Scope: project component the fact applies to; repeatable.
        #[arg(long = "scope-component")]
        components: Vec<String>,
        /// Scope: repo-relative glob (`src/adapter/**`); repeatable.
        #[arg(long = "scope-path")]
        paths: Vec<String>,
        /// Scope: provider name (claude, devin, …); repeatable.
        #[arg(long = "scope-provider")]
        providers: Vec<String>,
        /// Scope: tag matching an issue's tags; repeatable.
        #[arg(long = "scope-tag")]
        tags: Vec<String>,
        /// Scope: the whole project — matches every dispatch.
        #[arg(long = "scope-project")]
        project_wide: bool,
        /// Where the fact was learned — issue id, note path or commit.
        #[arg(long)]
        source: Option<String>,
        /// low | medium | high [default: medium].
        #[arg(long)]
        confidence: Option<String>,
        /// Read the fact from a file — a full memory file (frontmatter
        /// kept) or a bare body.
        #[arg(long)]
        from: Option<PathBuf>,
        /// Inline body: fact line(s) then `**Why:**`/`**How to
        /// apply:**` sections.
        #[arg(short = 'm')]
        text: Option<String>,
        /// Explicit slug — else slugified from the fact's first line.
        #[arg(long)]
        id: Option<String>,
    },
    /// Finalize a proposed memory after two distinct native PM/worker reviews
    /// from non-author, non-contributor endpoints.
    Accept {
        /// Memory slug.
        slug: String,
        /// Project key — needed only when the slug is ambiguous.
        #[arg(long)]
        project: Option<String>,
        /// Body edits are refused because they invalidate the review digest.
        #[arg(short = 'm')]
        text: Option<String>,
    },
    /// Reject a memory through an authenticated PM endpoint.
    Reject {
        /// Memory slug.
        slug: String,
        #[arg(long)]
        project: Option<String>,
    },
    /// Submit one native independent PM/worker review. The daemon derives the reviewer
    /// from the Unix socket peer; the digest must be supplied explicitly.
    Review {
        slug: String,
        #[arg(long)]
        project: Option<String>,
        /// accept or verify.
        #[arg(long)]
        operation: String,
        /// pass or revise.
        #[arg(long)]
        verdict: String,
        #[arg(long)]
        evidence: String,
        #[arg(long)]
        digest: String,
    },
    /// Supersede is refused until crash-atomic pair recovery is available.
    Supersede {
        /// Slug being replaced.
        old: String,
        /// Slug replacing it.
        new: String,
        #[arg(long)]
        project: Option<String>,
    },
    /// Finalize a fresh native verify cycle on an accepted memory.
    Verify {
        /// Memory slug.
        slug: String,
        #[arg(long)]
        project: Option<String>,
    },
    /// List memories — a compact table on a TTY, `--json` for agents.
    /// Value flags repeat and comma-join and match ANY of their values;
    /// different flags AND.
    #[command(after_long_help = crate::filter::GRAMMAR)]
    Ls {
        /// Project key; repeatable.
        #[arg(long, value_delimiter = ',')]
        project: Vec<String>,
        /// proposed | accepted | rejected | superseded; repeatable.
        #[arg(long, value_delimiter = ',')]
        status: Vec<String>,
        /// rule | gotcha | decision | recipe; repeatable.
        #[arg(long = "type", value_delimiter = ',')]
        kind: Vec<String>,
        /// Only memories applying to this component; repeatable.
        #[arg(long, value_delimiter = ',')]
        component: Vec<String>,
        /// Only memories whose path globs match this file; repeatable.
        #[arg(long, value_delimiter = ',')]
        path: Vec<String>,
        /// Accepted memories whose evidence is not current, read the
        /// way retrieval reads it: marked stale (withheld), no verify
        /// inside the window (unverified), or path globs matching files
        /// changed since the last verify in a project repo.
        #[arg(long)]
        stale: bool,
        /// Freshness window in days [default: the project's
        /// `memory.stale_days`, else 30]. --component/--path select a
        /// retrieval scope and don't apply to the stale axis.
        #[arg(long)]
        days: Option<u64>,
        /// Sort by project slug type status confidence created;
        /// `-KEY` descending. Under --stale the row set is different:
        /// project slug type reason label verified_at.
        #[arg(long, allow_hyphen_values = true)]
        sort: Option<String>,
        /// Keep only the first N rows.
        #[arg(long)]
        limit: Option<usize>,
        /// Keep only these keys in each --json row (comma-joined).
        #[arg(long, value_delimiter = ',')]
        fields: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show one memory's frontmatter and body.
    Show {
        /// Memory slug.
        slug: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Ranked accepted memories applying to a context: `--issue <ID>`
    /// (component/tags/recorded-commit paths) and/or explicit axes.
    Match {
        /// Issue id — supplies component, tags and commit paths.
        #[arg(long)]
        issue: Option<String>,
        /// Project key for explicit-axis matching; defaults to the cwd's
        /// registered project when `--issue` is absent.
        #[arg(long)]
        project: Option<String>,
        /// Provider for scope matching (the dispatch target's).
        #[arg(long)]
        provider: Option<String>,
        /// Component axis; repeatable.
        #[arg(long)]
        component: Vec<String>,
        /// Repo-relative path axis; repeatable.
        #[arg(long)]
        path: Vec<String>,
        /// Tag axis; repeatable.
        #[arg(long)]
        tag: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Lint memory files — schema, body contract, components,
    /// dangling supersedes. Non-zero exit on any error.
    Lint {
        #[arg(long)]
        project: Option<String>,
    },
}

fn open_pm() -> Result<Pm> {
    Pm::open_default()
}

fn scope_of(m: &memory::Memory) -> &Scope {
    &m.front.scope
}

/// `cadence memory …` — returns the process exit code.
pub fn run(action: &MemoryAction, state_dir: &std::path::Path) -> Result<i32> {
    match action {
        MemoryAction::Propose {
            project,
            kind,
            components,
            paths,
            providers,
            tags,
            project_wide,
            source,
            confidence,
            from,
            text,
            id,
        } => {
            let scope = Scope {
                components: components.clone(),
                paths: paths.clone(),
                providers: providers.clone(),
                tags: tags.clone(),
                project: *project_wide,
            };
            if from.is_some() && text.is_some() {
                return Err(Error::rejected("propose takes --from or -m, not both"));
            }
            let from_text = from
                .as_ref()
                .map(std::fs::read_to_string)
                .transpose()
                .map_err(|e| Error::rejected(format!("cannot read proposal source: {e}")))?;
            let out = client::rpc(
                state_dir,
                "memory_propose",
                json!({
                    "project": project,
                    "kind": kind,
                    "scope": scope,
                    "source": source,
                    "confidence": confidence,
                    "from": from_text,
                    "text": text,
                    "id": id,
                }),
            )?;
            crate::issue::cli::print_json(&out);
            Ok(0)
        }
        MemoryAction::Accept {
            slug,
            project,
            text,
        } => {
            let pm = open_pm()?;
            let (_, mem) = memory::find(&pm, project.as_deref(), slug)?;
            let mut params = json!({
                "project": project,
                "slug": slug,
                "operation": "accept",
                "digest": memory::semantic_digest(&mem),
            });
            if let Some(body) = text {
                params["body"] = json!(body);
            }
            let out = client::rpc(state_dir, "memory_finalize", params)?;
            crate::issue::cli::print_json(&out);
            Ok(0)
        }
        MemoryAction::Reject { slug, project } => {
            let out = client::rpc(
                state_dir,
                "memory_finalize",
                json!({
                    "project": project,
                    "slug": slug,
                    "operation": "reject",
                }),
            )?;
            crate::issue::cli::print_json(&out);
            Ok(0)
        }
        MemoryAction::Supersede { old, new, project } => {
            let out = client::rpc(
                state_dir,
                "memory_finalize",
                json!({
                    "project": project,
                    "old": old,
                    "new": new,
                    "operation": "supersede",
                }),
            )?;
            crate::issue::cli::print_json(&out);
            Ok(0)
        }
        MemoryAction::Verify { slug, project } => {
            let pm = open_pm()?;
            let (_, mem) = memory::find(&pm, project.as_deref(), slug)?;
            let out = client::rpc(
                state_dir,
                "memory_finalize",
                json!({
                    "project": project,
                    "slug": slug,
                    "operation": "verify",
                    "digest": memory::semantic_digest(&mem),
                }),
            )?;
            crate::issue::cli::print_json(&out);
            Ok(0)
        }
        MemoryAction::Review {
            slug,
            project,
            operation,
            verdict,
            evidence,
            digest,
        } => {
            let out = client::rpc(
                state_dir,
                "memory_review",
                json!({
                    "project": project,
                    "slug": slug,
                    "operation": operation,
                    "verdict": verdict,
                    "evidence": evidence,
                    "digest": digest,
                }),
            )?;
            crate::issue::cli::print_json(&out);
            Ok(0)
        }
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
            crate::filter::fields_need_json(fields, *as_json)?;
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
                if *as_json {
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
            if *as_json {
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
                crate::issue::cli::print_json(&memory::detail_json(&m, &fresh));
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
            component,
            path,
            tag,
            json: as_json,
        } => {
            let pm = open_pm()?;
            let (mems, ctx, load_errors, project_key) = if let Some(id) = issue {
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
                let mut ctx = memory::issue_ctx(&pm, &issue_obj, provider.as_deref())?;
                ctx.components.extend(component.clone());
                ctx.paths.extend(path.clone());
                ctx.tags.extend(tag.clone());
                let (pool, errors) = memory::load_project_report(&pm.dir, &issue_obj.project);
                let fresh = memory::Freshness::for_project(Some(&proj));
                (
                    memory::match_memories(&pool, &ctx, &fresh),
                    ctx,
                    errors,
                    issue_obj.project,
                )
            } else {
                let cwd = std::env::current_dir()?;
                let proj = crate::issue::project::resolve(&pm.dir, project.as_deref(), &cwd)?;
                let ctx = MatchCtx {
                    components: component.clone(),
                    paths: path.clone(),
                    providers: provider.clone().into_iter().collect(),
                    tags: tag.clone(),
                };
                let (pool, errors) = memory::load_project_report(&pm.dir, &proj.key);
                let fresh = memory::Freshness::for_project(Some(&proj));
                (
                    memory::match_memories(&pool, &ctx, &fresh),
                    ctx,
                    errors,
                    proj.key,
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
                        "providers": ctx.providers,
                        "tags": ctx.tags,
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
                eprintln!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
                Ok(1)
            }
        }
    }
}
