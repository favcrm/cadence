//! CAD-2758 / AOS-739: `cadence agent enroll start|status|list|approve|revoke`.
//!
//! Hosted-only enrollment of a separate `cadence login` session (`hosted_cli_sessions`)
//! as an existing workspace agent profile (`agent_ap_<id>`). `start` and `status`
//! act on the caller's own CLI login; `list`, `approve`, and `revoke` require a
//! live human workspace owner carrying `audit.approve`.

use super::*;

#[derive(Subcommand, Clone, Debug)]
pub(crate) enum EnrollAction {
    /// Request enrollment of this CLI login as an agent profile.
    Start {
        /// Optional profile id (`ap_…`) to suggest; the owner chooses at approval.
        #[arg(long)]
        profile: Option<String>,
    },
    /// Poll this CLI login's enrollment state.
    Status,
    /// List CLI enrollment requests for this workspace (owner + `audit.approve`).
    List,
    /// Approve a pending CLI enrollment request as a profile (owner + `audit.approve`).
    Approve {
        /// Request id (`cae_…`) returned by `cadence agent enroll start`.
        request: String,
        /// Target agent profile id (`ap_…`).
        #[arg(long)]
        profile: String,
        /// Grant `cli.write` in addition to `cli.read` (defaults to `cli.read` only).
        #[arg(long)]
        write: bool,
    },
    /// Revoke a pending or active CLI enrollment request (owner + `audit.approve`).
    Revoke {
        /// Request id (`cae_…`) to revoke.
        request: String,
    },
}

impl EnrollAction {
    pub(crate) fn is_owner_action(&self) -> bool {
        matches!(
            self,
            Self::List | Self::Approve { .. } | Self::Revoke { .. }
        )
    }

    fn verb(&self) -> &'static str {
        match self {
            Self::Start { .. } => "agent enroll start",
            Self::Status => "agent enroll status",
            Self::List => "agent enroll list",
            Self::Approve { .. } => "agent enroll approve",
            Self::Revoke { .. } => "agent enroll revoke",
        }
    }
}

/// Refuse hosted CLI enrollment commands for disallowed caller identities before
/// any request is sent:
/// - Managed daemon callers (`CADENCE_ALIAS`, `CADENCE_GRANT_TOKEN`, or pinned
///   `target.identity`) share the host's login and may not run ANY `agent enroll`
///   subcommand (`start`, `status`, `list`, `approve`, or `revoke`).
/// - Unmanaged non-human callers: `list`, `approve`, and `revoke` require a
///   live human owner (`refuse_non_human_actor`); `start` and `status` remain
///   allowed in an external coding harness unless explicit `--agent` /
///   `CADENCE_CLI_ACTOR=assistant` / `--as-assistant` was chosen.
pub(crate) fn refuse_disallowed_caller(
    action: &EnrollAction,
    target: &cadence_agent::remote_cli::RemoteTarget,
) -> Result<()> {
    if target.identity.is_some()
        || std::env::var_os("CADENCE_ALIAS").is_some()
        || std::env::var_os("CADENCE_GRANT_TOKEN").is_some()
    {
        return Err(Error::invalid(
            "managed_caller_refused",
            format!(
                "`cadence {}` is refused for a managed agent: hosted CLI enrollment acts on a \
                 standalone CLI login (`cadence login`), not a managed daemon session. Nothing \
                 was sent.",
                action.verb()
            ),
        ));
    }
    if action.is_owner_action() {
        return cadence_agent::remote_cli::refuse_non_human_actor(Some(target), action.verb());
    }
    if cadence_agent::remote_cli::saved_login_is_assistant_only(target)
        || cadence_agent::remote_cli::cli_actor_is_explicit_non_human()
    {
        return cadence_agent::remote_cli::refuse_non_human_actor(Some(target), action.verb());
    }
    Ok(())
}

/// An enrollment command acts on one hosted workspace; a saved default or
/// `CADENCE_ORG` is not naming it. Refused before any request is sent.
pub(crate) fn require_named_org(
    org_flag: Option<&str>,
    command: &Commands,
    resolved_org: &str,
) -> Result<()> {
    if org_flag.is_none()
        && matches!(
            command,
            Commands::Agent {
                action: AgentAction::Enroll { .. }
            }
        )
    {
        return Err(Error::invalid(
            "org_not_named",
            format!(
                "`cadence agent enroll` acts on one hosted workspace and a saved default is not \
                 naming it: this would have used `{resolved_org}`. Pass `--org <name>` on this \
                 command. Nothing was sent."
            ),
        ));
    }
    Ok(())
}

pub(crate) fn request(action: &EnrollAction) -> (&'static str, serde_json::Map<String, Value>) {
    let mut args = serde_json::Map::new();
    let verb = match action {
        EnrollAction::Start { profile } => {
            if let Some(profile_id) = profile {
                args.insert("profile_id".into(), json!(profile_id));
            }
            "agent_enroll_start"
        }
        EnrollAction::Status => "agent_enroll_status",
        EnrollAction::List => "agent_enroll_list",
        EnrollAction::Approve {
            request,
            profile,
            write,
        } => {
            args.insert("request_id".into(), json!(request));
            args.insert("profile_id".into(), json!(profile));
            if *write {
                args.insert("capabilities".into(), json!(["cli.read", "cli.write"]));
            }
            "agent_enroll_approve"
        }
        EnrollAction::Revoke { request } => {
            args.insert("request_id".into(), json!(request));
            "agent_enroll_revoke"
        }
    };
    (verb, args)
}

pub(crate) fn run_native(action: &EnrollAction) -> Result<i32> {
    Err(Error::rejected(format!(
        "`cadence {}` requires a hosted workspace (`--org <name>`): a native board has no \
         hosted CLI login enrollment. Nothing was sent.",
        action.verb()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed_env<T>(f: impl FnOnce() -> T) -> T {
        let _lock = crate::cli::org::tests::RESOLVE_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CADENCE_ALIAS", "synthetic-worker");
        let out = f();
        std::env::remove_var("CADENCE_ALIAS");
        out
    }

    #[test]
    fn enroll_actions_map_to_contract_verbs_and_args() {
        let ap = "ap_11111111-2222-4333-8444-555555555555";
        let req = "cae_11111111-2222-4333-8444-555555555555";

        let cases = [
            (
                EnrollAction::Start { profile: None },
                "agent_enroll_start",
                json!({}),
            ),
            (
                EnrollAction::Start {
                    profile: Some(ap.into()),
                },
                "agent_enroll_start",
                json!({"profile_id": ap}),
            ),
            (EnrollAction::Status, "agent_enroll_status", json!({})),
            (EnrollAction::List, "agent_enroll_list", json!({})),
            (
                EnrollAction::Approve {
                    request: req.into(),
                    profile: ap.into(),
                    write: false,
                },
                "agent_enroll_approve",
                json!({"request_id": req, "profile_id": ap}),
            ),
            (
                EnrollAction::Approve {
                    request: req.into(),
                    profile: ap.into(),
                    write: true,
                },
                "agent_enroll_approve",
                json!({"request_id": req, "profile_id": ap, "capabilities": ["cli.read", "cli.write"]}),
            ),
            (
                EnrollAction::Revoke {
                    request: req.into(),
                },
                "agent_enroll_revoke",
                json!({"request_id": req}),
            ),
        ];

        for (action, expected_verb, expected_args) in cases {
            let (verb, args) = request(&action);
            assert_eq!(verb, expected_verb);
            assert_eq!(Value::Object(args), expected_args);
            assert!(
                cadence_agent::remote_cli::REMOTE_VERBS.contains(&verb),
                "{verb} must be in REMOTE_VERBS"
            );
        }
    }

    #[test]
    fn managed_and_explicit_non_human_callers_are_refused_through_remote_run() {
        let target = || cadence_agent::remote_cli::RemoteTarget {
            org: "acme".into(),
            endpoint: "https://127.0.0.1:1".into(),
            org_id: "ws_synthetic".into(),
            identity: None,
        };
        let cmd = |action| Commands::Agent {
            action: AgentAction::Enroll { action },
        };
        let actions = || {
            vec![
                EnrollAction::Start { profile: None },
                EnrollAction::Status,
                EnrollAction::List,
                EnrollAction::Approve {
                    request: "cae_12345678".into(),
                    profile: "ap_11111111-2222-4333-8444-555555555555".into(),
                    write: false,
                },
                EnrollAction::Revoke {
                    request: "cae_12345678".into(),
                },
            ]
        };

        // Managed daemon caller (CADENCE_ALIAS) refuses ALL 5 actions through super::remote::run
        // before attempting any credential load or HTTP request to 127.0.0.1:1.
        let managed_results: Vec<_> = managed_env(|| {
            actions()
                .into_iter()
                .map(|a| super::remote::run(target(), cmd(a), 1, None, None))
                .collect()
        });
        for result in managed_results {
            let err = result.unwrap_err();
            assert_eq!(err.code(), Some("managed_caller_refused"), "{err}");
        }

        // Unmanaged external harness default (implicit_harness = true) allows start/status past
        // the caller guard (failing later at missing login), while refusing list/approve/revoke.
        let _lock = crate::cli::org::tests::RESOLVE_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let env_bare = crate::cli::org::tests::EnvGuard::bare();
        env_bare.write_registry(&crate::cli::org::tests::remote_registry(
            "acme",
            crate::cli::org::tests::EP_A,
            "ws_synthetic",
            true,
        ));
        cadence_agent::remote_cli::bind_cli_actor(Some(cadence_agent::remote_cli::CliActor {
            mode: cadence_agent::remote_cli::CliActorMode::Assistant,
            implicit_harness: true,
        }));
        let start_res = super::remote::run(
            target(),
            cmd(EnrollAction::Start { profile: None }),
            1,
            None,
            None,
        )
        .unwrap_err();
        assert_ne!(
            start_res.code(),
            Some("managed_caller_refused"),
            "{start_res}"
        );
        assert!(
            !start_res.to_string().contains("human/owner authority"),
            "{start_res}"
        );
        let approve_err = super::remote::run(
            target(),
            cmd(EnrollAction::Approve {
                request: "cae_12345678".into(),
                profile: "ap_11111111-2222-4333-8444-555555555555".into(),
                write: false,
            }),
            1,
            None,
            None,
        )
        .unwrap_err();
        assert!(
            approve_err.to_string().contains("human/owner authority"),
            "{approve_err}"
        );

        // Explicit --agent refuses start/status as well.
        cadence_agent::remote_cli::bind_cli_actor(Some(cadence_agent::remote_cli::CliActor {
            mode: cadence_agent::remote_cli::CliActorMode::Agent("ap_x".into()),
            implicit_harness: false,
        }));
        let explicit_start_err = super::remote::run(
            target(),
            cmd(EnrollAction::Start { profile: None }),
            1,
            None,
            None,
        )
        .unwrap_err();
        assert!(
            explicit_start_err
                .to_string()
                .contains("human/owner authority"),
            "{explicit_start_err}"
        );
        cadence_agent::remote_cli::unbind_cli_actor();
    }

    #[test]
    fn omitted_org_is_refused_through_dispatch_and_client_only_routes() {
        use crate::cli::org::tests::{remote_registry, EnvGuard, EP_A, RESOLVE_ENV};
        let _lock = RESOLVE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = EnvGuard::bare();
        env.write_registry(&remote_registry("acme", EP_A, "ws_alpha", true));
        for args in [
            vec!["cadence", "agent", "enroll", "status"],
            vec!["cadence", "agent", "enroll", "start"],
            vec!["cadence", "agent", "enroll", "list"],
        ] {
            let cli = Cli::try_parse_from(&args).unwrap();
            let err = super::action_dispatch::run_cli(cli).unwrap_err();
            assert_eq!(err.code(), Some("org_not_named"), "{err}");

            let cli_only = Cli::try_parse_from(&args).unwrap();
            let err_only = super::client_update::route_client_only(cli_only).unwrap_err();
            assert_eq!(err_only.code(), Some("org_not_named"), "{err_only}");
        }
    }
}
