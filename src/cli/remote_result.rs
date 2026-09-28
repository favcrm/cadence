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
    /// POST a retained command to its original board using a child bearer from stdin.
    Send {
        #[arg(long)]
        outbox_dir: PathBuf,
        #[arg(long)]
        command_id: String,
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
        } => {
            let outbox = existing_outbox(outbox_dir)?;
            let mut raw = Vec::new();
            std::io::stdin()
                .lock()
                .take(129)
                .read_to_end(&mut raw)
                .map_err(|_| Error::rejected("Unable to read hosted child bearer from stdin"))?;
            if raw.len() > 128 {
                return Err(Error::rejected("Hosted child bearer is invalid"));
            }
            let raw = std::str::from_utf8(&raw)
                .map_err(|_| Error::rejected("Hosted child bearer is invalid"))?;
            let bearer = raw
                .strip_suffix("\r\n")
                .or_else(|| raw.strip_suffix('\n'))
                .unwrap_or(raw);
            let receipt = deliver_with(&outbox, command_id, bearer, post_queued)?;
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
