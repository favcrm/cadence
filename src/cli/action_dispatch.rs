//! `run()` and the `match` dispatch — moved verbatim from `cli/mod.rs` (CAD-984).
//!
//! CAD-1654: the command families live in `action_dispatch/<family>.rs`.
//! `dispatch` is the router; it lists every `Commands` variant so a new
//! variant fails compilation until it is assigned to one family.

use super::*;

mod admin;
mod agents;
mod issues;
mod providers;

fn show_login_code(url: &str, code: &str, no_open: bool) -> Result<()> {
    eprintln!("Approve hosted Cadence access at: {url}\nCode: {code}");
    open_login_url(url, no_open);
    Ok(())
}

fn show_register_code(url: &str, code: &str, no_open: bool) -> Result<()> {
    eprintln!("Complete registration and approve this CLI at: {url}\nDevice code: {code}");
    open_login_url(url, no_open);
    Ok(())
}

fn report_register_error(error: &Error, json: bool) {
    if !json {
        return;
    }
    println!(
        "{}",
        serde_json::json!({
            "registered": false,
            "authenticated": false,
            "code": error.code(),
            "message": error.to_string()
        })
    );
}

/// Open one server-selected browser URL through the host opener. The URL
/// carries only a selector (a login nonce in its fragment, or a metadata
/// request id); a credential, token or key never rides it.
pub(super) fn open_login_url(url: &str, no_open: bool) {
    if no_open {
        return;
    }
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
        // `ids()` reports propagated global args (e.g. `settings agents`
        // `--grant`) on nested subcommand matches too, but `value_source`
        // panics on an id the subcommand never declared. `try_contains_id`
        // verifies the declaration first; only then is `value_source` safe.
        .filter(|id| matches!(m.try_contains_id(id.as_str()), Ok(true)))
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
        Commands::Register {
            name,
            slug,
            issuer,
            use_,
            no_open,
            json,
            auth_dir,
        } => {
            let dir = cadence_agent::remote_auth::auth_dir(auth_dir.as_deref())?;
            let issuer = issuer
                .as_deref()
                .unwrap_or(cadence_agent::remote_enrollment::HOSTED_LOGIN_ISSUER);
            let grant = cadence_agent::remote_enrollment::register_portal(
                issuer,
                name.as_deref(),
                slug.as_deref(),
                &dir,
                |url, code| show_register_code(url, code, *no_open),
            )
            .inspect_err(|error| report_register_error(error, *json))?;
            org::record_remote(
                &grant.slug,
                &grant.endpoint,
                &grant.organization_id,
                *use_,
                || grant.save_credential(&dir),
            )
            .inspect_err(|error| report_register_error(error, *json))?;
            if *json {
                println!(
                    "{}",
                    serde_json::json!({
                        "registered": true,
                        "authenticated": true,
                        "slug": grant.slug,
                        "endpoint": grant.endpoint,
                        "organization_id": grant.organization_id,
                        "expires_at": grant.expires_at,
                        "credential_stored": true
                    })
                );
            } else {
                println!(
                    "Registered and signed in to {} ({})",
                    grant.slug, grant.endpoint
                );
            }
            return Ok(0);
        }
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
            settings,
            cloud_agent,
            audit_approve,
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
            let grant = if *web || *settings || *cloud_agent || *audit_approve {
                cadence_agent::remote_enrollment::login_portal_scoped(
                    issuer,
                    hint.as_deref().or(slug.as_deref()),
                    &dir,
                    |url, code| show_login_code(url, code, *no_open),
                    *web,
                    *settings,
                    *cloud_agent,
                    *audit_approve,
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
            // Registry replacement and credential save are one recovery
            // unit: a failed credential write restores the old binding, so a
            // replay never leaves a new binding pointing at an old token.
            org::record_remote(
                &grant.slug,
                &grant.endpoint,
                &grant.organization_id,
                *use_,
                || grant.save_credential(&dir),
            )?;
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
    // CAD-1946: `audit verdicts` only parses the notes directory, so it
    // runs locally whatever org is selected — before org resolution, the
    // client-only router and any daemon or state dir.
    if let Commands::Audit {
        action:
            Some(audit::AuditAction::Verdicts {
                issue,
                pr,
                head,
                notes_dir,
            }),
        ..
    } = &cli.command
    {
        return audit::verdicts(issue, *pr, head, notes_dir.as_deref());
    }
    // Hosted metadata never needs local sandbox adoption, permission replay,
    // daemon startup, or the generic native/container verb map. Refuse an
    // absent/local hosted selection before any of those paths, including on
    // client-only installations.
    if matches!(
        &cli.command,
        Commands::Agent {
            action: AgentAction::Directory { .. } | AgentAction::Notify { .. },
        } | Commands::LocalAgent {
            action: local_agent::LocalAgentAction::Directory { .. }
                | local_agent::LocalAgentAction::Notify { .. },
        }
    ) {
        let target = match org::resolve(cli.org.as_deref(), cli.state_dir.clone(), None)? {
            org::Resolved::Remote(target) => target,
            org::Resolved::Local(_) => {
                return Err(Error::usage(
                    "agent directory/notify require a selected hosted org; pass --org <logged-in-workspace>. Nothing was sent and no daemon was started",
                ));
            }
        };
        return remote::run(target, cli.command, cli.wake_timeout, cli.state_dir);
    }
    // Published app lifecycle is hosted-only and must refuse before local
    // state/daemon startup, including on a client-only installation.
    if matches!(
        &cli.command,
        Commands::App {
            action: app::AppAction::Catalog {
                action: app::CatalogAction::ReleaseCheck { .. }
                    | app::CatalogAction::ReleaseInstall { .. }
                    | app::CatalogAction::ReleaseUpdate { .. }
                    | app::CatalogAction::ReleaseRevoke { .. }
                    | app::CatalogAction::Operation { .. }
                    | app::CatalogAction::Pending { .. }
                    | app::CatalogAction::Reconcile { .. }
            }
        }
    ) {
        let target = match org::resolve(cli.org.as_deref(), cli.state_dir.clone(), None)? {
            org::Resolved::Remote(target) => target,
            org::Resolved::Local(_) => return Err(Error::usage(
                "published app lifecycle requires --org <logged-in-workspace>; no local daemon or hosted request was started",
            )),
        };
        return remote::run(target, cli.command, cli.wake_timeout, cli.state_dir);
    }
    // Hosted human team administration. A managed agent is refused before
    // org resolution, so a saved human login is never loaded. A local org
    // is a usage error and does not open a daemon. A remote org uses the
    // existing authorize/call path.
    if matches!(cli.command, Commands::Team { .. }) {
        if std::env::var_os("CADENCE_ALIAS").is_some() {
            return Err(Error::rejected(
                "a managed agent cannot administer the human team. A saved login is not team authority. Nothing was sent",
            ));
        }
        let tracker = std::env::var_os("CADENCE_PM_DIR")
            .map(|d| cadence_agent::home::guard_tracker(PathBuf::from(d)))
            .transpose()?;
        let target = match org::resolve(cli.org.as_deref(), cli.state_dir.clone(), tracker)? {
            org::Resolved::Remote(target) => target,
            org::Resolved::Local(_) => {
                return Err(Error::usage(
                    "team requires a hosted org: pass --org <org> for a logged-in workspace, or switch back from `--org local`. Nothing was sent",
                ));
            }
        };
        return remote::run(target, cli.command, cli.wake_timeout, cli.state_dir);
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
        let local_state = match cli.state_dir.clone() {
            Some(dir) => dir,
            None => cadence_agent::client::state_dir()?,
        };
        return web::run(&local_state, action, &target, cli.wake_timeout);
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
                integrity_only,
            },
    } = &cli.command
    {
        return cadence_agent::app_preview_export::write_frame_kit(
            out,
            bundle.as_deref(),
            parent_origin,
            *integrity_only,
        );
    }
    if let Commands::App {
        action:
            app::AppAction::Fixtures {
                bundle,
                json,
                integrity_only,
            },
    } = &cli.command
    {
        return cadence_agent::app_preview_export::write_fixtures(bundle, *json, *integrity_only);
    }
    if let Commands::App {
        action:
            app::AppAction::Dev {
                name,
                source,
                package,
                port,
                host,
                allow_host,
            },
    } = &cli.command
    {
        return app::run_dev(
            name.as_deref(),
            source.as_deref(),
            package.as_deref(),
            *port,
            host,
            allow_host,
        );
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
    if let Commands::App {
        action: app::AppAction::Inspect { source, json },
    } = &cli.command
    {
        return app::run_inspect(source, *json);
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
        | Commands::Register { .. }
        | Commands::Login { .. }
        | Commands::Auth { .. }
        | Commands::Web { .. } => {
            unreachable!("auth or web dispatched before state-dir")
        }
        // Dispatched above, before the state dir resolved — the lane
        // must never run `adopt` or touch `~` under sudo.
        Commands::AgentUid { .. } => unreachable!("agent-uid dispatched before state-dir"),
        Commands::Search { .. } | Commands::Schema { .. } => {
            unreachable!("commands and schema return before state resolution")
        }
        Commands::InspectPortable { .. }
        | Commands::Doctor { .. }
        | Commands::Setup { .. }
        | Commands::Daemon { .. }
        | Commands::Rollout { .. }
        | Commands::Staging { .. }
        | Commands::Backup { .. }
        | Commands::Export { .. }
        | Commands::Restore { .. }
        | Commands::Upgrade { .. }
        | Commands::Update { .. }
        | Commands::Test { .. }
        | Commands::BuildSlot { .. }
        | Commands::Confine { .. }
        | Commands::Dev { .. } => admin::dispatch(state_dir, command),
        Commands::Agent { .. }
        | Commands::Message { .. }
        | Commands::Send { .. }
        | Commands::Dispatch { .. }
        | Commands::Join { .. }
        | Commands::Attach { .. }
        | Commands::Resume { .. }
        | Commands::Stop { .. }
        | Commands::Interrupt { .. }
        | Commands::SelfInfo
        | Commands::Done { .. }
        | Commands::Inbox { .. }
        | Commands::Skill { .. }
        | Commands::Events { .. }
        | Commands::Job { .. }
        | Commands::Thread { .. }
        | Commands::Topic { .. }
        | Commands::Monitor { .. }
        | Commands::Status { .. }
        | Commands::LocalAgent { .. }
        | Commands::Team { .. }
        | Commands::McpPermission { .. }
        | Commands::McpAgent
        | Commands::Artifact { .. }
        | Commands::Attachment { .. } => agents::dispatch(state_dir, command),
        Commands::Devin { .. }
        | Commands::Codex { .. }
        | Commands::Claude { .. }
        | Commands::Cursor { .. } => providers::dispatch(state_dir, command),
        Commands::Org { .. }
        | Commands::Issue { .. }
        | Commands::Connection { .. }
        | Commands::Platform { .. }
        | Commands::Idea { .. }
        | Commands::Plan { .. }
        | Commands::Workflow { .. }
        | Commands::App { .. }
        | Commands::Project { .. }
        | Commands::Milestone { .. }
        | Commands::Master { .. }
        | Commands::Delivery { .. }
        | Commands::Report { .. }
        | Commands::Intake { .. }
        | Commands::Memory { .. }
        | Commands::Wiki { .. }
        | Commands::Secret { .. }
        | Commands::Template { .. }
        | Commands::Document { .. }
        | Commands::Ui { .. }
        | Commands::Settings { .. }
        | Commands::MyWork
        | Commands::Review { .. }
        | Commands::Session { .. }
        | Commands::Audit { .. }
        | Commands::Overview { .. } => issues::dispatch(state_dir, command),
    }
}
