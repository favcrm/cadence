//! CAD-535: `cadence inbox` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum InboxAction {
    /// Acknowledge consumed messages: `inbox ack <alias> <seq>...`
    /// completes every queued message at or below the greatest seq and
    /// records the reader's durable cursor — a restart resumes after
    /// it. Acking is a watermark: every queued message with `seq <=
    /// through` is consumed, so never ack past a message you have not
    /// processed.
    Ack {
        /// Inbox agent alias.
        alias: String,
        /// Message seqs consumed — the watermark is the greatest.
        seqs: Vec<i64>,
        /// Consume through this seq (same as passing it as a seq).
        #[arg(long)]
        through: Option<i64>,
        /// Reader name for the server-side ack cursor (default
        /// "default").
        #[arg(long)]
        reader: Option<String>,
        /// Drop the reader's cursor instead of acking: its next peek
        /// resumes from 0 and re-delivers everything still queued.
        /// Operator-only.
        #[arg(long)]
        reset: bool,
    },
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    state_dir: PathBuf,
    alias: Option<String>,
    action: Option<InboxAction>,
    peek: bool,
    after: Option<i64>,
    wait: u64,
    follow: bool,
    reader: Option<String>,
    exec: Option<Vec<String>>,
    exec_retry_ms: u64,
    exec_timeout_ms: u64,
    exec_max_failures: u32,
) -> Result<i32> {
    if let Some(InboxAction::Ack {
        alias,
        seqs,
        through,
        reader,
        reset,
    }) = action
    {
        if reset && (through.is_some() || !seqs.is_empty()) {
            return Err(Error::rejected(
                "`cadence inbox ack --reset` takes no seqs — it drops \
                 the reader's cursor instead of acking",
            ));
        }
        if !reset && seqs.is_empty() && through.is_none() {
            return Err(Error::rejected(
                "`cadence inbox ack` needs at least one seq — \
                 `inbox ack <alias> <seq>...` or `--through <seq>`",
            ));
        }
        let mut req = json!({"alias": alias, "seqs": seqs});
        if reset {
            req["reset"] = json!(true);
        }
        if let Some(t) = through {
            req["through"] = json!(t);
        }
        if let Some(r) = reader {
            req["reader"] = json!(r);
        }
        let r = client::rpc(&state_dir, "agent_inbox_ack", req)?;
        print_json(&r);
        return Ok(0);
    }
    let Some(alias) = alias else {
        return Err(Error::rejected(
            "`cadence inbox` needs an alias — `cadence inbox <alias>` to \
             read, `cadence inbox ack <alias> <seq>...` to acknowledge",
        ));
    };
    let reader = reader.clone().unwrap_or_else(|| "default".to_string());
    if let Some(argv) = exec {
        return run_inbox_exec(
            &state_dir,
            &alias,
            &reader,
            &argv,
            exec_retry_ms,
            exec_timeout_ms,
            exec_max_failures,
        );
    }
    // One JSON object per message, oldest first. The default
    // drain completes each `via=inbox_read` as printed —
    // printed output is proof of consumption, never re-read.
    // `--peek` changes nothing: the same lines, still queued.
    let mut after = after;
    loop {
        let mut req = json!({"alias": alias, "reader": reader,
                             "wait": if follow { 25 } else { wait }});
        if peek {
            req["peek"] = json!(true);
        }
        if let Some(a) = after {
            req["after"] = json!(a);
        }
        let page = client::rpc(&state_dir, "agent_inbox", req)?;
        let messages = page["messages"].as_array().cloned().unwrap_or_default();
        for m in &messages {
            println!("{}", serde_json::to_string(m).unwrap_or_default());
        }
        after = Some(page["cursor"].as_i64().unwrap_or(after.unwrap_or(0)));
        if !follow {
            // An empty drain/peek prints nothing — the output
            // is the complete record of what was seen.
            break;
        }
    }
    Ok(0)
}
