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
         remote reads are: status, agent list, agent show, issue ls, \
         issue show, issue log, message read, inbox (peek). \
         Use `--org local` or `cadence org switch` to run locally.",
        name(command)
    ))
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
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    Ok(0)
}
