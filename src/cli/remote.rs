//! CAD-1019 slice 2: dispatch for a `Remote` org selection — maps the
//! allowlisted local commands to AOS-128 verbs and runs them through
//! [`cadence_agent::remote_cli`]. Anything outside the allowlist is refused
//! *here*, before a request is ever built. Operator-only verbs never map.
//! `issue project policy` is `project_policy`; project add/list/set-remote stay local.

use super::*;
use cadence_agent::remote_cli::RemoteTarget;
use cadence_agent::remote_migration::MigrationCommand;
use cadence_agent::wiki::cli::WikiAction;
use std::path::PathBuf;

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
        Commands::MyWork => "my_work",
        Commands::Status {
            group,
            all,
            json: _,
            watch,
            placement: _,
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
                    placement: _,
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
        Commands::Issue {
            action:
                cadence_agent::issue::cli::IssueAction::Project {
                    action:
                        cadence_agent::issue::cli::ProjectAction::Policy {
                            key,
                            visibility,
                            grants,
                        },
                },
        } => {
            // Exact shared arguments. Visibility is the canonical spelling
            // (`workspace-visible` | `private`), never the serde rename of
            // the tracker enum. Grants replace the recorded list; an empty
            // list is sent as `[]`, not omitted. No role, actor, or org.
            let visibility = match visibility {
                cadence_agent::issue::cli::PolicyVisibility::WorkspaceVisible => {
                    "workspace-visible"
                }
                cadence_agent::issue::cli::PolicyVisibility::Private => "private",
            };
            args.insert("project".into(), json!(key));
            args.insert("visibility".into(), json!(visibility));
            args.insert("grants".into(), json!(grants));
            "project_policy"
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
        // CAD-1568: `cadence send` and `cadence message send` map to
        // `message_send`. The remote send is a durable hosted enqueue:
        // `--message` (the caller-chosen stable id) is REQUIRED so a lost
        // reply can be reconciled by id instead of re-delivering blindly.
        // The local-endpoint surfaces (`--ready`/`--force`/`--nudge`),
        // local routing (`--reply-to`, `--task`) and the steering flags
        // (`--priority`/`--supersedes`, an operator-or-PM gate the remote
        // caller cannot satisfy) are refused before a request exists —
        // never silently dropped onto the wire.
        Commands::Send {
            alias,
            to,
            text,
            file,
            message,
            reply_to,
            task,
            ready,
            force,
            nudge,
            steer,
            wait,
        } => {
            if wait.is_some() {
                return Err(unsupported(
                    "send",
                    "--wait",
                    "a remote send is a durable enqueue; read the outcome with `message read`",
                ));
            }
            let recipient = alias.as_deref().or(to.as_deref()).unwrap_or_default();
            return remote_send_args(
                recipient, text, file, message, reply_to, task, *ready, *force, *nudge, steer,
            )
            .map(|args| ("message_send", args));
        }
        Commands::Message {
            action:
                MessageAction::Send {
                    alias,
                    text,
                    file,
                    message,
                    reply_to,
                    task,
                    ready,
                    force,
                    nudge,
                    steer,
                },
        } => {
            return remote_send_args(
                alias, text, file, message, reply_to, task, *ready, *force, *nudge, steer,
            )
            .map(|args| ("message_send", args));
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
        Commands::Team { action } => team_for(action, &mut args),
        // CAD-1586: `cadence master status` on a remote org — the hosted
        // `master_status` verb (settings.read) carrying only an optional
        // grant id; it translates to the same `master.status` settings
        // operation the local RPC path takes.
        Commands::Master {
            action: MasterAction::Status { grant },
        } => {
            if let Some(id) = grant {
                args.insert("grant_id".into(), json!(id));
            }
            "master_status"
        }
        // CAD-1586: `cadence master resume` on a remote org — the bounded
        // recovery action rides the existing `settings_agents` verb as
        // operation `master.resume` (settings.write; no dedicated hosted
        // verb, no caller-supplied alias).
        Commands::Master {
            action: MasterAction::Resume { grant },
        } => {
            args.insert("operation".into(), json!("master.resume"));
            args.insert("expected_revision".into(), Value::Null);
            let mut payload = serde_json::Map::new();
            if let Some(id) = grant {
                payload.insert("grant_id".into(), json!(id));
            }
            args.insert("payload".into(), Value::Object(payload));
            "settings_agents"
        }
        _ => return Err(unavailable(command)),
    };
    Ok((verb, args))
}

/// CAD-1568: `send`/`message send` → `message_send` arguments. Exactly
/// `{alias, text, message}` crosses the wire — three strings, the same
/// recipient + body + stable-id the local `agent_send` call would take,
/// minus every surface that cannot ride the remote envelope faithfully.
/// Sender identity lives only in the verified envelope; no flag in this
/// build may name a sender, a role, or a different recipient class.
#[allow(clippy::too_many_arguments)]
fn remote_send_args(
    recipient: &str,
    text: &Option<String>,
    file: &Option<std::path::PathBuf>,
    message: &Option<String>,
    reply_to: &Option<String>,
    task: &Option<String>,
    ready: bool,
    force: bool,
    nudge: bool,
    steer: &SteerArgs,
) -> Result<serde_json::Map<String, Value>> {
    if ready || force {
        return Err(unsupported(
            "send",
            "--ready/--force",
            "they claim a local pane — the remote send is a durable hosted enqueue",
        ));
    }
    if nudge {
        return Err(unsupported(
            "send",
            "--nudge",
            "mid-turn steering pastes into a local endpoint; a remote send is a queued message",
        ));
    }
    if steer.priority.is_some() || !steer.supersedes.is_empty() {
        return Err(unsupported(
            "send",
            "--priority/--supersedes",
            "steering ranks need the local operator-or-PM gate, which a remote call never carries",
        ));
    }
    if reply_to.is_some() {
        return Err(unsupported(
            "send",
            "--reply-to",
            "the remote send has no result-routing surface — read the outcome with \
             `cadence message read <id> --org <org>`",
        ));
    }
    if task.is_some() {
        return Err(unsupported(
            "send",
            "--task",
            "a remote send cannot bind to a local job task",
        ));
    }
    let Some(id) = message.as_deref().map(str::trim).filter(|m| !m.is_empty()) else {
        return Err(Error::rejected(
            "remote `send` needs an explicit --message <id> — a stable caller-chosen id is \
             the reconciliation key if the reply is lost. Pick one (1-64 lowercase letters, \
             digits or hyphens, not starting with '-', never `sys-*`) and reuse it to retry \
             this exact send; nothing was sent",
        ));
    };
    let id = cadence_agent::proto::identifier(id, "--message")?;
    if id.starts_with(cadence_agent::proto::DAEMON_MESSAGE_PREFIX) {
        return Err(Error::rejected(
            "--message may not start with `sys-` — that prefix is daemon-reserved; \
             pick another id",
        ));
    }
    if recipient.trim().is_empty() {
        return Err(Error::rejected(
            "remote `send` needs a recipient alias — nothing was sent",
        ));
    }
    // Body: `--text` verbatim, or a client-side file/stdin read bounded by
    // the daemon's enqueue cap — a hosted caller has no file access, and an
    // oversized body is refused whole rather than truncated (no silent cut).
    let body = read_body_capped(
        text.clone(),
        file.clone(),
        cadence_agent::store::ENQUEUE_BYTES as u64,
    )?;
    if body.len() > cadence_agent::store::ENQUEUE_BYTES {
        return Err(Error::rejected(
            "message body exceeds the enqueue cap — trim it; nothing was sent",
        ));
    }
    if body.trim().is_empty() {
        return Err(Error::rejected(
            "message body is empty — pass --text or --file; nothing was sent",
        ));
    }
    let mut args = serde_json::Map::new();
    args.insert("alias".into(), json!(recipient));
    args.insert("text".into(), json!(body));
    args.insert("message".into(), json!(id));
    Ok(args)
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
        WikiAction::Cp { .. } => {
            return Err(Error::rejected("wiki cp is sent by its own path"));
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
                 (text), wiki cp. Use `--org local` or `cadence org switch` to run locally.",
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

/// Remote blob cats stay under the default wiki upload cap. A larger
/// pointer is refused before a range is fetched or a spool is created.
const REMOTE_BLOB_SPOOL_BOUND: u64 = 100 * 1024 * 1024;

/// Normal remote `wiki cat` of a blob (no `--meta`, no range flags).
/// Discovery already returned pointer metadata. This fetches bounded
/// ranges pinned to that revision, spools them privately, and writes
/// stdout only after the spool matches the pointer SHA-256.
fn remote_blob_cat(
    target: &cadence_agent::remote_cli::RemoteTarget,
    command: &Commands,
    discovered: &Value,
    deadline: std::time::Instant,
    wake_timeout: u64,
) -> Result<bool> {
    let Commands::Wiki {
        action:
            WikiAction::Cat {
                path,
                meta,
                offset,
                bytes,
                rev,
            },
    } = command
    else {
        return Ok(false);
    };
    if *meta || offset.is_some() || bytes.is_some() || rev.is_some() || discovered["kind"] != "blob"
    {
        return Ok(false);
    }
    stream_remote_blob(target, path, discovered, deadline, wake_timeout)?;
    Ok(true)
}

fn stream_remote_blob(
    target: &cadence_agent::remote_cli::RemoteTarget,
    path: &str,
    discovered: &Value,
    deadline: std::time::Instant,
    wake_timeout: u64,
) -> Result<()> {
    let mut spool = collect_remote_blob(discovered, |offset, rev| {
        remote_blob_range(target, path, offset, rev, deadline, wake_timeout)
    })?;
    std::io::copy(&mut spool, &mut std::io::stdout())
        .map_err(|e| Error::rejected(format!("remote wiki cat: cannot write stdout: {e}")))?;
    Ok(())
}

fn collect_remote_blob(
    discovered: &Value,
    mut fetch: impl FnMut(u64, &str) -> Result<Value>,
) -> Result<tempfile::NamedTempFile> {
    use sha2::Digest;
    use std::io::{Seek, SeekFrom, Write};
    use std::os::unix::fs::PermissionsExt;

    let sha = blob_meta_str(discovered, "sha256")?;
    if sha.len() != 64 || !sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(Error::rejected(
            "remote wiki cat refused: blob sha256 is not 64 lowercase hex digits; nothing was written",
        ));
    }
    let rev = blob_meta_str(discovered, "rev")?;
    let canon = blob_meta_str(discovered, "path")?;
    let size = discovered["size"].as_u64().ok_or_else(|| {
        Error::rejected(
            "remote wiki cat refused: blob size is not a non-negative integer; nothing was written",
        )
    })?;
    if size > REMOTE_BLOB_SPOOL_BOUND {
        return Err(Error::rejected(format!(
            "remote wiki cat refused: blob is {size} bytes, over the {REMOTE_BLOB_SPOOL_BOUND}-byte spool bound; nothing was written"
        )));
    }
    let mut spool = tempfile::Builder::new()
        .prefix(".cadence-wiki-blob-")
        .permissions(std::fs::Permissions::from_mode(0o600))
        .tempfile()
        .map_err(|e| {
            Error::rejected(format!(
                "remote wiki cat refused: cannot open a private spool: {e}; nothing was written"
            ))
        })?;
    let mut offset = 0u64;
    let mut hasher = sha2::Sha256::new();
    loop {
        let page = fetch(offset, rev)?;
        if !blob_range_fits(&page) {
            return Err(Error::rejected(
                "remote wiki cat refused: complete blob range exceeds the native bound; nothing was written",
            ));
        }
        if page.get("data_base64").is_none() {
            return Err(Error::rejected(
                "remote wiki cat refused: the server returned blob metadata without original bytes (data_base64); nothing was written",
            ));
        }
        for key in ["kind", "mime", "name", "rev", "sha256", "size"] {
            if page.get(key) != discovered.get(key) {
                return Err(Error::rejected(format!(
                    "remote wiki cat refused: blob range changed '{key}'; nothing was written"
                )));
            }
        }
        if page["path"].as_str() != Some(canon) {
            return Err(Error::rejected(
                "remote wiki cat refused: blob range path does not match the pointer; nothing was written",
            ));
        }
        let start = page["range"]["start"].as_u64().ok_or_else(|| {
            Error::rejected(
                "remote wiki cat refused: blob range start is missing; nothing was written",
            )
        })?;
        let end = page["range"]["end"].as_u64().ok_or_else(|| {
            Error::rejected(
                "remote wiki cat refused: blob range end is missing; nothing was written",
            )
        })?;
        let total = page["range"]["total"].as_u64().ok_or_else(|| {
            Error::rejected(
                "remote wiki cat refused: blob range total is missing; nothing was written",
            )
        })?;
        if start != offset || total != size || end < start || end > size {
            return Err(Error::rejected(format!(
                "remote wiki cat refused: blob range {start}..{end} of {total} does not continue offset {offset} of {size}; nothing was written"
            )));
        }
        let encoded = page["data_base64"].as_str().ok_or_else(|| {
            Error::rejected(
                "remote wiki cat refused: data_base64 is not a string; nothing was written",
            )
        })?;
        if encoded.len() >= cadence_agent::wiki::NATIVE_TOOL_LINE_BOUND {
            return Err(Error::rejected(
                "remote wiki cat refused: encoded blob range exceeds the native bound; nothing was written",
            ));
        }
        use base64::Engine;
        let chunk = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| {
                Error::rejected(
                    "remote wiki cat refused: data_base64 is not standard base64; nothing was written",
                )
            })?;
        if chunk.len() as u64 != end - start
            || chunk.len() >= cadence_agent::wiki::NATIVE_TOOL_LINE_BOUND
        {
            return Err(Error::rejected(
                "remote wiki cat refused: blob range bytes do not match the declared slice; nothing was written",
            ));
        }
        if end == offset && size != offset {
            return Err(Error::rejected(
                "remote wiki cat refused: blob range made no progress; nothing was written",
            ));
        }
        spool.write_all(&chunk).map_err(|e| {
            Error::rejected(format!(
                "remote wiki cat refused: cannot spool blob bytes: {e}; nothing was written"
            ))
        })?;
        hasher.update(&chunk);
        offset = end;
        if offset == size {
            break;
        }
    }
    let got: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if got != sha || offset != size {
        return Err(Error::rejected(
            "remote wiki cat refused: spooled bytes do not match the pointer sha256; nothing was written",
        ));
    }
    spool.seek(SeekFrom::Start(0)).map_err(|e| {
        Error::rejected(format!(
            "remote wiki cat refused: cannot read the blob spool: {e}; nothing was written"
        ))
    })?;
    Ok(spool)
}

fn blob_range_fits(page: &Value) -> bool {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() >= cadence_agent::wiki::NATIVE_TOOL_LINE_BOUND - 1 - self.0 {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Count(0), page).is_ok()
}

fn blob_meta_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            Error::rejected(format!(
                "remote wiki cat refused: blob metadata '{key}' is missing; nothing was written"
            ))
        })
}

fn remote_blob_range(
    target: &cadence_agent::remote_cli::RemoteTarget,
    path: &str,
    offset: u64,
    rev: &str,
    deadline: std::time::Instant,
    wake_timeout: u64,
) -> Result<Value> {
    let mut args = serde_json::Map::new();
    args.insert("path".into(), json!(path));
    args.insert("offset".into(), json!(offset));
    args.insert("rev".into(), json!(rev));
    match cadence_agent::remote_cli::call_verb(
        target,
        "read",
        args,
        deadline,
        wake_timeout,
        |line| eprintln!("{line}"),
    )? {
        cadence_agent::remote_cli::RemoteAnswer::Ok(body) => Ok(body),
        cadence_agent::remote_cli::RemoteAnswer::Conflict(_) => Err(Error::rejected(
            "remote wiki cat refused: blob range returned a conflict; nothing was written",
        )),
    }
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

fn team_for(
    action: &super::commands::TeamAction,
    args: &mut serde_json::Map<String, Value>,
) -> &'static str {
    use super::commands::TeamAction;
    match action {
        TeamAction::List => "team_list",
        TeamAction::Invites => "team_invites",
        TeamAction::Invite { email, role } => {
            args.insert("email".into(), json!(email));
            args.insert("role".into(), json!(role.as_str()));
            "team_invite"
        }
        TeamAction::RevokeInvite { invite_id } => {
            args.insert("invite_id".into(), json!(invite_id));
            "team_revoke_invite"
        }
        TeamAction::SetRole { user_id, role } => {
            args.insert("user_id".into(), json!(user_id));
            args.insert("role".into(), json!(role.as_str()));
            "team_set_role"
        }
        TeamAction::Remove { user_id } => {
            args.insert("user_id".into(), json!(user_id));
            "team_remove"
        }
    }
}

fn unavailable(command: &Commands) -> Error {
    // The refusal names the command so an agent can branch on it; the
    // allowlisted alternatives are the only remote surface.
    Error::rejected(format!(
        "`cadence {}` is not available on a remote org in this build - \
         remote verbs are: status, agent list, agent show, issue ls, \
         issue show, issue log, issue new, issue comment, issue set, \
         send/message send (--message required), message read, inbox (peek), \
         issue project policy, wiki ls, wiki search, wiki cat, \
         wiki put (text), wiki cp, settings agents <verb>, master status, \
         team list, team invites, team invite, team revoke-invite, \
         team set-role, team remove, delivery requirements, audit approval, \
         audit scope, audit approve (merge). \
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
        Commands::Team { .. } => "team <verb>",
        Commands::Settings { .. } => "settings agents <verb>",
        Commands::Master { .. } => "master <verb>",
        _ => "<command>",
    }
}

/// The placement filter a remote read carries, if any.
fn placement_of(command: &Commands) -> Option<registry::Placement> {
    match command {
        Commands::Status { placement, .. } => placement.map(PlacementArg::placement),
        Commands::Agent {
            action: AgentAction::List { placement, .. },
        } => placement.map(PlacementArg::placement),
        _ => None,
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
    local_state: Option<std::path::PathBuf>,
) -> Result<i32> {
    if let Commands::Org { action } = command {
        return org::run(action);
    }
    // CAD-1586: remote settings authority is the stored LOGIN credential —
    // the gateway mints an envelope from it, and a `user`+`owner` envelope
    // lands as RemoteOwner: blanket settings authority with no agent
    // grant binding. A managed caller (CADENCE_ALIAS — worker, master or
    // a daemon-approved argv child under CADENCE_GRANT_TOKEN) spending it
    // would impersonate the human owner. There is no client-side
    // agents/<h> credential path yet (remote_auth notes per-agent
    // enrollment is not configured), so remote settings verbs are refused
    // for managed callers outright — never narrowed to "no grant" and
    // never silently allowed. Operators on their own shell unaffected.
    if (matches!(&command, Commands::Settings { .. })
        || matches!(
            &command,
            Commands::Master {
                action: MasterAction::Status { .. } | MasterAction::Resume { .. },
            }
        ))
        && (target.identity.is_some()
            || std::env::var_os("CADENCE_ALIAS").is_some()
            || std::env::var_os("CADENCE_GRANT_TOKEN").is_some())
    {
        return Err(Error::invalid(
            "settings_agent_session_required",
            format!(
                "remote `{}` refused for a managed caller: the call would spend the \
             stored owner login — owner-class settings authority an agent can \
             never hold — and there is no hosted agent-principal credential to \
             bind a settings grant to yet; the operator runs this from a plain \
             shell, or grant-bound ops run on the local daemon (`cadence \
             settings agents grant request …`)",
                name(&command)
            ),
        ));
    }
    // CAD-1586: resource commands carry their payloads directly through the
    // explicit `settings_agents` verb before the generic
    // verb map; nothing settings-shaped ever falls back to local state.
    if let Commands::Settings {
        action: settings::SettingsAction::Agents { action, grant },
    } = &command
    {
        return settings::run_remote(&target, action, grant, wake_timeout, local_state.as_deref());
    }
    if let Commands::Agent { action } = &command {
        // `--cloud` and `agent cloud` talk to the local daemon. A saved
        // remote org must not send them over the hosted pickup path:
        // `agent connect` without `--cloud` is that path.
        match action {
            AgentAction::Connect {
                cloud: true,
                workspace,
                alias,
                ..
            } => {
                let state = local_state
                    .clone()
                    .map(Ok)
                    .unwrap_or_else(cadence_agent::client::state_dir)?;
                return cloud_agent::connect(state, workspace.clone(), alias.clone());
            }
            AgentAction::Cloud { action } => {
                let state = local_state
                    .clone()
                    .map(Ok)
                    .unwrap_or_else(cadence_agent::client::state_dir)?;
                return cloud_agent::run(state, action.clone());
            }
            _ => {}
        }
        if let Some(bridge) = action.bridge() {
            return local_agent::run(&target, &bridge, wake_timeout, local_state);
        }
    }
    if let Commands::LocalAgent { action } = &command {
        return local_agent::run(&target, action, wake_timeout, local_state);
    }
    if let Commands::App { action } = &command {
        return super::hosted_app::run(&target, action);
    }
    if let Commands::Restore {
        source,
        repos,
        force,
        portable,
        preflight,
        replace,
        controller_state,
        expected_package_digest,
        expected_destination_digest,
        expected_generation,
        activate,
        activate_disabled,
        activate_preflight,
        reconciliation,
        request_id,
        status,
        rollback,
    } = command
    {
        return remote_restore(
            target,
            portable,
            preflight,
            replace,
            source,
            repos,
            force,
            controller_state,
            expected_package_digest,
            expected_destination_digest,
            expected_generation,
            activate,
            activate_disabled,
            activate_preflight,
            reconciliation,
            request_id,
            status,
            rollback,
            local_state,
        );
    }
    if let Commands::Dispatch { issue, to, job, .. } = &command {
        if *job {
            return Err(Error::usage(
                "hosted dispatch does not open a job. Nothing was sent.",
            ));
        }
        let Some(to) = to.as_deref() else {
            return Err(Error::usage("hosted dispatch requires --to <alias>"));
        };
        return local_agent::dispatch(&target, issue, to);
    }
    if let Commands::Issue {
        action:
            cadence_agent::issue::cli::IssueAction::Start {
                id,
                repo,
                name,
                base,
                owner,
                job,
                pm,
                spec,
                assignee,
                force,
                by,
                take_over,
            },
    } = &command
    {
        if *job
            || base.is_some()
            || owner.is_some()
            || pm.is_some()
            || spec.is_some()
            || assignee.is_some()
            || *force
            || by.is_some()
            || take_over.is_some()
        {
            return Err(Error::usage(
                "hosted issue start accepts --repo and --name only. It claims this machine's local agent and does not open a job. Nothing was sent.",
            ));
        }
        return local_agent::start(
            &target,
            id,
            repo.as_deref(),
            name.as_deref(),
            wake_timeout,
            local_state,
        );
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
    if let Commands::Wiki {
        action:
            WikiAction::Cp {
                source,
                dest,
                recursive,
                update,
                resume,
                wait_index,
            },
    } = &command
    {
        return cadence_agent::wiki::transfer_client::copy_remote(
            &target,
            wake_timeout,
            &cadence_agent::wiki::transfer_client::CopyRequest {
                source,
                dest,
                recursive: *recursive,
                update: *update,
                resume: resume.as_deref(),
                wait_index: *wait_index,
            },
        );
    }
    if let Some(code) = merge_gate(&target, &command, wake_timeout)? {
        return Ok(code);
    }
    let (verb, args) = verb_for(&command)?;
    // CAD-1568: a remote send keeps its caller-chosen `--message` id for
    // reconciliation. An `unknown` outcome — the response was lost after
    // the call was attempted — may have recorded the message anyway; the
    // note names the id and the readback so the caller reconciles by id
    // instead of resubmitting a possible duplicate. Definite refusals
    // (`rejected`) and pre-forward `busy` verdicts name their own remedy.
    let send_key = (verb == "message_send")
        .then(|| {
            args.get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .flatten();
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(wake_timeout))
        .ok_or_else(|| Error::rejected("invalid wake timeout"))?;
    let out = match cadence_agent::remote_cli::call_verb(
        &target,
        verb,
        args,
        deadline,
        wake_timeout,
        |line| eprintln!("{line}"),
    ) {
        Ok(out) => out,
        Err(error) => {
            if let Some(id) = send_key.filter(|_| error.kind() == "unknown") {
                eprintln!(
                    "remote send `{id}` was not confirmed — it may or may not have \
                     been recorded. Reconcile before resubmitting: `cadence message \
                     read {id} --org {}` returns your durable message and its state. \
                     If nothing is recorded, retry `send` with the same --message {id} \
                     — the same id and content does not enqueue another message.",
                    target.org
                );
            }
            return remote_failure(error);
        }
    };
    match out {
        cadence_agent::remote_cli::RemoteAnswer::Ok(mut body) => {
            // A placement read filter is a client-side projection of the
            // same derived value, so the hosted read applies it before
            // printing and recomputes the capacity count it projects.
            if let Some(want) = placement_of(&command) {
                retain_by_placement(&mut body, want);
            }
            match remote_blob_cat(&target, &command, &body, deadline, wake_timeout) {
                Ok(true) => return Ok(0),
                Ok(false) => {}
                Err(error) => return remote_failure(error),
            }
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

/// CAD-1946: the merge gate's commands against a hosted org — the reads
/// `scripts/enqueue-reviewed` makes and the operator's merge approval.
/// `None` for every other command. Output and exit codes match the local
/// commands; the checks live in [`cadence_agent::delivery::hosted_gate`].
fn merge_gate(target: &RemoteTarget, command: &Commands, wake_timeout: u64) -> Result<Option<i32>> {
    use cadence_agent::delivery::hosted_gate;
    let repo_or_origin = |repo: &Option<String>| -> Result<String> {
        match repo {
            Some(r) => Ok(r.clone()),
            None => cadence_agent::audit::origin_slug(&std::env::current_dir()?).ok_or_else(|| {
                Error::rejected("no github.com origin remote in the cwd — pass --repo owner/name")
            }),
        }
    };
    let printed = |(doc, code): (Value, i32)| {
        print_json(&doc);
        Some(code)
    };
    Ok(match command {
        Commands::Delivery {
            action: DeliveryAction::Requirements {
                issue, pr, head, ..
            },
        } => printed((
            hosted_gate::requirements(target, wake_timeout, issue, pr, head)?,
            0,
        )),
        Commands::Audit {
            action: Some(AuditAction::Approval { pr, head, repo }),
            ..
        } => {
            let head = super::audit::full_head(head)?;
            let repo = repo_or_origin(repo)?;
            printed(hosted_gate::approval(
                target,
                wake_timeout,
                &repo,
                *pr,
                &head,
            )?)
        }
        Commands::Audit {
            action: Some(AuditAction::Scope { issue }),
            ..
        } => printed(hosted_gate::scope(target, wake_timeout, issue.trim())?),
        Commands::Audit {
            action:
                Some(AuditAction::Approve {
                    pr,
                    head,
                    issue,
                    delegated,
                    scope,
                    source,
                    repo,
                    action,
                    id,
                }),
            ..
        } => {
            // The stored login is the human's; a managed caller spending
            // it would approve as the operator (the CAD-1586 rule above).
            if target.identity.is_some()
                || std::env::var_os("CADENCE_ALIAS").is_some()
                || std::env::var_os("CADENCE_GRANT_TOKEN").is_some()
            {
                return Err(Error::rejected(
                    "remote `audit approve` refused for a managed caller: an approval is \
                     the operator's own act and the stored login is not an agent's to spend. \
                     Nothing was sent.",
                ));
            }
            if issue.is_some() || *delegated || scope.is_some() || id.is_some() || action != "merge"
            {
                return Err(Error::usage(
                    "remote `audit approve` records a merge approval only: --issue/--action \
                     scope, --delegated, --scope and --id are not available on a hosted org. \
                     Nothing was sent.",
                ));
            }
            let pr = pr.ok_or_else(|| Error::rejected("--pr is required"))?;
            let head = super::audit::full_head(head.as_deref().unwrap_or_default())?;
            let repo = repo_or_origin(repo)?;
            let receipt = hosted_gate::approve(target, wake_timeout, &repo, pr, &head, source)?;
            printed((receipt, 0))
        }
        _ => None,
    })
}

/// CAD-1593: `restore` against a remote org. Only `--portable` forms exist
/// remotely — everything else refuses before a request is built, exactly
/// like the local flag checks, so a misplaced knob is a usage error, never
/// a silently ignored local-mode option. `dir` is the *credential* state
/// dir (`cli_material_until` reads the registered workspace login); a
/// caller-chosen `--state-dir` never becomes a hosted path.
#[allow(clippy::too_many_arguments)]
fn remote_restore(
    target: RemoteTarget,
    portable: bool,
    preflight: bool,
    replace: bool,
    source: Option<PathBuf>,
    repos: Vec<PathBuf>,
    force: bool,
    controller_state: Option<PathBuf>,
    expected_package_digest: Option<String>,
    expected_destination_digest: Option<String>,
    expected_generation: Option<String>,
    activate: bool,
    activate_disabled: bool,
    activate_preflight: bool,
    reconciliation: Option<PathBuf>,
    request_id: Option<String>,
    status: Option<String>,
    rollback: Option<String>,
    state_dir: Option<PathBuf>,
) -> Result<i32> {
    if state_dir.is_some() {
        return Err(unsupported(
            "restore --portable",
            "--state-dir",
            "it names a local destination — the hosted migration never opens one",
        ));
    }
    if !portable {
        return Err(Error::rejected(
            "remote `restore` is `restore --portable` only — a hosted workspace \
             holds no local backup or export-bundle path",
        ));
    }
    for (flag, present) in [
        ("--repo", !repos.is_empty()),
        ("--force", force),
        ("--controller-state", controller_state.is_some()),
        ("--activate", activate),
        ("--activate-disabled", activate_disabled),
        ("--activate-preflight", activate_preflight),
        ("--reconciliation", reconciliation.is_some()),
    ] {
        if present {
            return Err(unsupported(
                "restore --portable",
                flag,
                "it names a local destination or controller — the hosted \
                 migration never opens one",
            ));
        }
    }
    let note = |line: &str| eprintln!("{line}");
    let digests = expected_package_digest.is_some()
        || expected_destination_digest.is_some()
        || expected_generation.is_some();
    if let Some(id) = status.as_deref() {
        if source.is_some() || rollback.is_some() || preflight || replace || digests {
            return Err(Error::usage(
                "--status takes an operation id or request:<id>, without a source — choose one remote \
                 restore action: --preflight, --status, --rollback or an \
                 import/--replace run",
            ));
        }
        let receipt = cadence_agent::remote_migration::run(
            &target,
            MigrationCommand::Status {
                operation_id: id.to_string(),
                // A caller-supplied request id binds the receipt check.
                request_id,
            },
            note,
        )?;
        print_json(&receipt);
        return Ok(0);
    }
    if let Some(id) = rollback {
        if source.is_some() || preflight || replace || expected_package_digest.is_some() {
            return Err(Error::usage(
                "choose one remote restore action: --rollback does not combine \
                 with --preflight, --replace or --expected-package-digest",
            ));
        }
        if request_id.is_none() {
            return Err(Error::usage(
                "--rollback needs --request-id — the same stable id the \
                 replacement run minted, never a fresh one",
            ));
        }
        if expected_destination_digest.is_none() || expected_generation.is_none() {
            return Err(Error::usage(
                "--rollback needs --expected-destination-digest and \
                 --expected-generation — the CURRENT values from \
                 `restore --portable --status`, never a preflight pair",
            ));
        }
        let receipt = cadence_agent::remote_migration::run(
            &target,
            MigrationCommand::Rollback {
                operation_id: id,
                request_id: request_id.unwrap_or_default(),
                expected_destination_digest: expected_destination_digest.unwrap_or_default(),
                expected_generation: expected_generation.unwrap_or_default(),
            },
            note,
        )?;
        print_json(&receipt);
        return Ok(migration_outcome_exit_code(&receipt));
    }
    let source = source.ok_or_else(|| {
        Error::usage(
            "remote `restore --portable` needs the package directory — \
             only `--status`/`--rollback` omit the source",
        )
    })?;
    if preflight {
        if digests || request_id.is_some() || replace {
            return Err(Error::usage(
                "--preflight is read-only: no --request-id, --replace or \
                 --expected-* — it prints the values those flags need",
            ));
        }
        let receipt = cadence_agent::remote_migration::run(
            &target,
            MigrationCommand::Preflight { source },
            note,
        )?;
        print_json(&receipt);
        return Ok(0);
    }
    let request_id = request_id.ok_or_else(|| {
        Error::usage(
            "remote `restore --portable` needs --request-id — a stable id you \
             mint once and reuse verbatim on replay",
        )
    })?;
    if replace {
        if expected_package_digest.is_none()
            || expected_destination_digest.is_none()
            || expected_generation.is_none()
        {
            return Err(Error::usage(
                "remote `restore --portable --replace` needs \
                 --expected-package-digest, --expected-destination-digest \
                 and --expected-generation — the exact values from \
                 `restore --portable --preflight`",
            ));
        }
        let receipt = cadence_agent::remote_migration::run(
            &target,
            MigrationCommand::Replace {
                source,
                request_id,
                expected_package_digest: expected_package_digest.unwrap_or_default(),
                expected_destination_digest: expected_destination_digest.unwrap_or_default(),
                expected_generation: expected_generation.unwrap_or_default(),
            },
            note,
        )?;
        print_json(&receipt);
        return Ok(migration_outcome_exit_code(&receipt));
    }
    if digests {
        return Err(Error::usage(
            "--expected-* digests belong to --replace; a plain remote \
             `restore --portable` imports inactive with just --request-id",
        ));
    }
    // A plain remote `restore --portable --request-id …` imports INACTIVE —
    // it never replaces or activates a running workspace.
    let receipt = cadence_agent::remote_migration::run(
        &target,
        MigrationCommand::Import { source, request_id },
        note,
    )?;
    print_json(&receipt);
    Ok(migration_outcome_exit_code(&receipt))
}

/// Mutation success means a verified terminal result, not HTTP acceptance.
/// Status queries remain successful reads even when the recorded job failed.
fn migration_outcome_exit_code(receipt: &Value) -> i32 {
    match receipt.get("phase").and_then(Value::as_str) {
        Some("committed_inactive" | "replaced_inactive" | "rolled_back") => 0,
        Some("failed") => cadence_agent::error::exit_code_for_kind("rejected"),
        Some("unknown") => cadence_agent::error::exit_code_for_kind("unknown"),
        _ => cadence_agent::error::exit_code_for_kind("busy"),
    }
}

pub(super) fn remote_failure(error: Error) -> Result<i32> {
    if error.kind() == "busy" && error.code() == Some("waking") {
        eprintln!("{}", json!({"kind": "busy", "state": "waking"}));
        Ok(75)
    } else {
        Err(error)
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
                placement: None,
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
                placement: None,
            },
            Commands::Status {
                group: None,
                all: true,
                json: false,
                watch: None,
                placement: None,
            },
            Commands::Status {
                group: None,
                all: false,
                json: false,
                watch: Some(2),
                placement: None,
            },
        ] {
            assert!(outcome(&command).is_err());
        }
        let (verb, args) = outcome(&Commands::Status {
            group: None,
            all: false,
            json: false,
            watch: None,
            placement: None,
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
    fn remote_project_add_and_list_stay_unmapped() {
        use cadence_agent::issue::cli::ProjectAction;
        let add = outcome(&Commands::Issue {
            action: IssueAction::Project {
                action: ProjectAction::Add {
                    key: "cadence".into(),
                    prefix: "CAD".into(),
                    repos: vec![],
                    components: vec![],
                    tags: vec![],
                    owner: None,
                },
            },
        })
        .unwrap_err();
        assert!(add.contains("not available on a remote org"), "{add}");
        let ls = outcome(&Commands::Issue {
            action: IssueAction::Project {
                action: ProjectAction::Ls {
                    sort: None,
                    limit: None,
                    fields: vec![],
                    json: false,
                },
            },
        })
        .unwrap_err();
        assert!(ls.contains("not available on a remote org"), "{ls}");
        let set_remote = outcome(&Commands::Issue {
            action: IssueAction::Project {
                action: ProjectAction::SetRemote {
                    key: "cadence".into(),
                    from: "github.com/old/name".into(),
                    to: "github.com/new/name".into(),
                },
            },
        })
        .unwrap_err();
        assert!(
            set_remote.contains("not available on a remote org"),
            "{set_remote}"
        );
    }

    /// CAD-1568: `cadence send`/`message send` become `message_send` with
    /// exactly `alias`+`text`+`message`. `--message` is required: it is the
    /// stable key a lost reply reconciles by and a safe resubmission dedupes
    /// under. Every local-only surface is refused before a request exists.
    fn send_cmd(f: impl FnOnce(&mut Commands)) -> Commands {
        let mut command = Commands::Send {
            alias: Some("master".into()),
            to: None,
            text: Some("please coordinate".into()),
            file: None,
            message: Some("cad1568-proof-1".into()),
            reply_to: None,
            task: None,
            ready: false,
            force: false,
            nudge: false,
            steer: SteerArgs::default(),
            wait: None,
        };
        f(&mut command);
        command
    }

    #[test]
    fn remote_send_refuses_wait() {
        let with_wait = send_cmd(|c| {
            let Commands::Send { wait, .. } = c else {
                unreachable!()
            };
            *wait = Some(30);
        });
        let err = outcome(&with_wait).unwrap_err();
        assert!(err.to_string().contains("--wait"), "{err}");
    }

    #[test]
    fn remote_send_requires_an_explicit_stable_message_id() {
        let missing_id = send_cmd(|c| {
            let Commands::Send { message, .. } = c else {
                unreachable!()
            };
            *message = None;
        });
        assert!(outcome(&missing_id).is_err());
        let reserved_id = send_cmd(|c| {
            let Commands::Send { message, .. } = c else {
                unreachable!()
            };
            *message = Some("sys-wake-9".into());
        });
        assert!(outcome(&reserved_id).is_err());
    }

    #[test]
    fn remote_send_refuses_local_only_flags_before_any_request() {
        let cases: [fn(&mut Commands); 6] = [
            |c| {
                let Commands::Send { ready, .. } = c else {
                    unreachable!()
                };
                *ready = true;
            },
            |c| {
                let Commands::Send { nudge, .. } = c else {
                    unreachable!()
                };
                *nudge = true;
            },
            |c| {
                let Commands::Send { steer, .. } = c else {
                    unreachable!()
                };
                steer.priority = Some("urgent".into());
            },
            |c| {
                let Commands::Send { steer, .. } = c else {
                    unreachable!()
                };
                steer.supersedes = vec!["m1".into()];
            },
            |c| {
                let Commands::Send { reply_to, .. } = c else {
                    unreachable!()
                };
                *reply_to = Some("pm".into());
            },
            |c| {
                let Commands::Send { task, .. } = c else {
                    unreachable!()
                };
                *task = Some("j1-t1".into());
            },
        ];
        for mutate in cases {
            assert!(outcome(&send_cmd(mutate)).is_err());
        }
    }

    #[test]
    fn remote_send_refuses_missing_or_empty_body() {
        let root = tempfile::tempdir().unwrap();
        let missing_file = send_cmd(|c| {
            let Commands::Send { text, file, .. } = c else {
                unreachable!()
            };
            *text = None;
            *file = Some(root.path().join("absent.md"));
        });
        assert!(outcome(&missing_file).is_err());
        let empty_text = send_cmd(|c| {
            let Commands::Send { text, .. } = c else {
                unreachable!()
            };
            *text = Some("   ".into());
        });
        assert!(outcome(&empty_text).is_err());
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

#[cfg(test)]
mod blob_download_tests {
    use super::*;
    use cadence_agent::{issue::Pm, wiki};
    use std::io::Read;

    fn plant(pm: &Pm, state: &std::path::Path, bytes: &[u8]) -> Value {
        let uploads = state.join(wiki::UPLOAD_DIR);
        std::fs::create_dir_all(&uploads).unwrap();
        let tmp = uploads.join("blob");
        std::fs::write(&tmp, bytes).unwrap();
        wiki::put_blob(
            pm,
            state,
            &wiki::Caller::Operator,
            "global/proof.bin",
            &tmp,
            None,
            None,
        )
        .unwrap()
    }

    #[test]
    fn original_blob_bytes_are_assembled_and_verified_before_release() {
        for bytes in [
            Vec::new(),
            b"%PDF-1.4\nsynthetic original\n%%EOF\n".to_vec(),
            (0..100_000).map(|n| (n % 256) as u8).collect(),
        ] {
            let root = tempfile::tempdir().unwrap();
            let pm = Pm::init(&root.path().join("pm")).unwrap();
            let meta = plant(&pm, &root.path().join("state"), &bytes);
            let mut calls = 0;
            let mut spool = collect_remote_blob(&meta, |offset, rev| {
                calls += 1;
                wiki::read_range(
                    &pm,
                    &wiki::Caller::Operator,
                    "global/proof.bin",
                    wiki::BodyRange {
                        offset,
                        bytes: None,
                        rev: Some(rev),
                    },
                )
            })
            .unwrap();
            let mut actual = Vec::new();
            spool.read_to_end(&mut actual).unwrap();
            assert_eq!(actual, bytes);
            if bytes.len() == 100_000 {
                assert!(calls > 1, "large binary must exercise continuation");
            }
        }
    }

    #[test]
    fn corrupt_or_replaced_blob_cannot_be_released() {
        for replace_pointer in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let pm = Pm::init(&root.path().join("pm")).unwrap();
            let state = root.path().join("state");
            let bytes = vec![0xff; 100_000];
            let meta = plant(&pm, &state, &bytes);
            if !replace_pointer {
                let object = wiki::blobs_dir(&wiki::vault_dir(&pm).unwrap())
                    .join(meta["sha256"].as_str().unwrap());
                std::fs::write(object, vec![0xfe; bytes.len()]).unwrap();
            }
            let mut calls = 0;
            let error = collect_remote_blob(&meta, |offset, rev| {
                if replace_pointer && calls == 1 {
                    plant(&pm, &state, b"replacement");
                }
                calls += 1;
                wiki::read_range(
                    &pm,
                    &wiki::Caller::Operator,
                    "global/proof.bin",
                    wiki::BodyRange {
                        offset,
                        bytes: None,
                        rev: Some(rev),
                    },
                )
            })
            .unwrap_err();
            let message = error.to_string();
            assert!(
                message.contains(if replace_pointer {
                    "revision"
                } else {
                    "sha256"
                }),
                "{message}"
            );
        }
    }

    #[test]
    fn oversized_range_with_valid_bytes_is_not_released() {
        use base64::Engine;
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let bytes = vec![0xff; 37_497];
        let meta = plant(&pm, &root.path().join("state"), &bytes);
        let error = collect_remote_blob(&meta, |offset, rev| {
            let mut page = wiki::read_range(
                &pm,
                &wiki::Caller::Operator,
                "global/proof.bin",
                wiki::BodyRange {
                    offset,
                    bytes: None,
                    rev: Some(rev),
                },
            )?;
            // A peer ignores the native bound, but keeps the bytes and hash valid.
            page["data_base64"] = json!(base64::engine::general_purpose::STANDARD.encode(&bytes));
            page["range"]["end"] = json!(bytes.len());
            Ok(page)
        })
        .unwrap_err();
        assert!(error.to_string().contains("complete blob range"), "{error}");
    }
}
