//! CAD-1019 slice 2: dispatch for a `Remote` org selection — maps the
//! allowlisted local commands to AOS-128 verbs and runs them through
//! [`cadence_agent::remote_cli`]. Anything outside the read-only allowlist
//! is refused *here*, before a request is ever built — remote writes land
//! in their own slice, and operator-only verbs never do.

use super::*;
use cadence_agent::wiki::cli::WikiAction;

fn unsupported(verb: &str, flag: &str, why: &str) -> Error {
    Error::rejected(format!("remote `{verb}` cannot {flag} — {why}"))
}

/// `cadence <verb>` → AOS-128 `(verb, arguments)`. Every returned verb is
/// in `remote_cli::REMOTE_VERBS` or `remote_cli::WIKI_ACTIONS`; every argument value is a primitive the
/// server's `arguments` object accepts. A command outside the set — or a
/// flag that cannot cross the wire faithfully — refuses locally; nothing
/// is sent (contract I2: an allowlist, not a tunnel).
fn verb_for(command: &Commands) -> Result<(&'static str, serde_json::Map<String, Value>)> {
    if let Commands::Wiki { action } = command {
        return wiki_for(action);
    }
    let mut args = serde_json::Map::new();
    let verb = match command {
        Commands::Status {
            group,
            all,
            json: _,
            watch,
        } => {
            if watch.is_some() {
                return Err(unsupported(
                    "status",
                    "--watch",
                    "it polls a local render loop; run remote status without it",
                ));
            }
            if group.is_some() {
                return Err(unsupported(
                    "status",
                    "--group",
                    "the remote status is the whole board agent list, not a group view",
                ));
            }
            if *all {
                return Err(unsupported(
                    "status",
                    "--all",
                    "the remote status already lists every board agent",
                ));
            }
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
                    json: _,
                },
        } => {
            if *all {
                return Err(unsupported(
                    "agent ls",
                    "--all",
                    "a remote list is already the whole board — there is no pane scope to widen",
                ));
            }
            if !project.is_empty() {
                return Err(unsupported(
                    "agent ls",
                    "--project",
                    "the remote read model has no project filter",
                ));
            }
            if sort.is_some() {
                return Err(unsupported(
                    "agent ls",
                    "--sort",
                    "sorting is a local transform",
                ));
            }
            if limit.is_some() {
                return Err(unsupported(
                    "agent ls",
                    "--limit",
                    "truncating is a local transform",
                ));
            }
            if !fields.is_empty() {
                return Err(unsupported(
                    "agent ls",
                    "--fields",
                    "field selection is a local transform",
                ));
            }
            for (key, values) in [("states", state), ("providers", provider), ("kinds", kind)] {
                if !values.is_empty() {
                    args.insert(key.into(), json!(values));
                }
            }
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
            if since.is_some() {
                return Err(unsupported(
                    "agent show",
                    "--since",
                    "the remote route accepts only alias and limit",
                ));
            }
            args.insert("alias".into(), json!(alias));
            if !*all {
                args.insert(
                    "limit".into(),
                    json!(limit.unwrap_or(agent::DEFAULT_SHOW_MESSAGES)),
                );
            }
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
                    json: _,
                },
        } => {
            // Straight pass-through of the list grammar; the remote
            // tracker owns the same flags the local `issue ls` parses.
            for (flag, present) in [
                ("--project", !project.is_empty()),
                ("--stage", !stage.is_empty()),
                ("--health", !health.is_empty()),
                ("--since", since.is_some()),
                ("--until", until.is_some()),
                ("--ready", *ready),
                ("--blocked", *blocked),
                ("--at", at.is_some()),
                ("--sort", sort.is_some()),
                ("--limit", limit.is_some()),
                ("--fields", !fields.is_empty()),
                ("--summary", *summary),
            ] {
                if present {
                    return Err(unsupported(
                        "issue ls",
                        flag,
                        "the remote route's filter does not implement it",
                    ));
                }
            }
            for (key, values) in [
                ("status", status),
                ("tag", tags),
                ("epic", epic),
                ("owner", owner),
                ("component", component),
                ("priority", priority),
                ("type", types),
                ("milestone", milestone),
                ("plan", plan),
            ] {
                if !values.is_empty() {
                    args.insert(key.into(), json!(values));
                }
            }
            if *open {
                args.insert("open".into(), json!(true));
            }
            "issue_ls"
        }
        Commands::Issue {
            action: cadence_agent::issue::cli::IssueAction::Show { id, json: _ },
        } => {
            args.insert("id".into(), json!(id));
            "issue_show"
        }
        Commands::Issue {
            action: cadence_agent::issue::cli::IssueAction::Log { id, limit },
        } => {
            args.insert("id".into(), json!(id));
            if *limit != 50 {
                args.insert("limit".into(), json!(limit));
            }
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
            if *offset != 0 {
                args.insert("offset".into(), json!(offset));
            }
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
            exec_retry_ms,
            exec_timeout_ms,
            exec_max_failures,
        } => {
            // A remote inbox read is one bounded peek/drain. `--follow`
            // and `--exec` are local streaming/exec surfaces — never sent.
            if *follow || exec.is_some() {
                return Err(unsupported(
                    "inbox",
                    "--follow or --exec",
                    "the remote read is one bounded peek, not a stream or a runner",
                ));
            }
            if !*peek {
                return Err(unsupported(
                    "inbox",
                    "drain a mailbox",
                    "a remote inbox read never consumes — pass --peek explicitly",
                ));
            }
            if *wait != 0 {
                return Err(unsupported(
                    "inbox",
                    "--wait",
                    "the remote peek returns immediately",
                ));
            }
            for (flag, present) in [
                ("--exec-retry-ms", *exec_retry_ms != 1000),
                ("--exec-timeout-ms", *exec_timeout_ms != 120_000),
                ("--exec-max-failures", *exec_max_failures != 5),
            ] {
                if present {
                    return Err(unsupported(
                        "inbox",
                        flag,
                        "it tunes the local --exec runner that never runs remotely",
                    ));
                }
            }
            let Some(alias) = alias else {
                return Err(unsupported(
                    "inbox",
                    "omit the alias",
                    "the remote route needs arguments.alias",
                ));
            };
            args.insert("alias".into(), json!(alias));
            if let Some(n) = after {
                args.insert("after".into(), json!(n));
            }
            if let Some(r) = reader {
                args.insert("reader".into(), json!(r));
            }
            "message_inbox"
        }
        _ => return Err(unavailable(command)),
    };
    Ok((verb, args))
}

/// `cadence wiki …` → the contract action (CAD-1401). `ls` and `search` are
/// `enumerate` (search carries `q`, ls does not), `cat` is `read`, a text
/// `put` is `write`. The argument names are the local CLI's own. Blob upload,
/// index, mkdir/mv/rm and history have no remote mapping and refuse here.
fn wiki_for(action: &WikiAction) -> Result<(&'static str, serde_json::Map<String, Value>)> {
    let mut args = serde_json::Map::new();
    let verb = match action {
        WikiAction::Upload { .. } => {
            return Err(Error::rejected("wiki upload is sent by its own path"));
        }
        WikiAction::Ls { path } => {
            args.insert("path".into(), json!(path.clone().unwrap_or_default()));
            "enumerate"
        }
        WikiAction::Search { q, path, .. } => {
            args.insert("q".into(), json!(q));
            args.insert("path".into(), json!(path.clone().unwrap_or_default()));
            "enumerate"
        }
        WikiAction::Cat {
            path,
            offset,
            bytes,
            rev,
            ..
        } => {
            args.insert("path".into(), json!(path));
            if let Some(offset) = offset {
                args.insert("offset".into(), json!(offset));
            }
            if let Some(bytes) = bytes {
                args.insert("bytes".into(), json!(bytes));
            }
            if let Some(rev) = rev {
                args.insert("rev".into(), json!(rev));
            }
            "read"
        }
        WikiAction::Put {
            path,
            text,
            file,
            blob,
            if_rev,
        } => {
            if *blob {
                return Err(Error::rejected(
                    "remote `wiki put --blob` is not available in this build — blob upload \
                     is out of scope; nothing was sent",
                ));
            }
            // The text is read client-side, as `issue new --file` does: a
            // remote caller has no master confinement, and `-` is stdin.
            let body = match (text, file) {
                (Some(t), _) => t.clone(),
                (None, Some(f)) if f.as_os_str() == "-" => {
                    std::io::read_to_string(std::io::stdin())?
                }
                (None, Some(f)) => std::fs::read_to_string(f).map_err(|e| {
                    Error::rejected(format!(
                        "wiki put --file {}: cannot read it locally: {e}",
                        f.display()
                    ))
                })?,
                (None, None) => std::io::read_to_string(std::io::stdin())?,
            };
            args.insert("path".into(), json!(path));
            args.insert("text".into(), json!(body));
            if let Some(rev) = if_rev {
                args.insert("if_rev".into(), json!(rev));
            }
            "write"
        }
        _ => {
            return Err(Error::rejected(
                "`cadence wiki` this subcommand is not available on a remote org in this \
                 build — remote wiki verbs are: wiki ls, wiki search, wiki cat, wiki put \
                 (text). Use `--org local` or `cadence org switch` to run locally.",
            ))
        }
    };
    Ok((verb, args))
}

/// CAD-913: `wiki upload <file> <path>` on a remote org. Every local check
/// runs before a request: the logical path, the regular-file and symlink
/// refusal, and the 15 MiB cap. A refused file sends nothing. The reply is
/// the remote's JSON body.
fn upload(
    target: &cadence_agent::remote_cli::RemoteTarget,
    file: &std::path::Path,
    path: &str,
    overwrite: bool,
    wake_timeout: u64,
) -> Result<i32> {
    use cadence_agent::wiki::upload::{
        logical_upload_path, part_file_name, read_local_file, UPLOAD_CAP_BYTES,
    };
    let logical = logical_upload_path(path)?;
    let bytes = read_local_file(file, UPLOAD_CAP_BYTES)?;
    let name = part_file_name(logical.rsplit('/').next().unwrap_or("upload"));
    let out = cadence_agent::remote_cli::upload_file(
        target,
        &logical,
        &name,
        &bytes,
        overwrite,
        wake_timeout,
        |line| eprintln!("{line}"),
    )
    .inspect_err(|error| {
        if error.kind() == "busy" && error.code() == Some("waking") {
            eprintln!(
                "{}",
                serde_json::to_string(&json!({"kind": "busy", "state": "waking"}))
                    .unwrap_or_default()
            );
        }
    })?;
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    Ok(0)
}

/// `wiki ls|cat|search` answers print as the host prints them. The `--json`,
/// `--meta` and `put` forms keep the JSON body. Returns whether it printed.
/// A directory is refused for `cat` (with or without `--meta`), as the local
/// `wiki cat` refuses it.
fn wiki_text(command: &Commands, body: &Value) -> Result<bool> {
    let Commands::Wiki { action } = command else {
        return Ok(false);
    };
    Ok(match action {
        WikiAction::Ls { .. } => {
            cadence_agent::wiki::cli::entries(body);
            true
        }
        WikiAction::Cat { path, .. } if body["kind"] == "dir" => {
            return Err(Error::rejected(format!(
                "wiki cat '{path}' refused: a directory — use `wiki ls`"
            )))
        }
        WikiAction::Cat {
            offset, bytes, rev, ..
        } if offset.is_some() || bytes.is_some() || rev.is_some() => {
            // The daemon already bounded this compact line. Pretty-printing
            // it could push one line over the native tool bound.
            println!("{}", serde_json::to_string(body).unwrap_or_default());
            true
        }
        WikiAction::Cat { meta: false, .. } if body["kind"] == "text" => {
            print!("{}", body["text"].as_str().unwrap_or(""));
            true
        }
        WikiAction::Search { json: false, .. } => {
            for m in body["matches"].as_array().cloned().unwrap_or_default() {
                println!(
                    "{}:{}: {}",
                    m["path"].as_str().unwrap_or(""),
                    m["line"].as_u64().unwrap_or(0),
                    m["text"].as_str().unwrap_or("")
                );
            }
            true
        }
        _ => false,
    })
}

fn unavailable(command: &Commands) -> Error {
    // The refusal names the command so an agent can branch on it; the
    // allowlisted alternatives are the only remote surface.
    Error::rejected(format!(
        "`cadence {}` is not available on a remote org in this build — \
         remote verbs are: status, agent list, agent show, issue ls, \
         issue show, issue log, issue new, issue comment, issue set, \
         message read, inbox (peek), wiki ls, wiki search, wiki cat, \
         wiki put (text). \
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
    if let Commands::Wiki {
        action:
            WikiAction::Upload {
                file,
                path,
                overwrite,
            },
    } = &command
    {
        return upload(&target, file, path, *overwrite, wake_timeout);
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
            if wiki_text(&command, &body)? {
                return Ok(0);
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_wiki_cat_refuses_a_directory_like_local_cat() {
        // CAD-1401 repair: a directory is a nonzero refusal (local `wiki cat`
        // refuses it with or without --meta), never printed as JSON.
        let dir = json!({"kind": "dir", "path": "global"});
        for meta in [false, true] {
            let cat = Commands::Wiki {
                action: WikiAction::Cat {
                    path: "global".into(),
                    meta,
                    offset: None,
                    bytes: None,
                    rev: None,
                },
            };
            let err = wiki_text(&cat, &dir).unwrap_err();
            assert!(err.to_string().contains("use `wiki ls`"), "{err}");
        }
    }
}

#[cfg(test)]
mod read_contract_tests {
    use super::*;
    use cadence_agent::issue::cli::IssueAction;

    fn outcome(command: &Commands) -> std::result::Result<(&'static str, Value), String> {
        match verb_for(command) {
            Ok((verb, args)) => Ok((verb, Value::Object(args))),
            Err(e) => Err(e.to_string()),
        }
    }

    struct AgentLs {
        all: bool,
        state: Vec<String>,
        provider: Vec<String>,
        kind: Vec<String>,
        project: Vec<String>,
        sort: Option<String>,
        limit: Option<usize>,
        fields: Vec<String>,
    }
    fn agent_ls(f: impl FnOnce(&mut AgentLs)) -> Commands {
        let mut ls = AgentLs {
            all: false,
            state: vec![],
            provider: vec![],
            kind: vec![],
            project: vec![],
            sort: None,
            limit: None,
            fields: vec![],
        };
        f(&mut ls);
        Commands::Agent {
            action: AgentAction::List {
                all: ls.all,
                state: ls.state,
                provider: ls.provider,
                kind: ls.kind,
                project: ls.project,
                sort: ls.sort,
                limit: ls.limit,
                fields: ls.fields,
                json: false,
            },
        }
    }

    #[derive(Default)]
    struct IssueLs {
        project: Vec<String>,
        status: Vec<String>,
        tags: Vec<String>,
        epic: Vec<String>,
        owner: Vec<String>,
        component: Vec<String>,
        priority: Vec<String>,
        types: Vec<String>,
        milestone: Vec<String>,
        stage: Vec<String>,
        health: Vec<String>,
        plan: Vec<String>,
        since: Option<String>,
        until: Option<String>,
        open: bool,
        ready: bool,
        blocked: bool,
        at: Option<String>,
        sort: Option<String>,
        limit: Option<usize>,
        fields: Vec<String>,
        summary: bool,
    }
    fn issue_ls(f: impl FnOnce(&mut IssueLs)) -> Commands {
        let mut ls = IssueLs::default();
        f(&mut ls);
        Commands::Issue {
            action: IssueAction::Ls {
                project: ls.project,
                status: ls.status,
                tags: ls.tags,
                epic: ls.epic,
                owner: ls.owner,
                component: ls.component,
                priority: ls.priority,
                types: ls.types,
                milestone: ls.milestone,
                stage: ls.stage,
                health: ls.health,
                plan: ls.plan,
                since: ls.since,
                until: ls.until,
                open: ls.open,
                ready: ls.ready,
                blocked: ls.blocked,
                at: ls.at,
                sort: ls.sort,
                limit: ls.limit,
                fields: ls.fields,
                summary: ls.summary,
                json: false,
            },
        }
    }

    fn inbox_cmd(
        alias: Option<&str>,
        peek: bool,
        after: Option<i64>,
        wait: u64,
        follow: bool,
        reader: Option<&str>,
        exec: Option<Vec<String>>,
    ) -> Commands {
        Commands::Inbox {
            alias: alias.map(str::to_string),
            action: None,
            peek,
            after,
            wait,
            follow,
            reader: reader.map(str::to_string),
            exec,
            exec_retry_ms: 1000,
            exec_timeout_ms: 120_000,
            exec_max_failures: 5,
        }
    }

    #[test]
    fn agent_list_filters_use_the_plural_names_the_route_reads() {
        let (verb, args) = outcome(&agent_ls(|l| {
            l.state = vec!["idle".into(), "busy".into()];
            l.provider = vec!["pi".into()];
            l.kind = vec!["pty".into()];
        }))
        .unwrap();
        assert_eq!(verb, "agent_list");
        assert_eq!(
            args,
            json!({
                "states": ["idle", "busy"],
                "providers": ["pi"],
                "kinds": ["pty"],
            })
        );
    }

    #[test]
    fn agent_list_unsupported_flags_refuse_before_any_request() {
        type Cases = Vec<(&'static str, fn(&mut AgentLs))>;
        let cases: Cases = vec![
            ("--all", |l| l.all = true),
            ("--project", |l| l.project = vec!["cadence".into()]),
            ("--sort", |l| l.sort = Some("alias".into())),
            ("--limit", |l| l.limit = Some(3)),
            ("--fields", |l| l.fields = vec!["alias".into()]),
        ];
        for (flag, over) in cases {
            let err = outcome(&agent_ls(over)).unwrap_err();
            assert!(err.contains(flag), "{flag}: {err}");
        }
    }

    #[test]
    fn agent_show_sends_alias_and_limit_or_refuses_since() {
        let (verb, args) = outcome(&Commands::Agent {
            action: AgentAction::Show {
                alias: "w1".into(),
                limit: Some(5),
                since: None,
                all: false,
            },
        })
        .unwrap();
        assert_eq!(verb, "agent_show");
        assert_eq!(args, json!({"alias": "w1", "limit": 5}));

        let (_, args) = outcome(&Commands::Agent {
            action: AgentAction::Show {
                alias: "w1".into(),
                limit: None,
                since: None,
                all: false,
            },
        })
        .unwrap();
        assert_eq!(args["limit"], json!(20));

        let (_, args) = outcome(&Commands::Agent {
            action: AgentAction::Show {
                alias: "w1".into(),
                limit: None,
                since: None,
                all: true,
            },
        })
        .unwrap();
        assert_eq!(args, json!({"alias": "w1"}));
        let err = outcome(&Commands::Agent {
            action: AgentAction::Show {
                alias: "w1".into(),
                limit: None,
                since: Some("m1".into()),
                all: false,
            },
        })
        .unwrap_err();
        assert!(err.contains("--since"), "{err}");
    }

    #[test]
    fn status_refuses_group_all_and_watch() {
        for command in [
            Commands::Status {
                group: Some("pm".into()),
                all: false,
                json: false,
                watch: None,
            },
            Commands::Status {
                group: None,
                all: true,
                json: false,
                watch: None,
            },
            Commands::Status {
                group: None,
                all: false,
                json: false,
                watch: Some(2),
            },
        ] {
            assert!(outcome(&command).is_err());
        }
        let (verb, args) = outcome(&Commands::Status {
            group: None,
            all: false,
            json: false,
            watch: None,
        })
        .unwrap();
        assert_eq!(verb, "status");
        assert_eq!(args, json!({}));
    }

    #[test]
    fn issue_ls_sends_only_the_filter_the_route_owns() {
        let (verb, args) = outcome(&issue_ls(|l| {
            l.status = vec!["doing".into()];
            l.tags = vec!["t".into()];
            l.owner = vec!["me".into()];
            l.plan = vec!["approved".into()];
            l.open = true;
        }))
        .unwrap();
        assert_eq!(verb, "issue_ls");
        assert_eq!(
            args,
            json!({
                "status": ["doing"], "tag": ["t"], "owner": ["me"],
                "plan": ["approved"], "open": true,
            })
        );
    }

    #[test]
    fn issue_ls_rejects_every_flag_the_route_cannot_apply() {
        type Cases = Vec<(&'static str, fn(&mut IssueLs))>;
        let cases: Cases = vec![
            ("--project", |l| l.project = vec!["cadence".into()]),
            ("--stage", |l| l.stage = vec!["build".into()]),
            ("--health", |l| l.health = vec!["on_track".into()]),
            ("--since", |l| l.since = Some("7d".into())),
            ("--until", |l| l.until = Some("now".into())),
            ("--ready", |l| l.ready = true),
            ("--blocked", |l| l.blocked = true),
            ("--at", |l| l.at = Some("HEAD~1".into())),
            ("--sort", |l| l.sort = Some("id".into())),
            ("--limit", |l| l.limit = Some(5)),
            ("--fields", |l| l.fields = vec!["id".into()]),
            ("--summary", |l| l.summary = true),
        ];
        for (flag, over) in cases {
            let err = outcome(&issue_ls(over)).unwrap_err();
            assert!(err.contains(flag), "{flag}: {err}");
        }
    }

    #[test]
    fn issue_show_and_history_keep_their_id_limit_contract() {
        let (verb, args) = outcome(&Commands::Issue {
            action: IssueAction::Show {
                id: "CAD-1".into(),
                json: false,
            },
        })
        .unwrap();
        assert_eq!(verb, "issue_show");
        assert_eq!(args, json!({"id": "CAD-1"}));

        let (verb, args) = outcome(&Commands::Issue {
            action: IssueAction::Log {
                id: "CAD-1".into(),
                limit: 50,
            },
        })
        .unwrap();
        assert_eq!(verb, "issue_history");
        assert_eq!(args, json!({"id": "CAD-1"}));
        let (_, args) = outcome(&Commands::Issue {
            action: IssueAction::Log {
                id: "CAD-1".into(),
                limit: 10,
            },
        })
        .unwrap();
        assert_eq!(args, json!({"id": "CAD-1", "limit": 10}));
    }

    #[test]
    fn message_read_keeps_its_window_contract() {
        let (verb, args) = outcome(&Commands::Message {
            action: MessageAction::Read {
                message: "m1".into(),
                offset: 0,
                limit: None,
            },
        })
        .unwrap();
        assert_eq!(verb, "message_read");
        assert_eq!(args, json!({"message": "m1"}));
        let (_, args) = outcome(&Commands::Message {
            action: MessageAction::Read {
                message: "m1".into(),
                offset: 40,
                limit: Some(2000),
            },
        })
        .unwrap();
        assert_eq!(args, json!({"message": "m1", "offset": 40, "limit": 2000}));
    }

    #[test]
    fn inbox_is_peek_only_and_needs_an_alias() {
        let err = outcome(&inbox_cmd(Some("m"), false, None, 0, false, None, None)).unwrap_err();
        assert!(err.contains("--peek"), "{err}");
        let err = outcome(&inbox_cmd(None, true, None, 0, false, None, None)).unwrap_err();
        assert!(err.contains("alias"), "{err}");
        for command in [
            inbox_cmd(Some("m"), true, None, 0, true, None, None),
            inbox_cmd(
                Some("m"),
                true,
                None,
                0,
                false,
                None,
                Some(vec!["cat".into()]),
            ),
            inbox_cmd(Some("m"), true, None, 5, false, None, None),
        ] {
            assert!(outcome(&command).is_err());
        }
        let (verb, args) = outcome(&inbox_cmd(
            Some("m"),
            true,
            Some(7),
            0,
            false,
            Some("ops"),
            None,
        ))
        .unwrap();
        assert_eq!(verb, "message_inbox");
        assert_eq!(args, json!({"alias": "m", "after": 7, "reader": "ops"}));
    }

    #[test]
    fn inbox_runner_knobs_refuse_even_under_peek() {
        for (flag, retry, timeout, failures) in [
            ("--exec-retry-ms", 2000, 120_000, 5),
            ("--exec-timeout-ms", 1000, 60_000, 5),
            ("--exec-max-failures", 1000, 120_000, 3),
        ] {
            let mut command = inbox_cmd(Some("m"), true, None, 0, false, None, None);
            let Commands::Inbox {
                exec_retry_ms,
                exec_timeout_ms,
                exec_max_failures,
                ..
            } = &mut command
            else {
                unreachable!()
            };
            *exec_retry_ms = retry;
            *exec_timeout_ms = timeout;
            *exec_max_failures = failures;
            let err = outcome(&command).unwrap_err();
            assert!(err.contains(flag), "{flag}: {err}");
        }
        let (verb, _) = outcome(&inbox_cmd(Some("m"), true, None, 0, false, None, None)).unwrap();
        assert_eq!(verb, "message_inbox");
    }

    #[test]
    fn verbs_outside_the_read_allowlist_refuse() {
        let err = outcome(&Commands::Agent {
            action: AgentAction::Stop { alias: "w1".into() },
        })
        .unwrap_err();
        assert!(err.contains("not available on a remote org"), "{err}");
    }
}
