//! Proven-operator connection management; no agent grants or offline fallback.
use super::*;

fn validate_connection_id(value: &str) -> Result<&str> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(Error::rejected("connection ID is invalid"));
    }
    Ok(value)
}

#[derive(Subcommand)]
pub(crate) enum ConnectionAction {
    /// List reviewed provider descriptors and supported connection shapes.
    Providers,
    /// List exact authorized account references and built-in Local outbox.
    #[command(visible_alias = "list")]
    Ls,
    /// Show one durable connection incarnation.
    Show { connection_id: String },
    /// Enroll an explicitly supported scoped token; grants no workers.
    Create {
        provider: String,
        #[arg(long)]
        account: String,
        #[arg(long = "scope", required = true)]
        scopes: Vec<String>,
        /// Read a bounded scoped credential from stdin, never argv.
        #[arg(long, required = true)]
        token_stdin: bool,
        /// Explicitly accept the existing same-uid custody risk.
        #[arg(long)]
        accept_same_uid_risk: bool,
    },
    /// Create a metadata-only protected enrollment request and open the
    /// trusted entry page; the account handle, transport and secret are
    /// typed there, never here.
    Enroll {
        provider: String,
        #[arg(long = "scope")]
        scopes: Vec<String>,
        /// `token` (default), `smtp`, or a native-model source shape
        /// (`native-model-api-key`, `native-model-devin-credentials` for
        /// provider `devin`).
        #[arg(long)]
        shape: Option<String>,
        /// Bind the request to this existing agent (the daemon resolves its
        /// stable UID; the alias is not authority).
        #[arg(long)]
        agent: Option<String>,
        /// Print the entry link without opening a browser.
        #[arg(long)]
        no_open: bool,
    },
    /// Replace the credential for this exact connection, preserving its ID.
    /// By default this creates a protected request and opens the trusted
    /// entry page; `--token-stdin` selects the legacy direct-stdin path.
    Rotate {
        connection_id: String,
        #[arg(long = "scope")]
        scopes: Vec<String>,
        /// Legacy: read the replacement token from stdin instead of the
        /// trusted entry page (explicitly unprotected transport).
        #[arg(long)]
        token_stdin: bool,
        #[arg(long)]
        accept_same_uid_risk: bool,
        /// Print the entry link without opening a browser.
        #[arg(long)]
        no_open: bool,
    },
    /// Create a metadata-only human SMTP verification request and open the
    /// trusted entry page. Sends no mail; the stable request ID is the
    /// verification ID.
    Verify {
        connection_id: String,
        /// Print the entry link without opening a browser.
        #[arg(long)]
        no_open: bool,
    },
    /// Read one credential request's metadata-only status by its ID.
    RequestStatus { request_id: String },
    /// Revoke this incarnation, its grants and pending effects.
    Revoke { connection_id: String },
    /// Check local configuration only; does not test upstream connectivity.
    Check { connection_id: String },
    /// Open the trusted human management page in the operator's own browser:
    /// key session, finite limits, manifest/policy approval and UID grants.
    /// Carries no key, credential or argv/env value.
    Manage {
        /// Print the management link without opening a browser.
        #[arg(long)]
        no_open: bool,
    },
    /// Explicit owner key-session management; never carries key bytes/paths.
    Custody {
        #[command(subcommand)]
        action: CustodyAction,
    },
    /// Enroll a typed SMTP sender; read its credential only from bounded stdin.
    CreateSmtp {
        provider: String,
        #[arg(long)]
        account: String,
        #[arg(long = "scope", required = true)]
        scopes: Vec<String>,
        #[arg(long)]
        host: String,
        #[arg(long)]
        port: u16,
        #[arg(long)]
        tls_mode: String,
        #[arg(long)]
        username: String,
        #[arg(long)]
        sender: String,
        #[arg(long)]
        sender_name: Option<String>,
        #[arg(long, required = true)]
        secret_stdin: bool,
        #[arg(long)]
        accept_same_uid_risk: bool,
    },
    /// Rotate an SMTP sender; inherit only omitted transport fields.
    RotateSmtp {
        connection_id: String,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        tls_mode: Option<String>,
        #[arg(long)]
        username: Option<String>,
        #[arg(long)]
        sender: Option<String>,
        #[arg(long)]
        sender_name: Option<String>,
        #[arg(long, required = true)]
        secret_stdin: bool,
        #[arg(long)]
        accept_same_uid_risk: bool,
    },
}
#[derive(Subcommand)]
pub(crate) enum CustodyAction {
    /// Provision a fresh store using the configured external key.
    Initialize {
        #[arg(long, required = true)]
        accept_same_uid_risk: bool,
    },
    /// Deliberately inspect current authority and unlock this daemon session.
    Unlock {
        #[arg(long, required = true)]
        accept_same_uid_risk: bool,
    },
    /// Close custody and drop its in-memory key; contention refuses now.
    Lock,
    /// Report persistence and isolation separately; never key material.
    Status,
}
fn stdin_token() -> Result<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::io::stdin().take(8193).read_to_end(&mut bytes)?;
    if bytes.len() > 8192 {
        return Err(Error::rejected("credential exceeds the 8192-byte bound"));
    }
    let text = String::from_utf8(bytes).map_err(|_| Error::rejected("credential must be UTF-8"))?;
    let text = text.trim_end_matches(['\r', '\n']);
    if text.is_empty() {
        return Err(Error::rejected("credential stdin is empty"));
    }
    Ok(text.to_owned())
}
/// A one-shot show, not a retry or execution permit. Native daemon caller proof
/// and the mutation's exact core CAS remain independently authoritative.
fn expected_identity(state_dir: &Path, id: &str) -> Result<(String, u64)> {
    let row = client::rpc(state_dir, "connection_show", json!({"connection_id": id}))?;
    let shown = row["connection"]["id"]
        .as_str()
        .ok_or_else(|| Error::rejected("connection is unavailable or stale"))?;
    if shown != id {
        return Err(Error::rejected(
            "connection identity changed under preflight",
        ));
    }
    let revision = row["connection"]["revision"]
        .as_u64()
        .filter(|revision| *revision > 0 && *revision <= i64::MAX as u64)
        .ok_or_else(|| Error::rejected("connection revision is unavailable"))?;
    Ok((id.to_string(), revision))
}

/// The exact provider and credential revision one protected rotate/verify
/// request binds to. A missing or changed row refuses; it is never
/// re-read after the request records these values.
fn connection_target(state_dir: &Path, id: &str) -> Result<(String, u64)> {
    let row = client::rpc(state_dir, "connection_show", json!({"connection_id": id}))?;
    let connection = &row["connection"];
    if connection["id"].as_str() != Some(id) {
        return Err(Error::rejected(
            "connection identity changed under preflight",
        ));
    }
    let provider = connection["provider"]
        .as_str()
        .filter(|provider| {
            !provider.is_empty()
                && provider.len() <= 128
                && provider
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        })
        .ok_or_else(|| Error::rejected("connection provider is unavailable"))?
        .to_string();
    let revision = connection["revision"]
        .as_u64()
        .filter(|revision| *revision > 0 && *revision <= i64::MAX as u64)
        .ok_or_else(|| Error::rejected("connection revision is unavailable"))?;
    Ok((provider, revision))
}

fn request_id_shape(value: &str) -> bool {
    value.len() == 36
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase() || b == b'-')
        && uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}

/// A CLI inside a registered agent pane cannot choose the human's browser:
/// `xdg-open` there opens whatever that session controls (a guest has no
/// host browser), which is exactly the operator-profile separation the
/// credential boundary requires. The URL is still printed for the human.
fn managed_agent_cli() -> bool {
    std::env::var_os("CADENCE_ALIAS").is_some_and(|value| !value.is_empty())
}

/// A protected profile refuses the legacy directly-typed stdin transport
/// BEFORE reading stdin: the trusted entry origin is the only credential
/// collector on that deployment.
fn refuse_legacy_secret_on_protected(state_dir: &Path) -> Result<()> {
    if cadence_agent::ui::protected_profile_configured(state_dir) {
        return Err(Error::rejected(
            "this protected profile collects credentials only on the trusted entry origin — \
             use `cadence connection enroll` or `connection rotate <id>` and complete the form \
             there; a secret on stdin is not accepted",
        ));
    }
    Ok(())
}

/// Attach the server-selected entry URL to a freshly created metadata
/// request and open it in the operator's own browser. The link carries the
/// request selector only, and its origin is the deployment-pinned one — a
/// profile with no separate trusted origin refuses rather than pointing at
/// the ordinary board.
fn entry_receipt(
    state_dir: &Path,
    request: serde_json::Value,
    no_open: bool,
) -> Result<serde_json::Value> {
    let id = request["request_id"]
        .as_str()
        .filter(|id| request_id_shape(id))
        .ok_or_else(|| Error::internal("the daemon answered no credential request ID"))?;
    // A positive, operator-proven daemon answer is required before this
    // CLI claims any browser/entry layout. An unavailable/unknown daemon
    // is explicit unsupported, never a board-origin fallback.
    let (installed, origin) = cadence_agent::ui::credential_entry_config_proven(state_dir)
        .map_err(|error| {
            Error::rejected(format!(
                "credential request {id} was created, but the trusted credential origin cannot \
                 be proven on this CLI ({error}) — open it from the operator's own host session; \
                 no browser was opened here"
            ))
        })?;
    if !installed {
        return Err(Error::rejected(format!(
            "credential request {id} was created, but this deployment has no installed \
             protected profile — protected credential entry is unsupported here"
        )));
    }
    let base = origin.ok_or_else(|| {
        Error::rejected(format!(
            "credential request {id} was created, but this deployment has no separate trusted \
             credential origin — protected entry is unsupported here"
        ))
    })?;
    let url = format!("{base}/connections/enroll/{id}");
    if managed_agent_cli() {
        eprintln!(
            "This CLI is running inside a managed agent session, so it cannot open the \
             operator's browser or vouch for its profile. Open the link below in the \
             operator's separate browser profile (one that agent browser/CDP tools cannot \
             inspect). It carries only the request ID:\n\n  {url}\n"
        );
    } else {
        if !no_open {
            super::action_dispatch::open_login_url(&url, false);
        }
        eprintln!(
            "Complete the credential in the operator's own browser profile — a browser that \
             managed agents and their tools cannot inspect. This link carries only the request \
             ID:\n\n  {url}\n"
        );
    }
    Ok(json!({"request": request, "entry_url": url}))
}

pub(super) fn run(state_dir: PathBuf, action: ConnectionAction) -> Result<i32> {
    let (method, params) = match action {
        ConnectionAction::Providers => ("connection_providers", json!({})),
        ConnectionAction::Ls => ("connection_list", json!({})),
        ConnectionAction::Show { connection_id } => {
            ("connection_show", json!({"connection_id":connection_id}))
        }
        ConnectionAction::Check { connection_id } => {
            ("connection_check", json!({"connection_id":connection_id}))
        }
        ConnectionAction::Manage { no_open } => {
            // Management is the ordinary board's metadata page: the
            // isolated entry origin does not serve `/settings/connections`.
            // Require the same positive operator-proven daemon answer
            // before auto-opening a human-browser URL; an unavailable or
            // unproven layout is explicit unsupported, not a fallback.
            cadence_agent::ui::credential_entry_config_proven(&state_dir).map_err(|error| {
                Error::rejected(format!(
                    "the protected management layout cannot be proven on this CLI \
                     ({error}) — open `{}/settings/connections` from the operator's own host session",
                    cadence_agent::ui::board_base(&state_dir)
                ))
            })?;
            let url = format!(
                "{}/settings/connections",
                cadence_agent::ui::board_base(&state_dir)
            );
            if !managed_agent_cli() && !no_open {
                super::action_dispatch::open_login_url(&url, false);
            }
            eprintln!(
                "Manage protected custody and approvals in the operator's own browser. This \
                 link carries no credential:\n\n  {url}\n"
            );
            return Ok(0);
        }
        ConnectionAction::Revoke { connection_id } => {
            let id = validate_connection_id(&connection_id)?;
            let (expected_id, revision) = expected_identity(&state_dir, id)?;
            (
                "connection_revoke",
                json!({"connection_id":expected_id,
                "expected_connection_id":expected_id,"expected_revision":revision}),
            )
        }
        ConnectionAction::Create {
            provider,
            account,
            scopes,
            token_stdin,
            accept_same_uid_risk,
        } => {
            if !token_stdin {
                return Err(Error::rejected("use --token-stdin"));
            }
            refuse_legacy_secret_on_protected(&state_dir)?;
            (
                "connection_create",
                json!({"provider":provider,"account":account,"shape":"token","token":stdin_token()?,"scopes":scopes,"accept_same_uid_risk":accept_same_uid_risk}),
            )
        }
        ConnectionAction::Enroll {
            provider,
            scopes,
            shape,
            agent,
            no_open,
        } => {
            let shape = shape.unwrap_or_else(|| {
                if provider == "smtp" {
                    "smtp".to_string()
                } else {
                    "token".to_string()
                }
            });
            let mut params = json!({"provider":provider,"verb":"enroll",
                "shape":shape,"scopes":scopes});
            if let Some(agent) = agent {
                params["agent"] = json!(agent);
            }
            let out = client::rpc(&state_dir, "credential_request_create", params)?;
            print_json(&entry_receipt(&state_dir, out["request"].clone(), no_open)?);
            return Ok(0);
        }
        ConnectionAction::Rotate {
            connection_id,
            scopes,
            token_stdin,
            accept_same_uid_risk,
            no_open,
        } => {
            let id = validate_connection_id(&connection_id)?;
            if token_stdin {
                // The legacy direct-stdin transport stays explicitly
                // separate from the protected entry page — and is refused
                // outright on a protected profile, before stdin is read.
                refuse_legacy_secret_on_protected(&state_dir)?;
                let (expected_id, revision) = expected_identity(&state_dir, id)?;
                let mut params = json!({"connection_id":expected_id,"expected_connection_id":expected_id,
                "expected_revision":revision,"token":stdin_token()?,"accept_same_uid_risk":accept_same_uid_risk});
                if !scopes.is_empty() {
                    params["scopes"] = json!(scopes);
                }
                ("connection_rotate", params)
            } else {
                if accept_same_uid_risk {
                    return Err(Error::rejected(
                        "--accept-same-uid-risk belongs to the legacy direct-stdin rotate",
                    ));
                }
                let (provider, revision) = connection_target(&state_dir, id)?;
                let mut params = json!({"provider":provider,"verb":"rotate","connection_id":id,
                    "expected_credential_revision":revision});
                if !scopes.is_empty() {
                    params["scopes"] = json!(scopes);
                }
                let out = client::rpc(&state_dir, "credential_request_create", params)?;
                print_json(&entry_receipt(&state_dir, out["request"].clone(), no_open)?);
                return Ok(0);
            }
        }
        ConnectionAction::Verify {
            connection_id,
            no_open,
        } => {
            let id = validate_connection_id(&connection_id)?;
            let (provider, revision) = connection_target(&state_dir, id)?;
            let out = client::rpc(
                &state_dir,
                "credential_request_create",
                json!({"provider":provider,"verb":"verify","connection_id":id,
                    "expected_credential_revision":revision}),
            )?;
            let mut receipt = entry_receipt(&state_dir, out["request"].clone(), no_open)?;
            receipt["verification_request_id"] = out["request"]["verification_request_id"].clone();
            print_json(&receipt);
            return Ok(0);
        }
        ConnectionAction::RequestStatus { request_id } => {
            if !request_id_shape(&request_id) {
                return Err(Error::rejected("request ID is invalid"));
            }
            let out = client::rpc(
                &state_dir,
                "credential_request_status",
                json!({"request_id":request_id}),
            )?;
            print_json(&out);
            return Ok(0);
        }
        ConnectionAction::Custody { action } => match action {
            CustodyAction::Initialize {
                accept_same_uid_risk,
            } => (
                "connection_custody_initialize",
                json!({"accept_same_uid_risk":accept_same_uid_risk}),
            ),
            CustodyAction::Unlock {
                accept_same_uid_risk,
            } => (
                "connection_custody_unlock",
                json!({"accept_same_uid_risk":accept_same_uid_risk}),
            ),
            CustodyAction::Lock => ("connection_custody_lock", json!({})),
            CustodyAction::Status => ("connection_custody_status", json!({})),
        },
        ConnectionAction::CreateSmtp {
            provider,
            account,
            scopes,
            host,
            port,
            tls_mode,
            username,
            sender,
            sender_name,
            secret_stdin,
            accept_same_uid_risk,
        } => {
            if !secret_stdin {
                return Err(Error::rejected("use --secret-stdin"));
            }
            refuse_legacy_secret_on_protected(&state_dir)?;
            let mut params = json!({"provider":provider,"account":account,"shape":"smtp",
                "host":host,"port":port,"tls_mode":tls_mode,"username":username,
                "secret":stdin_token()?,"sender":sender,"scopes":scopes,
                "accept_same_uid_risk":accept_same_uid_risk});
            if let Some(name) = sender_name {
                params["sender_name"] = json!(name);
            }
            ("connection_create", params)
        }
        ConnectionAction::RotateSmtp {
            connection_id,
            host,
            port,
            tls_mode,
            username,
            sender,
            sender_name,
            secret_stdin,
            accept_same_uid_risk,
        } => {
            if !secret_stdin {
                return Err(Error::rejected("use --secret-stdin"));
            }
            refuse_legacy_secret_on_protected(&state_dir)?;
            let id = validate_connection_id(&connection_id)?;
            let (expected_id, revision) = expected_identity(&state_dir, id)?;
            let mut params = json!({"connection_id":expected_id,"expected_connection_id":expected_id,
                "expected_revision":revision,"secret":stdin_token()?,
                "accept_same_uid_risk":accept_same_uid_risk});
            for (field, value) in [
                ("host", host.map(Into::into)),
                ("port", port.map(|p| json!(p))),
                ("tls_mode", tls_mode.map(Into::into)),
                ("username", username.map(Into::into)),
                ("sender", sender.map(Into::into)),
                ("sender_name", sender_name.map(Into::into)),
            ] {
                if let Some(value) = value {
                    params[field] = value;
                }
            }
            ("connection_rotate", params)
        }
    };
    print_json(&client::rpc(&state_dir, method, params)?);
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn connection_cli_has_no_account_substitution_or_argv_token() {
        assert!(Cli::try_parse_from(["cadence", "connection", "providers"]).is_ok());
        assert!(Cli::try_parse_from([
            "cadence",
            "connection",
            "create",
            "fixture",
            "--account",
            "work",
            "--scope",
            "widgets:read",
            "--token-stdin"
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "cadence",
            "connection",
            "rotate",
            "conn-a",
            "--token-stdin"
        ])
        .is_ok());
        for args in [
            vec![
                "cadence",
                "connection",
                "rotate",
                "conn-a",
                "--token-stdin",
                "--account",
                "other",
            ],
            vec![
                "cadence",
                "connection",
                "rotate",
                "conn-a",
                "--token",
                "secret",
            ],
            vec![
                "cadence",
                "connection",
                "create",
                "fixture",
                "--account",
                "work",
                "--scope",
                "widgets:read",
            ],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    /// CAD-1128: the custody verbs exist, carry no credential fields and
    /// refuse anything but the operator's explicit acknowledgement flag.
    #[test]
    fn custody_verbs_parse_and_carry_no_secret_fields() {
        assert!(Cli::try_parse_from(["cadence", "connection", "custody", "status"]).is_ok());
        assert!(Cli::try_parse_from(["cadence", "connection", "custody", "lock"]).is_ok());
        assert!(Cli::try_parse_from([
            "cadence",
            "connection",
            "custody",
            "initialize",
            "--accept-same-uid-risk"
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "cadence",
            "connection",
            "custody",
            "unlock",
            "--accept-same-uid-risk"
        ])
        .is_ok());
        for args in [
            vec![
                "cadence",
                "connection",
                "custody",
                "initialize",
                "--key-file",
                "/tmp/k",
            ],
            vec![
                "cadence",
                "connection",
                "custody",
                "unlock",
                "--secret",
                "x",
            ],
            vec!["cadence", "connection", "custody", "initialize"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    /// CAD-1128: the SMTP create/rotate verbs take typed transport fields and
    /// a stdin-only secret; argv token/secret is refused.
    #[test]
    fn smtp_verbs_parse_and_refuse_argv_secret() {
        assert!(Cli::try_parse_from([
            "cadence",
            "connection",
            "create-smtp",
            "smtp",
            "--account",
            "news",
            "--scope",
            "smtp:send",
            "--host",
            "mail.example",
            "--port",
            "465",
            "--tls-mode",
            "implicit",
            "--username",
            "bot",
            "--sender",
            "bot@example",
            "--secret-stdin"
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "cadence",
            "connection",
            "rotate-smtp",
            "conn-a",
            "--secret-stdin"
        ])
        .is_ok());
        for args in [
            vec![
                "cadence",
                "connection",
                "create-smtp",
                "smtp",
                "--account",
                "a",
                "--scope",
                "s",
                "--host",
                "h",
                "--port",
                "465",
                "--tls-mode",
                "implicit",
                "--username",
                "u",
                "--sender",
                "s",
                "--secret",
                "x",
            ],
            vec![
                "cadence",
                "connection",
                "rotate-smtp",
                "conn-a",
                "--secret",
                "x",
            ],
            vec![
                "cadence",
                "connection",
                "create-smtp",
                "smtp",
                "--account",
                "a",
                "--scope",
                "s",
                "--host",
                "h",
                "--port",
                "465",
                "--tls-mode",
                "implicit",
                "--username",
                "u",
                "--sender",
                "s",
            ],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }
}
