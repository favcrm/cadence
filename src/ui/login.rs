//! `cadence ui login` and `cadence ui sessions` (CAD-313, ADR 0004 §5.2,
//! §5.5): the operator's CLI door to board sessions. Both read the
//! operator secret from its file under strict modes
//! ([`crate::operator_auth::read_secret`]) and hand it to the daemon
//! over the socket — never through argv, the environment or a URL. The
//! daemon additionally requires positive operator proof of this
//! process, so an agent is refused however it runs this.

use std::path::Path;

use serde_json::{json, Value};

use super::load_opts;
use super::DeviceLoginAction;
use crate::client;
use crate::error::{Error, Result};
use crate::operator_auth;

/// The board URL a link for `tailnet` opens.
fn board_base(state_dir: &Path, tailnet: bool, port: Option<u16>) -> Result<String> {
    let persisted = load_opts(state_dir);
    if tailnet {
        return persisted
            .tailscale
            .as_ref()
            .map(|t| t.url())
            .ok_or_else(|| {
                Error::rejected(
                    "the board is not shared on the tailnet — run `cadence ui tailscale start` \
                     first, or drop --tailnet for this host's link",
                )
            });
    }
    // This board's own name, not 127.0.0.1 or a shared vhost: cookies
    // ignore ports, so only a per-board host keeps the session cookie
    // away from every other server on this machine.
    let port = port.or(persisted.port).unwrap_or(3010);
    Ok(format!("http://{}", super::operator::board_host(port)))
}

pub(super) fn login(
    state_dir: &Path,
    tailnet: bool,
    rotate: bool,
    port: Option<u16>,
    as_json: bool,
) -> Result<i32> {
    let base = board_base(state_dir, tailnet, port)?;
    operator_auth::ensure_secret(state_dir)?;
    let mut secret = operator_auth::read_secret(state_dir)?;
    let mut revoked = None;
    if rotate {
        let out = client::rpc(
            state_dir,
            "operator_secret_rotate",
            json!({ "secret": secret }),
        )?;
        revoked = out["revoked"].as_u64();
        secret = operator_auth::read_secret(state_dir)?;
    }
    let origin = if tailnet { "tailnet" } else { "loopback" };
    let out = client::rpc(
        state_dir,
        "operator_link_mint",
        json!({ "secret": secret, "origin": origin }),
    )?;
    drop(secret);
    let nonce = out["nonce"]
        .as_str()
        .filter(|n| operator_auth::well_formed(n))
        .ok_or_else(|| Error::internal("the daemon answered no login nonce"))?;
    // The nonce rides in the fragment only: a browser never sends it to
    // any server, so it never reaches a request line or a log.
    let link = format!("{base}/login#n={nonce}");
    let ttl = out["expires_in"]
        .as_i64()
        .unwrap_or(operator_auth::LINK_TTL_SECS);
    if as_json {
        println!(
            "{}",
            json!({"link": link, "origin": origin, "expires_in": ttl, "revoked": revoked})
        );
    } else {
        if let Some(n) = revoked {
            println!("Rotated the operator secret and revoked {n} session(s).");
        }
        println!("Open this link in your browser — it works once, within {ttl} s:\n");
        println!("  {link}\n");
        println!("It signs that browser in as the operator on {base}.");
    }
    Ok(0)
}

pub(super) fn sessions(
    state_dir: &Path,
    revoke: Option<&str>,
    revoke_all: bool,
    as_json: bool,
) -> Result<i32> {
    let secret = operator_auth::read_secret(state_dir)?;
    let mut params = json!({ "secret": secret });
    if let Some(id) = revoke {
        params["revoke"] = json!(id);
    }
    if revoke_all {
        params["revoke_all"] = json!(true);
    }
    let out = client::rpc(state_dir, "operator_sessions", params)?;
    if as_json {
        println!("{out}");
        return Ok(0);
    }
    if let Some(n) = out["revoked"].as_u64().filter(|n| *n > 0) {
        println!("Revoked {n} session(s).");
    }
    let rows = out["sessions"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("No operator sessions. `cadence ui login` opens one.");
        return Ok(0);
    }
    let time = |v: &Value| {
        v.as_i64()
            .map(crate::issue::time::iso)
            .unwrap_or_else(|| "-".into())
    };
    println!("ID        ORIGIN    CREATED               LAST USED             USER AGENT");
    for r in rows {
        println!(
            "{:<9} {:<9} {:<21} {:<21} {}",
            r["id"].as_str().unwrap_or("-"),
            r["origin"].as_str().unwrap_or("-"),
            time(&r["created"]),
            time(&r["last_used"]),
            r["user_agent"].as_str().unwrap_or("")
        );
    }
    Ok(0)
}

/// `cadence ui device-login set|clear|show` (CAD-841): the operator's
/// door to the daemon-owned device-login config — the same gate as
/// `login`/`sessions` (operator proof + the operator secret read
/// under strict modes), so an agent is refused however it runs this.
/// The config lives in the daemon store; a running board picks a
/// `set`/`clear` up on the next request, no restart.
pub(super) fn device_login(state_dir: &Path, action: &DeviceLoginAction) -> Result<i32> {
    let secret = operator_auth::read_secret(state_dir)?;
    match action {
        DeviceLoginAction::Set {
            issuer,
            org,
            subject,
        } => {
            let out = client::rpc(
                state_dir,
                "operator_device_login_set",
                json!({"secret": secret, "issuer": issuer, "org": org,
                       "subjects": subject}),
            )?;
            println!(
                "device login configured — issuer {} · workspace {}",
                out["issuer"].as_str().unwrap_or("-"),
                out["org"].as_str().unwrap_or("-"),
            );
        }
        DeviceLoginAction::Clear => {
            client::rpc(
                state_dir,
                "operator_device_login_clear",
                json!({"secret": secret}),
            )?;
            println!("device login cleared — the board's sign-in menu stops offering it.");
        }
        DeviceLoginAction::Show { json: as_json } => {
            let out = client::rpc(
                state_dir,
                "operator_device_login_show",
                json!({"secret": secret}),
            )?;
            if *as_json {
                println!("{out}");
            } else if out["configured"].as_bool() == Some(true) {
                println!("issuer:   {}", out["issuer"].as_str().unwrap_or("-"));
                println!("org:      {}", out["org"].as_str().unwrap_or("-"));
                let subjects = out["subjects"].as_array().cloned().unwrap_or_default();
                println!(
                    "subjects: {}",
                    subjects
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            } else {
                println!(
                    "device login is not configured — `cadence ui device-login set` enables it."
                );
            }
        }
    }
    Ok(0)
}
