//! Start a GitHub App connection from the CLI (CAD-1946 / AOS-463).
//!
//! The CLI holds no AgenticOS browser session and must never hold GitHub
//! authority, so it asks the issuer for a device code, shows the verification
//! URL, and polls until the owner finishes in a browser. The browser's GitHub
//! authorization is the only proof; the poll answers a status, never a token.

use std::io::Read;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::error::{Error, Result};

const CODE_PATH: &str = "/v1/github-connection/device/code";
const TOKEN_PATH: &str = "/v1/github-connection/device/token";
const MAX_RESPONSE: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MIN_INTERVAL: u64 = 1;
const MAX_INTERVAL: u64 = 60;

fn refuse(why: impl std::fmt::Display) -> Error {
    Error::rejected(format!("github connection: {why}"))
}

/// One JSON POST to the issuer. Status is data here: a refusal body is the
/// answer we report, not a transport failure.
fn post(issuer: &str, path: &str, body: &Value) -> Result<(u16, Value)> {
    let url = format!("{issuer}{path}");
    let builder = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None);
    let response = crate::issuer_transport::agent(builder)
        .post(&url)
        .header("Content-Type", "application/json")
        .send_json(body)
        .map_err(|_| refuse("the issuer did not answer"))?;
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    response
        .into_body()
        .as_reader()
        .take(MAX_RESPONSE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| refuse("the issuer answer could not be read"))?;
    if bytes.len() > MAX_RESPONSE {
        return Err(refuse("the issuer answer was too large"));
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| refuse("the issuer answer was not JSON"))?;
    Ok((status, value))
}

/// The envelope's `data`, or the error code the issuer named.
fn data_of(status: u16, body: &Value) -> Result<Value> {
    if status != 200 {
        let code = body
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("request refused");
        return Err(refuse(format!("the issuer refused: {code}")));
    }
    body.get("data")
        .cloned()
        .ok_or_else(|| refuse("the issuer answer carried no data"))
}

fn text<'a>(data: &'a Value, key: &str) -> Result<&'a str> {
    data.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| refuse(format!("the issuer answer has no {key}")))
}

fn number(data: &Value, key: &str) -> u64 {
    data.get(key).and_then(Value::as_u64).unwrap_or(0)
}

#[derive(Debug, PartialEq)]
pub struct Issued {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
    pub expires_in: u64,
}

/// Ask the issuer for a code. Nothing is approved by this call.
pub fn start(issuer: &str) -> Result<Issued> {
    let (status, body) = post(issuer, CODE_PATH, &json!({}))?;
    let data = data_of(status, &body)?;
    let verification_uri = text(&data, "verification_uri")?;
    if !verification_uri.starts_with("https://") {
        return Err(refuse("the verification URL is not https"));
    }
    let interval = number(&data, "interval").clamp(MIN_INTERVAL, MAX_INTERVAL);
    Ok(Issued {
        device_code: text(&data, "device_code")?.to_string(),
        user_code: text(&data, "user_code")?.to_string(),
        verification_uri: verification_uri.to_string(),
        interval,
        expires_in: number(&data, "expires_in"),
    })
}

/// The workspace a connection grant was bound to.
#[derive(Debug, PartialEq)]
pub struct Workspace {
    pub id: String,
    pub name: String,
}

#[derive(Debug, PartialEq)]
pub enum Poll {
    Pending,
    Approved,
    /// The grant settled: repositories the installation exposes, and the
    /// workspace it bound. The workspace is absent only when the org no longer
    /// exists, so a missing name is reported rather than guessed.
    Connected {
        repositories: u64,
        workspace: Option<Workspace>,
    },
    Expired,
    Denied,
}

/// The workspace a settled grant bound, when the issuer names one.
fn workspace_of(data: &serde_json::Value) -> Option<Workspace> {
    let workspace = data.get("workspace")?.as_object()?;
    Some(Workspace {
        id: workspace.get("id")?.as_str()?.to_string(),
        name: workspace.get("name")?.as_str()?.to_string(),
    })
}

pub fn poll_once(issuer: &str, device_code: &str) -> Result<Poll> {
    let (status, body) = post(issuer, TOKEN_PATH, &json!({ "device_code": device_code }))?;
    let data = data_of(status, &body)?;
    match text(&data, "status")? {
        "pending" => Ok(Poll::Pending),
        "approved" => Ok(Poll::Approved),
        "connected" => Ok(Poll::Connected {
            repositories: number(&data, "repositories"),
            workspace: workspace_of(&data),
        }),
        "expired" => Ok(Poll::Expired),
        "denied" => Ok(Poll::Denied),
        other => Err(refuse(format!(
            "the issuer answered an unknown status {other:?}"
        ))),
    }
}

/// Start, show the URL, then poll until the grant settles or the code expires.
/// `on_code` is called once with the URL and the user code.
pub fn connect(issuer: &str, wait: Duration, mut on_code: impl FnMut(&str, &str)) -> Result<Poll> {
    let issued = start(issuer)?;
    on_code(&issued.verification_uri, &issued.user_code);
    let deadline = Instant::now() + wait;
    let step = Duration::from_secs(issued.interval);
    loop {
        match poll_once(issuer, &issued.device_code)? {
            Poll::Pending | Poll::Approved => {}
            settled => return Ok(settled),
        }
        if Instant::now() + step > deadline {
            return Ok(Poll::Expired);
        }
        std::thread::sleep(step);
    }
}

/// The line a settled grant prints. `None` for anything still in flight.
pub fn outcome_line(poll: &Poll) -> Option<String> {
    match poll {
        Poll::Connected {
            repositories: count,
            workspace,
        } => Some(match workspace {
            Some(workspace) => format!(
                "Connected. {count} repositor{} available to Cadence,\n\
                 authorized for {} ({}). Compare that with `cadence org ls`: a grant\n\
                 bound to another workspace leaves this one's repositories empty.",
                if *count == 1 { "y" } else { "ies" },
                workspace.name,
                workspace.id
            ),
            None => format!(
                "Connected. {count} repositor{} available to Cadence, but the issuer\n\
                 named no workspace — check `cadence org ls` before recording a delivery input.",
                if *count == 1 { "y" } else { "ies" }
            ),
        }),
        Poll::Expired => {
            Some("The code expired before it was approved. Run `connect` again.".into())
        }
        Poll::Denied => Some("The owner declined the connection.".into()),
        Poll::Pending | Poll::Approved => None,
    }
}

#[cfg(test)]
mod workspace_tests {
    use super::*;

    #[test]
    fn a_settled_grant_names_the_workspace_it_bound() {
        let data = serde_json::json!({
            "status": "connected",
            "repositories": 18,
            "workspace": {"id": "ws_26e7601c-d62f-4fc3-8b19-df2980d098d8", "name": "Fav Limited"},
        });
        let poll = Poll::Connected {
            repositories: number(&data, "repositories"),
            workspace: workspace_of(&data),
        };
        assert_eq!(
            poll,
            Poll::Connected {
                repositories: 18,
                workspace: Some(Workspace {
                    id: "ws_26e7601c-d62f-4fc3-8b19-df2980d098d8".into(),
                    name: "Fav Limited".into(),
                }),
            }
        );
        let line = outcome_line(&poll).unwrap();
        assert!(line.contains("18 repositories"), "{line}");
        assert!(line.contains("Fav Limited"), "{line}");
        assert!(
            line.contains("ws_26e7601c-d62f-4fc3-8b19-df2980d098d8"),
            "{line}"
        );
    }

    #[test]
    fn a_grant_without_a_named_workspace_is_said_so_rather_than_guessed() {
        let data = serde_json::json!({"status": "connected", "repositories": 1});
        let poll = Poll::Connected {
            repositories: number(&data, "repositories"),
            workspace: workspace_of(&data),
        };
        assert_eq!(
            poll,
            Poll::Connected {
                repositories: 1,
                workspace: None
            }
        );
        let line = outcome_line(&poll).unwrap();
        assert!(line.contains("named no workspace"), "{line}");
        assert!(line.contains("1 repository "), "{line}");
    }
}
