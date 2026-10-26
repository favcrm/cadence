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
    /// Replace the credential for this exact connection, preserving its ID.
    Rotate {
        connection_id: String,
        #[arg(long = "scope")]
        scopes: Vec<String>,
        #[arg(long, required = true)]
        token_stdin: bool,
        #[arg(long)]
        accept_same_uid_risk: bool,
    },
    /// Revoke this incarnation, its grants and pending effects.
    Revoke { connection_id: String },
    /// Check local configuration only; does not test upstream connectivity.
    Check { connection_id: String },
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
            (
                "connection_create",
                json!({"provider":provider,"account":account,"shape":"token","token":stdin_token()?,"scopes":scopes,"accept_same_uid_risk":accept_same_uid_risk}),
            )
        }
        ConnectionAction::Rotate {
            connection_id,
            scopes,
            token_stdin,
            accept_same_uid_risk,
        } => {
            if !token_stdin {
                return Err(Error::rejected("use --token-stdin"));
            }
            let id = validate_connection_id(&connection_id)?;
            let (expected_id, revision) = expected_identity(&state_dir, id)?;
            let mut params = json!({"connection_id":expected_id,"expected_connection_id":expected_id,
                "expected_revision":revision,"token":stdin_token()?,"accept_same_uid_risk":accept_same_uid_risk});
            if !scopes.is_empty() {
                params["scopes"] = json!(scopes);
            }
            ("connection_rotate", params)
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
