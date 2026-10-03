//! `run()` and the `match` dispatch — moved verbatim from `cli/mod.rs` (CAD-984).

use super::*;

pub(crate) fn run() -> Result<i32> {
    let argv: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    // CAD-888: `help operator|advanced|all` print the sections the
    // default root help leaves out; `help <verb>` stays clap's own.
    if let Some(text) = help::help_section(&argv) {
        print!("{text}");
        return Ok(0);
    }
    let parsed = help::root_command(help::in_pane())
        .try_get_matches()
        .and_then(|m| <Cli as clap::FromArgMatches>::from_arg_matches(&m));
    let cli = match parsed {
        Ok(cli) => cli,
        Err(e) => {
            let e = email_flag_error(&e).unwrap_or(e);
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
            issuer,
            org,
            slug,
            use_,
            token_stdin,
            no_open,
            auth_dir,
        } => {
            // `--token-stdin` is the legacy AgenticOS token path, not org login.
            let dir = cadence_agent::remote_auth::auth_dir(auth_dir.as_deref())?;
            if *token_stdin {
                return cadence_agent::remote_auth::login(issuer, org, &dir, true, *no_open);
            }
            let slug = slug
                .as_deref()
                .ok_or_else(|| Error::rejected("login needs --slug <org-slug>"))?;
            let grant = cadence_agent::remote_enrollment::login_browser(
                issuer,
                org,
                slug,
                &dir,
                |url, code| {
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
                },
            )?;
            // Record first: a refused registry write (changed endpoint, name
            // taken by `local`) must leave no credential behind.
            let view =
                org::record_remote(&grant.slug, &grant.endpoint, &grant.organization_id, *use_)?;
            grant.save_credential(&dir)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&view).unwrap_or_default()
            );
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
                AuthAction::Logout { auth_dir } => {
                    let dir = cadence_agent::remote_auth::auth_dir(auth_dir.as_deref())?;
                    cadence_agent::remote_auth::logout(&dir)
                }
            }
        }
        _ => {}
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
    let resolved = org::resolve(
        cli.org.as_deref(),
        cli.state_dir.clone(),
        std::env::var_os("CADENCE_PM_DIR").map(PathBuf::from),
    )?;
    let state_dir = match resolved {
        org::Resolved::Local(state_dir) => state_dir,
        org::Resolved::Remote(target) => {
            // CAD-1019 slice 2: a remote org never opens local state or
            // adopts a sandbox profile — the allowlisted verb goes over
            // HTTPS; everything else refuses inside `remote::run`.
            return remote::run(target, cli.command, cli.wake_timeout);
        }
    };
    // CAD-310: a sandbox's state dir decides its profile and tracker,
    // not the caller's env. `sandbox` verbs resolve their own roots.
    if !matches!(cli.command, Commands::Sandbox { .. }) {
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
        Commands::Remote { .. } | Commands::Login { .. } | Commands::Auth { .. } => {
            unreachable!("auth dispatched before state-dir")
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
        Commands::Export { out } => export::run(state_dir, out),
        Commands::Restore {
            source,
            repos,
            force,
        } => restore::run(state_dir, source, repos, force),
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
                print_json(&json!({
                    "alias": show["agent"]["alias"],
                    "endpoint_kind": kind,
                    "queued": show["queued"],
                    // CAD-480: the unread backlog and its age — what a
                    // consumer owes this mailbox.
                    "unread": show["inbox"]["queued"],
                    "oldest_unread_age_secs": show["inbox"]["oldest_age_secs"],
                }));
                return Ok(0);
            }
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
            // CAD-375: the daemon shows a running turn's token only to
            // the agent's own pane or endpoint. A `null` here means this
            // process is not in it — say so rather than print a report
            // instruction that cannot work.
            if running.iter().any(|m| m["turn_id"].is_null()) {
                return Err(Error::rejected(format!(
                    "turn tokens for '{alias}' are shown only to that agent's own pane \
                     or managed endpoint, and this process does not descend from it — \
                     run `cadence self` inside the agent's pane (CAD-375)"
                )));
            }
            print_json(&json!({"alias": alias, "running": running}));
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
        ),
        Commands::Update {
            action,
            check,
            rollback,
            drain,
            now,
            keep,
            backup_dir,
            json,
            progress,
            target,
        } => update::run(
            state_dir, action, check, rollback, drain, now, keep, backup_dir, json, progress,
            target,
        ),
        Commands::Sandbox { action } => sandbox::run(state_dir, action),
        Commands::Confine {
            read,
            write,
            command,
        } => confine::run(state_dir, read, write, command),
        Commands::McpPermission { timeout_secs } => mcp_permission::run(state_dir, timeout_secs),
        Commands::McpAgent => mcp_agent::run(state_dir),
    }
}
