//! CAD-535: `cadence message` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum MessageAction {
    /// Enqueue a message; returns once it is durable.
    ///
    /// `cadence message send <ALIAS> --text <body>`: the recipient is
    /// the positional alias and the body is `--text`, `-m` or `--file`.
    /// There are no email-style `--to`, `--subject`, `--body` or `--cc`
    /// flags.
    ///
    /// Multi-topic reports: open the body with a `SUBJECT: <topic>`
    /// line (a single-line pty body leads with `SUBJECT: <topic> —`)
    /// so the recipient can scan topics; there is no subject field.
    Send {
        /// Agent alias or provider-native id.
        alias: String,
        /// Literal body.
        #[arg(short = 'm', long, conflicts_with = "file")]
        text: Option<String>,
        /// Read the body from a file.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Idempotency key; retries with the same id+content dedupe.
        #[arg(long)]
        message: Option<String>,
        /// Route the result to another agent when the turn finishes.
        #[arg(long)]
        reply_to: Option<String>,
        /// Attach this delivery to a task — ad-hoc follow-up inside a
        /// job's delivery record.
        #[arg(long)]
        task: Option<String>,
        /// Claim `agent ready` for the target first — the flag IS the
        /// operator's explicit claim, fused with the send; the claim
        /// probes the pane and refuses a visibly busy one.
        /// No-op on non-pty endpoints.
        #[arg(long)]
        ready: bool,
        /// Force the ready claim past a busy probe verdict.
        #[arg(long, requires = "ready")]
        force: bool,
        /// Mid-turn steering (pty only): paste into the live pane without
        /// owning a turn — passes the one-running-turn hold, owes no
        /// report, completes when the paste is confirmed, never replayed
        /// after a daemon restart. Live pane only; at most 500 chars;
        /// takes no `--reply-to` or `--task`.
        #[arg(long, conflicts_with_all = ["ready", "reply_to", "task", "priority", "supersedes"])]
        nudge: bool,
        #[command(flatten)]
        steer: SteerArgs,
    },
    /// Send and wait for the turn's terminal state.
    Ask {
        alias: String,
        #[arg(long, conflicts_with = "file")]
        text: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        message: Option<String>,
        /// Route the result to another agent when the turn finishes.
        #[arg(long)]
        reply_to: Option<String>,
        /// Attach this delivery to a task — ad-hoc follow-up inside a
        /// job's delivery record.
        #[arg(long)]
        task: Option<String>,
        /// Claim `agent ready` for the target first — same operator
        /// claim as `send --ready`; no-op on non-pty endpoints.
        #[arg(long)]
        ready: bool,
        /// Force the ready claim past a busy probe verdict.
        #[arg(long, requires = "ready")]
        force: bool,
        /// Seconds to wait (max 600).
        #[arg(long, default_value_t = 120)]
        wait: u64,
    },
    /// Pull the stored body of a message in bounded windows of Unicode
    /// scalars — the read half of push/pull delivery (CAD-565). The
    /// pane notice names the message; this returns the text itself,
    /// `--offset` scalars in and at most `--limit` scalars per call
    /// (server-capped). Offsets count scalars, never bytes.
    Read {
        /// Message id.
        message: String,
        /// Start at this Unicode-scalar offset (0-based).
        #[arg(long, default_value_t = 0)]
        offset: u64,
        /// At most this many Unicode scalars per call.
        #[arg(long)]
        limit: Option<u64>,
    },
    /// Record an explicit acknowledgement for a submitted PTY message.
    /// The token is the `turn_id` `cadence self` prints — shown only
    /// to the agent's own pane or endpoint (CAD-375).
    Ack {
        /// Message id.
        message: String,
        /// Submission token (pty-<generation>-<uuid>).
        #[arg(long)]
        token: String,
        /// Optional acknowledgement note.
        #[arg(long)]
        text: Option<String>,
    },
    /// Report the result of a submitted PTY message; completes it and
    /// routes to `reply_to` when set.
    Result {
        /// Message id.
        message: String,
        /// Submission token (pty-<generation>-<uuid>).
        #[arg(long)]
        token: String,
        /// Result text reported for the message.
        #[arg(long)]
        text: String,
        /// The commit this report produced — binds the message to an
        /// exact revision for `job verdict`.
        #[arg(long)]
        sha: Option<String>,
        /// A task report file (`cadence.report/2`, see `cadence report
        /// file`) whose frontmatter names kind and task. It is filed on
        /// the ticket first — a malformed report refuses the result —
        /// and the result text gains a `Report: <ID>/reports/<file>`
        /// line. A retry with the same file reuses the stored report.
        #[arg(long)]
        report: Option<PathBuf>,
    },
    /// Operator reconcile of an `unknown` message — the exit that keeps
    /// history. No turn token: `unknown` means the submission token is
    /// stale by definition. `interrupted` records that the outcome was
    /// never learned and routes nothing; `completed`/`failed` route
    /// `reply_to` exactly like a normal finish. Refused for any other
    /// current state.
    Reconcile {
        /// Message id (must currently be `unknown`).
        message: String,
        /// Terminal state to record: interrupted|completed|failed.
        #[arg(long, value_enum)]
        status: ReconcileStatus,
        /// Single-line note recorded with the reconcile event.
        #[arg(long)]
        note: Option<String>,
        /// Commit the operator states for a `completed` reconcile —
        /// bound exactly like a worker's `--sha`.
        #[arg(long)]
        sha: Option<String>,
    },
    /// Cancel a still-`queued` message — it is never delivered. A
    /// `reply_to` gets one `worker_notice` so a waiter isn't left
    /// hanging. Refused once a turn is claimed or terminal — a running
    /// turn is interrupted at the provider. Task-bound deliveries are
    /// refused: `job task cancel` owns that lifecycle.
    Cancel {
        /// Message id (must currently be `queued`).
        message: String,
        /// Who cancelled — recorded on the event and result.
        #[arg(long)]
        by: Option<String>,
        /// Why — recorded on the event, result and the routed notice.
        #[arg(long)]
        reason: Option<String>,
    },
}

pub(super) fn run(state_dir: PathBuf, action: MessageAction) -> Result<i32> {
    let (result, pending) = match action {
        MessageAction::Send {
            alias,
            text,
            file,
            message,
            reply_to,
            task,
            ready,
            force,
            nudge,
            steer,
        } => send_message(
            &state_dir, &alias, text, file, message, reply_to, ready, force, task, nudge, steer,
        )?,
        MessageAction::Read {
            message,
            offset,
            limit,
        } => (
            client::rpc(
                &state_dir,
                "message_read",
                json!({"message": message, "offset": offset, "limit": limit}),
            )?,
            false,
        ),
        MessageAction::Ack {
            message,
            token,
            text,
        } => (
            client::rpc(
                &state_dir,
                "message_report",
                json!({"message": message, "token": token,
                       "kind": "ack", "text": text}),
            )?,
            false,
        ),
        MessageAction::Result {
            message,
            token,
            text,
            sha,
            report,
        } => {
            let text = match report {
                Some(path) => {
                    report_result_text(&state_dir, &message, &token, text, sha.as_deref(), path)?
                }
                None => text,
            };
            (
                client::rpc(
                    &state_dir,
                    "message_report",
                    json!({"message": message, "token": token,
                           "kind": "result", "text": text, "sha": sha}),
                )?,
                false,
            )
        }
        MessageAction::Reconcile {
            message,
            status,
            note,
            sha,
        } => (
            client::rpc(
                &state_dir,
                "message_reconcile",
                json!({"message": message, "status": status.as_str(),
                       "note": note, "sha": sha}),
            )?,
            false,
        ),
        // Same `by` convention as reconcile: the cadence alias
        // when an agent cancels, "operator" otherwise.
        MessageAction::Cancel {
            message,
            by,
            reason,
        } => (
            client::rpc(
                &state_dir,
                "message_cancel",
                json!({"message": message, "reason": reason,
                       "by": by.or_else(|| std::env::var("CADENCE_ALIAS").ok())}),
            )?,
            false,
        ),
        MessageAction::Ask {
            alias,
            text,
            file,
            message,
            reply_to,
            task,
            ready,
            force,
            wait,
        } => {
            let body = read_body(text, file)?;
            // Same flag semantics as send: --ready IS the claim,
            // and the claim probes the pane unless --force.
            if ready {
                let show = client::rpc(&state_dir, "agent_show", json!({"alias": alias}))?;
                let agent = &show["agent"];
                if registry::ready_gate(
                    agent["provider"].as_str().unwrap_or_default(),
                    agent["endpoint_kind"].as_str().unwrap_or_default(),
                ) {
                    let by = std::env::var("CADENCE_ALIAS").ok();
                    client::rpc(
                        &state_dir,
                        "agent_ready",
                        json!({"alias": alias, "by": by, "force": force}),
                    )?;
                }
            }
            let result = client::rpc(
                &state_dir,
                "agent_ask",
                json!({"alias": alias, "text": body,
                       "message": message, "reply_to": reply_to,
                       "task": task, "wait": wait}),
            )?;
            let state = result
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let pending = matches!(state, "queued" | "submitting" | "running");
            (result, pending)
        }
    };
    print_json(&result);
    Ok(if pending { 2 } else { 0 })
}
