//! CAD-1019 slice 2: dispatch for a `Remote` org selection — maps the
//! allowlisted local commands to AOS-128 verbs and runs them through
//! [`cadence_agent::remote_cli`]. Anything outside the read-only allowlist
//! is refused *here*, before a request is ever built — remote writes land
//! in their own slice, and operator-only verbs never do.

use super::*;

fn unsupported(verb: &str, flag: &str, why: &str) -> Error {
    Error::rejected(format!("remote `{verb}` cannot {flag} — {why}"))
}

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

#[cfg(test)]
mod tests {
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
