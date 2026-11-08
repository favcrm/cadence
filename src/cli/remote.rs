//! CAD-1019 slice 2: dispatch for a `Remote` org selection — maps the
//! allowlisted local commands to AOS-128 verbs and runs them through
//! [`cadence_agent::remote_cli`]. Anything outside the allowlist is refused
//! *here*, before a request is ever built. Operator-only daemon verbs never
//! map.
//! `issue project policy` is `project_policy`; `project new` is
//! `project_new` and `issue project ls` is `project_ls` on a hosted org,
//! while `issue project add`/`set-remote` stay local.

use super::*;
use cadence_agent::remote_cli::RemoteTarget;
use cadence_agent::remote_migration::MigrationCommand;
use cadence_agent::wiki::cli::WikiAction;
use std::path::PathBuf;

fn unsupported(verb: &str, flag: &str, why: &str) -> Error {
    Error::rejected(format!("remote `{verb}` cannot {flag} — {why}"))
}

pub(super) const HOSTED_ISSUE_LS_REFUSAL: &str = "the remote route's filter does not implement it";

/// The presence checks and names used by both hosted routing and offline discovery.
#[derive(Default)]
struct HostedIssueLsOptions {
    stage: bool,
    health: bool,
    since: bool,
    until: bool,
    ready: bool,
    blocked: bool,
    at: bool,
    sort: bool,
    limit: bool,
    fields: bool,
    summary: bool,
}

impl HostedIssueLsOptions {
    fn restrictions(self) -> [(&'static str, bool); 11] {
        [
            ("--stage", self.stage),
            ("--health", self.health),
            ("--since", self.since),
            ("--until", self.until),
            ("--ready", self.ready),
            ("--blocked", self.blocked),
            ("--at", self.at),
            ("--sort", self.sort),
            ("--limit", self.limit),
            ("--fields", self.fields),
            ("--summary", self.summary),
        ]
    }
}

pub(super) fn hosted_issue_ls_unsupported_flags() -> [&'static str; 11] {
    HostedIssueLsOptions::default()
        .restrictions()
        .map(|(flag, _)| flag)
}

/// `cadence <verb>` → AOS-128 `(verb, arguments)`. Every returned verb is
/// in `remote_cli::REMOTE_VERBS` or `remote_cli::WIKI_ACTIONS`; every argument value is a primitive the
/// server's `arguments` object accepts. A command outside the set — or a
/// flag that cannot cross the wire faithfully — refuses locally; nothing
/// is sent (contract I2: an allowlist, not a tunnel).
fn verb_for(
    command: &Commands,
    command_path: Option<&str>,
) -> Result<(&'static str, serde_json::Map<String, Value>)> {
    if let Commands::Wiki { action } = command {
        return wiki_for(action, command_path);
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
            action: AgentAction::Profiles,
        } => "agent_profile_list",
        // `agent roles` is a pure read of a static table, so a hosted org
        // answers it from its pinned copy instead of refusing. It takes no
        // arguments at all.
        Commands::Agent {
            action: AgentAction::Roles,
        } => "agent_roles",
        // CAD-2101: the hosted profile verbs. The board owns the field names,
        // limits, revision fence and who may write.
        Commands::Agent {
            action: AgentAction::Profile { action },
        } => {
            let (verb, built) = super::agent_profile::request(action)?;
            args = built;
            verb
        }
        Commands::Agent {
            action: AgentAction::Context { id },
        } => {
            // The id is the board's own `ap_…`; a caller cannot name a path,
            // a verb or an authority field through it.
            args.insert("id".into(), json!(id));
            "agent_profile_context"
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
            // Refuse list filters that cannot cross the hosted route faithfully.
            // CAD-1999: `--project` now crosses the wire — the hosted
            // route filters one registered (or ticket-derived) project,
            // so a repeated flag refuses rather than silently keeping one.
            let hosted_options = HostedIssueLsOptions {
                stage: !stage.is_empty(),
                health: !health.is_empty(),
                since: since.is_some(),
                until: until.is_some(),
                ready: *ready,
                blocked: *blocked,
                at: at.is_some(),
                sort: sort.is_some(),
                limit: limit.is_some(),
                fields: !fields.is_empty(),
                summary: *summary,
            };
            for (flag, present) in hosted_options.restrictions() {
                if present {
                    return Err(unsupported("issue ls", flag, HOSTED_ISSUE_LS_REFUSAL));
                }
            }
            if project.len() > 1 {
                return Err(Error::rejected(
                    "remote `issue ls` filters one --project at a time — the \
                     hosted route takes a single key; nothing was sent",
                ));
            }
            if let Some(key) = project.first() {
                args.insert("project".into(), json!(key));
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
            action: cadence_agent::issue::cli::IssueAction::Attach { id, file },
        } => {
            use base64::Engine;
            use std::io::Read;
            use std::os::unix::fs::OpenOptionsExt;

            const LIMIT: u64 = 1_048_576;
            let name = file
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| cadence_agent::issue::model::valid_artifact_name(name))
                .filter(|name| !cadence_agent::issue::model::reserved_artifact_name(name))
                .ok_or_else(|| Error::rejected("invalid artifact name — nothing was sent"))?;
            let opened = if cadence_agent::master::caller_is_master() {
                let state = cadence_agent::client::state_dir()?;
                cadence_agent::master::open_command_file(&state, file)?
            } else {
                std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(file)?
            };
            let meta = opened.metadata()?;
            if !meta.is_file() || meta.len() > LIMIT {
                return Err(Error::rejected(
                    "artifact must be a regular file of at most 1 MiB — nothing was sent",
                ));
            }
            let mut bytes = Vec::with_capacity(meta.len() as usize);
            opened.take(LIMIT + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > LIMIT {
                return Err(Error::rejected("artifact exceeds 1 MiB — nothing was sent"));
            }
            args.insert("id".into(), json!(id));
            args.insert("name".into(), json!(name));
            args.insert(
                "data".into(),
                json!(base64::engine::general_purpose::STANDARD.encode(bytes)),
            );
            "issue_attach"
        }
        Commands::Issue {
            action: cadence_agent::issue::cli::IssueAction::Artifact { id, name },
        } => {
            if !name
                .strip_prefix("hosted:")
                .is_some_and(cadence_agent::issue::model::valid_artifact_name)
            {
                return Err(Error::rejected("invalid artifact name — nothing was sent"));
            }
            args.insert("id".into(), json!(id));
            args.insert("name".into(), json!(name));
            "issue_artifact"
        }
        Commands::Issue {
            action:
                cadence_agent::issue::cli::IssueAction::Review {
                    id,
                    artifact,
                    digest,
                    decision,
                    text,
                    file,
                },
        } => {
            if !artifact
                .strip_prefix("hosted:")
                .is_some_and(cadence_agent::issue::model::valid_artifact_name)
            {
                return Err(Error::rejected(
                    "invalid artifact identifier — nothing was sent",
                ));
            }
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err(Error::rejected(
                    "digest must be the lowercase hex sha256 you read — nothing was sent",
                ));
            }
            if !matches!(decision.as_str(), "pass" | "revise" | "blocked") {
                return Err(Error::rejected(
                    "decision must be pass, revise or blocked — nothing was sent",
                ));
            }
            let body = match (text, file) {
                (Some(text), None) => text.clone(),
                (None, Some(path)) if path.as_os_str() == "-" => {
                    read_stdin_remote("issue review --file")?
                }
                (None, Some(path)) => std::fs::read_to_string(path).map_err(|e| {
                    Error::rejected(format!(
                        "issue review --file {}: cannot read it locally: {e}",
                        path.display()
                    ))
                })?,
                _ => {
                    return Err(Error::rejected(
                        "one of -m or --file is required — nothing was sent",
                    ))
                }
            };
            if body.trim().is_empty() {
                return Err(Error::rejected(
                    "a verdict needs its findings — nothing was sent",
                ));
            }
            if body.len() > 16 * 1024 {
                return Err(Error::rejected(
                    "verdict text exceeds the 16 KB cap — trim it; nothing was sent",
                ));
            }
            args.insert("id".into(), json!(id));
            args.insert("artifact".into(), json!(artifact));
            args.insert("digest".into(), json!(digest));
            args.insert("decision".into(), json!(decision));
            args.insert("text".into(), json!(body));
            "issue_review"
        }
        Commands::Issue {
            action:
                cadence_agent::issue::cli::IssueAction::Say {
                    id,
                    to,
                    from,
                    message,
                    text,
                    file,
                },
        } => {
            let body = read_body_capped(text.clone(), file.clone(), TICKET_MESSAGE_BYTES as u64)?;
            if body.trim().is_empty() {
                return Err(Error::rejected(
                    "a ticket message needs text — nothing was sent",
                ));
            }
            if body.len() > TICKET_MESSAGE_BYTES {
                return Err(Error::rejected(
                    "ticket message exceeds the 16,000 byte cap — trim it; nothing was sent",
                ));
            }
            args.insert("id".into(), json!(id));
            args.insert("to".into(), json!(ticket_role(to, "--to")?));
            if let Some(from) = from {
                args.insert("from".into(), json!(ticket_role(from, "--from")?));
            }
            args.insert("message".into(), json!(ticket_message_id(message)?));
            args.insert("text".into(), json!(body));
            "issue_message_send"
        }
        Commands::Issue {
            action: cadence_agent::issue::cli::IssueAction::Messages { id, to, after },
        } => {
            args.insert("id".into(), json!(id));
            if let Some(to) = to {
                args.insert("to".into(), json!(ticket_role(to, "--to")?));
            }
            if let Some(after) = after {
                args.insert("after".into(), json!(ticket_seq(*after, "--after")?));
            }
            "issue_messages"
        }
        Commands::Issue {
            action: cadence_agent::issue::cli::IssueAction::MessagesRead { id, role, through },
        } => {
            args.insert("id".into(), json!(id));
            args.insert("role".into(), json!(ticket_role(role, "--role")?));
            args.insert("through".into(), json!(ticket_seq(*through, "--through")?));
            "issue_messages_read"
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
            action: cadence_agent::issue::cli::IssueAction::Link { id, kind, target },
        } => {
            link_args(&mut args, id, kind, target, "link")?;
            "issue_link"
        }
        Commands::Issue {
            action: cadence_agent::issue::cli::IssueAction::Unlink { id, kind, target },
        } => {
            link_args(&mut args, id, kind, target, "unlink")?;
            "issue_unlink"
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
            // the tracker enum; `wire()` is that one spelling, shared with the
            // daemon's parser. Grants replace the recorded list; an empty
            // list is sent as `[]`, not omitted. No role, actor, or org.
            args.insert("project".into(), json!(key));
            args.insert("visibility".into(), json!(visibility.wire()));
            args.insert("grants".into(), json!(grants));
            "project_policy"
        }
        // CAD-1999: `cadence project new` on a hosted org registers an
        // empty project — its key, its human-readable name and an optional
        // issue-id prefix. The local-only flags register a git checkout and
        // seed its PROJECT.md, which a hosted org has no path to, so they
        // refuse before a request exists rather than being dropped. The
        // name is required and sent as given: the registry keeps it
        // distinct from the key (the key is lower-case, the name is the
        // human label).
        Commands::Project {
            action:
                ProjectCmd::New {
                    key,
                    name,
                    repo,
                    prefix,
                    goal,
                    agents,
                    issue,
                    visibility,
                    grants,
                },
        } => {
            // CAD-1950: the hosted create contract is `{key, name, prefix?}`
            // and records no access policy. Accepting the flags here would
            // drop them silently, so they refuse before a request exists —
            // the same rule the local-only flags above follow. `--visibility`
            // is checked by presence, not value: even the local default
            // spelled out on the command line is a request this verb cannot
            // honour. The policy is set afterwards with
            // `issue project policy`, which is hosted.
            if visibility.is_some() || !grants.is_empty() {
                return Err(Error::rejected(
                    "remote `project new` records no access policy — --visibility \
                     and --grant are local-only; create the project, then set its \
                     policy with `cadence issue project policy`; nothing was sent",
                ));
            }
            let mut local = Vec::new();
            if repo.is_some() {
                local.push("--repo");
            }
            if goal.is_some() {
                local.push("--goal");
            }
            if !agents.is_empty() {
                local.push("--agent");
            }
            if issue.is_some() {
                local.push("--issue");
            }
            if !local.is_empty() {
                return Err(Error::rejected(format!(
                    "remote `project new` does not support {} — it registers an \
                     empty hosted project, not a repo checkout or a seeded \
                     PROJECT.md; nothing was sent",
                    local.join(" ")
                )));
            }
            let name = name.as_deref().map(str::trim).unwrap_or_default();
            if name.is_empty() {
                return Err(Error::rejected(
                    "remote `project new` needs --name <human name> — the hosted \
                     registry keeps it beside the key; nothing was sent",
                ));
            }
            if name.len() > 200 {
                return Err(Error::rejected(
                    "remote `project new` --name must be at most 200 bytes — \
                     nothing was sent",
                ));
            }
            args.insert("key".into(), json!(key));
            args.insert("name".into(), json!(name));
            if let Some(prefix) = prefix {
                args.insert("prefix".into(), json!(prefix));
            }
            "project_new"
        }
        // CAD-1999: `issue project ls` reads the hosted registry, which
        // includes projects that exist only as their tickets' key. The
        // route takes no arguments, so the local row transforms refuse
        // instead of being applied to a shape the client does not own.
        Commands::Issue {
            action:
                cadence_agent::issue::cli::IssueAction::Project {
                    action:
                        cadence_agent::issue::cli::ProjectAction::Ls {
                            sort,
                            limit,
                            fields,
                            json: _,
                        },
                },
        } => {
            for (flag, present) in [
                ("--sort", sort.is_some()),
                ("--limit", limit.is_some()),
                ("--fields", !fields.is_empty()),
            ] {
                if present {
                    return Err(unsupported(
                        "issue project ls",
                        flag,
                        "the hosted project read returns the registry as it is; \
                         sorting, truncating and field selection are local transforms",
                    ));
                }
            }
            "project_ls"
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
        Commands::Platform { action } => {
            // CAD-1678: a hosted workspace owns its catalog, its accounts and
            // its per-project default, so a flag naming any of them cannot
            // cross faithfully — it refuses here rather than being silently
            // ignored by the door. The tool and its input are the only caller
            // facts that travel.
            match action {
                PlatformAction::Tools { project, .. } => {
                    if project.is_some() {
                        return Err(unsupported(
                            "platform tools",
                            "--project",
                            "a hosted workspace resolves its own default account",
                        ));
                    }
                    args.insert("operation".into(), json!("tools"));
                }
                PlatformAction::Call {
                    tool,
                    input,
                    query,
                    request,
                    platform,
                    account,
                    project,
                    connection,
                    max_charge,
                } => {
                    if platform.is_some() {
                        return Err(unsupported(
                            "platform call",
                            "--platform",
                            "a hosted workspace runs its own registered platforms",
                        ));
                    }
                    if account.is_some() {
                        return Err(unsupported(
                            "platform call",
                            "--account",
                            "a hosted workspace funds and resolves its own account",
                        ));
                    }
                    if project.is_some() {
                        return Err(unsupported(
                            "platform call",
                            "--project",
                            "a hosted workspace resolves its own project",
                        ));
                    }
                    // CAD-1974: the connected-read door is the local
                    // `agenticos-provider` adapter. The hosted board has no
                    // route for it in this build, so the flags refuse here
                    // rather than being dropped from a call that then reads
                    // the wrong account.
                    if connection.is_some() {
                        return Err(unsupported(
                            "platform call",
                            "--connection",
                            "a hosted workspace has no connected-read door in this build; \
                             run the read against the local agenticos-provider account",
                        ));
                    }
                    if max_charge.is_some() {
                        return Err(unsupported(
                            "platform call",
                            "--max-charge",
                            "a hosted workspace has no connected-read door in this build; \
                             run the read against the local agenticos-provider account",
                        ));
                    }
                    if !valid_platform_tool_slug(tool) {
                        return Err(Error::invalid(
                            "platform_call",
                            "tool must be 1-128 of a-z 0-9 _ . - starting with a-z or 0-9",
                        ));
                    }
                    let body = match input {
                        Some(input) => Some(super::platform::parse_platform_input(input)?),
                        None => None,
                    };
                    let query = match query {
                        Some(query) => Some(super::platform::parse_platform_input(query)?),
                        None => None,
                    };
                    if body.is_none() && query.is_none() {
                        return Err(Error::usage(
                            "platform call needs --input or, on a hosted org, --query",
                        ));
                    }
                    // The handle names the retry: an identical call replays the
                    // door's recorded outcome instead of charging again, so a
                    // second intentional run needs a fresh --request.
                    let key = match request {
                        Some(request) => {
                            if !valid_platform_handle(request) {
                                return Err(Error::invalid(
                                    "platform_call",
                                    "request must be 1-128 of A-Z a-z 0-9 _ . : -",
                                ));
                            }
                            request.clone()
                        }
                        None => platform_replay_key(tool, query.as_ref(), body.as_ref()),
                    };
                    args.insert("operation".into(), json!("call"));
                    args.insert("tool".into(), json!(tool));
                    args.insert("key".into(), json!(key));
                    if let Some(body) = body {
                        args.insert("body".into(), body);
                    }
                    if let Some(query) = query {
                        args.insert("query".into(), query);
                    }
                }
                PlatformAction::Status {
                    request,
                    job,
                    tool,
                    input,
                    query,
                } => {
                    let body = match input {
                        Some(input) => Some(super::platform::parse_platform_input(input)?),
                        None => None,
                    };
                    let query = match query {
                        Some(query) => Some(super::platform::parse_platform_input(query)?),
                        None => None,
                    };
                    if (body.is_some() || query.is_some()) && tool.is_none() {
                        return Err(Error::usage(
                            "platform status with --input or --query requires --tool <tool>",
                        ));
                    }
                    let key = match request {
                        Some(request) => {
                            if !valid_platform_handle(request) {
                                return Err(Error::invalid(
                                    "platform_status",
                                    "request must be 1-128 of A-Z a-z 0-9 _ . : -",
                                ));
                            }
                            Some(request.clone())
                        }
                        None if tool.is_some() => {
                            if body.is_none() && query.is_none() {
                                return Err(Error::usage(
                                    "platform status with --tool needs --request <handle>, or --input/--query to derive the handle",
                                ));
                            }
                            Some(platform_replay_key(
                                tool.as_deref().unwrap_or_default(),
                                query.as_ref(),
                                body.as_ref(),
                            ))
                        }
                        None => None,
                    };
                    if let Some(job_id) = job {
                        if !valid_media_job_id(job_id) {
                            return Err(Error::invalid(
                                "platform_status",
                                "job must be med_ followed by 1-80 of A-Z a-z 0-9 _ -",
                            ));
                        }
                        args.insert("jobId".into(), json!(job_id));
                    }
                    if key.is_none() && job.is_none() {
                        return Err(Error::usage(
                            "platform status requires --request <handle>, --job <job-id>, or --tool <tool> with --input/--query",
                        ));
                    }
                    args.insert("operation".into(), json!("status"));
                    if let Some(key) = key {
                        args.insert("key".into(), json!(key));
                    }
                    if let Some(tool) = tool {
                        if !valid_platform_tool_slug(tool) {
                            return Err(Error::invalid(
                                "platform_status",
                                "tool must be 1-128 of a-z 0-9 _ . - starting with a-z or 0-9",
                            ));
                        }
                        args.insert("tool".into(), json!(tool));
                    }
                    if let Some(body) = body {
                        args.insert("body".into(), body);
                    }
                    if let Some(query) = query {
                        args.insert("query".into(), query);
                    }
                }
                PlatformAction::Artifact { reference, .. } => {
                    if !valid_media_artifact_ref(reference) {
                        return Err(Error::invalid(
                            "platform_artifact",
                            "artifact ref must be med_<job-id>.<64-lowercase-hex-sha256>",
                        ));
                    }
                    args.insert("operation".into(), json!("artifact"));
                    args.insert("ref".into(), json!(reference));
                }
                _ => return Err(unavailable(command_path)),
            }
            "platform_call"
        }
        _ => return Err(unavailable(command_path)),
    };
    Ok((verb, args))
}

/// The hosted reply-token grammar (`[A-Za-z0-9_.:-]{1,128}`), checked here so
/// a bad `--request` never reaches a paid call.
fn valid_platform_handle(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-'))
}

fn valid_media_job_id(value: &str) -> bool {
    value.strip_prefix("med_").is_some_and(|rest| {
        !rest.is_empty()
            && rest.len() <= 80
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    })
}

fn valid_media_artifact_ref(value: &str) -> bool {
    let Some((job_id, digest)) = value.split_once('.') else {
        return false;
    };
    valid_media_job_id(job_id)
        && digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_platform_tool_slug(value: &str) -> bool {
    let mut bytes = value.bytes();
    value.len() <= 128
        && bytes
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && bytes.all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
        })
}

/// CAD-1678: the reply key a hosted tool call replays under. The relay body
/// has no header channel, so the token rides in the arguments; deriving it
/// from the tool and its input means a resubmitted identical call reads the
/// door's recorded outcome instead of charging a second time, while a
/// different input is a different call.
fn platform_replay_key(tool: &str, query: Option<&Value>, body: Option<&Value>) -> String {
    use sha2::{Digest, Sha256};
    #[derive(serde::Serialize)]
    struct Material<'a> {
        // The field order preserves the existing BTreeMap serialization.
        body: Option<&'a Value>,
        query: Option<&'a Value>,
        tool: &'a str,
    }
    struct HashWriter(Sha256);
    impl std::io::Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, &Material { body, query, tool })
        .expect("JSON values serialize to the hash writer");
    format!("pcli_{:x}", writer.0.finalize())
}

/// The tenant's own cap on a ticket message, so an oversized one stays off the wire.
const TICKET_MESSAGE_BYTES: usize = 16_000;

/// A role on a ticket: a free label, in the tenant's grammar.
/// One link request, validated where the answer is already known: the four
/// kinds are fixed, an id must satisfy the shared grammar, and a self-link is
/// always refused. All three cost a round trip otherwise, and each says
/// nothing was sent. Nothing is trimmed: a value the board would refuse must
/// be refused here rather than quietly repaired into a different write.
fn link_args(
    args: &mut serde_json::Map<String, Value>,
    id: &str,
    kind: &str,
    target: &str,
    action: &str,
) -> Result<()> {
    if !matches!(kind, "blocked_by" | "relates" | "parent" | "duplicate_of") {
        return Err(Error::rejected(format!(
            "unknown link kind '{kind}' — use blocked_by, relates, parent or \
             duplicate_of; nothing was sent"
        )));
    }
    for (label, value) in [("issue", id), ("target", target)] {
        if !cadence_core::issue::valid_id(value) {
            return Err(Error::rejected(format!(
                "invalid {label} id '{value}' — expected <PREFIX>-<n> like CAD-16; \
                 nothing was sent"
            )));
        }
    }
    if id == target {
        return Err(Error::rejected(format!(
            "a ticket cannot {action} itself ({id}); nothing was sent"
        )));
    }
    args.insert("id".into(), json!(id));
    args.insert("kind".into(), json!(kind));
    args.insert("target".into(), json!(target));
    Ok(())
}

fn ticket_role<'a>(value: &'a str, flag: &str) -> Result<&'a str> {
    let mut chars = value.chars();
    let shaped = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && value.len() <= 32;
    if shaped {
        Ok(value)
    } else {
        Err(Error::rejected(format!(
            "{flag} must be a role of 1-32 lowercase letters, digits or hyphens, starting \
             with a letter — nothing was sent"
        )))
    }
}

/// A ticket message's stable id, in the tenant's grammar.
fn ticket_message_id(value: &str) -> Result<&str> {
    let mut chars = value.chars();
    let shaped = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && value.len() <= 64
        && !value.starts_with(cadence_agent::proto::DAEMON_MESSAGE_PREFIX);
    if shaped {
        Ok(value)
    } else {
        Err(Error::rejected(
            "--message must be 1-64 lowercase letters, digits or hyphens, not starting with \
             '-', never `sys-*` — nothing was sent",
        ))
    }
}

/// A `seq` the tenant can compare exactly: it is a JavaScript safe integer there.
fn ticket_seq(value: u64, flag: &str) -> Result<u64> {
    if value < (1 << 53) {
        Ok(value)
    } else {
        Err(Error::rejected(format!(
            "{flag} is too large to be a message seq — nothing was sent"
        )))
    }
}

/// Explain a refusal that a narrowed consent caused (CAD-2213): the credential
/// is valid, it simply never asked for `scope`. Only the uncertain 401 is
/// enriched — an authorization, availability or mutation-uncertain refusal
/// already says something true and is left alone.
fn scope_hint(error: Error, capabilities: &[String], scope: &str, hint: &str) -> Error {
    if capabilities.iter().any(|c| c == scope)
        || !error
            .to_string()
            .contains("did not confirm why it refused the credential")
    {
        return error;
    }
    let carried = if capabilities.is_empty() {
        "none recorded".to_string()
    } else {
        capabilities.join(", ")
    };
    // Replace, never append: a scope gap is a known cause, so leaving the
    // "uncertain 401" text in front would still read as an auth outage.
    Error::rejected(format!(
        "the remote refused this action because this login does not carry `{scope}` \
         (it has: {carried}); run `{hint}` once, then repeat the command. Nothing was sent"
    ))
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
fn wiki_for(
    action: &WikiAction,
    command_path: Option<&str>,
) -> Result<(&'static str, serde_json::Map<String, Value>)> {
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
        _ => return Err(unavailable(command_path)),
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
        eprint_line,
    )
    .inspect_err(|error| {
        if error.kind() == "busy" && error.code() == Some("waking") {
            eprint_line(&json!({"kind": "busy", "state": "waking"}).to_string());
        }
    })?;
    print_json(&out);
    Ok(0)
}

/// A version's size on a hosted org; the server refuses more.
const ARTIFACT_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// AOS-346: `cadence artifact` on a hosted org. The server decides who may see
/// what from the credential; nothing here names an actor or a scope.
fn artifact(
    target: &cadence_agent::remote_cli::RemoteTarget,
    action: &super::artifact::ArtifactAction,
    wake_timeout: u64,
) -> Result<i32> {
    use super::artifact::ArtifactAction;
    use cadence_agent::remote_cli::{call_verb, upload_artifact, ArtifactUpload, RemoteAnswer};
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(wake_timeout))
        .ok_or_else(|| Error::usage("invalid wake timeout"))?;
    let read = |verb: &str, args: serde_json::Map<String, Value>| -> Result<Value> {
        match call_verb(target, verb, args, deadline, wake_timeout, eprint_line)? {
            RemoteAnswer::Ok(body) => Ok(body),
            RemoteAnswer::Conflict(_) => Err(Error::rejected("remote read answered a conflict")),
        }
    };
    match action {
        ArtifactAction::Put {
            kind,
            title,
            file,
            id,
            issue,
            label,
            customer,
        } => {
            if cadence_agent::master::caller_is_master() {
                return Err(Error::rejected(
                    "the master puts artifacts on its own board; nothing was sent",
                ));
            }
            let bytes = cadence_agent::wiki::upload::read_local_file(file, ARTIFACT_MAX_BYTES)?;
            let name = file
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "artifact".into());
            let title = title.join(" ");
            let out = upload_artifact(
                target,
                &ArtifactUpload {
                    kind,
                    title: &title,
                    id: id.as_deref(),
                    issue: issue.as_deref(),
                    label: label.as_deref(),
                    customer: customer.as_deref(),
                    file_name: &cadence_agent::wiki::upload::part_file_name(&name),
                    bytes: &bytes,
                },
                wake_timeout,
                eprint_line,
            )?;
            print_json(&out);
            Ok(0)
        }
        ArtifactAction::Find {
            q,
            label,
            customer,
            kind,
            limit,
        } => {
            let mut args = serde_json::Map::new();
            for (key, value) in [
                ("q", q),
                ("label", label),
                ("customer", customer),
                ("kind", kind),
            ] {
                if let Some(value) = value {
                    args.insert(key.into(), json!(value));
                }
            }
            if let Some(limit) = limit {
                args.insert("limit".into(), json!(limit));
            }
            let out = read("artifact_find", args)?;
            print_json(&out);
            Ok(0)
        }
        ArtifactAction::Show { id, version, out } => {
            let Some(path) = out else {
                if version.is_some() {
                    return Err(Error::usage(
                        "--version selects the bytes to write; pass --out <new file>",
                    ));
                }
                let mut args = serde_json::Map::new();
                args.insert("id".into(), json!(id));
                let found = read("artifact_show", args)?;
                print_json(&found);
                return Ok(0);
            };
            fn malformed_page() -> Error {
                Error::rejected("remote answered a malformed or inconsistent artifact page; nothing was written")
            }
            let mut bytes = Vec::new();
            let mut pinned = version.map(u64::from);
            let mut pinned_total: Option<usize> = None;
            let mut pinned_digest: Option<[u8; 64]> = None;
            let file = loop {
                let mut args = serde_json::Map::new();
                args.insert("id".into(), json!(id));
                args.insert("offset".into(), json!(bytes.len()));
                if let Some(version) = pinned {
                    args.insert("version".into(), json!(version));
                }
                let mut body = read("artifact_read", args)?;
                let metadata = body
                    .get("file")
                    .and_then(Value::as_object)
                    .ok_or_else(malformed_page)?;
                if metadata.get("id").and_then(Value::as_str) != Some(id.as_str()) {
                    return Err(malformed_page());
                }
                let page_version = metadata
                    .get("version")
                    .and_then(Value::as_u64)
                    .filter(|v| *v > 0 && *v <= 9_007_199_254_740_991)
                    .ok_or_else(malformed_page)?;
                if pinned.is_some_and(|version| version != page_version) {
                    return Err(malformed_page());
                }
                pinned = Some(page_version);
                let total = body
                    .get("total")
                    .and_then(Value::as_u64)
                    .filter(|total| *total <= ARTIFACT_MAX_BYTES)
                    .and_then(|total| usize::try_from(total).ok())
                    .ok_or_else(malformed_page)?;
                if pinned_total.is_some_and(|expected| expected != total)
                    || body.get("offset").and_then(Value::as_u64) != Some(bytes.len() as u64)
                    || metadata
                        .get("bytes")
                        .is_some_and(|size| size.as_u64() != Some(total as u64))
                {
                    return Err(malformed_page());
                }
                pinned_total = Some(total);
                let digest = metadata
                    .get("digest")
                    .and_then(Value::as_str)
                    .filter(|digest| {
                        digest.len() == 64
                            && digest
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    })
                    .ok_or_else(malformed_page)?;
                if let Some(expected) = &pinned_digest {
                    if expected.as_slice() != digest.as_bytes() {
                        return Err(malformed_page());
                    }
                } else {
                    let mut expected = [0; 64];
                    expected.copy_from_slice(digest.as_bytes());
                    pinned_digest = Some(expected);
                }
                let encoded = body
                    .get("content_base64")
                    .and_then(Value::as_str)
                    .ok_or_else(malformed_page)?;
                let remaining = total.checked_sub(bytes.len()).ok_or_else(malformed_page)?;
                if encoded.len() > remaining.div_ceil(3) * 4 {
                    return Err(malformed_page());
                }
                let previous_len = bytes.len();
                use base64::Engine;
                base64::engine::general_purpose::STANDARD
                    .decode_vec(encoded, &mut bytes)
                    .map_err(|_| malformed_page())?;
                if bytes.len() > total || (bytes.len() == previous_len && bytes.len() < total) {
                    return Err(malformed_page());
                }
                if bytes.len() == total {
                    break body["file"].take();
                }
            };
            use sha2::{Digest, Sha256};
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let actual = Sha256::digest(&bytes);
            let expected = pinned_digest.expect("a completed page has a digest");
            if !actual
                .iter()
                .zip(expected.as_chunks::<2>().0.iter())
                .all(|(byte, pair)| {
                    pair[0] == HEX[(byte >> 4) as usize] && pair[1] == HEX[(byte & 0xf) as usize]
                })
            {
                return Err(Error::rejected(
                    "the artifact's bytes do not match its digest. Nothing was written",
                ));
            }
            use std::io::Write;
            #[cfg(unix)]
            use std::os::unix::fs::OpenOptionsExt;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600); // An artifact may be private to its creator.
            let mut target_file = options
                .open(path)
                .map_err(|e| Error::rejected(format!("cannot create {}: {e}", path.display())))?;
            if let Err(error) = target_file.write_all(&bytes) {
                let _ = std::fs::remove_file(path);
                return Err(Error::rejected(format!(
                    "cannot write {}: {error}",
                    path.display()
                )));
            }
            print_json(&json!({
                "file": file,
                "written": path.display().to_string(),
            }));
            Ok(0)
        }
    }
}

/// Maximum size of a retained media artifact (`10 MiB`, matching `ARTIFACT_BYTES_MAX`).
const PLATFORM_MEDIA_ARTIFACT_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// Video is streamed into custody with a separate 100 MiB limit.
const PLATFORM_VIDEO_ARTIFACT_MAX_BYTES: u64 = 100 * 1024 * 1024;

fn remote_platform_artifact<E, P>(
    target: &cadence_agent::remote_cli::RemoteTarget,
    reference: &str,
    out: &std::path::Path,
    wake_timeout: u64,
    eprint_line: E,
    print_json: P,
) -> Result<i32>
where
    E: Fn(&str),
    P: Fn(&Value),
{
    if !valid_media_artifact_ref(reference) {
        return Err(Error::invalid(
            "platform_artifact",
            "artifact ref must be med_<job-id>.<64-lowercase-hex-sha256>",
        ));
    }
    if std::fs::symlink_metadata(out).is_ok() {
        return Err(Error::rejected(format!(
            "cannot create {}: file already exists; nothing was requested or written",
            out.display()
        )));
    }
    eprint_line(&format!(
        "cadence platform org={} operation=artifact ref={reference}",
        target.org
    ));
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(wake_timeout))
        .ok_or_else(|| Error::rejected("invalid wake timeout"))?;
    let receipt = collect_platform_artifact(reference, out, |offset| {
        let mut args = serde_json::Map::new();
        args.insert("operation".into(), json!("artifact"));
        args.insert("ref".into(), json!(reference));
        args.insert("offset".into(), json!(offset));
        match cadence_agent::remote_cli::call_verb(
            target,
            "platform_call",
            args,
            deadline,
            wake_timeout,
            &eprint_line,
        )? {
            cadence_agent::remote_cli::RemoteAnswer::Ok(body) => Ok(body),
            cadence_agent::remote_cli::RemoteAnswer::Conflict(_) => {
                Err(Error::rejected("remote media artifact read conflicted"))
            }
        }
    });
    let receipt = match receipt {
        Ok(receipt) => receipt,
        Err(error) => return remote_failure(error),
    };
    print_json(&receipt);
    Ok(0)
}

fn collect_platform_artifact(
    reference: &str,
    out: &std::path::Path,
    mut fetch: impl FnMut(u64) -> Result<Value>,
) -> Result<Value> {
    use sha2::{Digest, Sha256};
    use std::io::{Seek, SeekFrom, Write};
    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    if !valid_media_artifact_ref(reference) {
        return Err(Error::invalid(
            "platform_artifact",
            "artifact ref must be med_<job-id>.<64-lowercase-hex-sha256>",
        ));
    }
    if std::fs::symlink_metadata(out).is_ok() {
        return Err(Error::rejected(format!(
            "cannot create {}: file already exists; nothing was written",
            out.display()
        )));
    }
    let (expected_job, expected_digest) = reference.split_once('.').expect("validated ref");
    let mut spool = {
        let mut builder = tempfile::Builder::new();
        builder.prefix(".cadence-media-artifact-");
        #[cfg(unix)]
        builder.permissions(std::fs::Permissions::from_mode(0o600));
        builder.tempfile().map_err(|e| {
            Error::rejected(format!(
                "cannot open private media artifact spool: {e}; nothing was written"
            ))
        })?
    };
    let mut offset = 0u64;
    let mut pinned_total: Option<u64> = None;
    let mut pinned_mime: Option<String> = None;
    let mut hasher = Sha256::new();
    loop {
        let reply = fetch(offset)?;
        let data = reply
            .get("data")
            .filter(|v| v.is_object())
            .unwrap_or(&reply);
        if data.get("ref").and_then(Value::as_str) != Some(reference)
            || data.get("jobId").and_then(Value::as_str) != Some(expected_job)
            || data.get("digest").and_then(Value::as_str) != Some(expected_digest)
        {
            return Err(Error::rejected(
                "remote media artifact response does not match the requested ref/digest; nothing was written",
            ));
        }
        let mime = data
            .get("mime")
            .and_then(Value::as_str)
            .filter(|m| matches!(*m, "image/png" | "image/jpeg" | "video/mp4" | "video/webm"))
            .ok_or_else(|| {
                Error::rejected(
                    "remote media artifact has an unsupported or missing MIME type; nothing was written",
                )
            })?;
        if let Some(prev) = pinned_mime.as_deref() {
            if prev != mime {
                return Err(Error::rejected(
                    "remote media artifact MIME changed across pages; nothing was written",
                ));
            }
        } else {
            pinned_mime = Some(mime.to_string());
        }
        let page_offset = data.get("offset").and_then(Value::as_u64).ok_or_else(|| {
            Error::rejected("remote media artifact page is missing offset; nothing was written")
        })?;
        let total = data
            .get("total")
            .and_then(Value::as_u64)
            .filter(|t| {
                let max = if mime.starts_with("video/") {
                    PLATFORM_VIDEO_ARTIFACT_MAX_BYTES
                } else {
                    PLATFORM_MEDIA_ARTIFACT_MAX_BYTES
                };
                *t > 0 && *t <= max
            })
            .ok_or_else(|| {
                Error::rejected(
                    "remote media artifact total size is missing or out of bounds; nothing was written",
                )
            })?;
        if let Some(prev_total) = pinned_total {
            if prev_total != total {
                return Err(Error::rejected(
                    "remote media artifact total size changed across pages; nothing was written",
                ));
            }
        } else {
            pinned_total = Some(total);
        }
        if page_offset != offset {
            return Err(Error::rejected(
                "remote media artifact page offset is not contiguous; nothing was written",
            ));
        }
        let encoded = data
            .get("content_base64")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::rejected(
                    "remote media artifact page is missing content_base64; nothing was written",
                )
            })?;
        use base64::Engine;
        let chunk = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| {
                Error::rejected(
                    "remote media artifact content_base64 is invalid; nothing was written",
                )
            })?;
        if let Some(declared_bytes) = data.get("bytes").and_then(Value::as_u64) {
            if declared_bytes != chunk.len() as u64 {
                return Err(Error::rejected(
                    "remote media artifact page byte count does not match decoded bytes; nothing was written",
                ));
            }
        }
        if chunk.is_empty() || offset + (chunk.len() as u64) > total {
            return Err(Error::rejected(
                "remote media artifact page made no progress or exceeded total; nothing was written",
            ));
        }
        spool.write_all(&chunk).map_err(|e| {
            Error::rejected(format!(
                "cannot write media artifact spool: {e}; nothing was written"
            ))
        })?;
        hasher.update(&chunk);
        offset += chunk.len() as u64;
        if offset == total {
            break;
        }
    }
    let total = pinned_total.unwrap_or(0);
    let got_digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if got_digest != expected_digest || offset != total {
        return Err(Error::rejected(
            "the media artifact's bytes do not match its SHA-256 digest; nothing was written",
        ));
    }
    spool.seek(SeekFrom::Start(0)).map_err(|e| {
        Error::rejected(format!(
            "cannot rewind media artifact spool: {e}; nothing was written"
        ))
    })?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut target_file = options
        .open(out)
        .map_err(|e| Error::rejected(format!("cannot create {}: {e}", out.display())))?;
    if let Err(error) = std::io::copy(&mut spool, &mut target_file).and_then(|copied| {
        if copied != total {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "verified media spool changed length",
            ));
        }
        target_file.sync_all()
    }) {
        let _ = std::fs::remove_file(out);
        return Err(Error::rejected(format!(
            "cannot write {}: {error}; partial file removed",
            out.display()
        )));
    }
    Ok(json!({
        "ref": reference,
        "jobId": expected_job,
        "digest": expected_digest,
        "mime": pinned_mime.unwrap_or_else(|| "image/png".into()),
        "bytes": total,
        "written": out.display().to_string(),
    }))
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
    use std::io::Write as _;
    let mut spool = collect_remote_blob(discovered, |offset, rev| {
        remote_blob_range(target, path, offset, rev, deadline, wake_timeout)
    })?;
    // The spool stays bounded in memory by streaming through `io::copy`.
    // A reader that stopped early (`| head -c …`) is a success: the bytes
    // it wanted were already written, so the download is not retried.
    let mut stdout = std::io::stdout().lock();
    match std::io::copy(&mut spool, &mut stdout).and_then(|_| stdout.flush()) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(Error::rejected(format!(
            "remote wiki cat: cannot write stdout: {error}"
        ))),
    }
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
        eprint_line,
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
            // it could push one line over the native tool bound, so this is
            // a deliberate exception to the stdout JSON policy.
            print_line(&serde_json::to_string(body).unwrap_or_default());
            true
        }
        WikiAction::Cat { meta: false, .. } if body["kind"] == "text" => {
            // Raw text: verbatim bytes, no policy, no added newline.
            print_text(body["text"].as_str().unwrap_or(""));
            true
        }
        WikiAction::Search { json: false, .. } => {
            for m in body["matches"].as_array().cloned().unwrap_or_default() {
                print_line(&format!(
                    "{}:{}: {}",
                    m["path"].as_str().unwrap_or(""),
                    m["line"].as_u64().unwrap_or(0),
                    m["text"].as_str().unwrap_or("")
                ));
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

/// The rows in `hosted-verbs.toml` (CAD-2180): the verbs whose
/// hosted refusal needs a fact the generic message cannot give. Every other
/// refusal keeps the default — not implemented for a remote org in this
/// build — and names `cadence commands --all`, which is a real answer.
const HOSTED_VERBS_TOML: &str = include_str!("hosted-verbs.toml");

#[derive(serde::Deserialize)]
struct HostedVerbs {
    #[serde(default)]
    verb: Vec<HostedVerb>,
}

#[derive(serde::Deserialize)]
struct HostedVerb {
    path: String,
    class: String,
    #[serde(default)]
    ticket: Option<String>,
    #[serde(default)]
    alternative: Option<String>,
}

fn hosted_verbs() -> &'static HostedVerbs {
    static TABLE: std::sync::OnceLock<HostedVerbs> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        toml::from_str(HOSTED_VERBS_TOML).expect("src/cli/hosted-verbs.toml is a valid verb table")
    })
}

fn hosted_gap(path: &str) -> Option<&'static HostedVerb> {
    hosted_verbs().verb.iter().find(|row| row.path == path)
}

fn unavailable(command_path: Option<&str>) -> Error {
    let path = command_path.unwrap_or("<command>");
    let listed = "`cadence commands --all` lists every verb this client offers.";
    if let Some(gap) = hosted_gap(path) {
        let alternative = gap
            .alternative
            .as_deref()
            .map(|alt| format!(" Use `cadence {alt}` today."))
            .unwrap_or_default();
        let case = match gap.class.as_str() {
            "planned" => format!(
                "it is planned ({})",
                gap.ticket.as_deref().unwrap_or("no ticket")
            ),
            _ => "it is local-only and will not be sent to a remote org".to_string(),
        };
        return Error::rejected(format!(
            "`cadence {path}` is not available on a remote org in this build: {case}; \
             nothing was sent.{alternative} {listed}"
        ));
    }
    Error::rejected(format!(
        "`cadence {path}` is not available on a remote org in this build; nothing was sent. {listed}"
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

/// A stable command name for authorization refusals — never a trust decision.
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

fn print_issue_artifact(command: &Commands, body: &Value) -> Result<bool> {
    use base64::Engine;
    use sha2::{Digest, Sha256};

    let Commands::Issue {
        action: cadence_agent::issue::cli::IssueAction::Artifact { id, name },
    } = command
    else {
        return Ok(false);
    };
    let bad = || Error::rejected("hosted artifact response does not match the requested bytes");
    let artifact = body
        .get("artifact")
        .and_then(Value::as_object)
        .ok_or_else(bad)?;
    if !artifact
        .get("ticketId")
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case(id))
        || artifact.get("name").and_then(Value::as_str) != Some(name.as_str())
    {
        return Err(bad());
    }
    let data = artifact
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(bad)?;
    if data.len() > 1_398_104 {
        return Err(bad());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| bad())?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if bytes.len() > 1_048_576
        || artifact.get("size").and_then(Value::as_u64) != Some(bytes.len() as u64)
        || artifact.get("sha256").and_then(Value::as_str) != Some(digest.as_str())
    {
        return Err(bad());
    }
    // Raw artifact bytes: no JSON policy, no re-encoding. A reader that
    // stopped early already has what it asked for, so it is not a failed
    // download; any other stdout fault still fails the command.
    write_stdout(&bytes)?;
    Ok(true)
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
    command_path: Option<String>,
) -> Result<i32> {
    run_with_finish(
        target,
        command,
        wake_timeout,
        local_state,
        command_path,
        local_agent::hosted_finish,
    )
}

fn run_with_finish<F>(
    target: cadence_agent::remote_cli::RemoteTarget,
    command: Commands,
    wake_timeout: u64,
    local_state: Option<std::path::PathBuf>,
    command_path: Option<String>,
    finish: F,
) -> Result<i32>
where
    F: FnOnce(
        &RemoteTarget,
        &str,
        Option<&std::path::Path>,
        Option<&std::path::Path>,
    ) -> Result<serde_json::Value>,
{
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
            cadence_agent::issue::cli::IssueAction::Finish {
                id,
                worktree,
                force,
                keep_branch,
                remote,
                merged,
                dry_run,
                ..
            },
    } = &command
    {
        // CAD-2178: hosted finish releases one merged, clean, idle lane and
        // nothing else. An option that would widen or soften that is refused
        // rather than ignored — `--keep-branch` ignored would delete a branch
        // the caller asked to keep.
        for (set, flag) in [
            (*force, "--force"),
            (*keep_branch, "--keep-branch"),
            (*remote, "--remote"),
            (*merged, "--merged"),
            (*dry_run, "--dry-run"),
        ] {
            if set {
                return Err(Error::rejected(format!(
                    "`issue finish {flag}` is not available on a hosted org; hosted finish \
                     takes only <ID> [--worktree <path>] and releases a merged, clean, idle \
                     lane. Nothing was removed"
                )));
            }
        }
        let Some(id) = id.as_deref() else {
            return Err(Error::usage("hosted issue finish requires an issue id"));
        };
        let value = finish(&target, id, worktree.as_deref(), local_state.as_deref())?;
        print_json(&value);
        return Ok(0);
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
                profile,
            },
    } = &command
    {
        if *job
            || owner.is_some()
            || pm.is_some()
            || spec.is_some()
            || assignee.is_some()
            || *force
            || by.is_some()
            || take_over.is_some()
            || (base.is_some() && profile.is_none())
        {
            return Err(Error::usage(
                "hosted issue start accepts --repo, --name, --profile and --base (with --profile). It claims this issue for this session and does not open a job. Nothing was sent.",
            ));
        }
        return local_agent::start(
            &target,
            id,
            repo.as_deref(),
            name.as_deref(),
            profile.as_deref(),
            base.as_deref(),
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
    if let Commands::Artifact { action } = &command {
        return artifact(&target, action, wake_timeout);
    }
    if let Commands::Platform {
        action: PlatformAction::Artifact { reference, out },
    } = &command
    {
        return remote_platform_artifact(
            &target,
            reference,
            out,
            wake_timeout,
            eprint_line,
            print_json,
        );
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
    // CAD-1999: a hosted project create is the live human workspace owner's
    // own act — the same CAD-1586 rule the merge approval and Settings
    // follow. A managed caller (worker, master, grant-bound argv child)
    // spending the stored login would act as that owner, so it refuses
    // before a request exists; the board's own owner check would otherwise
    // be satisfied by the owner's credential, not by the caller.
    if matches!(
        &command,
        Commands::Project {
            action: ProjectCmd::New { .. }
        }
    ) {
        refuse_managed(&target, "project new")?;
    }
    let mut command = command;
    if let Commands::Agent {
        action: AgentAction::Profile { action },
    } = &mut command
    {
        // CAD-2101: refused in the client before any request exists; the
        // board cannot tell a managed agent on its owner's login from the
        // owner (docs/guides/hosted-workspace.md).
        super::agent_profile::refuse_managed_write(action, target.identity.is_some())?;
        super::agent_profile::resolve_latest(&target, action, wake_timeout)?;
    }
    let (verb, args) = verb_for(&command, command_path.as_deref())?;
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
    let platform_key = (verb == "platform_call")
        .then(|| args.get("key").and_then(Value::as_str).map(str::to_string))
        .flatten();
    if verb == "platform_call" {
        let op = args
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or("call");
        let mut line = format!("cadence platform org={} operation={op}", target.org);
        if let Some(tool) = args.get("tool").and_then(Value::as_str) {
            line.push_str(&format!(" tool={tool}"));
        }
        if let Some(key) = platform_key.as_deref() {
            line.push_str(&format!(" request={key}"));
        }
        if let Some(job_id) = args.get("jobId").and_then(Value::as_str) {
            line.push_str(&format!(" job={job_id}"));
        }
        eprint_line(&line);
    }
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(wake_timeout))
        .ok_or_else(|| Error::rejected("invalid wake timeout"))?;
    let out = match cadence_agent::remote_cli::call_verb(
        &target,
        verb,
        args,
        deadline,
        wake_timeout,
        eprint_line,
    ) {
        Ok(out) => out,
        Err(error) => {
            if let Some(id) = send_key.filter(|_| error.kind() == "unknown") {
                eprint_line(&format!(
                    "remote send `{id}` was not confirmed — it may or may not have \
                     been recorded. Reconcile before resubmitting: `cadence message \
                     read {id} --org {}` returns your durable message and its state. \
                     If nothing is recorded, retry `send` with the same --message {id} \
                     — the same id and content does not enqueue another message.",
                    target.org
                ));
            }
            if let Some(key) = platform_key.filter(|_| error.kind() == "unknown") {
                eprint_line(&format!(
                    "remote platform call `{key}` on org `{}` was not confirmed — \
                     reconcile read-only with `cadence --org {} platform status --request {key}` \
                     before repeating; replaying `platform call` returns a recorded outcome, \
                     or starts and charges a new call if `{key}` was not recorded.",
                    target.org, target.org
                ));
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
            if print_issue_artifact(&command, &body)? {
                return Ok(0);
            }
            match remote_blob_cat(&target, &command, &body, deadline, wake_timeout) {
                Ok(true) => return Ok(0),
                Ok(false) => {}
                Err(error) => return remote_failure(error),
            }
            if wiki_text(&command, &body)? {
                return Ok(0);
            }
            if matches!(&command, Commands::Platform { .. }) {
                for summary in super::platform::platform_human_summary(&body) {
                    eprint_line(&summary);
                }
            }
            // The ordinary hosted answer is one JSON document under the
            // shared stdout policy, like a local verb's.
            print_json(&body);
            Ok(0)
        }
        cadence_agent::remote_cli::RemoteAnswer::Conflict(body) => {
            // A checked conflict is the remote's "not written" verdict:
            // print the whole body so the caller resyncs from
            // `current_rev`/`card`, and exit 5 — the same conflict code
            // the local write path exits (see `error::EXIT_TABLE`). The
            // report cannot change that verdict: `print_json` absorbs a
            // closed consumer instead of panicking.
            print_json(&body);
            Ok(5)
        }
    }
}

/// The explicit project a hosted gate call is scoped to. CAD-2001: the review
/// evidence is project-scoped, so a missing project is an error — it is never
/// inferred from `--repo` or the cwd checkout's origin, and no repo's first
/// project is ever selected.
fn project_arg(project: &Option<String>) -> Result<&str> {
    project.as_deref().ok_or_else(|| {
        Error::rejected(
            "a hosted `audit` call needs an explicit --project <key>: the review evidence is \
             project-scoped, and the project is never inferred from the repo or the cwd. \
             Nothing was sent",
        )
    })
}

/// The explicit issue a hosted approval is about. An approval is unique per
/// issue in its project, so an approval recorded for another ticket can never
/// answer this one.
fn issue_arg(issue: &Option<String>) -> Result<&str> {
    issue.as_deref().ok_or_else(|| {
        Error::rejected(
            "a hosted `audit approval|approve` needs an explicit --issue <ID>: the approval is \
             that issue's, and one recorded for another ticket never answers it. Nothing was \
             sent",
        )
    })
}

/// The exact `{project, repo, issue, pr, head}` binding every project-scoped
/// gate call carries. Each part is validated before a request exists.
fn binding(
    project: &Option<String>,
    repo: &str,
    issue: &str,
    pr: u64,
    head: &str,
) -> Result<cadence_agent::delivery::hosted_gate::GateBinding> {
    cadence_agent::delivery::hosted_gate::GateBinding::new(
        project_arg(project)?,
        repo,
        issue,
        pr,
        head,
    )
}

/// CAD-1946: the merge gate's commands against a hosted org — the reads
/// `scripts/enqueue-reviewed` makes and the operator's merge approval.
/// `None` for every other command. Output and exit codes match the local
/// commands; the checks live in [`cadence_agent::delivery::hosted_gate`].
///
/// CAD-2001: every read and the approval are project-scoped. `--project` and
/// `--issue` are required here (a hosted call without them refuses rather
/// than guessing), and the tenant serves the project-scoped verbs. An older
/// tenant refuses those names, which
/// [`cadence_agent::remote_cli::PROJECT_SCOPED_GATE_VERBS`] reports as the
/// release-order problem it is — never as "no evidence".
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
            action:
                Some(AuditAction::Approval {
                    pr,
                    head,
                    issue,
                    project,
                    repo,
                }),
            ..
        } => {
            let head = super::audit::full_head(head)?;
            let repo = repo_or_origin(repo)?;
            printed(hosted_gate::approval(
                target,
                wake_timeout,
                &binding(project, &repo, issue_arg(issue)?, *pr, &head)?,
            )?)
        }
        Commands::Audit {
            action:
                Some(AuditAction::Verdict {
                    issue,
                    pr,
                    head,
                    kind,
                    outcome,
                    title_outcome,
                    section_outcome,
                    risk,
                    file,
                    text,
                    repo,
                    project,
                }),
            ..
        } => {
            let head = super::audit::full_head(head)?;
            // AOS-473: the kinds the merge gate counts. `spec` is retired: the
            // service refuses it, and a hosted Spec verdict under a name the
            // gate never counts would refuse every two-review hosted PR.
            if !["standards", "spec-security", "combined", "browser-qa"].contains(&kind.as_str()) {
                return Err(Error::usage(
                    "audit verdict --kind is `standards`, `spec-security`, `combined` or \
                     `browser-qa`; nothing was sent",
                ));
            }
            if outcome != "pass" && outcome != "revise" {
                return Err(Error::usage(
                    "audit verdict --outcome is `pass` or `revise`; nothing was sent",
                ));
            }
            let title_outcome = title_outcome.as_deref().unwrap_or(outcome);
            let section_outcome = section_outcome.as_deref().unwrap_or(outcome);
            for value in [title_outcome, section_outcome] {
                if value != "pass" && value != "revise" {
                    return Err(Error::usage(
                        "audit verdict title/section outcome is `pass` or `revise`; nothing was sent",
                    ));
                }
            }
            // The findings are read client-side; `-` is stdin. A verdict
            // without findings is refused before send.
            let body =
                match (text, file) {
                    (Some(t), _) => t.clone(),
                    (None, Some(f)) if f.as_os_str() == "-" => {
                        std::io::read_to_string(std::io::stdin())?
                    }
                    (None, Some(f)) => std::fs::read_to_string(f).map_err(|e| {
                        Error::rejected(format!(
                            "audit verdict --file {}: cannot read it locally: {e}",
                            f.display()
                        ))
                    })?,
                    (None, None) => return Err(Error::usage(
                        "a verdict needs its findings — pass --text or --file; nothing was sent",
                    )),
                };
            if body.trim().is_empty() {
                return Err(Error::usage(
                    "a verdict needs its findings — nothing was sent",
                ));
            }
            if body.len() > 16 * 1024 {
                return Err(Error::usage(
                    "verdict text exceeds the 16 KB cap — trim it; nothing was sent",
                ));
            }
            let repo = repo_or_origin(repo)?;
            let recorded = hosted_gate::verdict(
                target,
                wake_timeout,
                &binding(project, &repo, issue, *pr, &head)?,
                kind,
                outcome,
                title_outcome,
                section_outcome,
                risk,
                &body,
            )?;
            printed((recorded, 0))
        }
        Commands::Audit {
            action:
                Some(AuditAction::Verdicts {
                    issue,
                    pr,
                    head,
                    repo,
                    project,
                    body,
                    ..
                }),
            ..
        } => {
            let head = super::audit::full_head(head)?;
            let repo = repo_or_origin(repo)?;
            printed(hosted_gate::verdicts(
                target,
                wake_timeout,
                &binding(project, &repo, issue, *pr, &head)?,
                *body,
            )?)
        }
        Commands::Audit {
            action: Some(AuditAction::Reviewers { project, action }),
            ..
        } => match action {
            None => {
                let project = project
                    .as_deref()
                    .ok_or_else(|| Error::rejected("audit reviewers needs --project"))?;
                printed(hosted_gate::reviewers(target, wake_timeout, project)?)
            }
            Some(super::audit::ReviewersAction::Set { project, reviewer }) => {
                // The eligibility map is the workspace owner's own act: the
                // stored login is not an agent's to spend (the CAD-1586 rule).
                refuse_managed(target, "audit reviewers set")?;
                let subjects = super::audit::reviewer_subjects(reviewer)?;
                printed((
                    hosted_gate::set_reviewers(target, wake_timeout, project, &subjects)?,
                    0,
                ))
            }
        },
        Commands::Audit {
            action: Some(AuditAction::Identities { action }),
            ..
        } => match action {
            None => printed(hosted_gate::identities(target, wake_timeout)?),
            Some(super::audit::IdentitiesAction::Set { identity }) => {
                // Attesting a login decides whose verdicts count, so it is the
                // owner's own act exactly like the merge approval.
                refuse_managed(target, "audit identities set")?;
                let pairs = super::audit::identity_pairs(identity)?;
                printed((
                    hosted_gate::set_identities(target, wake_timeout, &pairs)?,
                    0,
                ))
            }
        },
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
                    project,
                }),
            ..
        } => {
            refuse_managed(target, "audit approve")?;
            if *delegated || scope.is_some() || id.is_some() || action != "merge" {
                return Err(Error::usage(
                    "remote `audit approve` records a merge approval only: --delegated, \
                     --scope and --id are not available on a hosted org. Nothing was sent.",
                ));
            }
            let pr = pr.ok_or_else(|| Error::rejected("--pr is required"))?;
            let head = super::audit::full_head(head.as_deref().unwrap_or_default())?;
            let repo = repo_or_origin(repo)?;
            let receipt = hosted_gate::approve(
                target,
                wake_timeout,
                &binding(project, &repo, issue_arg(issue)?, pr, &head)?,
                source,
            )
            .map_err(|error| {
                scope_hint(
                    error,
                    &cadence_agent::remote_cli::saved_capabilities(target),
                    "audit.approve",
                    "cadence login --audit-approve",
                )
            })?;
            printed((receipt, 0))
        }
        Commands::Audit {
            action:
                Some(AuditAction::AcceptRevise {
                    pr,
                    head,
                    issue,
                    project,
                    source,
                    repo,
                }),
            ..
        } => {
            refuse_managed(target, "audit accept-revise")?;
            let head = super::audit::full_head(head)?;
            let repo = repo_or_origin(repo)?;
            let project = Some(project.clone());
            let receipt = hosted_gate::accept_revise(
                target,
                wake_timeout,
                &binding(&project, &repo, issue.trim(), *pr, &head)?,
                source,
            )
            .map_err(|error| {
                scope_hint(
                    error,
                    &cadence_agent::remote_cli::saved_capabilities(target),
                    "audit.approve",
                    "cadence login --audit-approve",
                )
            })?;
            printed((receipt, 0))
        }
        Commands::Audit {
            action:
                Some(AuditAction::Findings {
                    pr,
                    head,
                    issue,
                    project,
                    kind,
                    repo,
                }),
            ..
        } => {
            let head = super::audit::full_head(head)?;
            let repo = repo_or_origin(repo)?;
            let project = Some(project.clone());
            let doc = hosted_gate::findings(
                target,
                wake_timeout,
                &binding(&project, &repo, issue.trim(), *pr, &head)?,
                kind.as_deref(),
            )?;
            printed((doc, 0))
        }
        Commands::Issue {
            action:
                cadence_agent::issue::cli::IssueAction::Project {
                    action:
                        cadence_agent::issue::cli::ProjectAction::Delivery {
                            key,
                            repos,
                            delivery_file,
                        },
                },
        } => {
            refuse_managed(target, "issue project delivery")?;
            let delivery = delivery_file
                .as_ref()
                .map(|path| {
                    std::fs::read_to_string(path).map_err(|e| {
                        Error::rejected(format!("--delivery-file {}: {e}", path.display()))
                    })
                })
                .transpose()?;
            let receipt =
                hosted_gate::set_delivery(target, wake_timeout, key, repos, delivery.as_deref())?;
            printed((receipt, 0))
        }
        _ => None,
    })
}

/// The stored login is the human owner's; a managed caller spending it
/// would act as the owner (the CAD-1586 rule in [`run`]). It covers the merge
/// approval, the project eligibility map and the workspace identity
/// attestation: each decides whose verdicts count.
fn refuse_managed(target: &RemoteTarget, command: &str) -> Result<()> {
    if target.identity.is_some()
        || std::env::var_os("CADENCE_ALIAS").is_some()
        || std::env::var_os("CADENCE_GRANT_TOKEN").is_some()
    {
        return Err(Error::rejected(format!(
            "remote `{command}` refused for a managed caller: it is the workspace \
             owner's own act and the stored login is not an agent's to spend. \
             Nothing was sent."
        )));
    }
    Ok(())
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
    let note = eprint_line;
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
        // The wake verdict is a nonzero status (75): print it through the
        // closed-consumer-tolerant writer so a closed stderr cannot turn
        // it into a panic that exits 0.
        eprint_line(&json!({"kind": "busy", "state": "waking"}).to_string());
        Ok(75)
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod hosted_finish_acceptance;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accept_revise_is_refused_before_any_request_for_a_managed_caller() {
        let target = cadence_agent::remote_cli::RemoteTarget {
            org: "fav-limited".into(),
            endpoint: "https://example.invalid".into(),
            org_id: "org_synthetic".into(),
            identity: Some(cadence_agent::remote_cli::CliIdentityPin {
                subject_id: "usr_agent".into(),
                issuer: "iss_synthetic".into(),
            }),
        };
        let err = refuse_managed(&target, "audit accept-revise").unwrap_err();
        assert!(err.to_string().contains("managed caller"), "{err}");
        assert!(err.to_string().contains("Nothing was sent"), "{err}");
    }

    #[test]
    fn hosted_finish_refuses_through_remote_dispatch_without_tracker_access() {
        use cadence_agent::issue::cli::IssueAction;
        use cadence_agent::local_assignment::{CoordHttp, Coordinator, Reply};
        use std::{cell::Cell, os::unix::fs::PermissionsExt, rc::Rc};

        struct FakeRemote(Rc<Cell<usize>>);
        impl CoordHttp for FakeRemote {
            fn send(&mut self, _: &str, _: &str, _: Option<&Value>) -> Result<Reply> {
                self.0.set(self.0.get() + 1);
                Err(Error::rejected("unexpected remote call"))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let calls = Rc::new(Cell::new(0));
        let coordinator = Coordinator::new(
            FakeRemote(calls.clone()),
            "ws_synthetic".into(),
            dir.path().to_path_buf(),
        )
        .unwrap();
        let tracker = dir.path().join("local-tracker");
        let command = Commands::Issue {
            action: IssueAction::Finish {
                id: Some("CAD-42".into()),
                worktree: None,
                force: false,
                keep_branch: false,
                remote: false,
                merged: false,
                project: None,
                dry_run: false,
                json: false,
            },
        };
        let target = RemoteTarget {
            org: "synthetic".into(),
            endpoint: "https://synthetic.cadencecloud.app".into(),
            org_id: "ws_synthetic".into(),
            identity: None,
        };
        let error = run_with_finish(
            target,
            command,
            1,
            Some(tracker.clone()),
            Some("issue finish".into()),
            |_, issue, worktree, state| {
                cadence_agent::local_assignment::finish_guidance::release(
                    &coordinator,
                    issue,
                    worktree,
                    state.unwrap(),
                )
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), "rejected");
        assert!(error.to_string().contains("cannot be confirmed"), "{error}");
        assert!(!error.to_string().contains("reconcile"), "{error}");
        assert_eq!(calls.get(), 0, "no remote tracker call");
        assert!(!tracker.exists(), "no local tracker operation");
    }

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
        match verb_for(command, None) {
            Ok((verb, args)) => Ok((verb, Value::Object(args))),
            Err(e) => Err(e.to_string()),
        }
    }

    fn platform(action: PlatformAction) -> Commands {
        Commands::Platform { action }
    }

    fn call_with(
        tool: &str,
        input: Option<&str>,
        query: Option<&str>,
        request: Option<&str>,
    ) -> Commands {
        platform(PlatformAction::Call {
            tool: tool.into(),
            input: input.map(str::to_string),
            query: query.map(str::to_string),
            request: request.map(str::to_string),
            platform: None,
            account: None,
            project: None,
            connection: None,
            max_charge: None,
        })
    }

    #[test]
    fn hosted_platform_replay_key_is_stable_and_input_bound() {
        let (_, args) = outcome(&call_with(
            "generate_image",
            Some(r#"{"prompt": "synthetic"}"#),
            None,
            None,
        ))
        .unwrap();
        let key = args["key"].as_str().expect("a call carries a reply key");
        assert!(valid_platform_handle(key), "{key}");
        // The same command replays the same key; another input is another call.
        assert_eq!(
            outcome(&call_with(
                "generate_image",
                Some(r#"{"prompt": "synthetic"}"#),
                None,
                None
            ))
            .unwrap()
            .1["key"],
            json!(key)
        );
        assert_ne!(
            outcome(&call_with(
                "generate_image",
                Some(r#"{"prompt": "a different prompt"}"#),
                None,
                None
            ))
            .unwrap()
            .1["key"],
            json!(key)
        );
        let ordered = outcome(&call_with(
            "example_tool",
            Some(r#"{"nested":{"a":1,"b":2},"name":"x"}"#),
            None,
            None,
        ))
        .unwrap()
        .1["key"]
            .clone();
        let reordered = outcome(&call_with(
            "example_tool",
            Some(r#"{"name":"x","nested":{"b":2,"a":1}}"#),
            None,
            None,
        ))
        .unwrap()
        .1["key"]
            .clone();
        assert_eq!(
            ordered, reordered,
            "object key order must not trigger a new charge"
        );
        let query_key = outcome(&call_with(
            "read_posts",
            None,
            Some(r#"{"handle": "first"}"#),
            None,
        ))
        .unwrap()
        .1["key"]
            .clone();
        let other_query_key = outcome(&call_with(
            "read_posts",
            None,
            Some(r#"{"handle": "second"}"#),
            None,
        ))
        .unwrap()
        .1["key"]
            .clone();
        assert_ne!(
            query_key, other_query_key,
            "distinct queries cannot share a replay"
        );
    }

    #[test]
    fn hosted_platform_refuses_an_invalid_request_handle() {
        let refused = outcome(&call_with(
            "generate_image",
            Some("{}"),
            None,
            Some("bad handle"),
        ))
        .unwrap_err();
        assert!(refused.contains("1-128"), "{refused}");
    }

    #[test]
    fn hosted_platform_refuses_flags_the_workspace_owns() {
        for (label, action) in [
            (
                "--project",
                PlatformAction::Tools {
                    json: true,
                    project: Some("cadence".into()),
                },
            ),
            (
                "--platform",
                PlatformAction::Call {
                    tool: "generate_image".into(),
                    input: Some("{}".into()),
                    query: None,
                    request: None,
                    platform: Some("agenticos".into()),
                    account: None,
                    project: None,
                    connection: None,
                    max_charge: None,
                },
            ),
            (
                "--account",
                PlatformAction::Call {
                    tool: "generate_image".into(),
                    input: Some("{}".into()),
                    query: None,
                    request: None,
                    platform: None,
                    account: Some("work".into()),
                    project: None,
                    connection: None,
                    max_charge: None,
                },
            ),
        ] {
            let refused = outcome(&platform(action)).unwrap_err();
            assert!(refused.contains(label), "{label}: {refused}");
        }
    }

    #[test]
    fn hosted_platform_mixed_query_and_body_preserve_distinct_replay_keys() {
        use clap::Parser;

        let replay_key = |body: &str, query: &str| {
            let cli = Cli::try_parse_from([
                "cadence",
                "--org",
                "acme",
                "platform",
                "call",
                "example_tool",
                "--input",
                body,
                "--query",
                query,
            ])
            .unwrap_or_else(|error| {
                panic!("a hosted tool must accept query fields alongside its POST body: {error}")
            });
            let (_, args) = outcome(&cli.command).expect("mixed-input call needs a replay key");
            args["key"]
                .as_str()
                .expect("a hosted call needs a replay handle")
                .to_owned()
        };

        let original = replay_key(r#"{"prompt":"alpha"}"#, r#"{"resource":"first"}"#);
        let repeated = replay_key(r#"{"prompt":"alpha"}"#, r#"{"resource":"first"}"#);
        let changed_query = replay_key(r#"{"prompt":"alpha"}"#, r#"{"resource":"second"}"#);
        let changed_body = replay_key(r#"{"prompt":"beta"}"#, r#"{"resource":"first"}"#);

        assert_eq!(
            original, repeated,
            "repeating the same mixed-input invocation must reconcile the original call"
        );
        assert_ne!(
            original, changed_query,
            "changing only the query must not replay a call for a different resource"
        );
        assert_ne!(
            original, changed_body,
            "changing only the body must not replay a call with different content"
        );
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
    fn agent_roles_crosses_the_relay_without_arguments() {
        let (verb, args) = outcome(&Commands::Agent {
            action: AgentAction::Roles,
        })
        .unwrap();
        assert_eq!(verb, "agent_roles");
        assert_eq!(args, json!({}));
    }

    #[test]
    fn agent_profile_reads_send_only_the_profile_id() {
        let (verb, args) = outcome(&Commands::Agent {
            action: AgentAction::Profiles,
        })
        .unwrap();
        assert_eq!(verb, "agent_profile_list");
        assert_eq!(args, json!({}));

        let (verb, args) = outcome(&Commands::Agent {
            action: AgentAction::Context {
                id: "ap_11111111-2222-4333-8444-555555555555".into(),
            },
        })
        .unwrap();
        assert_eq!(verb, "agent_profile_context");
        assert_eq!(
            args,
            json!({"id": "ap_11111111-2222-4333-8444-555555555555"})
        );
    }

    #[test]
    fn agent_profile_commands_map_to_the_hosted_verbs() {
        use crate::cli::agent_profile::ProfileAction;
        let id = "ap_11111111-2222-4333-8444-555555555555";
        let profile = |action| Commands::Agent {
            action: AgentAction::Profile { action },
        };
        let (verb, args) = outcome(&profile(ProfileAction::Get { id: id.into() })).unwrap();
        assert_eq!((verb, args), ("agent_profile_show", json!({"id": id})));

        let (verb, args) = outcome(&profile(ProfileAction::Create {
            alias: "ledger".into(),
            catalogue: "accountant".into(),
            job_file: None,
            model: Some("inherit".into()),
        }))
        .unwrap();
        assert_eq!(verb, "agent_profile_create");
        assert_eq!(
            args,
            json!({"alias": "ledger", "catalogue": "accountant", "model": "inherit"})
        );

        let (verb, args) = outcome(&profile(ProfileAction::Edit {
            id: id.into(),
            set: vec!["role=Planner".into()],
            set_json: vec!["skills=[\"a\"]".into()],
            file: vec![],
            expect_rev: Some(4),
            latest: false,
        }))
        .unwrap();
        assert_eq!(verb, "agent_profile_update");
        assert_eq!(
            args,
            json!({"id": id, "expectedRevision": 4, "role": "Planner", "skills": ["a"]})
        );

        let (verb, args) = outcome(&profile(ProfileAction::Archive {
            id: id.into(),
            expect_rev: Some(4),
            latest: false,
        }))
        .unwrap();
        assert_eq!(verb, "agent_profile_update");
        assert_eq!(
            args,
            json!({"id": id, "expectedRevision": 4, "status": "archived"})
        );
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
            l.project = vec!["ops".into()];
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
                "project": "ops",
                "status": ["doing"], "tag": ["t"], "owner": ["me"],
                "plan": ["approved"], "open": true,
            })
        );
    }

    #[test]
    fn issue_ls_refuses_more_than_one_project() {
        // CAD-1999: the hosted route filters a single key; a repeated
        // `--project` must refuse, never silently keep one of them.
        let err = outcome(&issue_ls(|l| l.project = vec!["ops".into(), "cad".into()])).unwrap_err();
        assert!(err.contains("one --project"), "{err}");
    }

    #[test]
    fn issue_ls_rejects_every_flag_the_route_cannot_apply() {
        type Cases = Vec<(&'static str, fn(&mut IssueLs))>;
        let cases: Cases = vec![
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
    fn ticket_message_verbs_send_exactly_what_the_tenant_reads() {
        let say = |from: Option<&str>| {
            outcome(&Commands::Issue {
                action: IssueAction::Say {
                    id: "CAD-1".into(),
                    to: "developer".into(),
                    from: from.map(Into::into),
                    message: "tm-1".into(),
                    text: Some("REVISE: head moved".into()),
                    file: None,
                },
            })
            .unwrap()
        };
        let (verb, args) = say(Some("reviewer"));
        assert_eq!(verb, "issue_message_send");
        assert_eq!(
            args,
            json!({
                "id": "CAD-1",
                "to": "developer",
                "from": "reviewer",
                "message": "tm-1",
                "text": "REVISE: head moved",
            })
        );
        // Unset options are absent, not null: the tenant's argument check is exact.
        assert!(say(None).1.get("from").is_none());

        let messages = |to: Option<&str>, after: Option<u64>| {
            outcome(&Commands::Issue {
                action: IssueAction::Messages {
                    id: "CAD-1".into(),
                    to: to.map(Into::into),
                    after,
                },
            })
            .unwrap()
        };
        assert_eq!(
            messages(None, None),
            ("issue_messages", json!({"id": "CAD-1"}))
        );
        assert_eq!(
            messages(Some("developer"), Some(7)).1,
            json!({"id": "CAD-1", "to": "developer", "after": 7})
        );

        let (verb, args) = outcome(&Commands::Issue {
            action: IssueAction::MessagesRead {
                id: "CAD-1".into(),
                role: "developer".into(),
                through: 7,
            },
        })
        .unwrap();
        assert_eq!(verb, "issue_messages_read");
        assert_eq!(
            args,
            json!({"id": "CAD-1", "role": "developer", "through": 7})
        );
    }

    #[test]
    fn ticket_message_verbs_refuse_a_bad_value_before_the_wire() {
        let say = |to: &str, from: Option<&str>, message: &str, text: &str| {
            outcome(&Commands::Issue {
                action: IssueAction::Say {
                    id: "CAD-1".into(),
                    to: to.into(),
                    from: from.map(Into::into),
                    message: message.into(),
                    text: Some(text.into()),
                    file: None,
                },
            })
        };
        assert!(say(&"r".repeat(32), None, &"m".repeat(64), &"x".repeat(16_000)).is_ok());
        for bad in [
            say("Not A Role", None, "tm-1", "hi"),
            say("-dev", None, "tm-1", "hi"),
            say("", None, "tm-1", "hi"),
            say(&"r".repeat(33), None, "tm-1", "hi"),
            say("developer", Some("Not A Role"), "tm-1", "hi"),
            say("developer", None, "sys-reserved", "hi"),
            say("developer", None, "-leading", "hi"),
            say("developer", None, &"m".repeat(65), "hi"),
            say("developer", None, "tm-1", "   "),
            say("developer", None, "tm-1", &"x".repeat(16_001)),
        ] {
            assert!(bad.unwrap_err().contains("nothing was sent"));
        }
        let too_big = 1u64 << 53;
        assert!(outcome(&Commands::Issue {
            action: IssueAction::Messages {
                id: "CAD-1".into(),
                to: None,
                after: Some(too_big),
            },
        })
        .is_err());
        assert!(outcome(&Commands::Issue {
            action: IssueAction::MessagesRead {
                id: "CAD-1".into(),
                role: "developer".into(),
                through: too_big,
            },
        })
        .is_err());
    }

    // CAD-2083 acceptance, written from the ticket by the reviewer: real argv,
    // the real verb map, and the tenant's own grammar (`TICKET_ROLE`,
    // `TICKET_MESSAGE_ID`, `TICKET_MESSAGE_BODY_MAX`, `Number.isSafeInteger`).
    fn cad2083_argv(args: &[String]) -> std::result::Result<(&'static str, Value), String> {
        use clap::Parser;
        let mut argv = vec!["cadence".to_string(), "--org".into(), "acme".into()];
        argv.extend_from_slice(args);
        let cli = crate::cli::Cli::try_parse_from(argv).map_err(|e| e.to_string())?;
        outcome(&cli.command)
    }

    fn cad2083_say(to: &str, from: Option<&str>, message: &str, text: &str) -> Vec<String> {
        let mut args: Vec<String> = ["issue", "say", "CAD-1"].map(Into::into).to_vec();
        args.push(format!("--to={to}"));
        if let Some(from) = from {
            args.push(format!("--from={from}"));
        }
        args.push(format!("--message={message}"));
        args.push(format!("--text={text}"));
        args
    }

    #[test]
    fn cad2083_argv_sends_exactly_the_keys_the_tenant_admits() {
        assert_eq!(
            cad2083_argv(&cad2083_say("developer", Some("reviewer"), "tm-1", "hi")).unwrap(),
            (
                "issue_message_send",
                json!({"id": "CAD-1", "to": "developer", "from": "reviewer",
                    "message": "tm-1", "text": "hi"})
            )
        );
        // The tenant's `exactKeys` and role check refuse a null: unset is absent.
        assert_eq!(
            cad2083_argv(&cad2083_say("developer", None, "tm-1", "hi")).unwrap(),
            (
                "issue_message_send",
                json!({"id": "CAD-1", "to": "developer", "message": "tm-1", "text": "hi"})
            )
        );
        let argv =
            |args: &[&str]| cad2083_argv(&args.iter().map(ToString::to_string).collect::<Vec<_>>());
        assert_eq!(
            argv(&["issue", "messages", "CAD-1"]).unwrap(),
            ("issue_messages", json!({"id": "CAD-1"}))
        );
        assert_eq!(
            argv(&[
                "issue",
                "messages",
                "CAD-1",
                "--to",
                "developer",
                "--after",
                "0"
            ])
            .unwrap(),
            (
                "issue_messages",
                json!({"id": "CAD-1", "to": "developer", "after": 0})
            )
        );
        assert_eq!(
            argv(&[
                "issue",
                "messages-read",
                "CAD-1",
                "--role",
                "developer",
                "--through",
                "7"
            ])
            .unwrap(),
            (
                "issue_messages_read",
                json!({"id": "CAD-1", "role": "developer", "through": 7})
            )
        );
        // `--role` and `--through` are both required by the tenant.
        assert!(argv(&["issue", "messages-read", "CAD-1", "--role", "developer"]).is_err());
        assert!(argv(&["issue", "messages-read", "CAD-1", "--through", "7"]).is_err());
    }

    #[test]
    fn cad2083_values_follow_the_tenant_grammar_before_the_wire() {
        let refused = |args: Vec<String>| {
            let err = cad2083_argv(&args).unwrap_err();
            assert!(err.contains("nothing was sent"), "{args:?}: {err}");
        };
        let long_role = "r".repeat(32);
        for good in ["a", "a-", "a1", long_role.as_str()] {
            cad2083_argv(&cad2083_say(good, Some(good), "tm-1", "hi")).unwrap();
        }
        let too_long_role = "r".repeat(33);
        for bad in [
            "",
            "-a",
            "1a",
            "A",
            "a_b",
            "é",
            "a\n",
            too_long_role.as_str(),
        ] {
            refused(cad2083_say(bad, None, "tm-1", "hi"));
            refused(cad2083_say("developer", Some(bad), "tm-1", "hi"));
        }
        let long_id = "m".repeat(64);
        for good in ["1", "a-", "0-0", "sys", "sysx-1", long_id.as_str()] {
            cad2083_argv(&cad2083_say("developer", None, good, "hi")).unwrap();
        }
        let too_long_id = "m".repeat(65);
        for bad in ["", "-a", "A", "a_b", "sys-1", "a\n", too_long_id.as_str()] {
            refused(cad2083_say("developer", None, bad, "hi"));
        }
        // The cap is UTF-8 bytes, not characters.
        cad2083_argv(&cad2083_say("developer", None, "tm-1", &"é".repeat(8_000))).unwrap();
        refused(cad2083_say(
            "developer",
            None,
            "tm-1",
            &format!("{}x", "é".repeat(8_000)),
        ));
        for blank in ["   ", " \n\t"] {
            refused(cad2083_say("developer", None, "tm-1", blank));
        }
        let seq = |flag: &str, value: &str| -> Vec<String> {
            let mut args: Vec<String> = match flag {
                "--after" => ["issue", "messages", "CAD-1"].map(Into::into).to_vec(),
                _ => ["issue", "messages-read", "CAD-1", "--role", "developer"]
                    .map(Into::into)
                    .to_vec(),
            };
            args.push(format!("{flag}={value}"));
            args
        };
        for flag in ["--after", "--through"] {
            cad2083_argv(&seq(flag, "9007199254740991")).unwrap();
            refused(seq(flag, "9007199254740992"));
            assert!(cad2083_argv(&seq(flag, "-1")).is_err());
        }
    }

    #[test]
    fn issue_review_sends_the_digest_it_was_given_and_refuses_a_bad_one() {
        let digest = "7743ce348d9284d677a185f33295b92266cc435a5b5f775029b300066d26693a";
        let (verb, args) = outcome(&Commands::Issue {
            action: IssueAction::Review {
                id: "CAD-1".into(),
                artifact: "hosted:drafts.md".into(),
                digest: digest.into(),
                decision: "pass".into(),
                text: Some("Uses only the supplied facts.".into()),
                file: None,
            },
        })
        .unwrap();
        assert_eq!(verb, "issue_review");
        assert_eq!(
            args,
            json!({
                "id": "CAD-1",
                "artifact": "hosted:drafts.md",
                "digest": digest,
                "decision": "pass",
                "text": "Uses only the supplied facts.",
            })
        );

        let bad = |action: IssueAction| outcome(&Commands::Issue { action }).unwrap_err();
        let review = |artifact: &str, digest: &str, decision: &str, text: Option<&str>| {
            IssueAction::Review {
                id: "CAD-1".into(),
                artifact: artifact.into(),
                digest: digest.into(),
                decision: decision.into(),
                text: text.map(str::to_string),
                file: None,
            }
        };
        // A native basename, an uppercase or short digest, an unknown decision
        // and a missing body are refused before any request is built.
        assert!(bad(review("drafts.md", digest, "pass", Some("Supported.")))
            .contains("invalid artifact identifier"));
        assert!(bad(review(
            "hosted:drafts.md",
            &digest.to_uppercase(),
            "pass",
            Some("Supported.")
        ))
        .contains("lowercase hex sha256"));
        assert!(bad(review(
            "hosted:drafts.md",
            "abc",
            "pass",
            Some("Supported.")
        ))
        .contains("lowercase hex sha256"));
        assert!(bad(review(
            "hosted:drafts.md",
            digest,
            "approve",
            Some("Supported.")
        ))
        .contains("pass, revise or blocked"));
        assert!(
            bad(review("hosted:drafts.md", digest, "pass", None)).contains("one of -m or --file")
        );
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
    fn remote_project_new_and_ls_map_but_add_and_set_remote_stay_unmapped() {
        use cadence_agent::issue::cli::ProjectAction;
        let (verb, args) = outcome(&Commands::Project {
            action: ProjectCmd::New {
                key: "ops".into(),
                name: Some("Operations".into()),
                repo: None,
                prefix: Some("OPS".into()),
                goal: None,
                agents: vec![],
                issue: None,
                visibility: None,
                grants: vec![],
            },
        })
        .unwrap();
        assert_eq!(verb, "project_new");
        assert_eq!(
            args,
            json!({"key": "ops", "name": "Operations", "prefix": "OPS"})
        );

        // The name is the hosted registry's own field: a create without it
        // refuses, and the local repo/PROJECT.md flags never cross.
        let err = outcome(&Commands::Project {
            action: ProjectCmd::New {
                key: "ops".into(),
                name: None,
                repo: None,
                prefix: None,
                goal: None,
                agents: vec![],
                issue: None,
                visibility: None,
                grants: vec![],
            },
        })
        .unwrap_err();
        assert!(err.contains("--name"), "{err}");
        let err = outcome(&Commands::Project {
            action: ProjectCmd::New {
                key: "ops".into(),
                name: Some("Operations".into()),
                repo: Some(std::path::PathBuf::from("/tmp/ops")),
                prefix: None,
                goal: None,
                agents: vec![],
                issue: None,
                visibility: None,
                grants: vec![],
            },
        })
        .unwrap_err();
        assert!(err.contains("--repo"), "{err}");

        // CAD-1950: the hosted create contract carries no access policy, so
        // the policy flags refuse rather than being dropped; the policy is
        // set afterwards with `issue project policy`. The last case is the
        // one a value comparison missed: `workspace-visible` is the local
        // default, but naming it here is still a request this verb cannot
        // honour, so it refuses by presence.
        for (visibility, grants) in [
            (
                Some(cadence_agent::issue::cli::PolicyVisibility::Private),
                Vec::new(),
            ),
            (
                Some(cadence_agent::issue::cli::PolicyVisibility::WorkspaceVisible),
                vec!["users/alex".to_string()],
            ),
            (
                Some(cadence_agent::issue::cli::PolicyVisibility::WorkspaceVisible),
                Vec::new(),
            ),
        ] {
            let err = outcome(&Commands::Project {
                action: ProjectCmd::New {
                    key: "ops".into(),
                    name: Some("Operations".into()),
                    repo: None,
                    prefix: None,
                    goal: None,
                    agents: vec![],
                    issue: None,
                    visibility,
                    grants,
                },
            })
            .unwrap_err();
            assert!(err.contains("issue project policy"), "{err}");
        }

        let (verb, args) = outcome(&Commands::Issue {
            action: IssueAction::Project {
                action: ProjectAction::Ls {
                    sort: None,
                    limit: None,
                    fields: vec![],
                    json: false,
                },
            },
        })
        .unwrap();
        assert_eq!(verb, "project_ls");
        assert_eq!(args, json!({}));

        let err = outcome(&Commands::Issue {
            action: IssueAction::Project {
                action: ProjectAction::Ls {
                    sort: Some("key".into()),
                    limit: None,
                    fields: vec![],
                    json: false,
                },
            },
        })
        .unwrap_err();
        assert!(err.contains("--sort"), "{err}");

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
    fn remote_link_maps_to_the_board_verb_and_refuses_locally() {
        use cadence_agent::issue::cli::IssueAction;
        let link = |kind: &str, target: &str| Commands::Issue {
            action: IssueAction::Link {
                id: "CAD-16".into(),
                kind: kind.into(),
                target: target.into(),
            },
        };

        let (verb, args) = verb_for(&link("blocked_by", "CAD-9"), None).unwrap();
        assert_eq!(verb, "issue_link");
        assert_eq!(args.get("id").and_then(Value::as_str), Some("CAD-16"));
        assert_eq!(args.get("kind").and_then(Value::as_str), Some("blocked_by"));
        assert_eq!(args.get("target").and_then(Value::as_str), Some("CAD-9"));

        // Both refusals are knowable here, so neither reaches the wire.
        let unknown = verb_for(&link("cousin", "CAD-9"), None)
            .unwrap_err()
            .to_string();
        assert!(
            unknown.contains("blocked_by, relates, parent or duplicate_of"),
            "{unknown}"
        );
        assert!(unknown.contains("nothing was sent"), "{unknown}");

        // A value the board would refuse is refused here, never trimmed into a
        // different write: a trailing newline is not "repaired" to a valid kind.
        let padded_kind = verb_for(&link("relates\n", "CAD-9"), None)
            .unwrap_err()
            .to_string();
        assert!(padded_kind.contains("unknown link kind"), "{padded_kind}");

        let self_link = verb_for(&link("relates", "CAD-16"), None)
            .unwrap_err()
            .to_string();
        assert!(self_link.contains("cannot link itself"), "{self_link}");

        // The shared grammar bounds both ids, so a malformed one costs no request.
        for malformed in ["CAD-", "cad-16", "CAD-1234567890", "CAD-9\nX"] {
            let err = verb_for(&link("relates", malformed), None)
                .unwrap_err()
                .to_string();
            assert!(err.contains("invalid target id"), "{malformed}: {err}");
            assert!(err.contains("nothing was sent"), "{malformed}: {err}");
        }

        let unlink = Commands::Issue {
            action: IssueAction::Unlink {
                id: "CAD-16".into(),
                kind: "parent".into(),
                target: "CAD-9".into(),
            },
        };
        assert_eq!(verb_for(&unlink, None).unwrap().0, "issue_unlink");
    }

    #[test]
    fn verbs_outside_the_read_allowlist_refuse() {
        let err = outcome(&Commands::Agent {
            action: AgentAction::Stop { alias: "w1".into() },
        })
        .unwrap_err();
        assert!(err.contains("not available on a remote org"), "{err}");
    }

    fn collect_paths(cmd: &clap::Command, prefix: &mut Vec<String>, out: &mut Vec<String>) {
        for sub in cmd.get_subcommands() {
            if sub.get_name() == "help" {
                continue;
            }
            prefix.push(sub.get_name().to_string());
            out.push(prefix.join(" "));
            collect_paths(sub, prefix, out);
            prefix.pop();
        }
    }

    #[test]
    fn hosted_verb_rows_name_real_commands_and_supported_alternatives() {
        let root = super::super::help::root_command(false);
        let mut paths = Vec::new();
        collect_paths(&root, &mut Vec::new(), &mut paths);
        let real: std::collections::HashSet<&str> = paths.iter().map(String::as_str).collect();
        let rows = hosted_verbs();
        assert!(
            !rows.verb.is_empty(),
            "src/cli/hosted-verbs.toml has no rows"
        );
        let gaps: std::collections::HashSet<&str> =
            rows.verb.iter().map(|row| row.path.as_str()).collect();
        for row in &rows.verb {
            assert!(
                real.contains(row.path.as_str()),
                "{} is not a command",
                row.path
            );
            assert!(
                matches!(row.class.as_str(), "planned" | "local-only"),
                "{}: unknown class {}",
                row.path,
                row.class
            );
            if row.class == "planned" {
                let ticket = row.ticket.as_deref().unwrap_or("");
                let numbered = ticket
                    .strip_prefix("CAD-")
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
                assert!(numbered, "{}: planned without a ticket", row.path);
            }
            if let Some(alt) = &row.alternative {
                assert!(
                    real.contains(alt.as_str()),
                    "{}: alternative {alt} is not a command",
                    row.path
                );
                assert!(
                    !gaps.contains(alt.as_str()),
                    "{}: alternative {alt} is itself a gap",
                    row.path
                );
            }
        }
    }

    #[test]
    fn a_planned_verb_refusal_names_its_ticket_and_a_supported_alternative() {
        let message = unavailable(Some("issue edit")).to_string();
        assert!(message.contains("`cadence issue edit`"), "{message}");
        assert!(message.contains("planned (CAD-2202)"), "{message}");
        assert!(message.contains("`cadence issue comment`"), "{message}");
        assert!(message.contains("nothing was sent"), "{message}");
        let generic = unavailable(Some("ui")).to_string();
        assert!(
            generic.contains("not available on a remote org"),
            "{generic}"
        );
        assert!(generic.contains("cadence commands --all"), "{generic}");
        assert!(generic.contains("nothing was sent"), "{generic}");
    }

    #[test]
    fn a_scope_gap_refusal_names_the_login_flag_and_the_credential() {
        let caps = vec!["cli.read".to_string(), "cli.write".to_string()];
        let uncertain = Error::rejected(
            "the remote did not confirm why it refused the credential (uncertain, HTTP 401); \
             nothing was retried",
        );
        let hinted = scope_hint(
            uncertain,
            &caps,
            "audit.approve",
            "cadence login --audit-approve",
        )
        .to_string();
        assert!(
            hinted.contains("does not carry `audit.approve`"),
            "{hinted}"
        );
        assert!(hinted.contains("cadence login --audit-approve"), "{hinted}");
        assert!(hinted.contains("cli.read, cli.write"), "{hinted}");
        // A known cause replaces the uncertain-401 text; it never leads with it.
        assert!(!hinted.contains("uncertain"), "{hinted}");

        // A credential that carries the scope keeps the original message.
        let owned = vec!["audit.approve".to_string()];
        let plain =
            scope_hint(Error::rejected("boom"), &owned, "audit.approve", "unused").to_string();
        assert!(plain.contains("boom"), "{plain}");
        assert!(!plain.contains("does not carry"), "{plain}");

        // A different refusal is never rewritten.
        let ended = Error::rejected("the remote session ended (authentication, HTTP 401)");
        let kept = scope_hint(ended, &caps, "audit.approve", "unused").to_string();
        assert!(kept.contains("session ended"), "{kept}");
        assert!(!kept.contains("does not carry"), "{kept}");
    }

    #[test]
    fn unavailable_commands_keep_the_hosted_context_and_hide_arguments() {
        for (argv, expected) in [
            (
                vec![
                    "cadence",
                    "issue",
                    "edit",
                    "CAD-1",
                    "--unlink",
                    "blocked_by:private-value",
                ],
                "`cadence issue edit`",
            ),
            (
                vec!["cadence", "wiki", "mkdir", "private-value"],
                "`cadence wiki mkdir`",
            ),
        ] {
            let matches = super::super::help::root_command(false)
                .try_get_matches_from(argv)
                .unwrap();
            let cli = <Cli as clap::FromArgMatches>::from_arg_matches(&matches).unwrap();
            let path = super::super::action_dispatch::command_path(&matches);
            let err = verb_for(&cli.command, path.as_deref()).unwrap_err();
            let message = err.to_string();
            assert_eq!(err.kind(), "rejected");
            assert!(message.contains(expected), "{message}");
            assert!(!message.contains("private-value"), "{message}");
            assert!(!message.contains("--org local"), "{message}");
            assert!(!message.contains("org switch"), "{message}");
            assert!(message.contains("nothing was sent"), "{message}");
        }
    }

    // CAD-2127: `platform status` is a read that resolves the same request
    // handle a `platform call` used — by `--request`, by `--job`, or by
    // deriving the identical `pcli_<sha256>` key from `--tool` plus its
    // input/query.
    fn status_with(
        request: Option<&str>,
        job: Option<&str>,
        tool: Option<&str>,
        input: Option<&str>,
        query: Option<&str>,
    ) -> Commands {
        platform(PlatformAction::Status {
            request: request.map(str::to_string),
            job: job.map(str::to_string),
            tool: tool.map(str::to_string),
            input: input.map(str::to_string),
            query: query.map(str::to_string),
        })
    }

    /// The mapped refusal for a command that must never build a request.
    fn refusal(command: &Commands) -> Error {
        match verb_for(command, None) {
            Ok((verb, _)) => panic!("`{verb}` must refuse before any request is built"),
            Err(error) => error,
        }
    }

    #[test]
    fn hosted_platform_status_maps_a_request_handle_or_job_id_to_a_read() {
        let (verb, args) = outcome(&status_with(Some("cad2127-run-1"), None, None, None, None))
            .expect("a request handle is a complete status target");
        assert_eq!(verb, "platform_call");
        assert_eq!(args, json!({"operation": "status", "key": "cad2127-run-1"}));

        let (verb, args) = outcome(&status_with(None, Some("med_job-1"), None, None, None))
            .expect("a hosted media job id is a complete status target");
        assert_eq!(verb, "platform_call");
        assert_eq!(args, json!({"operation": "status", "jobId": "med_job-1"}));

        // Both targets may ride together; neither dispatches or polls.
        let (_, args) = outcome(&status_with(
            Some("cad2127-run-1"),
            Some("med_job-1"),
            None,
            None,
            None,
        ))
        .unwrap();
        assert_eq!(
            args,
            json!({"operation": "status", "key": "cad2127-run-1", "jobId": "med_job-1"})
        );
    }

    #[test]
    fn hosted_platform_status_derives_exactly_the_call_key_from_tool_and_body() {
        // The status key must be the same replay handle the failed call used:
        // a derived key that differed would reconcile the wrong request.
        for (tool, input, query) in [
            ("generate_image", Some(r#"{"prompt": "synthetic"}"#), None),
            ("read_posts", None, Some(r#"{"handle": "first"}"#)),
            (
                "example_tool",
                Some(r#"{"prompt": "synthetic"}"#),
                Some(r#"{"resource": "first"}"#),
            ),
        ] {
            let (verb, call_args) = outcome(&call_with(tool, input, query, None))
                .expect("the call maps to a replay key");
            assert_eq!(verb, "platform_call");
            let call_key = call_args["key"]
                .as_str()
                .expect("a call carries its reply key")
                .to_owned();
            assert!(
                call_key.starts_with("pcli_"),
                "a derived handle is the sha256 replay key: {call_key}"
            );
            assert_eq!(call_key.len(), "pcli_".len() + 64, "{call_key}");

            let (verb, status_args) = outcome(&status_with(None, None, Some(tool), input, query))
                .expect("tool plus its input derives the same handle");
            assert_eq!(verb, "platform_call");
            assert_eq!(status_args["operation"], json!("status"));
            assert_eq!(status_args["key"], json!(call_key));
            assert_eq!(status_args["tool"], json!(tool));
            assert!(
                valid_platform_handle(&call_key),
                "the derived handle is the wire grammar: {call_key}"
            );
        }
    }

    #[test]
    fn hosted_platform_status_refuses_bad_handles_jobs_and_missing_targets() {
        // Everything below is decided in `verb_for`, before any request exists.
        let bad_handles: Vec<String> = vec![
            String::new(),
            "bad handle".into(),
            "bad/handle".into(),
            "a".repeat(129),
        ];
        for handle in bad_handles {
            let error = refusal(&status_with(Some(handle.as_str()), None, None, None, None));
            assert_eq!(error.kind(), "rejected", "{handle:?}: {error}");
            let message = error.to_string();
            assert!(message.contains("1-128"), "{handle:?}: {message}");
        }

        let too_long = format!("med_{}", "a".repeat(81));
        for job in ["job-1", "med_", "med_bad/job", too_long.as_str()] {
            let error = refusal(&status_with(None, Some(job), None, None, None));
            assert_eq!(error.kind(), "rejected", "{job:?}: {error}");
            let message = error.to_string();
            assert!(message.contains("med_"), "{job:?}: {message}");
        }

        // No target at all.
        let error = refusal(&status_with(None, None, None, None, None));
        assert_eq!(error.kind(), "usage", "{error}");
        let message = error.to_string();
        assert!(message.contains("--request"), "{message}");
        assert!(message.contains("--job"), "{message}");

        // A tool with no request and no body/query cannot derive a handle.
        let error = refusal(&status_with(None, None, Some("generate_image"), None, None));
        assert_eq!(error.kind(), "usage", "{error}");
        let message = error.to_string();
        assert!(message.contains("--input"), "{message}");
        assert!(message.contains("--query"), "{message}");

        // Body/query without a tool cannot name the tool the key hashed.
        let error = refusal(&status_with(None, None, None, Some("{}"), None));
        assert_eq!(error.kind(), "usage", "{error}");
        let message = error.to_string();
        assert!(message.contains("--tool"), "{message}");
    }

    // CAD-2127 follow-up (independent security review): the hosted call echoes
    // the tool name on the pre-call correlation line and puts it on the wire,
    // so a name outside the slug grammar — notably one carrying a newline or
    // control byte that could forge that line — is refused while the call is
    // mapped, before any request or stderr correlation exists.
    #[test]
    fn hosted_platform_refuses_an_unsafe_tool_name_before_dispatch() {
        let too_long = format!("a{}", "a".repeat(128));
        for tool in [
            "GenerateImage",
            "generate image",
            "generate\nimage",
            "generate\rimage",
            "generate\u{1b}image",
            "-generate_image",
            ".generate_image",
            "generate/image",
            "genérate_image",
            "",
            too_long.as_str(),
        ] {
            let error = refusal(&call_with(tool, Some("{}"), None, None));
            assert_eq!(error.kind(), "rejected", "{tool:?}: {error}");
            let message = error.to_string();
            assert!(message.contains("a-z"), "{tool:?}: {message}");
            // The refusal is a fixed sentence: the rejected name cannot forge
            // a second line in the caller's terminal.
            assert!(!message.contains('\n'), "{tool:?}: {message:?}");
            assert!(!message.contains('\r'), "{tool:?}: {message:?}");
        }
        // The same guard accepts the declared slug, so it is a grammar check,
        // not a blanket refusal.
        assert!(outcome(&call_with("generate_image", Some("{}"), None, None)).is_ok());
    }

    #[test]
    fn hosted_platform_artifact_maps_a_verified_ref_and_refuses_bad_ones() {
        let digest = "a".repeat(64);
        let reference = format!("med_job-1.{digest}");
        let (verb, args) = outcome(&platform(PlatformAction::Artifact {
            reference: reference.clone(),
            out: PathBuf::from("/tmp/cad2127-image.png"),
        }))
        .expect("a complete ref maps to the artifact read");
        assert_eq!(verb, "platform_call");
        assert_eq!(args, json!({"operation": "artifact", "ref": reference}));

        // A ref without a job id, a short/upper-case digest, or a non-hex
        // digest cannot name a retained artifact, so it refuses locally.
        for bad in [
            "med_job-1".to_string(),
            format!("job-1.{digest}"),
            format!("med_.{digest}"),
            format!("med_job-1.{}", "a".repeat(63)),
            format!("med_job-1.{}", "A".repeat(64)),
            format!("med_job-1.{}", "z".repeat(64)),
        ] {
            let error = refusal(&platform(PlatformAction::Artifact {
                reference: bad.clone(),
                out: PathBuf::from("/tmp/cad2127-image.png"),
            }));
            assert_eq!(error.kind(), "rejected", "{bad}: {error}");
            let message = error.to_string();
            assert!(message.contains("med_<job-id>"), "{bad}: {message}");
        }
    }

    fn artifact_ref_for(bytes: &[u8]) -> (String, String) {
        use sha2::{Digest, Sha256};
        let digest: String = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        (format!("med_job1.{digest}"), digest)
    }

    fn artifact_page(
        reference: &str,
        job_id: &str,
        digest: &str,
        mime: &str,
        offset: u64,
        total: u64,
        chunk: &[u8],
    ) -> Value {
        use base64::Engine;
        json!({
            "ok": true,
            "data": {
                "ref": reference,
                "jobId": job_id,
                "digest": digest,
                "mime": mime,
                "offset": offset,
                "total": total,
                "bytes": chunk.len(),
                "content_base64": base64::engine::general_purpose::STANDARD.encode(chunk),
            },
        })
    }

    fn run_artifact_pages(
        reference: &str,
        out: &std::path::Path,
        pages: Vec<std::result::Result<Value, String>>,
    ) -> (Result<Value>, usize) {
        let served = std::cell::Cell::new(0usize);
        let mut pages = pages.into_iter();
        let result = collect_platform_artifact(reference, out, |_offset| {
            served.set(served.get() + 1);
            match pages.next() {
                Some(Ok(page)) => Ok(page),
                Some(Err(message)) => Err(Error::rejected(message)),
                None => Err(Error::rejected(
                    "the download asked for a page it should not need",
                )),
            }
        });
        (result, served.get())
    }

    // CAD-2127: a multi-page artifact is spooled, checked page by page and in
    // full, and only then created at the destination.
    #[test]
    fn a_multi_page_artifact_is_verified_before_the_destination_is_created() {
        let bytes = b"\x89PNG\r\n\x1a\nsynthetic-provider-image-bytes";
        let (reference, digest) = artifact_ref_for(bytes);
        let total = bytes.len() as u64;
        let split = 8usize; // page one keeps the PNG signature intact
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("image.png");
        let mut fetched = Vec::new();
        let mut existed_during_fetch = false;
        let receipt = collect_platform_artifact(&reference, &out, |offset| {
            fetched.push(offset);
            existed_during_fetch |= out.exists();
            let chunk = if offset == 0 {
                &bytes[..split]
            } else {
                &bytes[split..]
            };
            Ok(artifact_page(
                &reference,
                "med_job1",
                &digest,
                "image/png",
                offset,
                total,
                chunk,
            ))
        })
        .expect("a verified two-page artifact is written");
        assert_eq!(fetched, vec![0, split as u64], "pages are contiguous");
        assert!(
            !existed_during_fetch,
            "the destination must not exist before the full digest check"
        );
        assert_eq!(receipt["ref"].as_str(), Some(reference.as_str()));
        assert_eq!(receipt["jobId"], json!("med_job1"));
        assert_eq!(receipt["digest"].as_str(), Some(digest.as_str()));
        assert_eq!(receipt["mime"], json!("image/png"));
        assert_eq!(receipt["bytes"], json!(total));
        assert_eq!(
            receipt["written"].as_str(),
            Some(out.to_str().unwrap()),
            "the receipt names the created file"
        );
        assert_eq!(std::fs::read(&out).unwrap(), bytes.as_slice());
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&out).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a retained artifact stays private");
    }

    #[test]
    fn a_large_video_artifact_is_verified_before_the_destination_is_created() {
        let bytes = vec![0x56; 10 * 1024 * 1024 + 1];
        let (reference, digest) = artifact_ref_for(&bytes);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("video.mp4");
        let receipt = collect_platform_artifact(&reference, &out, |offset| {
            assert!(
                !out.exists(),
                "unverified video must not reach the destination"
            );
            let start = offset as usize;
            let end = (start + 1024 * 1024).min(bytes.len());
            Ok(artifact_page(
                &reference,
                "med_job1",
                &digest,
                "video/mp4",
                offset,
                bytes.len() as u64,
                &bytes[start..end],
            ))
        })
        .expect("a verified video above the image limit is written");
        assert_eq!(receipt["mime"], json!("video/mp4"));
        assert_eq!(receipt["bytes"], json!(bytes.len()));
        assert_eq!(std::fs::read(&out).unwrap(), bytes);
    }

    #[test]
    fn a_tampered_video_leaves_no_destination() {
        let bytes = b"synthetic-webm";
        let (reference, digest) = artifact_ref_for(bytes);
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("verified.webm");
        let (verified, _) = run_artifact_pages(
            &reference,
            &good,
            vec![Ok(artifact_page(
                &reference,
                "med_job1",
                &digest,
                "video/webm",
                0,
                bytes.len() as u64,
                bytes,
            ))],
        );
        assert!(verified.is_ok());
        assert_eq!(std::fs::read(&good).unwrap(), bytes);
        let out = dir.path().join("video.webm");
        let (result, _) = run_artifact_pages(
            &reference,
            &out,
            vec![Ok(artifact_page(
                &reference,
                "med_job1",
                &digest,
                "video/webm",
                0,
                bytes.len() as u64,
                b"tampered-webm!",
            ))],
        );
        assert!(result.is_err());
        assert!(!out.exists());
    }

    #[test]
    fn an_existing_destination_is_refused_before_any_page_is_fetched() {
        let bytes = b"\x89PNG\r\n\x1a\nsynthetic-provider-image-bytes";
        let (reference, digest) = artifact_ref_for(bytes);
        let split = 8usize;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("image.png");
        std::fs::write(&out, b"keep me").unwrap();
        let (result, served) = run_artifact_pages(
            &reference,
            &out,
            vec![Ok(artifact_page(
                &reference,
                "med_job1",
                &digest,
                "image/png",
                0,
                bytes.len() as u64,
                &bytes[..split],
            ))],
        );
        let error = result.expect_err("an existing file must never be adopted");
        assert_eq!(error.kind(), "rejected", "{error}");
        assert!(error.to_string().contains("already exists"), "{error}");
        assert_eq!(served, 0, "no page may be fetched for an existing file");
        assert_eq!(std::fs::read(&out).unwrap(), b"keep me".as_slice());
    }

    #[test]
    fn an_unverified_artifact_page_leaves_no_partial_file() {
        let bytes = b"\x89PNG\r\n\x1a\nsynthetic-provider-image-bytes";
        let (reference, digest) = artifact_ref_for(bytes);
        let total = bytes.len() as u64;
        let split = 8usize;
        let first = || {
            artifact_page(
                &reference,
                "med_job1",
                &digest,
                "image/png",
                0,
                total,
                &bytes[..split],
            )
        };
        let second = |offset, total, chunk: &[u8]| {
            artifact_page(
                &reference,
                "med_job1",
                &digest,
                "image/png",
                offset,
                total,
                chunk,
            )
        };
        let (other_reference, other_digest) = artifact_ref_for(b"not the served bytes");

        let cases: Vec<(&str, Vec<std::result::Result<Value, String>>)> = vec![
            (
                "a foreign ref",
                vec![
                    Ok(first()),
                    Ok(artifact_page(
                        &format!("med_other.{digest}"),
                        "med_job1",
                        &digest,
                        "image/png",
                        split as u64,
                        total,
                        &bytes[split..],
                    )),
                ],
            ),
            (
                "a foreign job id",
                vec![
                    Ok(first()),
                    Ok(artifact_page(
                        &reference,
                        "med_job2",
                        &digest,
                        "image/png",
                        split as u64,
                        total,
                        &bytes[split..],
                    )),
                ],
            ),
            (
                "a foreign digest",
                vec![
                    Ok(first()),
                    Ok(artifact_page(
                        &reference,
                        "med_job1",
                        &"0".repeat(64),
                        "image/png",
                        split as u64,
                        total,
                        &bytes[split..],
                    )),
                ],
            ),
            (
                "a non-contiguous offset",
                vec![
                    Ok(first()),
                    Ok(second(split as u64 + 1, total, &bytes[split..])),
                ],
            ),
            (
                "a changed total",
                vec![
                    Ok(first()),
                    Ok(second(split as u64, total + 1, &bytes[split..])),
                ],
            ),
            (
                "a changed mime",
                vec![
                    Ok(first()),
                    Ok(artifact_page(
                        &reference,
                        "med_job1",
                        &digest,
                        "image/jpeg",
                        split as u64,
                        total,
                        &bytes[split..],
                    )),
                ],
            ),
            (
                "a digest that does not match the bytes",
                vec![
                    Ok(artifact_page(
                        &other_reference,
                        "med_job1",
                        &other_digest,
                        "image/png",
                        0,
                        total,
                        &bytes[..split],
                    )),
                    Ok(artifact_page(
                        &other_reference,
                        "med_job1",
                        &other_digest,
                        "image/png",
                        split as u64,
                        total,
                        &bytes[split..],
                    )),
                ],
            ),
            (
                "a later page fetch error",
                vec![Ok(first()), Err("the second page never arrived".into())],
            ),
        ];

        for (label, pages) in cases {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("image.png");
            let (result, served) = run_artifact_pages(&reference, &out, pages);
            let error = result.unwrap_err();
            assert_eq!(error.kind(), "rejected", "{label}: {error}");
            assert!(
                !out.exists(),
                "{label}: a refused download must leave no file at {}",
                out.display()
            );
            assert!(served >= 1, "{label}: no page was fetched");
            assert!(
                std::fs::read_dir(dir.path()).unwrap().next().is_none(),
                "{label}: the destination directory must stay empty"
            );
        }

        // A total of zero or beyond the supported bound cannot be a real
        // artifact, so it is refused before the destination is created.
        for (total, mime) in [
            (0u64, "image/png"),
            (PLATFORM_MEDIA_ARTIFACT_MAX_BYTES + 1, "image/png"),
            (PLATFORM_VIDEO_ARTIFACT_MAX_BYTES + 1, "video/mp4"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("image.png");
            let (result, _) = run_artifact_pages(
                &reference,
                &out,
                vec![Ok(artifact_page(
                    &reference,
                    "med_job1",
                    &digest,
                    mime,
                    0,
                    total,
                    &bytes[..split],
                ))],
            );
            let error = result.expect_err("an impossible total must refuse");
            assert_eq!(error.kind(), "rejected", "{total}: {error}");
            assert!(!out.exists(), "{total}: {error}");
        }
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
