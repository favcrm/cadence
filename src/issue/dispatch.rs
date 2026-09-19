//! `cadence dispatch <ISSUE>` — the one-step dispatch: `issue start`
//! (idempotent) then exactly one templated kickoff message to the
//! worker, a tracker comment and a `message` ref on the issue. With
//! `--job` the kickoff goes through `job dispatch` instead, so the
//! task and the message are bound. Everything is validated before
//! anything is created — a refused dispatch leaves no worktree,
//! branch, commit or queued message behind.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::adapter::pty;
use crate::client;
use crate::error::{Error, Result};
use crate::issue::{start, write, Pm};

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

/// The same rules the pty endpoint enforces pre-write: 1–4000 chars,
/// no control characters (single line), and no leading character the
/// provider's TUI treats as a command. Checked here so a bad body
/// refuses before `issue start` creates anything.
fn check_body(body: &str, provider: &str) -> Result<()> {
    if body.is_empty() || body.len() > 4000 {
        return Err(Error::rejected("Dispatch body must be 1–4000 characters"));
    }
    if body.chars().any(|c| (c as u32) < 32 || c as u32 == 127) {
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

/// `dispatch <ISSUE> --to <worker> --note <path> [--job --spec f]`.
pub fn run(pm: &Pm, id: &str, args: &DispatchArgs, actor: &str, state_dir: &Path) -> Result<Value> {
    let (project, dir) = write::issue_dir(pm, id)?;
    let (front, _body) = write::load_front(&dir)?;
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
        let body = kickoff_body(
            &front.id, summary, &note, &wt_dir, &branch, &base_sha, &reply_to,
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
        }),
    };
    let started = start::run(pm, id, &start_args, actor, state_dir)?;

    // The body actually sent is rebuilt from start's returned refs and
    // re-checked: identical inputs make this a no-op, but nothing
    // unchecked ever reaches the pane.
    let body = body
        .map(|_| {
            let summary = args.summary.as_deref().unwrap_or(&front.title);
            let body = kickoff_body(
                &front.id,
                summary,
                &note,
                Path::new(started["worktree"].as_str().unwrap_or_default()),
                started["branch"].as_str().unwrap_or_default(),
                started["base"]["sha"].as_str().unwrap_or_default(),
                &reply_to,
            );
            check_body(&body, &provider).map(|_| body)
        })
        .transpose()?;

    // Duplicate dispatch: a previously recorded `message` ref still
    // live means a kickoff is in flight — reuse the worktree, queue
    // nothing, say so. The ref label names the worker it went to, so
    // a re-dispatch to a DIFFERENT worker is still caught.
    let mut live: Option<Value> = None;
    for r in front.refs.iter().filter(|r| r.kind == "message") {
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
    });
    if let Some(msg) = live {
        out["dispatched"] = json!(false);
        out["duplicate"] = json!(true);
        out["message"] = msg["id"].clone();
        out["message_state"] = msg["state"].clone();
        return Ok(out);
    }

    // Exactly one send — plain text kickoff, or `job dispatch`'s
    // spec-bound kickoff for --job (its state lives on the task).
    let (message, sent_state) = if let Some(body) = body {
        let sent = client::rpc(
            state_dir,
            "agent_send",
            json!({"alias": args.to, "text": body, "reply_to": reply_to}),
        )?;
        (
            sent["message"].as_str().unwrap_or_default().to_string(),
            ("message_state", sent["state"].clone()),
        )
    } else {
        let task = started["task"].as_str().unwrap_or_default().to_string();
        let sent = client::rpc(
            state_dir,
            "task_dispatch",
            json!({"task": task, "by": reply_to}),
        )?;
        (
            sent["message"].as_str().unwrap_or_default().to_string(),
            ("task_state", sent["task"]["state"].clone()),
        )
    };

    // The comment and the ref ride separate commits through the
    // existing helpers — each is independently consistent.
    let comment = write::add_comment(
        pm,
        id,
        &format!("Dispatched to {}: {}", args.to, note.display()),
        None,
        Some("dispatch"),
        None,
        actor,
    )?;
    write::add_ref(
        pm,
        id,
        "message",
        &message,
        Some(&format!("dispatch → {}", args.to)),
        None,
        actor,
    )?;

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
    out["comment"] = comment["comment"].clone();
    out["job"] = started["job"].clone();
    out["task"] = started["task"].clone();
    out["probe"] = probe.unwrap_or(Value::Null);
    Ok(out)
}
