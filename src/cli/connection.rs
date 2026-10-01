//! Proven-operator connection management; no agent grants or offline fallback.
use super::*;

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
            ("connection_revoke", json!({"connection_id":connection_id}))
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
            let mut params = json!({"connection_id":connection_id,"token":stdin_token()?,"accept_same_uid_risk":accept_same_uid_risk});
            if !scopes.is_empty() {
                params["scopes"] = json!(scopes);
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
}
