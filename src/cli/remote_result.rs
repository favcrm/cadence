//! Explicit local result custody and one pinned hosted sender. No daemon or credential resolver.
use cadence_agent::error::{Error, Result};
use cadence_agent::remote_enrollment;
use cadence_agent::remote_result_outbox::{
    deliver_with, DestinationPin, LocalReceipt, ResultCommand, ResultOutbox, MAX_COMMAND_BYTES,
};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Subcommand)]
pub(crate) enum RemoteAction {
    /// Retain, inspect or explicitly send local result custody.
    Result {
        #[command(subcommand)]
        action: ResultAction,
    },
    /// Establish or inspect an issuer-bound hosted child credential.
    Enrollment {
        #[command(subcommand)]
        action: EnrollmentAction,
    },
}
#[derive(Subcommand)]
pub(crate) enum EnrollmentAction {
    /// Exchange a service credential from stdin. Requires a private trusted-issuer file.
    Bootstrap {
        #[arg(long)]
        issuer: String,
        #[arg(long)]
        org: String,
        #[arg(long)]
        audience: String,
        #[arg(long)]
        client_agent: String,
        #[arg(long)]
        enrollment_dir: PathBuf,
    },
    /// Short-lived owner consent for one implementer; repeat after expiry.
    Browser {
        #[arg(long)]
        issuer: String,
        #[arg(long)]
        org: String,
        #[arg(long)]
        audience: String,
        #[arg(long)]
        client_agent: String,
        #[arg(long)]
        enrollment_dir: PathBuf,
        /// Print the consent URL and code without attempting to open a browser.
        #[arg(long)]
        no_open: bool,
    },
    /// Re-enroll after expiry using the private stored service credential.
    Renew {
        #[arg(long)]
        enrollment_dir: PathBuf,
    },
    /// Inspect bound IDs and expiry without printing either secret.
    Status {
        #[arg(long)]
        enrollment_dir: PathBuf,
    },
    /// Remove the local credential record; server revocation is separate.
    Remove {
        #[arg(long)]
        enrollment_dir: PathBuf,
    },
}
#[derive(Subcommand)]
pub(crate) enum ResultAction {
    /// Retain one strict result envelope from bounded stdin as local_pending.
    Retain {
        #[command(flatten)]
        destination: DestinationArgs,
    },
    /// Inspect existing custody for the exact original pin; metadata only.
    Pending {
        #[command(flatten)]
        destination: DestinationArgs,
    },
    /// POST a retained command using its issuer-bound child enrollment.
    Send {
        #[arg(long)]
        outbox_dir: PathBuf,
        #[arg(long)]
        command_id: String,
        /// Private enrollment directory whose issuer binding must match custody.
        #[arg(long)]
        enrollment_dir: PathBuf,
    },
    /// Inspect one command's local or queued custody; never contacts the server.
    Status {
        #[arg(long)]
        outbox_dir: PathBuf,
        #[arg(long)]
        command_id: String,
    },
}
#[derive(Args)]
pub(crate) struct DestinationArgs {
    /// Explicit absolute directory; never inferred from daemon or user defaults.
    #[arg(long)]
    outbox_dir: PathBuf,
    /// Original organization ID (structural metadata, not membership proof).
    #[arg(long)]
    org: String,
    /// Exact canonical HTTPS board origin, pinned before a later send.
    #[arg(long)]
    audience: String,
    #[arg(long)]
    subject: String,
    #[arg(long)]
    agent: String,
}
impl DestinationArgs {
    fn pin(&self) -> Result<DestinationPin> {
        if !self.outbox_dir.is_absolute() {
            return Err(Error::rejected("Offline outbox directory must be absolute"));
        }
        DestinationPin::new(&self.org, &self.audience, &self.subject, &self.agent)
    }
}
fn metadata(receipt: &LocalReceipt) -> Value {
    let pin = receipt.destination();
    json!({"state":receipt.state(),"commandId":receipt.command_id(),"digest":receipt.digest(),
        "storedAt":receipt.stored_at_ms(),"destination":{"org":pin.organization_id(),
        "audience":pin.audience(),"subject":pin.subject_id(),"agent":pin.agent_id()}})
}
fn existing_outbox(dir: &PathBuf) -> Result<ResultOutbox> {
    if !dir.is_absolute()
        || !fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir())
        || !fs::symlink_metadata(dir.join("results.sqlite3")).is_ok_and(|m| m.is_file())
    {
        return Err(Error::rejected("Existing offline custody is required"));
    }
    ResultOutbox::open(dir)
}
fn post_queued(url: &str, bearer: &str, body: &str) -> Result<(u16, Vec<u8>)> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(12)))
        .http_status_as_error(false)
        .max_redirects(0)
        // Ambient proxy settings cannot retarget a bearer bound to the pin.
        .proxy(None)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = agent
        .post(url)
        .header("Authorization", format!("Bearer {bearer}"))
        .header("Content-Type", "application/json")
        .send(body.as_bytes())
        .map_err(|_| Error::rejected("Hosted result send uncertain; local custody retained"))?;
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::rejected("Hosted result response uncertain; local custody retained"))?;
    Ok((status, bytes))
}
pub(crate) fn run(action: &RemoteAction) -> Result<i32> {
    if let RemoteAction::Enrollment { action } = action {
        let record = match action {
            EnrollmentAction::Bootstrap {
                issuer,
                org,
                audience,
                client_agent,
                enrollment_dir,
            } => {
                let mut bytes = Vec::new();
                std::io::stdin()
                    .lock()
                    .take(129)
                    .read_to_end(&mut bytes)
                    .map_err(|_| Error::rejected("Unable to read service credential from stdin"))?;
                if bytes.len() > 128 {
                    return Err(Error::rejected("Invalid service credential"));
                }
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| Error::rejected("Invalid service credential"))?;
                let token = text
                    .strip_suffix("\r\n")
                    .or_else(|| text.strip_suffix('\n'))
                    .unwrap_or(text);
                Some(remote_enrollment::enroll(
                    issuer,
                    org,
                    audience,
                    client_agent,
                    token,
                    enrollment_dir,
                )?)
            }
            EnrollmentAction::Browser {
                issuer,
                org,
                audience,
                client_agent,
                enrollment_dir,
                no_open,
            } => Some(remote_enrollment::enroll_browser(
                issuer,
                org,
                audience,
                client_agent,
                enrollment_dir,
                |url, code| {
                    eprintln!("Approve hosted Cadence access at: {url}\nCode: {code}");
                    if !no_open {
                        let program = if cfg!(target_os = "macos") {
                            "open"
                        } else {
                            "xdg-open"
                        };
                        let _ = std::process::Command::new(program)
                            .arg(url)
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn();
                    }
                    Ok(())
                },
            )?),
            EnrollmentAction::Renew { enrollment_dir } => {
                Some(remote_enrollment::renew(enrollment_dir)?)
            }
            EnrollmentAction::Status { enrollment_dir } => {
                Some(remote_enrollment::current(enrollment_dir)?)
            }
            EnrollmentAction::Remove { enrollment_dir } => {
                remote_enrollment::remove(enrollment_dir)?;
                None
            }
        };
        let output = if let Some(record) = record {
            json!({"org":record.organization_id(),"audience":record.audience(),
                "subject":record.subject_id(),"agent":record.agent_id(),
                "expiresAt":record.expires_at()})
        } else {
            json!({"removed":true})
        };
        println!("{output}");
        return Ok(0);
    }
    let RemoteAction::Result { action } = action else {
        unreachable!()
    };
    let output = match action {
        ResultAction::Retain { destination } => {
            let pin = destination.pin()?;
            let mut bytes = Vec::new();
            std::io::stdin()
                .lock()
                .take((MAX_COMMAND_BYTES * 2 + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| Error::rejected("Unable to read offline result stdin"))?;
            if bytes.len() > MAX_COMMAND_BYTES * 2 {
                return Err(Error::rejected("Offline result stdin exceeds size limit"));
            }
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| Error::rejected("Offline result stdin must be UTF-8 JSON"))?;
            let command = ResultCommand::parse_json(text)?;
            // Validate all input before any custody creation/recovery.
            let outbox = ResultOutbox::open(&destination.outbox_dir)?;
            metadata(&outbox.enqueue(&pin, &command)?)
        }
        ResultAction::Pending { destination } => {
            let pin = destination.pin()?;
            // The library can initialize new custody; inspection must not do so.
            // Hostile same-UID path replacement races remain outside its boundary.
            // Protected open can create an init lock or recover a valid journal;
            // this is metadata-only output, not a universally read-only open.
            let outbox = existing_outbox(&destination.outbox_dir)?;
            let receipts: Vec<_> = outbox
                .pending_for(&pin)?
                .iter()
                .map(|row| {
                    let mut value = metadata(row.receipt());
                    value["state"] = json!(row.state());
                    value
                })
                .collect();
            json!({"receipts":receipts})
        }
        ResultAction::Send {
            outbox_dir,
            command_id,
            enrollment_dir,
        } => {
            let outbox = existing_outbox(outbox_dir)?;
            let row = outbox.get(command_id)?;
            let pin = row.receipt().destination();
            let receipt = remote_enrollment::with_current(enrollment_dir, pin, |bearer| {
                deliver_with(
                    &outbox,
                    command_id,
                    pin.organization_id(),
                    pin.audience(),
                    bearer,
                    post_queued,
                )
            })?;
            json!({"state":"remote_queued","application":"applied_unknown", "receipt":{
                "commandId":receipt.command_id(),"digest":receipt.digest(),
                "acceptedAt":receipt.accepted_at(),"expiresAt":receipt.expires_at()}})
        }
        ResultAction::Status {
            outbox_dir,
            command_id,
        } => {
            let row = existing_outbox(outbox_dir)?.get(command_id)?;
            let mut value = metadata(row.receipt());
            value["state"] = json!(row.state());
            value["application"] = json!("applied_unknown");
            if let Some(queued) = row.queued_receipt() {
                value["receipt"] = json!({"commandId":queued.command_id(), "digest":queued.digest(),
                    "acceptedAt":queued.accepted_at(),"expiresAt":queued.expires_at()});
            }
            value
        }
    };
    println!("{output}");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    const CHILD: &str = "hct_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq";
    const AUDIENCE: &str = "https://board.example.invalid";
    const ROUTE: &str = "/__platform/hosted-cadence/org-1/results";

    fn command() -> ResultCommand {
        ResultCommand::parse_json(&json!({
            "version":"hosted-cadence-result.v1", "commandId":"command-1", "kind":"agent_result",
            "assignmentId":"assignment-1", "taskId":"task-1", "taskRevision":1,
            "turnId":"turn-1", "reportedHeadSha":"a".repeat(40), "text":"private result"
        }).to_string()).unwrap()
    }

    fn serve_once(
        status: &str,
        response: Vec<u8>,
        expected_body: String,
    ) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}{}", listener.local_addr().unwrap(), ROUTE);
        let status = status.to_owned();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line.trim_end(), format!("POST {ROUTE} HTTP/1.1"));
            let mut authorization = None;
            let mut content_type = None;
            let mut content_length = None;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let (name, value) = line.split_once(':').unwrap();
                let value = value.trim();
                match name.to_ascii_lowercase().as_str() {
                    "authorization" => authorization = Some(value.to_owned()),
                    "content-type" => content_type = Some(value.to_owned()),
                    "content-length" => content_length = Some(value.parse::<usize>().unwrap()),
                    _ => (),
                }
            }
            assert_eq!(authorization, Some(format!("Bearer {CHILD}")));
            assert_eq!(content_type.as_deref(), Some("application/json"));
            let mut body = vec![0; content_length.unwrap()];
            reader.read_exact(&mut body).unwrap();
            assert_eq!(body, expected_body.as_bytes());
            let headers = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nLocation: http://127.0.0.1:9/trap\r\nConnection: close\r\n\r\n",
                response.len()
            );
            socket.write_all(headers.as_bytes()).unwrap();
            socket.write_all(&response).unwrap();
        });
        (url, server)
    }

    #[test]
    fn real_http_transport_carries_only_pinned_command_and_accepts_strict_queued_202() {
        let root = tempfile::tempdir().unwrap();
        let outbox = ResultOutbox::open(&root.path().join("outbox")).unwrap();
        let cmd = command();
        let pin = DestinationPin::new("org-1", AUDIENCE, "subject-1", "agent-1").unwrap();
        outbox.enqueue(&pin, &cmd).unwrap();
        let response = json!({"ok":true,"receipt":{"commandId":"command-1","state":"queued",
            "acceptedAt":100,"expiresAt":200,"digest":cmd.digest()}})
        .to_string()
        .into_bytes();
        let (endpoint, server) = serve_once("202 Accepted", response, cmd.canonical_json().into());
        let queued = deliver_with(
            &outbox,
            "command-1",
            "org-1",
            AUDIENCE,
            CHILD,
            |url, bearer, body| {
                assert_eq!(url, format!("{AUDIENCE}{ROUTE}"));
                post_queued(&endpoint, bearer, body)
            },
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(queued.state(), "remote_queued");
        assert_eq!(
            outbox.get("command-1").unwrap().queued_receipt(),
            Some(&queued)
        );
    }

    #[test]
    fn real_http_transport_preserves_pending_on_redirect_and_oversized_202() {
        let root = tempfile::tempdir().unwrap();
        let outbox = ResultOutbox::open(&root.path().join("outbox")).unwrap();
        let cmd = command();
        let pin = DestinationPin::new("org-1", AUDIENCE, "subject-1", "agent-1").unwrap();
        outbox.enqueue(&pin, &cmd).unwrap();
        let (redirect, redirect_server) =
            serve_once("302 Found", b"moved".to_vec(), cmd.canonical_json().into());
        let (status, _) = post_queued(&redirect, CHILD, cmd.canonical_json()).unwrap();
        redirect_server.join().unwrap();
        assert_eq!(status, 302);
        let (endpoint, server) = serve_once(
            "202 Accepted",
            vec![b'x'; 4097],
            cmd.canonical_json().into(),
        );
        assert!(deliver_with(
            &outbox,
            "command-1",
            "org-1",
            AUDIENCE,
            CHILD,
            |_, bearer, body| { post_queued(&endpoint, bearer, body) }
        )
        .is_err());
        server.join().unwrap();
        assert_eq!(outbox.get("command-1").unwrap().state(), "local_pending");
    }

    #[test]
    fn proxy_env_cannot_retarget_hosted_send() {
        if std::env::var_os("CADENCE_PROXY_PROOF_CHILD").is_none() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .arg("proxy_env_cannot_retarget_hosted_send")
                .env("CADENCE_PROXY_PROOF_CHILD", "1")
                .env("HTTP_PROXY", "http://127.0.0.1:1")
                .env("http_proxy", "http://127.0.0.1:1")
                .env("ALL_PROXY", "http://127.0.0.1:1")
                .env("all_proxy", "http://127.0.0.1:1")
                .env("NO_PROXY", "")
                .env("no_proxy", "");
            let output = cadence_agent::reaper::output(&mut child).unwrap();
            assert!(
                output.status.success(),
                "proxy isolation proof failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }

        let cmd = command();
        let (endpoint, server) = serve_once(
            "202 Accepted",
            json!({"ok":true,"receipt":{"commandId":"command-1","state":"queued",
                "acceptedAt":100,"expiresAt":200,"digest":cmd.digest()}})
            .to_string()
            .into_bytes(),
            cmd.canonical_json().into(),
        );
        let (status, _) = post_queued(&endpoint, CHILD, cmd.canonical_json()).unwrap();
        server.join().unwrap();
        assert_eq!(status, 202);
    }
}
