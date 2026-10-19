//! `run()` and the `match` dispatch — moved verbatim from `cli/mod.rs` (CAD-984).

use super::*;

fn show_login_code(url: &str, code: &str, no_open: bool) -> Result<()> {
    eprintln!("Approve hosted Cadence access at: {url}\nCode: {code}");
    if !no_open {
        let program = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let mut opener = std::process::Command::new(program);
        opener
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let _ = cadence_agent::reaper::spawn(&mut opener);
    }
    Ok(())
}

pub(crate) fn run() -> Result<i32> {
    let argv: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    // CAD-888: `help operator|advanced|all` print the sections the
    // default root help leaves out; `help <verb>` stays clap's own.
    // CAD-1432: the master's help is bounded and refuses malformed or
    // disallowed forms with a usage error, so it runs before the others.
    if let Some(text) = help::master_help(&argv) {
        print!("{}", text?);
        return Ok(0);
    }
    // CAD-1432: the master never gets the unfiltered legacy help; its
    // leading-global or aliased help forms fall through to the Clap parser.
    if !cadence_agent::master::caller_is_master() {
        if let Some(text) = help::help_section(&argv) {
            print!("{text}");
            return Ok(0);
        }
    }
    let parsed = help::root_command(help::in_pane())
        .try_get_matches()
        .and_then(|m| {
            let typed = command_line_ids(&m);
            <Cli as clap::FromArgMatches>::from_arg_matches(&m).map(|mut cli| {
                cli.command_line = Some(typed);
                cli
            })
        });
    let cli = match parsed {
        Ok(cli) => cli,
        Err(e) => {
            let e = email_flag_error(&e).unwrap_or(e);
            // CAD-1432: any Clap help outcome for a master caller refuses
            // before printing. Clap's short-option clusters (`-hh`,
            // `-vh`) reach here as DisplayHelp, and a bare parent
            // (`cadence issue`) as DisplayHelpOnMissingArgumentOrSubcommand;
            // the exact-string guard sees neither. Scoped turns get
            // report-and-stop; the master gets the one bounded help route.
            if matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp
                    | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) && cadence_agent::master::caller_is_master()
            {
                let scoped = cadence_agent::master::in_app_conversation();
                return Err(if scoped {
                    Error::rejected(
                        "help is not available in a scoped chat turn — report the exact refusal and stop",
                    )
                } else {
                    Error::usage(
                        "--help and -h are not available — run cadence help <command-path> for the exact usage of a command you may run",
                    )
                });
            }
            // `--help` and `--version` print to stdout and exit 0 as before.
            // A bare parent command (`cadence issue`) prints its help as
            // plain text on stderr, exit 2, as before CAD-876: help stays
            // readable and JSON is for real usage errors.
            if !e.use_stderr()
                || e.kind() == clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            {
                e.exit();
            }
            // CAD-876: a usage error is a JSON error like every other one
            // (`main` prints it and exits 2).
            let text = e.to_string();
            let text = text.trim();
            return Err(Error::usage(text.strip_prefix("error: ").unwrap_or(text)));
        }
    };
    run_cli(cli)
}

/// Every argument id the command line typed, at the root and in each
/// subcommand (clap `value_source`), so a client-only `update` can refuse
/// native options even when they equal their defaults. Clap's own defaults
/// never count as typed.
fn command_line_ids(m: &clap::ArgMatches) -> Vec<String> {
    let typed = m
        .ids()
        .filter(|id| m.value_source(id.as_str()) == Some(clap::parser::ValueSource::CommandLine))
        .map(|id| id.as_str().to_string());
    let nested = m
        .subcommand()
        .map(|(_, sub)| command_line_ids(sub))
        .unwrap_or_default();
    typed.chain(nested).collect()
}

/// The `cadence self` payload for a mailbox with no actor: the inbound
/// backlog a consumer owes it (CAD-480). Extracted so a test can compare
/// its shape with the example in `skills/cadence/SKILL.md` (CAD-1514).
pub(super) fn self_inbox_payload(show: &Value, kind: &str) -> Value {
    json!({
        "alias": show["agent"]["alias"],
        "endpoint_kind": kind,
        "queued": show["queued"],
        "unread": show["inbox"]["queued"],
        "oldest_unread_age_secs": show["inbox"]["oldest_age_secs"],
    })
}

/// The `cadence self` payload for an actor: only the running turns
/// (CAD-879), never history. Extracted for the same reason as
/// [`self_inbox_payload`].
pub(super) fn self_actor_payload(alias: &str, show: &Value) -> Value {
    let running = show["messages"]
        .as_array()
        .map(|ms| {
            ms.iter()
                .filter(|m| m["state"].as_str() == Some("running"))
                .map(|m| {
                    json!({"id": m["id"], "turn_id": m["turn_id"],
                           "task": m["task_id"]})
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({"alias": alias, "running": running})
}

pub(super) fn run_cli(cli: Cli) -> Result<i32> {
    // Offline syntax discovery: no org, state, tracker, login, or daemon.
    if matches!(
        cli.command,
        Commands::Search { .. } | Commands::Schema { .. }
    ) {
        return discovery::run(&cli.command);
    }
    let client_only = client_update::client_only_mode()?;
    // CAD-1437: a valid client-only receipt selects the public updater before
    // any org, credential, tracker or state work. Native-only options refuse.
    if let Commands::Update {
        check,
        version,
        target,
        ..
    } = &cli.command
    {
        if client_only {
            return client_update::run(
                &cli,
                *check,
                version.as_deref(),
                target.release_source.as_deref(),
            );
        }
        if version.is_some() {
            return Err(client_update::client_only_required());
        }
    }
    // CAD-1425: the native daemon updater cannot prove its operator on
    // macOS. Refuse before org resolution, sandbox adoption or any state.
    if matches!(cli.command, Commands::Update { .. }) {
        client_update::native_platform_refusal()?;
    }
    // Offline custody never resolves daemon, org defaults or issuer credentials.
    if let Commands::Remote { action } = &cli.command {
        if cli.state_dir.is_some() {
            return Err(Error::rejected(
                "Offline results do not select daemon state; use --outbox-dir",
            ));
        }
        return remote_result::run(action);
    }
    // Remote credentials are independent of local daemon state and sandbox adoption.
    match &cli.command {
        Commands::Login {
            hint,
            issuer,
            org,
            slug,
            use_,
            token_stdin,
            no_open,
            auth_dir,
            web,
        } => {
            // `--token-stdin` is the legacy AgenticOS token path, not org login.
            let dir = cadence_agent::remote_auth::auth_dir(auth_dir.as_deref())?;
            if *token_stdin {
                let issuer = issuer
                    .as_deref()
                    .expect("clap requires --issuer with --token-stdin");
                let org = org
                    .as_deref()
                    .expect("clap requires --org with --token-stdin");
                return cadence_agent::remote_auth::login(issuer, org, &dir, true, *no_open);
            }
            let issuer = issuer
                .as_deref()
                .unwrap_or(cadence_agent::remote_enrollment::HOSTED_LOGIN_ISSUER);
            let grant = if *web {
                cadence_agent::remote_enrollment::login_portal_scoped(
                    issuer,
                    hint.as_deref().or(slug.as_deref()),
                    &dir,
                    |url, code| show_login_code(url, code, *no_open),
                    true,
                )?
            } else if let Some(org) = org.as_deref() {
                let slug = slug.as_deref().ok_or_else(|| {
                    Error::rejected("legacy login with --org needs --slug <org-slug>")
                })?;
                cadence_agent::remote_enrollment::login_browser(
                    issuer,
                    org,
                    slug,
                    &dir,
                    |url, code| show_login_code(url, code, *no_open),
                )?
            } else {
                cadence_agent::remote_enrollment::login_portal(
                    issuer,
                    hint.as_deref().or(slug.as_deref()),
                    &dir,
                    |url, code| show_login_code(url, code, *no_open),
                )?
            };
            // Record first: a refused registry write (changed endpoint, name
            // taken by `local`) must leave no credential behind.
            org::record_remote(&grant.slug, &grant.endpoint, &grant.organization_id, *use_)?;
            grant.save_credential(&dir)?;
            println!("Signed in to {} ({})", grant.slug, grant.endpoint);
            return Ok(0);
        }
        Commands::Auth { action } => {
            return match action {
                AuthAction::Status {
                    issuer,
                    org,
                    auth_dir,
                } => cadence_agent::remote_auth::status(
                    auth_dir.as_deref(),
                    issuer.as_deref(),
                    org.as_deref(),
                ),
                AuthAction::Logout { auth_dir, org } => {
                    if let Some(org) = org {
                        let dir = cadence_agent::remote_auth::auth_dir(auth_dir.as_deref())?;
                        match org::resolve(Some(org), None, None)? {
                            org::Resolved::Remote(target) => {
                                cadence_agent::remote_cli::logout(&dir, &target)
                            }
                            org::Resolved::Local(_) => Err(Error::rejected(
                                "auth logout --org requires a registered remote workspace",
                            )),
                        }
                    } else {
                        let dir = cadence_agent::remote_auth::auth_dir(auth_dir.as_deref())?;
                        cadence_agent::remote_auth::logout(&dir)
                    }
                }
            }
        }
        _ => {}
    }
    // CAD-1429: every `web` action is a hosted operation against one pinned
    // remote workspace. Resolve it once here; a local or unselected org
    // refuses before any request is built. It runs before the client-only
    // router too, so a client-only install reaches the web module rather than
    // the remote allowlist (which refuses web).
    if let Commands::Web { action } = &cli.command {
        let target = match org::resolve(cli.org.as_deref(), None, None)? {
            org::Resolved::Remote(target) => target,
            org::Resolved::Local(_) => {
                return Err(Error::rejected(
                    "cadence web needs a remote workspace: pass --org <org> for a logged-in workspace; nothing was sent",
                ))
            }
        };
        return web::run(action, &target, cli.wake_timeout);
    }
    // CAD-1425: a client-only installation runs local commands never; it
    // routes only through an actual remote org, before any local action.
    if client_only {
        if let Commands::Org { action } = cli.command {
            return client_update::client_org(action);
        }
        return client_update::route_client_only(cli);
    }
    // ADR 0007 T1: dispatch before any state-dir resolution or sandbox
    // adoption — under sudo those would resolve root's home and could
    // write beneath it. The agent-uid lane is its own host surface.
    if let Commands::AgentUid { action } = &cli.command {
        return match action {
            AgentUidAction::Provision {
                dry_run,
                helper,
                operator,
            } => cadence_agent::agent_uid::provision::cli(*dry_run, helper.clone(), operator),
            AgentUidAction::Doctor { json, operator } => {
                cadence_agent::agent_uid::audit::cli(*json, operator)
            }
            AgentUidAction::Runbook => cadence_agent::agent_uid::runbook::cli(),
        };
    }
    // CAD-647: trusted source development never resolves production state
    // or adopts its sandbox profile. The harness has no daemon/PM contract.
    if let Commands::App {
        action:
            app::AppAction::FrameKit {
                out,
                bundle,
                parent_origin,
            },
    } = &cli.command
    {
        return cadence_agent::app_preview_export::write_frame_kit(
            out,
            bundle.as_deref(),
            parent_origin,
        );
    }
    if let Commands::App {
        action: app::AppAction::Fixtures { bundle, json },
    } = &cli.command
    {
        return cadence_agent::app_preview_export::write_fixtures(bundle, *json);
    }
    if let Commands::App {
        action:
            app::AppAction::Dev {
                name,
                source,
                port,
                host,
                allow_host,
            },
    } = &cli.command
    {
        return app::run_dev(name, source, *port, host, allow_host);
    }
    // CAD-1270: `app check` is the offline byte validator — it resolves
    // no org or state dir, adopts no sandbox profile and never reaches
    // the daemon or a tracker write. Local bytes (or a built-in) only.
    if let Commands::App {
        action: app::AppAction::Check { source, json },
    } = &cli.command
    {
        return app::run_check(source, *json);
    }
    let tracker = std::env::var_os("CADENCE_PM_DIR")
        .map(|d| cadence_agent::home::guard_tracker(PathBuf::from(d)))
        .transpose()?;
    // CAD-1355 / CAD-1545: `update` and `dev` are local maintenance, so
    // they resolve through the shared local-maintenance selector — a saved
    // remote default never routes them, and an explicit remote
    // `--org`/`CADENCE_ORG` refuses with local-only guidance before any
    // transport, credentials or wake. `dev` also refuses a remote
    // `CADENCE_ORG` when a local pin is set.
    let resolved = if matches!(cli.command, Commands::Update { .. }) {
        org::resolve_update(cli.org.as_deref(), cli.state_dir.clone(), tracker)?
    } else if matches!(cli.command, Commands::Dev { .. }) {
        org::resolve_dev(cli.org.as_deref(), cli.state_dir.clone(), tracker)?
    } else {
        org::resolve(cli.org.as_deref(), cli.state_dir.clone(), tracker)?
    };
    let state_dir = match resolved {
        org::Resolved::Local(state_dir) => state_dir,
        org::Resolved::Remote(target) => {
            // CAD-1019 slice 2: a remote org never opens local state or
            // adopts a sandbox profile — the allowlisted verb goes over
            // HTTPS; everything else refuses inside `remote::run`.
            return remote::run(target, cli.command, cli.wake_timeout, cli.state_dir.clone());
        }
    };
    // CAD-310: a dev Cadence's state dir decides its profile and tracker,
    // not the caller's env. `dev` verbs (alias `sandbox`) resolve their own roots.
    if !matches!(cli.command, Commands::Dev { .. }) {
        cadence_agent::sandbox::adopt(&state_dir)?;
    }
    // CAD-615: the master retrying an approved command. The daemon
    // runs it and this process prints the output. No grant, or an
    // allowlisted command, falls through to the normal verb.
    if let Some(code) = permission_replay(&state_dir, &cli) {
        return Ok(code);
    }
    dispatch(state_dir, cli.command)
}

pub(crate) fn dispatch(state_dir: PathBuf, command: Commands) -> Result<i32> {
    match command {
        Commands::Remote { .. }
        | Commands::Login { .. }
        | Commands::Auth { .. }
        | Commands::Web { .. } => {
            unreachable!("auth or web dispatched before state-dir")
        }
        Commands::InspectPortable { source } => {
            print_json(&cadence_agent::backup::portable::inspect_portable(&source)?);
            Ok(0)
        }
        Commands::Doctor {
            host,
            json,
            reclaim_plan,
        } => doctor::run(state_dir, host, json, reclaim_plan),
        // Dispatched above, before the state dir resolved — the lane
        // must never run `adopt` or touch `~` under sudo.
        Commands::AgentUid { .. } => unreachable!("agent-uid dispatched before state-dir"),
        Commands::Setup {
            json,
            port,
            no_open,
        } => setup::run(state_dir, json, port, no_open),
        Commands::Daemon { action } => daemon::run(state_dir, action),
        Commands::Rollout { action } => rollout::run(state_dir, action),
        Commands::Staging { action } => staging::run(state_dir, action),
        Commands::Backup { dir, keep, reason } => backup::run(state_dir, dir, keep, reason),
        Commands::Export {
            out,
            portable,
            tracker,
            apps,
            snapshot_working_tree,
            redact_history_secrets,
        } => export::run(
            state_dir, out, portable, tracker, apps, snapshot_working_tree, redact_history_secrets,
        ),
        Commands::Restore {
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
            reconciliation,
        } => restore::run(restore::Options {
            state_dir,
            source,
            repos,
            force,
            portable,
            preflight,
            replace: restore::Replace {
                enabled: replace,
                controller_state,
                expected_package_digest,
                expected_destination_digest,
                expected_generation,
            },
            activate: restore::Activation {
                enabled: activate,
                reconciliation,
            },
        }),
        Commands::Agent { action } => agent::run(state_dir, action),
        Commands::Message { action } => message::run(state_dir, action),
        Commands::Devin {
            resume,
            detach,
            cwd,
            alias,
            role,
            team_role,
            instructions_file,
            worktree,
            bootstrap,
            no_bootstrap,
            auto_ready,
            agents_md,
            permission_mode,
            bypass,
            cloud,
            cloud_params,
        } => devin::run(
            state_dir,
            resume,
            detach,
            cwd,
            alias,
            role,
            team_role,
            instructions_file,
            worktree,
            bootstrap,
            no_bootstrap,
            auto_ready,
            agents_md,
            permission_mode,
            bypass,
            cloud,
            cloud_params,
        ),
        Commands::Codex {
            detach,
            cwd,
            alias,
            role,
            model,
            provider_default_model,
            team_role,
            effort,
            approval_policy,
            turn_idle_secs,
            turn_max_secs,
            sandbox,
            instructions_file,
            worktree,
            bootstrap,
            no_bootstrap,
            tui,
            agents_md,
        } => codex::run(
            state_dir,
            detach,
            cwd,
            alias,
            role,
            model,
            provider_default_model,
            team_role,
            effort,
            approval_policy,
            turn_idle_secs,
            turn_max_secs,
            sandbox,
            instructions_file,
            worktree,
            bootstrap,
            no_bootstrap,
            tui,
            agents_md,
        ),
        Commands::Claude {
            tui,
            resume,
            cwd,
            alias,
            role,
            model,
            provider_default_model,
            team_role,
            effort,
            permission_mode,
            allow,
            bypass,
            broker_approvals,
            permission_timeout_secs,
            turn_idle_secs,
            turn_max_secs,
            instructions_file,
            worktree,
            bootstrap,
            no_bootstrap,
            auto_ready,
            detach,
            agents_md,
        } => claude::run(
            state_dir,
            tui,
            resume,
            cwd,
            alias,
            role,
            model,
            provider_default_model,
            team_role,
            effort,
            permission_mode,
            allow,
            bypass,
            broker_approvals,
            permission_timeout_secs,
            turn_idle_secs,
            turn_max_secs,
            instructions_file,
            worktree,
            bootstrap,
            no_bootstrap,
            auto_ready,
            detach,
            agents_md,
        ),
        Commands::Cursor {
            resume,
            detach,
            cwd,
            alias,
            role,
            model,
            provider_default_model,
            team_role,
            permission_mode,
            bypass,
            instructions_file,
            worktree,
            bootstrap,
            no_bootstrap,
            auto_ready,
            agents_md,
        } => cursor::run(
            state_dir,
            resume,
            detach,
            cwd,
            alias,
            role,
            model,
            provider_default_model,
            team_role,
            permission_mode,
            bypass,
            instructions_file,
            worktree,
            bootstrap,
            no_bootstrap,
            auto_ready,
            agents_md,
        ),
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
        } => send::run(
            state_dir,
            // The ArgGroup guarantees exactly one of the two.
            alias.or(to).expect("clap target group"),
            text,
            file,
            message,
            reply_to,
            task,
            ready,
            force,
            nudge,
            steer,
        ),
        Commands::Dispatch {
            issue,
            to,
            note,
            name,
            base,
            repo,
            reply_to,
            summary,
            job,
            spec,
            no_lessons,
            force,
            take_over,
        } => dispatch::run(
            state_dir, issue, to, note, name, base, repo, reply_to, summary, job, spec, no_lessons,
            force, take_over,
        ),
        Commands::Join {
            group,
            provider,
            resume,
            tui,
            detach,
            cwd,
            alias,
            role,
            sandbox,
            instructions_file,
            worktree,
            no_bootstrap,
            auto_ready,
            agents_md,
            model,
            provider_default_model,
            team_role,
            effort,
            approval_policy,
            permission_mode,
            allow,
            bypass,
            turn_idle_secs,
            turn_max_secs,
            broker_approvals,
            permission_timeout_secs,
            cloud,
            cloud_params,
            confine,
            no_confine,
            profile,
        } => join::run(
            state_dir,
            group,
            provider,
            resume,
            tui,
            detach,
            cwd,
            alias,
            role,
            sandbox,
            instructions_file,
            worktree,
            no_bootstrap,
            auto_ready,
            agents_md,
            model,
            provider_default_model,
            team_role,
            effort,
            approval_policy,
            permission_mode,
            allow,
            bypass,
            turn_idle_secs,
            turn_max_secs,
            broker_approvals,
            permission_timeout_secs,
            cloud,
            cloud_params,
            confine,
            no_confine,
            profile,
        ),
        Commands::Attach { name, print } => attach::run(state_dir, name, print),
        Commands::Resume { group, all, detach } => resume::run(state_dir, group, all, detach),
        Commands::Stop { group } => stop::run(state_dir, group),
        Commands::Interrupt { alias, wait } => interrupt::run(state_dir, alias, wait),
        Commands::SelfInfo => {
            let alias = std::env::var("CADENCE_ALIAS").map_err(|_| {
                Error::rejected("CADENCE_ALIAS is not set — not inside a cadence-owned pane")
            })?;
            // CAD-879: only the running turns are read — no history.
            let show = client::rpc(&state_dir, "agent_show", self_show_params(&alias))?;
            // A mailbox has no running turn — report the inbound
            // backlog a consumer would drain instead.
            let (provider, kind) = (
                show["agent"]["provider"].as_str().unwrap_or_default(),
                show["agent"]["endpoint_kind"].as_str().unwrap_or_default(),
            );
            if !registry::has_actor(provider, kind) {
                // CAD-1514: the shape lives in `self_inbox_payload`, which
                // a test compares against the SKILL.md example.
                print_json(&self_inbox_payload(&show, kind));
                return Ok(0);
            }
            let payload = self_actor_payload(&alias, &show);
            // CAD-375: the daemon shows a running turn's token only to
            // the agent's own pane or endpoint. A `null` here means this
            // process is not in it — say so rather than print a report
            // instruction that cannot work.
            if payload["running"]
                .as_array()
                .is_some_and(|ms| ms.iter().any(|m| m["turn_id"].is_null()))
            {
                return Err(Error::rejected(format!(
                    "turn tokens for '{alias}' are shown only to that agent's own pane \
                     or managed endpoint, and this process does not descend from it — \
                     run `cadence self` inside the agent's pane (CAD-375)"
                )));
            }
            print_json(&payload);
            Ok(0)
        }
        Commands::Done {
            message,
            token,
            text,
            file,
            sha,
            report,
        } => {
            let (result, pending) =
                message::run_result(&state_dir, message, token, text, file, sha, report)?;
            print_json(&result);
            Ok(if pending { 2 } else { 0 })
        }
        Commands::Inbox {
            alias,
            action,
            peek,
            after,
            wait,
            follow,
            reader,
            exec,
            exec_retry_ms,
            exec_timeout_ms,
            exec_max_failures,
        } => inbox::run(
            state_dir,
            alias,
            action,
            peek,
            after,
            wait,
            follow,
            reader,
            exec,
            exec_retry_ms,
            exec_timeout_ms,
            exec_max_failures,
        ),
        Commands::Skill { action } => skill::run(state_dir, action),
        Commands::Events {
            alias,
            job,
            after,
            wait,
            follow,
        } => events::run(state_dir, alias, job, after, wait, follow),
        Commands::Job { action } => job::run(state_dir, action),
        Commands::Thread { action } => thread::run(state_dir, action),
        Commands::Topic { action } => topic::run(state_dir, action),
        Commands::Monitor { action } => monitor::run(state_dir, action),
        Commands::Org { action } => org::run(action),
        Commands::Issue { action } => issue::run(state_dir, action),
        Commands::Connection { action } => connection::run(state_dir, action),
        Commands::Platform { action } => platform::run(state_dir, action),
        Commands::Idea { action } => idea::run(&state_dir, action),
        Commands::Plan { action } => plan::run(state_dir, action),
        Commands::Workflow { action } => workflow::run(state_dir, action),
        Commands::App { action } => app::run(state_dir, action),
        Commands::Project { action } => project::run(state_dir, action),
        Commands::Milestone { action } => milestone::run(state_dir, action),
        Commands::Master { action } => master::run(state_dir, action),
        Commands::Delivery { action } => delivery::run(state_dir, action),
        Commands::Report {
            kind,
            project,
            issue,
            text,
            file,
            priority,
            action,
        } => report::run(
            state_dir, kind, project, issue, text, file, priority, action,
        ),
        Commands::Intake { action } => intake::run(state_dir, action),
        Commands::Memory { action } => memory::run(state_dir, action),
        Commands::Wiki { action } => wiki::run(state_dir, action),
        Commands::Secret { action } => secret::run(state_dir, action),
        Commands::Template { action } => business_templates::run_template(state_dir, action),
        Commands::Document { action } => business_templates::run_document(state_dir, action),
        Commands::Ui { action } => ui::run(state_dir, action),
        Commands::Status {
            group,
            all,
            json,
            watch,
        } => status::run(state_dir, group, all, json, watch),
        Commands::Test { action } => test_cmd::run(&state_dir, &action),
        Commands::BuildSlot { action } => build_slot::run(state_dir, action),
        Commands::Review {
            action,
            pr,
            repo,
            full,
            no_full,
            no_suite_lock,
            stress,
            keep,
            json,
        } => match action {
            Some(ReviewAction::Flakes { test }) => {
                cadence_agent::review::print_flakes(state_dir, test).map(|()| 0)
            }
            None => review::run(
                state_dir,
                pr.expect("clap requires PR without a subcommand"),
                repo,
                full,
                no_full,
                no_suite_lock,
                stress,
                keep,
                json,
            ),
        },
        Commands::Session { action } => session::run(state_dir, action),
        Commands::Audit {
            action,
            since,
            class,
            project,
            json,
            limit,
            repo,
            notes_dir,
            merge_report,
        } => audit::run(
            state_dir,
            action,
            since,
            class,
            project,
            json,
            limit,
            repo,
            notes_dir,
            merge_report,
        ),
        Commands::Overview {
            json,
            watch,
            project,
            group,
        } => overview::run(state_dir, json, watch, project, group),
        Commands::Upgrade {
            sha,
            latest_main,
            dry_run,
            restart,
            as_identity,
            allow_unattested,
            repo,
            link,
            releases_dir,
            release_source,
        } => upgrade::run(
            state_dir,
            sha,
            latest_main,
            dry_run,
            restart,
            as_identity,
            allow_unattested,
            repo,
            link,
            releases_dir,
            release_source,
        ),
        Commands::Update {
            action,
            version: _,
            check,
            rollback,
            drain,
            now,
            keep,
            backup_dir,
            json,
            progress,
            to,
            target,
        } => update::run(
            state_dir, action, check, rollback, drain, now, keep, backup_dir, json, progress, to,
            target,
        ),
        Commands::Dev { action } => sandbox::run(state_dir, action),
        Commands::Confine {
            read,
            write,
            command,
        } => confine::run(state_dir, read, write, command),
        Commands::McpPermission { timeout_secs } => mcp_permission::run(state_dir, timeout_secs),
        Commands::McpAgent => mcp_agent::run(state_dir),
        Commands::Attachment { action } => attachment::run(state_dir, action),
        Commands::LocalAgent { .. } => Err(Error::usage(
            "local-agent coordinates a hosted org with this machine. Select that org, or switch back from `--org local`. Nothing was claimed.",
        )),
        Commands::Search { .. } | Commands::Schema { .. } => unreachable!(
            "commands and schema return before state resolution"
        ),
    }
}
