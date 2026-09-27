//! Explicit offline custody. No daemon, credential resolver or remote sender.
use cadence_agent::error::{Error, Result};
use cadence_agent::remote_result_outbox::{
    DestinationPin, LocalReceipt, ResultCommand, ResultOutbox, MAX_COMMAND_BYTES,
};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::PathBuf;

#[derive(Subcommand)]
pub(crate) enum RemoteAction {
    /// Retain or inspect offline result custody; no hosted transport.
    Result {
        #[command(subcommand)]
        action: ResultAction,
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
}
#[derive(Args)]
pub(crate) struct DestinationArgs {
    /// Explicit absolute directory; never inferred from daemon or user defaults.
    #[arg(long)]
    outbox_dir: PathBuf,
    /// Original organization ID (structural metadata, not membership proof).
    #[arg(long)]
    org: String,
    /// Exact canonical HTTPS gateway origin; no request is sent.
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
pub(crate) fn run(action: &RemoteAction) -> Result<i32> {
    let RemoteAction::Result { action } = action;
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
            let existing = fs::symlink_metadata(&destination.outbox_dir).is_ok_and(|m| m.is_dir());
            let database = fs::symlink_metadata(destination.outbox_dir.join("results.sqlite3"))
                .is_ok_and(|m| m.is_file());
            if !existing || !database {
                return Err(Error::rejected(
                    "Pending inspection requires existing offline custody",
                ));
            }
            // Protected open can create an init lock or recover a valid journal;
            // this is metadata-only output, not a universally read-only open.
            let outbox = ResultOutbox::open(&destination.outbox_dir)?;
            let receipts: Vec<_> = outbox
                .pending_for(&pin)?
                .iter()
                .map(|row| metadata(row.receipt()))
                .collect();
            json!({"state":"local_pending","receipts":receipts})
        }
    };
    println!("{output}");
    Ok(0)
}
