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
mod update_notice;

pub(super) fn show_login_code(url: &str, code: &str, no_open: bool) -> Result<()> {
    cadence_agent::output::eprint_format_line(format_args!(
        "Approve hosted Cadence access at: {url}\nCode: {code}"
    ));
    open_login_url(url, no_open);
    Ok(())
}

pub(super) fn show_register_code(url: &str, code: &str, no_open: bool) -> Result<()> {
    cadence_agent::output::eprint_format_line(format_args!(
        "Complete registration and approve this CLI at: {url}\nDevice code: {code}"
    ));
    open_login_url(url, no_open);
    Ok(())
}
pub(super) fn issued_session_persistence_error(workspace: &str, dir: &Path, error: Error) -> Error {
    Error::unknown(format!(
        "a remote session was issued for workspace {workspace}, but local registration \
         could not be confirmed in {}: {error}. Inspect that directory and \
         reconcile with the issuer owner before requesting new consent",
        dir.display()
    ))
}

/// CAD-2213: the opt-in scopes `--full-access` turns on in one consent.
pub(super) fn login_scopes(
    web: bool,
    settings: bool,
    cloud_agent: bool,
    audit_approve: bool,
    full_access: bool,
) -> (bool, bool, bool, bool) {
    (
        web || full_access,
        settings || full_access,
        cloud_agent || full_access,
        audit_approve || full_access,
    )
}

/// The granted set, printed after login so a narrowed consent is visible
/// immediately rather than surfacing later as an opaque 401.
/// The scopes `--full-access` asks for, so login can name any the issuer
/// withheld instead of leaving the gap to surface at the next verb
/// (CAD-2213).
pub(super) const FULL_ACCESS_SCOPES: &[&str] = &[
    "web.read",
    "web.prepare",
    "settings.read",
    "settings.write",
    "bridge.enroll",
    "cloud_agent.read",
    "cloud_agent.write",
    "audit.approve",
];

/// Requested-but-not-granted scopes, in the order above.
pub(super) fn missing_full_access(capabilities: &[String]) -> Vec<&'static str> {
    FULL_ACCESS_SCOPES
        .iter()
        .copied()
        .filter(|scope| !capabilities.iter().any(|have| have == scope))
        .collect()
}

/// The scopes a login asked for, in a stable order, recorded beside the
/// granted set (CAD-2213).
pub(super) fn requested_scopes(
    web: bool,
    settings: bool,
    cloud_agent: bool,
    audit_approve: bool,
) -> Vec<String> {
    let mut scopes = Vec::new();
    if web {
        scopes.extend(["web.read", "web.prepare"]);
    }
    if settings {
        scopes.extend(["settings.read", "settings.write"]);
    }
    if cloud_agent {
        scopes.extend(["bridge.enroll", "cloud_agent.read", "cloud_agent.write"]);
    }
    if audit_approve {
        scopes.push("audit.approve");
    }
    scopes.into_iter().map(str::to_string).collect()
}

pub(super) fn login_granted_line(capabilities: &[String]) -> String {
    if capabilities.is_empty() {
        "none recorded".to_string()
    } else {
        capabilities.join(", ")
    }
}

fn show_workspace_handover(slug: &str, stored: &Value, explicit_auth_dir: bool) {
    let selected = stored.get("selected").and_then(Value::as_bool);
    match selected {
        Some(true) => println!("Saved default: {slug}"),
        Some(false) => println!("Saved default: unchanged"),
        None => println!("Saved default: unavailable"),
    }

    if std::env::var_os("CADENCE_ALIAS").is_some()
        || std::env::var_os("CADENCE_GRANT_TOKEN").is_some()
    {
        println!("Use the workspace bound to your agent assignment.");
        return;
    }
    if explicit_auth_dir {
        println!(
            "This login used --auth-dir. Hosted commands use the default credential directory; \
             if they differ, sign in without --auth-dir before using those commands."
        );
        if org::inherited_local_pin() {
            println!(
                "This shell is also bound to local Cadence; use a shell without that binding \
                 for hosted commands."
            );
        }
        return;
    }

    if org::inherited_local_pin() {
        println!(
            "This shell is bound to local Cadence. In a shell without that binding, run:\n  \
             cadence --org {slug} status"
        );
    } else {
        println!("Next: cadence --org {slug} status");
    }
    if selected == Some(false) {
        println!("To make it the saved default: cadence org switch {slug}");
    }
}

fn report_register_error(error: &Error, json: bool) {
    if !json {
        return;
    }
    print_json(&serde_json::json!({
        "registered": (error.kind() != "unknown").then_some(false),
        "authenticated": false,
        "credential_stored": false,
        "kind": error.kind(),
        "code": error.code(),
        "message": error.to_string()
    }));
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
    if argv.len() == 2 && argv[1] == update_notice::REFRESH_ARG {
        update_notice::refresh();
        return Ok(0);
    }
    // CAD-888: `help operator|advanced|all` print the sections the
    // default root help leaves out; `help <verb>` stays clap's own.
    // CAD-1432: the master's help is bounded and refuses malformed or
    // disallowed forms with a usage error, so it runs before the others.
    if let Some(text) = help::master_help(&argv) {
        cadence_agent::output::print_text(&text?);
        return Ok(0);
    }
    // CAD-1432: the master never gets the unfiltered legacy help; its
    // leading-global or aliased help forms fall through to the Clap parser.
    if !cadence_agent::master::caller_is_master() {
        if let Some(text) = help::help_section(&argv) {
            cadence_agent::output::print_text(&text);
            return Ok(0);
        }
    }
    let parsed = help::root_command(help::in_pane())
        .try_get_matches()
        .and_then(|m| {
            let path = command_path(&m);
            let typed = command_line_ids(&m);
            <Cli as clap::FromArgMatches>::from_arg_matches(&m).map(|mut cli| {
                cli.command_line = Some(typed);
                cli.command_path = path;
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
    let notice = update_notice::eligible(&cli);
    let result = run_cli(cli);
    if notice && result.as_ref().is_ok_and(|code| *code == 0) {
        update_notice::after_success();
    }
    result
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

/// Capture only Clap-validated subcommand names, never argument values.
pub(super) fn command_path(matches: &clap::ArgMatches) -> Option<String> {
    let (name, mut nested) = matches.subcommand()?;
    let mut path = String::from(name);
    while let Some((name, next)) = nested.subcommand() {
        path.push(' ');
        path.push_str(name);
        nested = next;
    }
    Some(path)
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

/// The inherited tracker pin, `CADENCE_PM_DIR`, guarded exactly as the
/// daemon's `provider_env` sets it. Passed to `org::resolve`/`org::route`
/// as their `tracker` pin; `None` when this process inherited no tracker.
fn tracker_pin() -> Result<Option<PathBuf>> {
    std::env::var_os("CADENCE_PM_DIR")
        .map(|d| cadence_agent::home::guard_tracker(PathBuf::from(d)))
        .transpose()
}

pub(super) fn run_cli(cli: Cli) -> Result<i32> {
    // CAD-1922: the home resolver treats `--state-dir` at a legacy root as
    // the same evidence as an exported `CADENCE_STATE_DIR`; record it before
    // anything resolves a path.
    cadence_agent::home::note_state_dir_flag(cli.state_dir.as_deref());
    // Offline syntax discovery: no org, state, tracker, login, or daemon.
    if matches!(
        cli.command,
        Commands::Search { .. } | Commands::Schema { .. }
    ) {
        return discovery::run(&cli.command);
    }
    let client_only = client_update::client_only_mode()?;
    // CAD-2111: plain doctor on a hosted org checks that connection from this
    // machine, on any installation, before any local state exists. `route`
    // reads the registry without a lock or a state dir, and its refusals (an
    // unknown org, a pin conflict) stand: there is no local fallback. A local
    // selection falls through to the environment probe unchanged.
    if let Commands::Doctor {
        host: false,
        json,
        offline,
        ..
    } = &cli.command
    {
        if let org::Routed::Remote(target) =
            org::route(cli.org.as_deref(), cli.state_dir.clone(), tracker_pin()?)?
        {
            return Ok(doctor::run_remote(
                &target,
                *json,
                *offline,
                cli.org.is_some(),
                client_only,
            ));
        }
    }
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
            if *use_ {
                org::refuse_managed_default_change()
                    .inspect_err(|error| report_register_error(error, *json))?;
            }
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
            let stored = org::record_remote(
                &grant.slug,
                &grant.endpoint,
                &grant.organization_id,
                *use_,
                || grant.save_credential(&dir),
            )
            .map_err(|error| issued_session_persistence_error(&grant.organization_id, &dir, error))
            .inspect_err(|error| report_register_error(error, *json))?;
            if *json {
                print_json(&serde_json::json!({
                    "registered": true,
                    "authenticated": true,
                    "slug": grant.slug,
                    "endpoint": grant.endpoint,
                    "organization_id": grant.organization_id,
                    "expires_at": grant.expires_at,
                    "credential_stored": true
                }));
            } else {
                cadence_agent::output::print_format_line(format_args!(
                    "Registered and signed in to {} ({})",
                    grant.slug, grant.endpoint
                ));
                show_workspace_handover(&grant.slug, &stored, auth_dir.is_some());
            }
            return Ok(0);
        }
        Commands::Connect { action } => {
            let commands::ConnectAction::Github {
                issuer,
                no_open,
                wait,
            } = action;
            let issuer = issuer
                .as_deref()
                .unwrap_or(cadence_agent::remote_enrollment::HOSTED_LOGIN_ISSUER);
            // The saved login is what binds the code to the workspace this CLI
            // already belongs to. Without one the flow stays anonymous and the
            // owner chooses explicitly in the browser.
            let token = org::saved_login_token(None)?;
            let poll = cadence_agent::github_connect::connect(
                issuer,
                token.as_deref(),
                std::time::Duration::from_secs(*wait),
                |url, code| {
                    let _ = show_login_code(url, code, *no_open);
                },
            )?;
            let line = cadence_agent::github_connect::outcome_line(&poll)
                .unwrap_or_else(|| "The connection did not settle in time.".to_string());
            // The outcome line precedes a possible exit 1: a closed stderr
            // must not panic the verdict into success.
            eprint_line(&line);
            return Ok(match poll {
                cadence_agent::github_connect::Poll::Connected(_) => 0,
                _ => 1,
            });
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
            full_access,
        } => {
            if *use_ && !*token_stdin {
                org::refuse_managed_default_change()?;
            }
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
            // CAD-2213: `--full-access` is the union of the opt-in scopes, so
            // one consent can approve, change settings and spend a cloud
            // agent instead of four logins discovered one failure at a time.
            let (web, settings, cloud_agent, audit_approve) =
                login_scopes(*web, *settings, *cloud_agent, *audit_approve, *full_access);
            let mut grant = if web || settings || cloud_agent || audit_approve {
                cadence_agent::remote_enrollment::login_portal_scoped(
                    issuer,
                    hint.as_deref().or(slug.as_deref()),
                    &dir,
                    |url, code| show_login_code(url, code, *no_open),
                    web,
                    settings,
                    cloud_agent,
                    audit_approve,
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
            grant.requested = requested_scopes(web, settings, cloud_agent, audit_approve);
            // Registry replacement and credential save are one recovery
            // unit: a failed credential write restores the old binding, so a
            // replay never leaves a new binding pointing at an old token.
            let stored = org::record_remote(
                &grant.slug,
                &grant.endpoint,
                &grant.organization_id,
                *use_,
                || grant.save_credential(&dir),
            )
            .map_err(|error| {
                issued_session_persistence_error(&grant.organization_id, &dir, error)
            })?;
            cadence_agent::output::print_format_line(format_args!(
                "Signed in to {} ({}); granted: {}",
                grant.slug,
                grant.endpoint,
                login_granted_line(grant.capabilities())
            ));
            if *full_access {
                let missing = missing_full_access(grant.capabilities());
                if !missing.is_empty() {
                    eprintln!(
                        "warning: requested {} and the issuer withheld {} — re-run \
                         `cadence login --full-access` after the workspace grants them",
                        FULL_ACCESS_SCOPES.join(", "),
                        missing.join(", ")
                    );
                }
            } else if !web && !settings && !cloud_agent && !audit_approve && org.is_none() {
                // Only a plain portal login: a scoped or legacy login already
                // asked for something specific, and `--full-access` conflicts
                // with the legacy `--org` form.
                eprintln!(
                    "note: this login requested board access only; --full-access also requests \
                     {} (required before approve, settings, web and cloud-agent verbs)",
                    FULL_ACCESS_SCOPES.join(", ")
                );
            }
            show_workspace_handover(&grant.slug, &stored, auth_dir.is_some());
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
    // CAD-1946 / CAD-1981: `audit verdicts` routes through the shared selector
    // as every other verb does, so this invocation's `--org`/`CADENCE_ORG`
    // selects the destination here too — the registry's selected row alone
    // cannot decide it, and an unknown or pinned selector must refuse rather
    // than read local notes. `org::route` answers the arm only, reading the
    // registry without its lock and materializing no local state dir: a local
    // selection therefore parses the notes directory offline, with no daemon,
    // no state dir and no `HOME`. A hosted selection answers through the org's
    // own read: a hosted org that cannot answer must refuse, never fall back to
    // whatever notes happen to be on this machine — that fallback answers an
    // empty list and reads as "no verdicts were filed".
    let verdict_args = match &cli.command {
        Commands::Audit {
            action:
                Some(audit::AuditAction::Verdicts {
                    issue,
                    pr,
                    head,
                    notes_dir,
                    repo,
                    project,
                    body,
                }),
            ..
        } => Some((
            issue.clone(),
            *pr,
            head.clone(),
            repo.clone(),
            project.clone(),
            notes_dir.clone(),
            *body,
        )),
        _ => None,
    };
    if let Some((issue, pr, head, repo, project, notes_dir, body)) = verdict_args {
        let tracker = tracker_pin()?;
        return match org::route(cli.org.as_deref(), cli.state_dir.clone(), tracker)? {
            org::Routed::Local if body => Err(Error::usage(
                "--body reads a hosted org's review text; a local read has the note files \
                 themselves. Nothing was read",
            )),
            org::Routed::Local => audit::verdicts(
                &issue,
                pr,
                &head,
                repo.as_deref(),
                project.as_deref(),
                notes_dir.as_deref(),
            ),
            org::Routed::Remote(target) => remote::run(
                target,
                cli.command,
                cli.wake_timeout,
                cli.state_dir,
                cli.command_path,
            ),
        };
    }
    // Hosted metadata never needs local sandbox adoption, permission replay,
    // daemon startup, or the generic native/container verb map. Refuse an
    // absent/local hosted selection before any of those paths, including on
    // client-only installations.
    if matches!(
        &cli.command,
        Commands::Agent {
            action: AgentAction::Directory { .. } | AgentAction::Notify { .. },
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
        return remote::run(
            target,
            cli.command,
            cli.wake_timeout,
            cli.state_dir,
            cli.command_path,
        );
    }
    // Hosted app actions must not fall through to a local daemon.
    if matches!(
        &cli.command,
        Commands::App {
            action: app::AppAction::Describe { .. } | app::AppAction::Call { .. }
        }
    ) {
        let target = match org::resolve(cli.org.as_deref(), cli.state_dir.clone(), tracker_pin()?)?
        {
            org::Resolved::Remote(target) => target,
            org::Resolved::Local(_) => {
                return Err(Error::usage(
                    "app describe/call require a hosted workspace; pass --org <workspace>. Nothing was sent and no daemon was started",
                ));
            }
        };
        return remote::run(
            target,
            cli.command,
            cli.wake_timeout,
            cli.state_dir,
            cli.command_path,
        );
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
        return remote::run(
            target,
            cli.command,
            cli.wake_timeout,
            cli.state_dir,
            cli.command_path,
        );
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
        let tracker = tracker_pin()?;
        let target = match org::resolve(cli.org.as_deref(), cli.state_dir.clone(), tracker)? {
            org::Resolved::Remote(target) => target,
            org::Resolved::Local(_) => {
                return Err(Error::usage(
                    "team requires a hosted org: pass --org <org> for a logged-in workspace, or switch back from `--org local`. Nothing was sent",
                ));
            }
        };
        return remote::run(
            target,
            cli.command,
            cli.wake_timeout,
            cli.state_dir,
            cli.command_path,
        );
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
    let tracker = tracker_pin()?;
    // Local maintenance and read-only skill discovery ignore a saved remote
    // default. Deliberate remote selectors refuse before transport; `dev`,
    // `setup` and skill reads also refuse a remote environment selector beside a pin.
    // Managed callers keep their original resolver and permission checks.
    let resolved = if matches!(cli.command, Commands::Update { .. }) {
        org::resolve_update(cli.org.as_deref(), cli.state_dir.clone(), tracker)?
    } else if matches!(cli.command, Commands::Dev { .. }) {
        org::resolve_dev(cli.org.as_deref(), cli.state_dir.clone(), tracker)?
    } else if matches!(cli.command, Commands::Setup { .. }) {
        // CAD-1922: `setup` installs on this machine. The local-maintenance
        // selector keeps a saved local connection's state/tracker pair,
        // never routes a saved remote default over the allowlist, and
        // refuses a remote `--org`/`CADENCE_ORG` instead of ignoring it.
        org::resolve_setup(cli.org.as_deref(), cli.state_dir.clone(), tracker)?
    } else if std::env::var_os("CADENCE_ALIAS").is_none()
        && std::env::var_os("CADENCE_GRANT_TOKEN").is_none()
        && matches!(
            cli.command,
            Commands::Skill {
                action: SkillAction::Status | SkillAction::Show { .. }
            }
        )
    {
        org::resolve_skill_read(cli.org.as_deref(), cli.state_dir.clone(), tracker)?
    } else {
        org::resolve(cli.org.as_deref(), cli.state_dir.clone(), tracker)?
    };
    let state_dir = match resolved {
        org::Resolved::Local(state_dir) => state_dir,
        org::Resolved::Remote(target) => {
            // CAD-2101: a profile command acts on one workspace's agents, so
            // the org must be named on this command line. A saved default or
            // `CADENCE_ORG` is not naming it: it could point at another
            // workspace than the caller means. Refused before any request.
            agent_profile::require_named_org(cli.org.as_deref(), &cli.command, &target.org)?;
            // CAD-1019 slice 2: a remote org never opens local state or
            // adopts a sandbox profile — the allowlisted verb goes over
            // HTTPS; everything else refuses inside `remote::run`.
            return remote::run(
                target,
                cli.command,
                cli.wake_timeout,
                cli.state_dir.clone(),
                cli.command_path,
            );
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
        | Commands::Connect { .. }
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
