//! `cadence dispatch <ISSUE>` — the one-step dispatch: `issue start`
//! (idempotent) then exactly one templated kickoff message to the
//! worker, a tracker comment and a `message` ref on the issue. With
//! `--job` the kickoff goes through `job dispatch` instead, so the
//! task and the message are bound. Everything is validated before
//! anything is created — a refused dispatch leaves no worktree,
//! branch, commit or queued message behind.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::adapter::pty;
use crate::client;
use crate::error::{Error, Result};
use crate::issue::parse::AcceptanceItem;
use crate::issue::{board, claim, parse, start, write, Pm};
use crate::memory;
use crate::store;

/// Message states that mean a dispatch is still in flight — a second
/// dispatch must not queue a duplicate while one is live.
const LIVE_MESSAGE_STATES: &[&str] = &["queued", "submitting", "running"];

pub struct DispatchArgs {
    pub to: String,
    /// The kickoff note; `None` points the worker at the ticket's own
    /// `issue.md` (CAD-339 — a plan ticket already carries its brief).
    pub note: Option<PathBuf>,
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
///
/// CAD-468: an explicitly-reported endpoint's kickoff also carries the
/// exact `cadence message result` command — the worker that missed or
/// never read its briefing still has the contract in the message that
/// opened its turn. Turn-result endpoints finish by their result text
/// and never run it, so theirs is omitted.
#[allow(clippy::too_many_arguments)]
fn kickoff_body(
    issue: &str,
    title: &str,
    note: &Path,
    wt_dir: &Path,
    branch: &str,
    base_sha: &str,
    reply_to: &str,
    provider: &str,
    endpoint_kind: &str,
) -> String {
    let sha7: String = base_sha.chars().take(7).collect();
    let report = if crate::adapter::registry::report_hint(provider, endpoint_kind)
        == crate::adapter::registry::Reporting::Explicit
    {
        " Report: `cadence message result <id> --token <turn_id> --text '<summary>'` \
         (`cadence self` prints both)."
    } else {
        ""
    };
    format!(
        "read {} — {issue}: {title}. Your worktree exists: {} (branch \
         {branch}, base {sha7}). Commit trailer: Issue: {issue}. PR to \
         main; reply to {reply_to}.{report}",
        note.display(),
        wt_dir.display()
    )
}

/// CAD-159: an issue's acceptance items on one line; `None` when there
/// are none. Always the whole list — CAD-160 removed the "N items, too
/// long to inline" pointer, since criteria are never dropped.
///
/// CAD-300: each item is numbered and its text is a JSON string
/// literal — `1) [ ] "a"; 2) [x] "b"` — so text that looks like the
/// listing's own syntax (`; `, `[x]`, `2)`) stays inside its quotes
/// and cannot read as another item or a checked box.
/// [`parse_acceptance_listing`] reads it back exactly. Quoting escapes
/// every control character and U+2028/U+2029, so the result can ride a
/// single-line pty kickoff without losing them.
pub(crate) fn acceptance_listing(items: &[AcceptanceItem]) -> Option<String> {
    (!items.is_empty()).then(|| inline_listing(items))
}

/// The CAD-300 listing of `items` at any length —
/// `1) [ ] "a"; 2) [x] "b"`, numbered from 1.
pub(crate) fn inline_listing(items: &[AcceptanceItem]) -> String {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let mark = if item.checked { 'x' } else { ' ' };
            format!("{}) [{mark}] {}", i + 1, quote_item(&item.text))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// CAD-160: the still-unchecked criteria in a task's stored
/// acceptance. A whole CAD-300 listing yields its `[ ]` items; any
/// other non-blank text (a free-form `--accept`) is one unchecked item.
pub(crate) fn outstanding_items(acceptance: Option<&str>) -> Vec<AcceptanceItem> {
    let Some(text) = acceptance.map(str::trim).filter(|t| !t.is_empty()) else {
        return vec![];
    };
    match parse_acceptance_listing(text) {
        Some((items, "")) => items.into_iter().filter(|i| !i.checked).collect(),
        _ => vec![AcceptanceItem {
            text: text.to_string(),
            checked: false,
        }],
    }
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

/// The plain-path kickoff with every one of the issue's acceptance
/// items appended as ` Acceptance: <listing>.` — never a pointer, never
/// cut (CAD-160). [`plain_kickoff`] fits the result to the pty ceiling.
fn with_acceptance(body: String, items: &[AcceptanceItem]) -> String {
    match acceptance_listing(items) {
        Some(listing) => format!("{body} Acceptance: {listing}."),
        None => body,
    }
}

/// CAD-160: the plain kickoff fitted to the pty ceiling. Prose gives
/// way — the title (or `--summary`) is shortened and ends in `…` — and
/// the acceptance items never do. When they cannot fit even with the
/// title cut to nothing, the dispatch refuses naming the ceiling and
/// the note; it runs before `issue start`, so nothing is created.
#[allow(clippy::too_many_arguments)]
fn plain_kickoff(
    issue: &str,
    title: &str,
    note: &Path,
    wt_dir: &Path,
    branch: &str,
    base_sha: &str,
    reply_to: &str,
    items: &[AcceptanceItem],
    provider: &str,
    endpoint_kind: &str,
) -> Result<String> {
    const CUT: &str = "…";
    let build = |title: &str| {
        with_acceptance(
            kickoff_body(
                issue,
                title,
                note,
                wt_dir,
                branch,
                base_sha,
                reply_to,
                provider,
                endpoint_kind,
            ),
            items,
        )
    };
    let full = build(title);
    let over = full.len().saturating_sub(pty::MAX_BODY);
    if over == 0 {
        return Ok(full);
    }
    if let Some(keep) = title.len().checked_sub(over + CUT.len()) {
        let mut end = keep;
        while !title.is_char_boundary(end) {
            end -= 1;
        }
        return Ok(build(&format!("{}{CUT}", &title[..end])));
    }
    let criteria = acceptance_listing(items).unwrap_or_default();
    Err(Error::rejected(format!(
        "{issue}'s acceptance criteria ({} bytes) do not fit the {}-char pty kickoff \
         ceiling even with the title cut — criteria are never truncated or dropped \
         (CAD-160). Shorten them (`cadence issue acceptance {issue} --from <file>`) \
         and keep the detail in the note {}. Nothing was created or queued.",
        criteria.len(),
        pty::MAX_BODY,
        note.display()
    )))
}

/// CAD-160: a `--job` dispatch whose kickoff cannot carry its
/// acceptance items whole is refused before `issue start` creates a
/// worktree, branch or job. The check builds the kickoff `job dispatch`
/// will send — same spec path, scope, criteria and report contract, for
/// the assignee's own endpoint and ceiling (48000 for a Devin cloud
/// session) — with placeholders the length of what the daemon mints
/// (`job-` + 8 hex, a 32-hex message id). `agent` is the assignee's
/// `agent_show` row.
fn check_job_kickoff(
    issue: &str,
    items: &[AcceptanceItem],
    spec: &Path,
    wt_name: &str,
    branch: &str,
    base_sha: &str,
    agent: &Value,
) -> Result<()> {
    // `issue start --job` records the canonical spec path on the job.
    let spec = spec.canonicalize().unwrap_or_else(|_| spec.to_path_buf());
    let spec = spec.to_string_lossy().into_owned();
    let job_id = "job-00000000";
    let job = store::Job {
        id: job_id.to_string(),
        title: None,
        spec_path: spec.clone(),
        spec_sha256: None,
        pm_alias: String::new(),
        issue_id: Some(issue.to_string()),
        repo: None,
        base_ref: None,
        state: "open".to_string(),
        max_revisions: 0,
        stall_secs: None,
        error: None,
        created: 0.0,
        updated: 0.0,
    };
    let task = store::Task {
        id: format!("{job_id}-t1"),
        job_id: job_id.to_string(),
        title: None,
        role: "worker".to_string(),
        assignee: None,
        spec_path: None,
        acceptance: acceptance_listing(items),
        worktree: Some(wt_name.to_string()),
        branch: Some(branch.to_string()),
        base_sha: Some(base_sha.to_string()),
        head_sha: None,
        state: "draft".to_string(),
        revision: 0,
        dispatch_message: None,
        error: None,
        created: 0.0,
        updated: 0.0,
    };
    let provider = agent["provider"].as_str().unwrap_or_default();
    let kind = agent["endpoint_kind"].as_str().unwrap_or_default();
    let Err(_) = store::job_kickoff(&job, &task, 1, &"0".repeat(32), provider, kind) else {
        return Ok(());
    };
    Err(Error::rejected(format!(
        "{issue}'s acceptance criteria ({} bytes) do not fit the {}-char kickoff ceiling \
         beside its fixed fields — criteria are never truncated or dropped (CAD-160). \
         Shorten them (`cadence issue acceptance {issue} --from <file>`) and keep the \
         detail in the spec file {spec}. Nothing was created or queued.",
        acceptance_listing(items).unwrap_or_default().len(),
        store::kickoff_ceiling(provider, kind),
    )))
}

/// The same rules the pty endpoint enforces pre-write: 1–4000 chars,
/// no control characters (single line), and no leading character the
/// provider's TUI treats as a command. Checked here so a bad body
/// refuses before `issue start` creates anything.
fn check_body(body: &str, provider: &str) -> Result<()> {
    if body.is_empty() || body.len() > pty::MAX_BODY {
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

/// The lane worktree's current HEAD — best-effort evidence carried on
/// a reported-duplicate record; `None` when git cannot answer (a lane
/// deleted mid-dispatch reads as unknown, never as a match).
fn lane_head(worktree: &Path) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(worktree).args(["rev-parse", "HEAD"]);
    let out = crate::proc::run_bounded(&mut cmd, Duration::from_secs(30)).ok()?;
    (out.status.success())
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// `dispatch <ISSUE> --to <worker> --note <path> [--job --spec f]`.
pub fn run(pm: &Pm, id: &str, args: &DispatchArgs, actor: &str, state_dir: &Path) -> Result<Value> {
    let (project, dir) = write::issue_dir(pm, id)?;
    let (front, body) = write::load_front(&dir)?;
    // CAD-360: a ticket of a plan dispatches only once the operator
    // approved the plan, and only with acceptance. Issues in no plan
    // pass unchanged.
    crate::issue::plan::gate(&pm.dir, &front, &body)?;
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
    let note_arg = args.note.clone().unwrap_or_else(|| dir.join("issue.md"));
    let note = note_arg
        .canonicalize()
        .map_err(|_| Error::rejected(format!("Note {} is unreadable", note_arg.display())))?;
    std::fs::metadata(&note)
        .map_err(|_| Error::rejected(format!("Note {} is unreadable", note_arg.display())))?;
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
    let endpoint_kind = agent["endpoint_kind"]
        .as_str()
        .unwrap_or_default()
        .to_string();
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

    // The body is fully determined before `issue start`:
    // `resolve_repo`, `resolve_base` and `resolve_lane` are the calls
    // start makes, so the lane checked here — the issue's open lane
    // when it has one (CAD-274), else a fresh one named from
    // `--name`/the title — is the lane start binds (CAD-388 R2-1). A
    // lane start would refuse (a `--name` for another slug, several
    // open lanes, another repo) refuses here, before anything is
    // created. Plain dispatch checks the template body; `--job` checks
    // the daemon's own kickoff (`check_job_kickoff`).
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = start::resolve_repo(&project, args.repo.as_deref(), &cwd)?;
    let (_base, base_sha) = start::resolve_base(&root, args.base.as_deref())?;
    let (wt_dir, branch) = start::resolve_lane(&front, args.name.as_deref(), &root)?;
    let body = if args.job_spec.is_none() {
        let summary = args.summary.as_deref().unwrap_or(&front.title);
        let body = plain_kickoff(
            &front.id,
            summary,
            &note,
            &wt_dir,
            &branch,
            &base_sha,
            &reply_to,
            &items,
            &provider,
            &endpoint_kind,
        )?;
        check_body(&body, &provider)?;
        Some(body)
    } else {
        if let Some(spec) = &args.job_spec {
            // `issue start --job` scopes the task to the lane's dir name.
            let wt_name = wt_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            check_job_kickoff(&front.id, &items, spec, &wt_name, &branch, &base_sha, agent)?;
        }
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
            let body = plain_kickoff(
                &front.id,
                summary,
                &note,
                Path::new(started["worktree"].as_str().unwrap_or_default()),
                started["branch"].as_str().unwrap_or_default(),
                started["base"]["sha"].as_str().unwrap_or_default(),
                &reply_to,
                &items,
                &provider,
                &endpoint_kind,
            )?;
            check_body(&body, &provider).map(|_| body)
        })
        .transpose()?;

    // Duplicate dispatch: a previously recorded `message` ref still
    // live means a kickoff is in flight — reuse the worktree, queue
    // nothing, say so. The ref names the worker it went to, so a
    // re-dispatch to a DIFFERENT worker is still caught.
    // CAD-467: a ref whose kickoff this worker already REPORTED —
    // completed against this same lane — is the late duplicate: the
    // work it kicks off is done, so nothing re-sends and the detection
    // is recorded on the issue. A kickoff bound to a different
    // worktree is another lane's history, not this dispatch's.
    let mut live: Option<Value> = None;
    let mut reported: Option<Value> = None;
    // A fresh read on the target — `show` predates `issue start`, and
    // a launch finishing mid-dispatch can have enqueued a bootstrap.
    let show_now = client::rpc(state_dir, "agent_show", json!({"alias": args.to}))
        .unwrap_or_else(|_| show.clone());
    for r in front
        .refs
        .iter()
        .filter(|r| r.kind == "message" && r.closed != Some(true))
    {
        let Some(mid) = r.path.as_deref() else {
            continue;
        };
        let owner = r
            .agent
            .as_deref()
            .or_else(|| {
                r.label
                    .as_deref()
                    .and_then(|l| l.strip_prefix("dispatch → "))
            })
            .unwrap_or(&args.to);
        let messages = if owner == args.to {
            Some(show_now.clone())
        } else {
            client::rpc(state_dir, "agent_show", json!({"alias": owner})).ok()
        };
        let Some(found) = messages.and_then(|s| {
            s["messages"]
                .as_array()
                .and_then(|ms| ms.iter().find(|m| m["id"].as_str() == Some(mid)).cloned())
        }) else {
            continue;
        };
        let state = found["state"].as_str().unwrap_or_default();
        if LIVE_MESSAGE_STATES.contains(&state) {
            live = Some(found);
            break;
        }
        if owner == args.to
            && state == "completed"
            && r.worktree
                .as_deref()
                .is_none_or(|w| Some(w) == started["worktree"].as_str())
        {
            reported = Some(found);
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
        out["duplicate_kind"] = json!("live");
        // CAD-300 (QA R4): a duplicate sends nothing, so its warning
        // must not say it dispatched.
        if items.is_empty() {
            out["acceptance"]["warning"] = json!(self::acceptance_warning(&front.id, false));
        }
        out["message"] = msg["id"].clone();
        out["message_state"] = msg["state"].clone();
        return Ok(out);
    }
    if let Some(msg) = reported {
        // The kickoff's own report sha against the lane's head now —
        // equal is the cleanest late duplicate; either way the issue
        // was reported by this worker in this lane, so nothing
        // re-sends. Both sides are recorded so the PM can judge.
        let mid = msg["id"].as_str().unwrap_or_default();
        let reported_sha = msg["result"]["sha"].as_str();
        let via = msg["result"]["via"].as_str().unwrap_or("report");
        let lane_head = started["worktree"]
            .as_str()
            .and_then(|wt| lane_head(Path::new(wt)));
        let same_head = match (reported_sha, lane_head.as_deref()) {
            (Some(s), Some(h)) => Some(s == h),
            _ => None,
        };
        out["dispatched"] = json!(false);
        out["duplicate"] = json!(true);
        out["duplicate_kind"] = json!("reported");
        if items.is_empty() {
            out["acceptance"]["warning"] = json!(self::acceptance_warning(&front.id, false));
        }
        out["message"] = msg["id"].clone();
        out["message_state"] = msg["state"].clone();
        out["reported"] = json!({
            "sha": reported_sha,
            "via": via,
            "lane_head": lane_head,
            "same_head": same_head,
        });
        let _ = write::add_comment(
            pm,
            id,
            &format!(
                "Late duplicate dispatch suppressed: {} already reported \
                 kickoff {} (via {}, sha {}, lane head {}) — nothing re-sent.",
                args.to,
                mid,
                via,
                reported_sha.unwrap_or("none recorded"),
                lane_head.as_deref().unwrap_or("unreadable"),
            ),
            None,
            Some("dispatch"),
            None,
            actor,
        );
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
    // CAD-203: in-scope lessons withheld for stale evidence, with the
    // reason — recorded on the dispatch comment and in the output so
    // "why did I not get this?" is answerable.
    let mut lessons_withheld: Vec<(String, String)> = vec![];
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
                    lessons_withheld = matched
                        .withheld
                        .iter()
                        .map(|(m, reason)| (m.front.id.clone(), reason.clone()))
                        .collect();
                    // Written whenever it has content: a dispatch whose
                    // every match was withheld still gets the file with
                    // only its Withheld section.
                    let (text, slugs) = memory::render_lessons(&matched);
                    if !text.is_empty() {
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

    // CAD-467: a freshly joined worker's `bootstrap-<alias>` still
    // `queued` would deliver as a bare onboarding turn AHEAD of this
    // kickoff — and a worker that treated it as the task would then
    // meet the kickoff as a second statement of the same work. Fold it
    // instead: cancel the queued row and carry its body verbatim ahead
    // of the kickoff, so the lane's first turn is the task and still
    // teaches identity, briefing path and the report command. A
    // bootstrap already `running` keeps its turn — this kickoff queues
    // behind it — and a folded body that outgrows the pty ceiling
    // leaves the bootstrap to deliver on its own. `--job` kickoffs are
    // daemon-templated and cannot carry it, so the fold is plain-path
    // only. The cancel is state-guarded: a bootstrap claimed in the
    // meantime refuses it and the kickoff queues behind, unchanged.
    let bootstrap_id = format!("bootstrap-{}", args.to);
    let mut send_body = body;
    let mut bootstrap_state: Option<String> = None;
    if let Some(boot) = send_body.as_ref().and_then(|_| {
        show_now["messages"].as_array().and_then(|ms| {
            ms.iter()
                .find(|m| m["id"].as_str() == Some(bootstrap_id.as_str()))
        })
    }) {
        bootstrap_state = boot["state"].as_str().map(str::to_string);
        if bootstrap_state.as_deref() == Some("queued") {
            if let (Some(boot_body), Some(b)) = (boot["body"].as_str(), send_body.as_deref()) {
                let folded = format!("{boot_body} {b}");
                if check_body(&folded, &provider).is_ok()
                    && client::rpc(
                        state_dir,
                        "message_cancel",
                        json!({"message": bootstrap_id, "by": reply_to,
                               "reason": format!("folded into kickoff {mid} for {}", front.id)}),
                    )
                    .is_ok()
                {
                    send_body = Some(folded);
                    bootstrap_state = Some("folded".to_string());
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
        // CAD-467: a folded bootstrap is already cancelled — the
        // worker's onboarding rode the kickoff that failed, so name it
        // on the failure comment for the operator to re-bootstrap.
        let folded = if bootstrap_state.as_deref() == Some("folded") {
            format!(" (bootstrap {bootstrap_id} was already folded into it — `cadence agent bootstrap {}` restores onboarding)", args.to)
        } else {
            String::new()
        };
        let _ = write::add_comment(
            pm,
            id,
            &format!("Dispatch send to {} failed: {e}{folded}", args.to),
            None,
            Some("dispatch"),
            None,
            actor,
        );
        e
    };

    // Exactly one send — plain text kickoff, or `job dispatch`'s
    // spec-bound kickoff for --job (its state lives on the task).
    let (message, sent_state) = if let Some(body) = send_body {
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
    if !lessons_withheld.is_empty() {
        let items: Vec<String> = lessons_withheld
            .iter()
            .map(|(slug, reason)| format!("{slug} ({reason})"))
            .collect();
        comment_text.push_str(&format!("\nLessons withheld: {}", items.join("; ")));
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
    // CAD-467: a folded bootstrap is recorded on the dispatch's own
    // comment — the cancelled row's history names this kickoff too.
    if bootstrap_state.as_deref() == Some("folded") {
        comment_text.push_str(&format!(
            "\nBootstrap {bootstrap_id} folded into this kickoff — the queued \
             onboarding turn was cancelled; its instructions ride this message."
        ));
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
    out["lessons_withheld"] = json!(lessons_withheld
        .iter()
        .map(|(slug, reason)| json!({"slug": slug, "reason": reason}))
        .collect::<Vec<_>>());
    out["lessons_error"] = lessons_error
        .as_ref()
        .map(|e| json!(e))
        .unwrap_or(Value::Null);
    out["comment"] = comment["comment"].clone();
    // CAD-467: `folded` when the queued bootstrap was cancelled into
    // this kickoff; a live-but-unfolded bootstrap (`queued`/`running`)
    // means the kickoff delivers after it reports — surfaced so the PM
    // sees why this kickoff is not the lane's first turn.
    out["bootstrap"] = bootstrap_state.map(|s| json!(s)).unwrap_or(Value::Null);
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
            "devin",
            "pty",
        )
    }

    /// CAD-468: the kickoff names the report command exactly when the
    /// endpoint reports explicitly — a turn-result endpoint's turn
    /// finishes by its result text and never runs the command.
    #[test]
    fn kickoff_teaches_the_report_command_only_for_explicit_endpoints() {
        assert!(
            kickoff().contains("cadence message result <id> --token <turn_id>"),
            "{}",
            kickoff()
        );
        let managed = kickoff_body(
            "D-1",
            "Title",
            Path::new("/tmp/note.md"),
            Path::new("/r/.cadence/wt/d-1-title"),
            "cadence/d-1-title",
            "0123456789abcdef",
            "pm",
            "claude",
            "managed",
        );
        assert!(!managed.contains("message result"), "{managed}");
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
            assert_eq!(acceptance_listing(&items), None);
            assert_eq!(with_acceptance(kickoff(), &items), kickoff());
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
            acceptance_listing(&items).as_deref(),
            Some(r#"1) [ ] "first\tpart"; 2) [x] "second""#)
        );
        let body = with_acceptance(kickoff(), &items);
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
        let listing = acceptance_listing(&items).unwrap();
        let (back, rest) = parse_acceptance_listing(&listing)
            .unwrap_or_else(|| panic!("listing does not parse: {listing}"));
        assert_eq!(back, items, "{listing}");
        assert_eq!(rest, "", "{listing}");
        let body = with_acceptance(kickoff(), &items);
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

    /// CAD-160 (replaces CAD-300 QA R3's "sent without the clause"):
    /// a near-limit `--summary` is prose, so it gives way — shortened,
    /// ending in `…` — and every acceptance item goes out whole.
    #[test]
    fn long_summary_gives_way_to_whole_criteria() {
        let summary = "s".repeat(3800);
        let items = vec![AcceptanceItem {
            text: "y".repeat(200),
            checked: false,
        }];
        let body = plain_kickoff(
            "D-1",
            &summary,
            Path::new("/tmp/note.md"),
            Path::new("/r/.cadence/wt/d-1-title"),
            "cadence/d-1-title",
            "0123456789abcdef",
            "pm",
            &items,
            "devin",
            "pty",
        )
        .unwrap();
        check_body(&body, "fake").unwrap();
        assert!(body.len() <= pty::MAX_BODY, "{} bytes", body.len());
        assert!(body.contains("D-1: sss"), "{body}");
        assert!(body.contains("s…. Your worktree exists"), "{body}");
        assert!(
            body.ends_with(&format!(" Acceptance: 1) [ ] \"{}\".", "y".repeat(200))),
            "{body}"
        );
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

    /// CAD-160 (replaces the CAD-159 readback pointer): a list that
    /// cannot fit whole refuses — plain and `--job` alike — naming the
    /// ceiling and the note or spec file; it is never replaced by a
    /// pointer. Both checks run before `issue start`.
    #[test]
    fn criteria_that_cannot_fit_refuse_naming_ceiling_and_file() {
        let items: Vec<AcceptanceItem> = (0..60)
            .map(|i| AcceptanceItem {
                text: format!("criterion {i} {}", "x".repeat(80)),
                checked: false,
            })
            .collect();
        let err = plain_kickoff(
            "D-1",
            "Title",
            Path::new("/tmp/note.md"),
            Path::new("/r/.cadence/wt/d-1-title"),
            "cadence/d-1-title",
            "0123456789abcdef",
            "pm",
            &items,
            "devin",
            "pty",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("4000-char"), "{err}");
        assert!(err.contains("note /tmp/note.md"), "{err}");
        assert!(err.contains("Nothing was created or queued"), "{err}");
        let err = job_check(&items, "pty").unwrap_err().to_string();
        assert!(err.contains("4000-char"), "{err}");
        assert!(err.contains("spec file /tmp/spec.md"), "{err}");
        job_check(&items[..10], "pty").unwrap();
    }

    fn items(n: usize) -> Vec<AcceptanceItem> {
        (0..n)
            .map(|i| AcceptanceItem {
                text: format!("criterion {i} {}", "x".repeat(80)),
                checked: false,
            })
            .collect()
    }

    /// The `--job` pre-check against a `devin/<kind>` assignee.
    fn job_check(items: &[AcceptanceItem], kind: &str) -> Result<()> {
        check_job_kickoff(
            "D-2",
            items,
            Path::new("/tmp/spec.md"),
            "d-2-job",
            "cadence/d-2-job",
            &"0".repeat(40),
            &json!({"provider": "devin", "endpoint_kind": kind}),
        )
    }

    /// CAD-160 (QA N1): the `--job` pre-check builds the kickoff the
    /// daemon will send, so a listing that fits alone but not beside
    /// the fixed fields (QA's 37-item probe) refuses before
    /// `issue start` — naming the criteria, not a sender's text. (QA
    /// N6) a Devin cloud assignee is held to its own 48000 ceiling.
    #[test]
    fn job_pre_check_reserves_the_kickoffs_fixed_fields() {
        // QA's probe: 37 items — a 3864-byte listing, 3878 bytes as the
        // kickoff's ` Acceptance: ….` clause — under 4000 alone.
        let gap = items(37);
        assert_eq!(acceptance_listing(&gap).unwrap().len(), 3864);
        let err = job_check(&gap, "pty").unwrap_err().to_string();
        assert!(
            err.contains("D-2's acceptance criteria (3864 bytes)"),
            "{err}"
        );
        assert!(err.contains("4000-char"), "{err}");
        assert!(err.contains("spec file /tmp/spec.md"), "{err}");
        assert!(err.contains("Nothing was created or queued"), "{err}");
        assert!(!err.contains("text"), "{err}");
        // A cloud kickoff carries 48000: the same list, and one past
        // 4000, go out; one past 48000 does not.
        job_check(&gap, "cloud").unwrap();
        job_check(&items(60), "cloud").unwrap();
        let err = job_check(&items(600), "cloud").unwrap_err().to_string();
        assert!(err.contains("48000-char"), "{err}");
    }

    /// CAD-160: the outstanding criteria of a stored task acceptance.
    #[test]
    fn outstanding_items_keep_only_unchecked_criteria() {
        let items = vec![
            AcceptanceItem {
                text: "a; 2) [ ] \"b\"".into(),
                checked: false,
            },
            AcceptanceItem {
                text: "done".into(),
                checked: true,
            },
        ];
        let listing = inline_listing(&items);
        assert_eq!(outstanding_items(Some(&listing)), items[..1].to_vec());
        // Free-form text, or a listing with trailing prose, is one item.
        for text in ["green tests", &format!("{listing} and more")] {
            assert_eq!(
                outstanding_items(Some(text)),
                vec![AcceptanceItem {
                    text: text.to_string(),
                    checked: false
                }]
            );
        }
        assert!(outstanding_items(None).is_empty());
        assert!(outstanding_items(Some("  ")).is_empty());
    }
}
