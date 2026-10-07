//! CAD-1019 slice 2: dispatch for a `Remote` org selection — maps the
//! allowlisted local commands to AOS-128 verbs and runs them through
//! [`cadence_agent::remote_cli`]. Anything outside the read-only allowlist
//! is refused *here*, before a request is ever built — remote writes land
//! in their own slice, and operator-only verbs never do.

use super::*;

/// `cadence <verb>` → AOS-128 `(verb, arguments)`. Every returned verb is
/// in `remote_cli::REMOTE_VERBS`; every argument value is a primitive the
/// server's `arguments` object accepts. A command outside the set — or a
/// flag that cannot cross the wire faithfully — refuses locally; nothing
/// is sent (contract I2: an allowlist, not a tunnel).
fn verb_for(command: &Commands) -> Result<(&'static str, serde_json::Map<String, Value>)> {
    let mut args = serde_json::Map::new();
    let verb = match command {
        Commands::Status {
            group,
            all,
            json,
            watch,
        } => {
            if watch.is_some() {
                return Err(Error::rejected(
                    "--watch polls a local render loop; run remote status without it",
                ));
            }
            if let Some(g) = group {
                args.insert("group".into(), json!(g));
            }
            args.insert("all".into(), json!(all));
            args.insert("json".into(), json!(json));
            "status"
        }
        Commands::Agent {
            action:
                AgentAction::List {
                    all,
                    state,
                    provider,
                    kind,
                    project,
                    sort,
                    limit,
                    fields,
                    json,
                },
        } => {
            args.insert("all".into(), json!(all));
            args.insert("state".into(), json!(state));
            args.insert("provider".into(), json!(provider));
            args.insert("kind".into(), json!(kind));
            args.insert("project".into(), json!(project));
            if let Some(s) = sort {
                args.insert("sort".into(), json!(s));
            }
            if let Some(n) = limit {
                args.insert("limit".into(), json!(n));
            }
            args.insert("fields".into(), json!(fields));
            args.insert("json".into(), json!(json));
            "agent_list"
        }
        Commands::Agent {
            action:
                AgentAction::Show {
                    alias,
                    limit,
                    since,
                    all,
                },
        } => {
            args.insert("alias".into(), json!(alias));
            if let Some(n) = limit {
                args.insert("limit".into(), json!(n));
            }
            if let Some(s) = since {
                args.insert("since".into(), json!(s));
            }
            args.insert("all".into(), json!(all));
            "agent_show"
        }
        Commands::Issue {
            action:
                cadence_agent::issue::cli::IssueAction::Ls {
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
                },
        } => {
            // Straight pass-through of the list grammar; the remote
            // tracker owns the same flags the local `issue ls` parses.
            args.insert("project".into(), json!(project));
            args.insert("status".into(), json!(status));
            args.insert("tag".into(), json!(tags));
            args.insert("epic".into(), json!(epic));
            args.insert("owner".into(), json!(owner));
            args.insert("component".into(), json!(component));
            args.insert("priority".into(), json!(priority));
            args.insert("type".into(), json!(types));
            args.insert("milestone".into(), json!(milestone));
            args.insert("stage".into(), json!(stage));
            args.insert("health".into(), json!(health));
            args.insert("plan".into(), json!(plan));
            if let Some(s) = since {
                args.insert("since".into(), json!(s));
            }
            if let Some(s) = until {
                args.insert("until".into(), json!(s));
            }
            args.insert("open".into(), json!(open));
            args.insert("ready".into(), json!(ready));
            args.insert("blocked".into(), json!(blocked));
            if let Some(s) = at {
                args.insert("at".into(), json!(s));
            }
            if let Some(s) = sort {
                args.insert("sort".into(), json!(s));
            }
            if let Some(n) = limit {
                args.insert("limit".into(), json!(n));
            }
            args.insert("fields".into(), json!(fields));
            args.insert("summary".into(), json!(summary));
            args.insert("json".into(), json!(json));
            "issue_ls"
        }
        Commands::Issue {
            action: cadence_agent::issue::cli::IssueAction::Show { id, json },
        } => {
            args.insert("id".into(), json!(id));
            args.insert("json".into(), json!(json));
            "issue_show"
        }
        Commands::Issue {
            action: cadence_agent::issue::cli::IssueAction::Log { id, limit },
        } => {
            args.insert("id".into(), json!(id));
            args.insert("limit".into(), json!(limit));
            "issue_history"
        }
        Commands::Issue {
            action:
                cadence_agent::issue::cli::IssueAction::New {
                    title,
                    project,
                    priority,
                    parent,
                    epic,
                    tags,
                    blocked_by,
                    owner,
                    component,
                    id,
                    file,
                    status,
                },
        } => {
            // Every flag the hosted `issue_new` cannot honour is refused
            // here — never silently dropped onto the wire. The hosted
            // tracker has no cwd to derive a project from, so `--project`
            // is required; `--id` would mint a caller-chosen id the
            // remote never agreed to, and `--status` is a local
            // master-only surface.
            let mut unsupported = Vec::new();
            if project.is_none() {
                return Err(Error::rejected(
                    "remote `issue new` needs an explicit --project — the hosted \
                     tracker has no cwd to derive one from",
                ));
            }
            if id.is_some() {
                unsupported.push("--id");
            }
            if status.is_some() {
                unsupported.push("--status");
            }
            if !unsupported.is_empty() {
                return Err(Error::rejected(format!(
                    "remote `issue new` does not support {} — nothing was sent",
                    unsupported.join(" ")
                )));
            }
            // `--file` is read client-side (an unmanaged remote caller
            // has no master confinement); `-` reads stdin — never a
            // remote path.
            let body = match file {
                Some(f) if f.as_os_str() == "-" => Some(read_stdin_remote("issue new --file")?),
                Some(f) => Some(std::fs::read_to_string(f).map_err(|e| {
                    Error::rejected(format!(
                        "issue new --file {}: cannot read it locally: {e}",
                        f.display()
                    ))
                })?),
                None => None,
            };
            if body
                .as_deref()
                .is_some_and(|b| b.len() > cadence_agent::issue::report::BODY_MAX)
            {
                return Err(Error::rejected(
                    "issue body exceeds the 32 KB cap — trim it; nothing was sent",
                ));
            }
            args.insert("project".into(), json!(project));
            args.insert("title".into(), json!(title));
            if let Some(p) = priority {
                args.insert("priority".into(), json!(p));
            }
            // `--epic` is the local alias of `--parent`; they conflict
            // at parse time, so one slot carries both.
            if let Some(p) = parent.as_ref().or(epic.as_ref()) {
                args.insert("parent".into(), json!(p));
            }
            if !tags.is_empty() {
                args.insert("tags".into(), json!(tags));
            }
            if !blocked_by.is_empty() {
                args.insert("blocked_by".into(), json!(blocked_by));
            }
            if let Some(o) = owner {
                args.insert("owner".into(), json!(o));
            }
            if let Some(c) = component {
                args.insert("component".into(), json!(c));
            }
            if let Some(b) = body {
                args.insert("body".into(), json!(b));
            }
            "issue_new"
        }
        Commands::Issue {
            action:
                cadence_agent::issue::cli::IssueAction::Comment {
                    id,
                    text,
                    file,
                    author,
                    kind,
                },
        } => {
            // The stored author is the envelope-derived actor the hosted
            // route stamps — a caller-picked `--author` is never sent.
            let mut unsupported = Vec::new();
            if author.is_some() {
                unsupported.push("--author (the author is your enrolled identity)");
            }
            if kind.is_some() {
                unsupported.push("--kind");
            }
            if !unsupported.is_empty() {
                return Err(Error::rejected(format!(
                    "remote `issue comment` does not support {} — nothing was sent",
                    unsupported.join(" ")
                )));
            }
            let body = match (text, file) {
                (Some(t), _) => Some(t.clone()),
                (None, Some(f)) if f.as_os_str() == "-" => {
                    Some(read_stdin_remote("issue comment --file")?)
                }
                (None, Some(f)) => Some(std::fs::read_to_string(f).map_err(|e| {
                    Error::rejected(format!(
                        "issue comment --file {}: cannot read it locally: {e}",
                        f.display()
                    ))
                })?),
                (None, None) => {
                    if atty_remote_stdin() {
                        return Err(Error::rejected("Provide -m or --file"));
                    }
                    Some(read_stdin_remote("issue comment")?)
                }
            };
            let Some(body) = body else {
                return Err(Error::rejected("Provide -m or --file"));
            };
            if body.trim().is_empty() {
                return Err(Error::rejected("Comment body is empty — pass -m or --file"));
            }
            if body.len() > cadence_agent::issue::report::BODY_MAX {
                return Err(Error::rejected(
                    "comment body exceeds the 32 KB cap — trim it; nothing was sent",
                ));
            }
            args.insert("id".into(), json!(id));
            args.insert("body".into(), json!(body));
            "issue_comment"
        }
        Commands::Issue {
            action:
                cadence_agent::issue::cli::IssueAction::Set {
                    args: items,
                    if_rev,
                    force,
                },
        } => {
            // Revisions are per-ticket, so a remote field edit names
            // exactly one issue, and `--if-rev` (the `rev` `issue show`
            // reports) is mandatory — never a hidden pre-read, never
            // last-writer-wins. The hosted route also refuses an
            // `issue_set` without `if_rev`; this refusal keeps the
            // bytes off the wire.
            let split = items
                .iter()
                .position(|a| a.contains('='))
                .unwrap_or(items.len());
            let (ids, pairs) = items.split_at(split);
            if let Some(stray) = pairs.iter().find(|p| !p.contains('=')) {
                return Err(Error::rejected(format!(
                    "'{stray}' after a key=value pair — ids come first: \
                     `cadence issue set CAD-16 key=value`"
                )));
            }
            if ids.len() != 1 {
                return Err(Error::rejected(
                    "remote `issue set` edits exactly one ticket — revisions are \
                     per-ticket; run one call per id",
                ));
            }
            if pairs.is_empty() {
                return Err(Error::rejected(
                    "remote `issue set` needs key=value pairs — e.g. \
                     `issue set CAD-16 status=doing`",
                ));
            }
            // `--force` overrides the local `status=done` evidence
            // gate — the hosted route does not accept a force field,
            // and an override must never ride silently.
            if force.is_some() {
                return Err(Error::rejected(
                    "remote `issue set` does not support --force — nothing was sent",
                ));
            }
            let Some(rev) = if_rev.as_deref().map(str::trim) else {
                return Err(Error::rejected(
                    "remote `issue set` needs --if-rev <rev> — take the `rev` \
                     field from `issue show <id>` so a stale read refuses \
                     instead of overwriting",
                ));
            };
            if rev.is_empty() {
                return Err(Error::rejected(
                    "remote `issue set` needs a nonempty --if-rev <rev>",
                ));
            }
            args.insert("ids".into(), json!(ids));
            args.insert("set".into(), json!(pairs));
            args.insert("if_rev".into(), json!(rev));
            "issue_set"
        }
        Commands::Message {
            action:
                MessageAction::Read {
                    message,
                    offset,
                    limit,
                },
        } => {
            args.insert("message".into(), json!(message));
            args.insert("offset".into(), json!(offset));
            if let Some(n) = limit {
                args.insert("limit".into(), json!(n));
            }
            "message_read"
        }
        Commands::Inbox {
            alias,
            action: None,
            peek,
            after,
            wait,
            follow,
            reader,
            exec,
            ..
        } => {
            // A remote inbox read is one bounded peek/drain. `--follow`
            // and `--exec` are local streaming/exec surfaces — never sent.
            if *follow || exec.is_some() {
                return Err(Error::rejected(
                    "remote `inbox` cannot --follow or --exec; use --peek/--wait \
                     for a bounded read",
                ));
            }
            if let Some(a) = alias {
                args.insert("alias".into(), json!(a));
            }
            args.insert("peek".into(), json!(peek));
            if let Some(n) = after {
                args.insert("after".into(), json!(n));
            }
            args.insert("wait".into(), json!(wait));
            if let Some(r) = reader {
                args.insert("reader".into(), json!(r));
            }
            "message_inbox"
        }
        _ => return Err(unavailable(command)),
    };
    Ok((verb, args))
}

fn unavailable(command: &Commands) -> Error {
    // The refusal names the command so an agent can branch on it; the
    // allowlisted alternatives are the only remote surface.
    Error::rejected(format!(
        "`cadence {}` is not available on a remote org in this build — \
         remote verbs are: status, agent list, agent show, issue ls, \
         issue show, issue log, issue new, issue comment, issue set, \
         message read, inbox (peek). \
         Use `--org local` or `cadence org switch` to run locally.",
        name(command)
    ))
}

/// stdin → the body string for `--file -` on a remote verb — bounded by
/// the same 32 KB cap the hosted route enforces, so an oversized body
/// is refused before a byte crosses.
fn read_stdin_remote(flag: &str) -> Result<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    let cap = cadence_agent::issue::report::BODY_MAX as u64 + 1;
    std::io::stdin()
        .take(cap)
        .read_to_end(&mut buf)
        .map_err(|e| Error::rejected(format!("{flag}: cannot read stdin: {e}")))?;
    if buf.len() as u64 > cap - 1 {
        return Err(Error::rejected(format!(
            "{flag}: body exceeds the 32 KB cap — trim it"
        )));
    }
    String::from_utf8(buf).map_err(|_| Error::rejected(format!("{flag}: body is not UTF-8")))
}

fn atty_remote_stdin() -> bool {
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

/// A stable command name for the refusal — never a trust decision.
fn name(command: &Commands) -> &'static str {
    match command {
        Commands::Issue { .. } => "issue <verb>",
        Commands::Agent { .. } => "agent <verb>",
        Commands::Message { .. } => "message <verb>",
        Commands::Send { .. } => "send",
        Commands::Inbox { .. } => "inbox",
        Commands::Status { .. } => "status",
        _ => "<command>",
    }
}

/// Entry point for a `Remote` resolution (called by `action_dispatch`
/// instead of the local `dispatch`). `Org` stays local — it edits the
/// registry, not a daemon — so `org switch local` still gets you home.
/// Everything else maps through [`verb_for`] or refuses before a request
/// exists; a remote command never opens local state or a socket.
pub(super) fn run(
    target: cadence_agent::remote_cli::RemoteTarget,
    command: Commands,
    wake_timeout: u64,
) -> Result<i32> {
    if let Commands::Org { action } = command {
        return org::run(action);
    }
    let (verb, args) = verb_for(&command)?;
    let out =
        match cadence_agent::remote_cli::call_verb(&target, verb, args, wake_timeout, |line| {
            eprintln!("{line}")
        }) {
            Ok(out) => out,
            Err(error) => {
                // The wake budget lapsed: the contract's exact payload on
                // stderr, exit 75 — a retryable `busy`, never a silent hang.
                if error.kind() == "busy" && error.code() == Some("waking") {
                    eprintln!(
                        "{}",
                        serde_json::to_string(&json!({"kind": "busy", "state": "waking"}))
                            .unwrap_or_default()
                    );
                    return Ok(75);
                }
                return Err(error);
            }
        };
    match out {
        cadence_agent::remote_cli::RemoteAnswer::Ok(body) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&body).unwrap_or_default()
            );
            Ok(0)
        }
        cadence_agent::remote_cli::RemoteAnswer::Conflict(body) => {
            // A checked conflict is the remote's "not written" verdict:
            // print the whole body so the caller resyncs from
            // `current_rev`/`card`, and exit 5 — the same conflict code
            // the local write path exits (see `error::EXIT_TABLE`).
            println!(
                "{}",
                serde_json::to_string_pretty(&body).unwrap_or_default()
            );
            Ok(5)
        }
    }
}
