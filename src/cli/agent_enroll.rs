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

/// Owner enrollment decisions (`list`, `approve`, `revoke`) require a live
/// human workspace owner. Because the board cannot observe `CADENCE_ALIAS`,
/// refuse managed callers here before any request is sent; `start` and `status`
/// stay allowed so an isolated agent session can enroll itself.
pub(crate) fn refuse_managed_owner_action(
    action: &EnrollAction,
    pinned_identity: bool,
) -> Result<()> {
    if action.is_owner_action()
        && (pinned_identity
            || std::env::var_os("CADENCE_ALIAS").is_some()
            || std::env::var_os("CADENCE_GRANT_TOKEN").is_some())
    {
        return Err(Error::invalid(
            "managed_caller_refused",
            format!(
                "`cadence {}` is refused for a managed agent: listing, approving, or revoking \
                 CLI enrollments is a live human workspace owner's decision. Nothing was sent.",
                action.verb()
            ),
        ));
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
    fn managed_caller_may_start_and_status_but_cannot_list_approve_or_revoke() {
        let start = EnrollAction::Start { profile: None };
        let status = EnrollAction::Status;
        let list = EnrollAction::List;
        let approve = EnrollAction::Approve {
            request: "cae_12345678".into(),
            profile: "ap_11111111-2222-4333-8444-555555555555".into(),
            write: false,
        };
        let revoke = EnrollAction::Revoke {
            request: "cae_12345678".into(),
        };

        assert!(refuse_managed_owner_action(&start, true).is_ok());
        assert!(refuse_managed_owner_action(&status, true).is_ok());
        assert_eq!(
            refuse_managed_owner_action(&list, true).unwrap_err().code(),
            Some("managed_caller_refused")
        );
        assert_eq!(
            refuse_managed_owner_action(&approve, true)
                .unwrap_err()
                .code(),
            Some("managed_caller_refused")
        );
        assert_eq!(
            refuse_managed_owner_action(&revoke, true)
                .unwrap_err()
                .code(),
            Some("managed_caller_refused")
        );

        let (start_env, status_env, list_env, approve_env, revoke_env) = managed_env(|| {
            (
                refuse_managed_owner_action(&start, false),
                refuse_managed_owner_action(&status, false),
                refuse_managed_owner_action(&list, false),
                refuse_managed_owner_action(&approve, false),
                refuse_managed_owner_action(&revoke, false),
            )
        });
        assert!(start_env.is_ok());
        assert!(status_env.is_ok());
        assert_eq!(list_env.unwrap_err().code(), Some("managed_caller_refused"));
        assert_eq!(
            approve_env.unwrap_err().code(),
            Some("managed_caller_refused")
        );
        assert_eq!(
            revoke_env.unwrap_err().code(),
            Some("managed_caller_refused")
        );
    }

    #[test]
    fn require_named_org_refuses_omitted_org_flag() {
        let cmd = Commands::Agent {
            action: AgentAction::Enroll {
                action: EnrollAction::Status,
            },
        };
        let err = require_named_org(None, &cmd, "fav-limited").unwrap_err();
        assert_eq!(err.code(), Some("org_not_named"));
        assert!(require_named_org(Some("fav-limited"), &cmd, "fav-limited").is_ok());
    }
}
