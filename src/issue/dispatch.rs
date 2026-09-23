//! `cadence dispatch <ISSUE>` — the one-step dispatch: `issue start`
//! (idempotent) then exactly one templated kickoff message to the
//! worker, a tracker comment and a `message` ref on the issue. With
//! `--job` the kickoff goes through `job dispatch` instead, so the
//! task and the message are bound. Everything is validated before
//! anything is created — a refused dispatch leaves no worktree,
//! branch, commit or queued message behind.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::adapter::pty;
use crate::client;
use crate::error::{Error, Result};
use crate::issue::parse::AcceptanceItem;
use crate::issue::{board, claim, parse, start, write, Pm};
use crate::memory;

/// Message states that mean a dispatch is still in flight — a second
/// dispatch must not queue a duplicate while one is live.
const LIVE_MESSAGE_STATES: &[&str] = &["queued", "submitting", "running"];

pub struct DispatchArgs {
    pub to: String,
    pub note: PathBuf,
    pub name: Option<String>,
    pub base: Option<String>,
    pub repo: Option<PathBuf>,
    pub reply_to: Option<String>,
    pub summary: Option<String>,
    /// `--job` — the spec file `job_new` hashes; the kickoff then goes
    /// through `task_dispatch` rather than `agent_send`.
    pub job_spec: Option<PathBuf>,
    /// `--no-lessons` — skip project-memory injection for this send.
    pub no_lessons: bool,
    /// `--force` — dispatch even when the worker's pty pane cwd is
    /// outside the project's repos (CAD-202); recorded on the issue.
    pub force: bool,
    /// CAD-383 `--take-over <reason>` — dispatch an issue another PM or
    /// lane holds in doing/review; recorded on the issue.
    pub take_over: Option<String>,
}

/// The fixed single-line kickoff body — note path, issue id, summary
/// or title, the worktree/branch/base, the trailer and the return
/// address. Built verbatim: a title or `--summary` carrying control
/// chars produces a violating body and `check_body` refuses rather
/// than laundering it into something the operator didn't write.
fn kickoff_body(
    issue: &str,
    title: &str,
    note: &Path,
    wt_dir: &Path,
    branch: &str,
    base_sha: &str,
    reply_to: &str,
) -> String {
    let sha7: String = base_sha.chars().take(7).collect();
    format!(
        "read {} — {issue}: {title}. Your worktree exists: {} (branch \
         {branch}, base {sha7}). Commit trailer: Issue: {issue}. PR to \
         main; reply to {reply_to}.",
        note.display(),
        wt_dir.display()
    )
}

/// Most bytes a `--job` task's acceptance listing may take. The
/// daemon's kickoff inlines the task's acceptance and truncates past
/// its 4000-char ceiling; staying well under that keeps the listing
/// whole, and a longer list becomes a pointer instead.
pub(crate) const JOB_ACCEPTANCE_BUDGET: usize = 2000;

/// CAD-159: an issue's acceptance items on one line when that fits
/// `budget` bytes, else a pointer to the CAD-238 section-scoped
/// readback. `None` when there are no items.
///
/// CAD-300: each item is numbered and its text is a JSON string
/// literal — `1) [ ] "a"; 2) [x] "b"` — so text that looks like the
/// listing's own syntax (`; `, `[x]`, `2)`) stays inside its quotes
/// and cannot read as another item or a checked box.
/// [`parse_acceptance_listing`] reads it back exactly. Quoting escapes
/// every control character and U+2028/U+2029, so the result can ride a
/// single-line pty kickoff without losing them.
pub(crate) fn acceptance_listing(
    issue: &str,
    items: &[AcceptanceItem],
    budget: usize,
) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let inline = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let mark = if item.checked { 'x' } else { ' ' };
            format!("{}) [{mark}] {}", i + 1, quote_item(&item.text))
        })
        .collect::<Vec<_>>()
        .join("; ");
    Some(if inline.len() <= budget {
        inline
    } else {
        format!(
            "{} items, too long to inline — `cadence issue show {issue} --json` \
             lists them under .acceptance",
            items.len()
        )
    })
}

/// `text` as a JSON string literal that is also safe on one pty line:
/// `"` and `\` are escaped as JSON requires, tab as `\t`, and every
/// other control character (C0, DEL, C1) and the line/paragraph
/// separators U+2028/U+2029 as `\uXXXX`. All of those are in the BMP,
/// so four hex digits always suffice.
fn quote_item(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') => {
                out.push_str(&format!("\\u{:04x}", u32::from(c)));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// CAD-300: read an [`acceptance_listing`] back — the numbered,
/// JSON-quoted items at the start of `s` — returning them and the rest
/// of `s` (the kickoff's `.` onwards). `None` when `s` does not start
/// with item `1)`, or an item is malformed: a numbering gap, a checkbox
/// other than `[ ]`/`[x]`, or text that is not a JSON string literal.
pub fn parse_acceptance_listing(s: &str) -> Option<(Vec<AcceptanceItem>, &str)> {
    let mut items = vec![];
    let mut rest = s;
    loop {
        rest = rest.strip_prefix(&format!("{}) ", items.len() + 1))?;
        let checked = match rest.get(..4)? {
            "[x] " => true,
            "[ ] " => false,
            _ => return None,
        };
        rest = &rest[4..];
        if !rest.starts_with('"') {
            return None;
        }
        let mut quoted = serde_json::Deserializer::from_str(rest).into_iter::<String>();
        let text = quoted.next()?.ok()?;
        rest = &rest[quoted.byte_offset()..];
        items.push(AcceptanceItem { text, checked });
        match rest.strip_prefix("; ") {
            Some(next) if next.starts_with(&format!("{}) ", items.len() + 1)) => rest = next,
            _ => return Some((items, rest)),
        }
    }
}

/// CAD-159: the warning for an issue with no acceptance items. The PM
/// decided (2026-09-23, ADR-0002 §8.3) to warn and still dispatch
/// until live issues are backfilled; refusal is a later ticket.
fn acceptance_warning(issue: &str, dispatched: bool) -> String {
    let outcome = if dispatched {
        "Dispatched anyway."
    } else {
        "Not re-sent: this issue's kickoff is already in flight."
    };
    format!(
        "{issue} has no acceptance criteria (its ## Acceptance section has no \
         checklist items; a bare `- [ ]` does not count) — add them with \
         `cadence issue acceptance {issue} --from <file>`. {outcome}"
    )
}

/// The plain-path kickoff with the issue's acceptance appended as
/// ` Acceptance: <listing>.` — inline when it fits the 4000-char
/// body limit, else the readback pointer. When even the pointer does
/// not fit (CAD-300), the body goes out without the clause rather than
/// being refused; the dispatch JSON still carries the items.
fn with_acceptance(body: String, issue: &str, items: &[AcceptanceItem]) -> String {
    const FRAME: usize = " Acceptance: .".len();
    let budget = 4000usize.saturating_sub(body.len() + FRAME);
    match acceptance_listing(issue, items, budget) {
        Some(listing) if body.len() + FRAME + listing.len() <= 4000 => {
            format!("{body} Acceptance: {listing}.")
        }
        _ => body,
    }
}

/// The same rules the pty endpoint enforces pre-write: 1–4000 chars,
/// no control characters (single line), and no leading character the
/// provider's TUI treats as a command. Checked here so a bad body
/// refuses before `issue start` creates anything.
fn check_body(body: &str, provider: &str) -> Result<()> {
    if body.is_empty() || body.len() > 4000 {
        return Err(Error::rejected("Dispatch body must be 1–4000 characters"));
    }
    if pty::has_control_chars(body) {
        return Err(Error::rejected(
            "Dispatch body must be a single line without control characters",
        ));
    }
    if let Some(prefix) = body.trim_start().chars().next() {
        if pty::forbidden_prefixes(provider).contains(&prefix) {
            return Err(Error::rejected(format!(
                "Dispatch body starts with '{prefix}', which {provider} \
                 treats as a command or mode switch — refusing to send it"
            )));
        }
    }
    Ok(())
}

/// CAD-202: a pty worker's live pane cwd must be a directory that
/// still exists and lies inside one of the issue project's repos (a
/// linked worktree under `<repo>/.cadence/wt/` counts) — otherwise the
/// issue's work would run nowhere, or in another project. Reads the
/// `pane_cwd` fact `agent show` reports; a worker with no live pane
/// (or a non-pty endpoint) is not checked here. A deleted cwd always
/// refuses (`cwd_deleted` — delivery would refuse anyway); a foreign
/// one refuses (`cwd_outside_project`) unless `force`, which returns
/// the override note the caller records.
pub(crate) fn check_lane_cwd(
    project: &crate::issue::project::Project,
    worker: &str,
    agent: &Value,
    force: bool,
) -> Result<Option<String>> {
    if agent["endpoint_kind"].as_str() != Some("pty") {
        return Ok(None);
    }
    let Some(path) = agent["pane_cwd"]["path"].as_str() else {
        return Ok(None);
    };
    if agent["pane_cwd"]["deleted"].as_bool() == Some(true) {
        return Err(Error::invalid(
            "cwd_deleted",
            format!(
                "cwd_deleted: worker '{worker}' has a deleted working directory ({path}) — its \
                 pane cannot run this work; re-home the lane (`cadence agent stop \
                 {worker}`, fix its cwd, resume) before dispatching"
            ),
        ));
    }
    let repos = start::declared_repos(project);
    if repos.is_empty() {
        return Ok(None);
    }
    let cwd = Path::new(path)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(path));
    if repos.iter().any(|repo| cwd.starts_with(repo)) {
        return Ok(None);
    }
    let listed = repos
        .iter()
        .map(|r| r.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if !force {
        return Err(Error::invalid(
            "cwd_outside_project",
            format!(
                "cwd_outside_project: worker '{worker}' pane cwd {} is outside every repo of project \
                 {} ({listed}) — its work would run in the wrong repo; re-home the \
                 lane, or pass --force to dispatch anyway (recorded on the issue)",
                cwd.display(),
                project.key
            ),
        ));
    }
    Ok(Some(format!(
        "Dispatch override (--force, cwd_outside_project): {worker}'s pane cwd {} \
         is outside every repo of project {} ({listed})",
        cwd.display(),
        project.key
    )))
}

/// `dispatch <ISSUE> --to <worker> --note <path> [--job --spec f]`.
pub fn run(pm: &Pm, id: &str, args: &DispatchArgs, actor: &str, state_dir: &Path) -> Result<Value> {
    let (project, dir) = write::issue_dir(pm, id)?;
    let (front, body) = write::load_front(&dir)?;
    // CAD-159: the CAD-238 section-scoped readback. No items warns;
    // it never refuses (PM decision, ADR-0002 §8.3).
    let items = parse::acceptance_items(&body);
    let acceptance_warning = items
        .is_empty()
        .then(|| acceptance_warning(&front.id, true));
    let acceptance = json!({
        "items": items.len(),
        "criteria": items.iter().map(AcceptanceItem::to_json).collect::<Vec<_>>(),
        "warning": acceptance_warning,
    });
    let reply_to = args
        .reply_to
        .clone()
        .or_else(|| std::env::var("CADENCE_ALIAS").ok())
        .filter(|a| !a.is_empty())
        .ok_or_else(|| {
            Error::rejected("Dispatch needs a return address — pass --reply-to <alias>")
        })?;
    let note = args
        .note
        .canonicalize()
        .map_err(|_| Error::rejected(format!("Note {} is unreadable", args.note.display())))?;
    std::fs::metadata(&note)
        .map_err(|_| Error::rejected(format!("Note {} is unreadable", args.note.display())))?;
    // CAD-383: an issue someone else holds in doing/review refuses here,
    // before the daemon is asked anything. The requester is the PM
    // (`--reply-to`) and the worker; `issue start` re-checks under the
    // tracker lock and records a take-over.
    claim::check(
        &front,
        &[reply_to.as_str(), args.to.as_str()],
        args.take_over.as_deref(),
        "dispatch",
        || claim::since(&pm.dir, &project.key, &front, Duration::from_secs(2)),
    )?;

    // Pre-flight, before ANYTHING is created: the worker exists and is
    // not fenced; for --job it is the PM itself or a group member.
    let show = client::rpc(state_dir, "agent_show", json!({"alias": args.to}))?;
    let agent = &show["agent"];
    if agent["state"].as_str() == Some("attention") {
        return Err(Error::rejected(format!(
            "Worker '{}' is fenced ({}) — reconcile it before dispatching",
            args.to,
            agent["error"].as_str().unwrap_or("attention")
        )));
    }
    // CAD-202: the lane's pane must still be inside this project.
    let cwd_override = check_lane_cwd(&project, &args.to, agent, args.force)?;
    let provider = agent["provider"].as_str().unwrap_or_default().to_string();
    if args.job_spec.is_some() {
        let member = agent["alias"].as_str() == Some(reply_to.as_str())
            || agent["params"]["upstream"].as_str() == Some(reply_to.as_str());
        if !member {
            return Err(Error::rejected(format!(
                "'{}' is not in '{reply_to}'s group — the job's tasks can \
                 only bind the PM or a member of its group",
                args.to
            )));
        }
    }

    // The body is fully determined before `issue start`: `names` +
    // `resolve_base` are the exact calls start makes — when they
    // diverge from the recorded refs start refuses rather than
    // silently reusing, so what is checked here is what would be
    // sent. `--job` sends the daemon's own kickoff instead, so the
    // template check is skipped.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let body = if args.job_spec.is_none() {
        let root = start::resolve_repo(&project, args.repo.as_deref(), &cwd)?;
        let (_base, base_sha) = start::resolve_base(&root, args.base.as_deref())?;
        let (wt_name, branch) = start::names(&front.id, &front.title, args.name.as_deref())?;
        let wt_dir = root.join(".cadence").join("wt").join(&wt_name);
        let summary = args.summary.as_deref().unwrap_or(&front.title);
        let body = with_acceptance(
            kickoff_body(
                &front.id, summary, &note, &wt_dir, &branch, &base_sha, &reply_to,
            ),
            &front.id,
            &items,
        );
        check_body(&body, &provider)?;
        Some(body)
    } else {
        None
    };

    // `issue start` — idempotent; --owner <worker>, and for --job the
    // PM is the return address and the worker the assignee.
    let start_args = start::StartArgs {
        repo: args.repo.clone(),
        name: args.name.clone(),
        base: args.base.clone(),
        owner: Some(args.to.clone()),
        job: args.job_spec.as_ref().map(|spec| start::JobArgs {
            pm: reply_to.clone(),
            spec: spec.clone(),
            assignee: Some(args.to.clone()),
            force: args.force,
        }),
        by: Some(reply_to.clone()),
        take_over: args.take_over.clone(),
    };
    let started = start::run(pm, id, &start_args, actor, state_dir)?;

    // The body actually sent is rebuilt from start's returned refs and
    // re-checked: identical inputs make this a no-op, but nothing
    // unchecked ever reaches the pane.
    let body = body
        .map(|_| {
            let summary = args.summary.as_deref().unwrap_or(&front.title);
            let body = with_acceptance(
                kickoff_body(
                    &front.id,
                    summary,
                    &note,
                    Path::new(started["worktree"].as_str().unwrap_or_default()),
                    started["branch"].as_str().unwrap_or_default(),
                    started["base"]["sha"].as_str().unwrap_or_default(),
                    &reply_to,
                ),
                &front.id,
                &items,
            );
            check_body(&body, &provider).map(|_| body)
        })
        .transpose()?;

    // Duplicate dispatch: a previously recorded `message` ref still
    // live means a kickoff is in flight — reuse the worktree, queue
    // nothing, say so. The ref label names the worker it went to, so
    // a re-dispatch to a DIFFERENT worker is still caught.
    let mut live: Option<Value> = None;
    for r in front
        .refs
        .iter()
        .filter(|r| r.kind == "message" && r.closed != Some(true))
    {
        let Some(mid) = r.path.as_deref() else {
            continue;
        };
        let owner = r
            .label
            .as_deref()
            .and_then(|l| l.strip_prefix("dispatch → "))
            .unwrap_or(&args.to);
        let messages = if owner == args.to {
            Some(show.clone())
        } else {
            client::rpc(state_dir, "agent_show", json!({"alias": owner})).ok()
        };
        let found = messages.and_then(|s| {
            s["messages"].as_array().and_then(|ms| {
                ms.iter()
                    .find(|m| {
                        m["id"].as_str() == Some(mid)
                            && LIVE_MESSAGE_STATES
                                .contains(&m["state"].as_str().unwrap_or_default())
                    })
                    .cloned()
            })
        });
        if found.is_some() {
            live = found;
            break;
        }
    }
    let mut out = json!({
        "issue": front.id,
        "worker": args.to,
        "reply_to": reply_to,
        "note": note,
        "worktree": started["worktree"],
        "branch": started["branch"],
        "base": started["base"],
        "created": started["created"],
        "target_dir": started["target_dir"],
        "slot_env": started["slot_env"],
        "acceptance": acceptance,
        "claim": started["claim"],
    });
    if let Some(msg) = live {
        out["dispatched"] = json!(false);
        out["duplicate"] = json!(true);
        // CAD-300 (QA R4): a duplicate sends nothing, so its warning
        // must not say it dispatched.
        if items.is_empty() {
            out["acceptance"]["warning"] = json!(self::acceptance_warning(&front.id, false));
        }
        out["message"] = msg["id"].clone();
        out["message_state"] = msg["state"].clone();
        return Ok(out);
    }

    // Project-memory lessons: accepted memories matching the issue's
    // component/tags/recorded-commit paths and the worker's provider
    // render into `<state>/dispatch/<message>-lessons.md`, written
    // before the send so the kickoff can name it. `--job`'s kickoff
    // is daemon-templated and cannot carry the path — it is skipped.
    // Memory failures never sink the dispatch — the worktree already
    // exists by now. A malformed file, an unwritable lessons file or a
    // suffix that pushes the body over the cap degrades to no lessons
    // with `lessons_error` naming the reason.
    let mut lessons: Vec<String> = vec![];
    let mut lessons_file: Option<PathBuf> = None;
    let mut lessons_error: Option<String> = None;
    // The message id is minted up front so the `message` ref commits
    // BEFORE the send: a finish racing the dispatch sees the binding
    // the moment the ref lands. The old send→add_ref order left the
    // kickoff live but unbound for a window (CAD-107).
    let mid = Uuid::new_v4().simple().to_string();
    let mut body = body;
    if !args.no_lessons {
        if let Some(b) = body.as_mut() {
            let issue_obj = board::Issue {
                project: project.key.clone(),
                dir: dir.clone(),
                front: front.clone(),
                body: String::new(),
                comments: vec![],
                artifacts: vec![],
            };
            match memory::match_for_issue(pm, &issue_obj, Some(&provider)) {
                Err(e) => lessons_error = Some(format!("memory match failed: {e}")),
                Ok((matched, mem_errors)) => {
                    // Valid records still reach the worker; every file
                    // that failed to load is named on the issue.
                    if let Some(line) = memory::load_errors_line(&mem_errors) {
                        lessons_error = Some(line);
                    }
                    let (text, slugs) = memory::render_lessons(&matched);
                    if !slugs.is_empty() {
                        let ddir = state_dir.join("dispatch");
                        let file = ddir.join(format!("{mid}-lessons.md"));
                        // tmp + rename: a failed write never leaves a
                        // partial or empty lessons artifact behind.
                        let tmp = ddir.join(format!("{mid}-lessons.tmp"));
                        let prior = b.clone();
                        *b = format!("{prior} Lessons: {}.", file.display());
                        if let Err(e) = check_body(b, &provider) {
                            *b = prior;
                            let why =
                                format!("lessons path pushed the kickoff over the body limit: {e}");
                            lessons_error = Some(match lessons_error {
                                Some(prev) => format!("{prev}; {why}"),
                                None => why,
                            });
                        } else if let Err(e) = std::fs::create_dir_all(&ddir)
                            .and_then(|_| std::fs::write(&tmp, &text))
                            .and_then(|_| std::fs::rename(&tmp, &file))
                        {
                            let _ = std::fs::remove_file(&tmp);
                            *b = prior;
                            let why = format!("lessons file unwritable: {e}");
                            lessons_error = Some(match lessons_error {
                                Some(prev) => format!("{prev}; {why}"),
                                None => why,
                            });
                        } else {
                            lessons = slugs;
                            lessons_file = Some(file);
                        }
                    }
                }
            }
        }
    }

    // The ref lands before the send, naming the worktree the kickoff
    // runs against — a re-start under `--name` must not inherit it.
    // If the send then fails the ref stays as the attempt's history
    // (a ref id no live message ever matches blocks nothing) and the
    // failure comment below records it.
    write::add_ref(
        pm,
        id,
        "message",
        &mid,
        Some(&format!("dispatch → {}", args.to)),
        started["worktree"].as_str(),
        Some(&args.to),
        None,
        actor,
    )?;
    let send_failed = |e: Error| -> Error {
        // The ref recorded pre-send is an orphan — no live message
        // will ever carry `mid`. Close it so it stays history without
        // counting as a binding (or a mid-finish "dispatch recorded"
        // stale reason) for a concurrent finish; best-effort — the
        // send error is the one that matters.
        let _ = write::close_ref(pm, id, "message", &mid, actor);
        let _ = write::add_comment(
            pm,
            id,
            &format!("Dispatch send to {} failed: {e}", args.to),
            None,
            Some("dispatch"),
            None,
            actor,
        );
        e
    };

    // Exactly one send — plain text kickoff, or `job dispatch`'s
    // spec-bound kickoff for --job (its state lives on the task).
    let (message, sent_state) = if let Some(body) = body {
        let sent = client::rpc(
            state_dir,
            "agent_send",
            json!({"alias": args.to, "text": body, "reply_to": reply_to, "message": mid}),
        )
        .map_err(&send_failed)?;
        (
            sent["message"].as_str().unwrap_or_default().to_string(),
            ("message_state", sent["state"].clone()),
        )
    } else {
        let task = started["task"].as_str().unwrap_or_default().to_string();
        let sent = client::rpc(
            state_dir,
            "task_dispatch",
            json!({"task": task, "by": reply_to, "message": mid}),
        )
        .map_err(&send_failed)?;
        (
            sent["message"].as_str().unwrap_or_default().to_string(),
            ("task_state", sent["task"]["state"].clone()),
        )
    };
    // A same-revision `task_dispatch` retry ignores the minted id and
    // returns the still-live kickoff's id — bind THAT message too so
    // the live one is never unbound. The pre-send ref stays as the
    // attempt's history. An absent `message` in the reply is `""` —
    // never a ref target.
    if !message.is_empty() && message != mid {
        write::add_ref(
            pm,
            id,
            "message",
            &message,
            Some(&format!("dispatch → {}", args.to)),
            started["worktree"].as_str(),
            Some(&args.to),
            None,
            actor,
        )?;
    }

    // The comment rides its own commit through the existing helper. A
    // second line records which lessons were injected.
    let mut comment_text = format!("Dispatched to {}: {}", args.to, note.display());
    if !lessons.is_empty() {
        comment_text.push_str(&format!("\nLessons injected: {}", lessons.join(", ")));
    }
    // A `--job` dispatch's override is already recorded by `issue
    // start`, which re-checks the assignee; the plain path records it
    // here, with the dispatch it allowed.
    if let Some(note) = cwd_override.as_ref().filter(|_| args.job_spec.is_none()) {
        comment_text.push_str(&format!("\n{note}"));
    }
    // CAD-159: the empty-acceptance warning is recorded once, on this
    // dispatch's own comment — a duplicate run returns before here.
    if let Some(warning) = &acceptance_warning {
        comment_text.push_str(&format!("\nAcceptance warning: {warning}"));
    }
    let comment = write::add_comment(pm, id, &comment_text, None, Some("dispatch"), None, actor)?;

    // The worker's current probe verdict — pty only; the operator
    // learns whether delivery happens now or when the pane idles.
    let probe = if agent["endpoint_kind"].as_str() == Some("pty")
        && agent["dead"].as_bool() != Some(true)
    {
        client::rpc(state_dir, "agent_probe", json!({"alias": args.to})).ok()
    } else {
        None
    };

    out["dispatched"] = json!(true);
    out["message"] = json!(message);
    out[sent_state.0] = sent_state.1;
    out["lessons"] = json!(lessons);
    out["lessons_file"] = lessons_file
        .as_ref()
        .map(|p| json!(p))
        .unwrap_or(Value::Null);
    out["lessons_error"] = lessons_error
        .as_ref()
        .map(|e| json!(e))
        .unwrap_or(Value::Null);
    out["comment"] = comment["comment"].clone();
    out["job"] = started["job"].clone();
    out["task"] = started["task"].clone();
    out["probe"] = probe.unwrap_or(Value::Null);
    out["cwd_override"] = cwd_override.map(Value::from).unwrap_or(Value::Null);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kickoff() -> String {
        kickoff_body(
            "D-1",
            "Title",
            Path::new("/tmp/note.md"),
            Path::new("/r/.cadence/wt/d-1-title"),
            "cadence/d-1-title",
            "0123456789abcdef",
            "pm",
        )
    }

    /// CAD-159: an empty section and a stub-only section both yield no
    /// items — no listing, the kickoff is unchanged, and the warning
    /// names the issue and the authoring command.
    #[test]
    fn empty_and_stub_only_acceptance_has_no_listing() {
        for body in [
            "Title\n\n## Acceptance\n\n",
            "Title\n\n## Acceptance\n\n- [ ]\n- [x]   \n",
        ] {
            let items = parse::acceptance_items(body);
            assert!(items.is_empty(), "{body:?}");
            assert_eq!(acceptance_listing("D-1", &items, 4000), None);
            assert_eq!(with_acceptance(kickoff(), "D-1", &items), kickoff());
        }
        let warning = acceptance_warning("D-1", true);
        assert!(
            warning.contains("D-1")
                && warning.contains("cadence issue acceptance D-1 --from <file>"),
            "{warning}"
        );
    }

    /// Populated acceptance is listed inline, one line, numbered and
    /// quoted, checked state kept, control characters escaped.
    #[test]
    fn populated_acceptance_is_listed_inline() {
        let body = "T\n\n## Acceptance\n\n- [ ] first\tpart\n- [x] second\n\n## Notes\n- [ ] not acceptance\n";
        let items = parse::acceptance_items(body);
        assert_eq!(
            acceptance_listing("D-1", &items, 4000).as_deref(),
            Some(r#"1) [ ] "first\tpart"; 2) [x] "second""#)
        );
        let body = with_acceptance(kickoff(), "D-1", &items);
        assert!(
            body.ends_with(r#" Acceptance: 1) [ ] "first\tpart"; 2) [x] "second"."#),
            "{body}"
        );
        check_body(&body, "fake").unwrap();
    }

    /// CAD-300: item text a reader could mistake for the listing's own
    /// syntax — `; `, `[x]`, a `2)` number, quotes, backslashes — and
    /// characters a single-line pty body cannot carry (tab, ESC, DEL,
    /// NEL, U+2028/U+2029) round-trip exactly through the kickoff, which
    /// stays one line and control-character free.
    #[test]
    fn tricky_items_round_trip_through_the_kickoff() {
        let items = vec![
            AcceptanceItem {
                text: "alpha; [x] beta".into(),
                checked: false,
            },
            AcceptanceItem {
                text: r#"done; 3) [ ] "fake" \ C:\path"#.into(),
                checked: true,
            },
            AcceptanceItem {
                text: "tab\there esc\u{1b}[31m del\u{7f} nel\u{85} ls\u{2028}ps\u{2029}end".into(),
                checked: false,
            },
            AcceptanceItem {
                text: "[x] looks checked but is not".into(),
                checked: false,
            },
        ];
        let listing = acceptance_listing("D-1", &items, 4000).unwrap();
        let (back, rest) = parse_acceptance_listing(&listing)
            .unwrap_or_else(|| panic!("listing does not parse: {listing}"));
        assert_eq!(back, items, "{listing}");
        assert_eq!(rest, "", "{listing}");
        let body = with_acceptance(kickoff(), "D-1", &items);
        check_body(&body, "fake").unwrap();
        assert!(
            !body
                .chars()
                .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')),
            "{body:?}"
        );
        let tail = body.split_once(" Acceptance: ").unwrap().1;
        let (back, rest) = parse_acceptance_listing(tail).unwrap();
        assert_eq!(back, items, "{body}");
        assert_eq!(rest, ".", "{body}");
    }

    /// CAD-300 (QA R3): when even the pointer would push the kickoff
    /// past the pty limit, the kickoff is sent without the acceptance
    /// clause rather than refused — the JSON still carries the items.
    #[test]
    fn pointer_that_does_not_fit_leaves_the_kickoff_unchanged() {
        let summary = "s".repeat(3800);
        let body = kickoff_body(
            "D-1",
            &summary,
            Path::new("/tmp/note.md"),
            Path::new("/r/.cadence/wt/d-1-title"),
            "cadence/d-1-title",
            "0123456789abcdef",
            "pm",
        );
        check_body(&body, "fake").unwrap();
        let items = vec![AcceptanceItem {
            text: "y".repeat(200),
            checked: false,
        }];
        assert_eq!(with_acceptance(body.clone(), "D-1", &items), body);
    }

    /// CAD-300 (QA R4): a duplicate run dispatched nothing, so its
    /// warning must not say "Dispatched anyway".
    #[test]
    fn duplicate_warning_does_not_claim_a_dispatch() {
        let sent = acceptance_warning("D-1", true);
        let dup = acceptance_warning("D-1", false);
        assert!(sent.ends_with("Dispatched anyway."), "{sent}");
        assert!(!dup.contains("Dispatched anyway"), "{dup}");
        for w in [&sent, &dup] {
            assert!(
                w.contains("cadence issue acceptance D-1 --from <file>"),
                "{w}"
            );
        }
    }

    /// A list too long for the budget becomes a pointer to the
    /// CAD-238 readback, and the kickoff stays within the pty limit.
    #[test]
    fn oversized_acceptance_points_at_the_readback() {
        let items: Vec<AcceptanceItem> = (0..60)
            .map(|i| AcceptanceItem {
                text: format!("criterion {i} {}", "x".repeat(80)),
                checked: false,
            })
            .collect();
        let pointer = acceptance_listing("D-1", &items, JOB_ACCEPTANCE_BUDGET).unwrap();
        assert!(
            pointer.starts_with("60 items") && pointer.contains("cadence issue show D-1 --json"),
            "{pointer}"
        );
        let body = with_acceptance(kickoff(), "D-1", &items);
        assert!(
            body.ends_with(&format!(" Acceptance: {pointer}.")),
            "{body}"
        );
        check_body(&body, "fake").unwrap();
    }
}
