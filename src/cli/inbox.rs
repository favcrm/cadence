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
        let r = client::rpc_relay(&state_dir, "agent_inbox_ack", req)?;
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
        let page = client::rpc_relay(&state_dir, "agent_inbox", req)?;
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

/// `cadence inbox <alias> --follow --exec <cmd>` (CAD-480): push each
/// queued message to the command as JSON on stdin; it is acked
/// (`agent_inbox_ack` watermark) only after the command exits 0. A
/// non-zero exit leaves the message queued and retries it with bounded
/// backoff — `retry_base_ms` doubling per attempt, capped at 30s —
/// head-of-line, so a later message is never acked past a failed one.
/// A run past `timeout_ms` (0 = no limit) is killed and counted as a
/// failure; after `max_failures` consecutive failures on one message
/// the follower parks it (`inbox_park` — it stays queued and unread
/// but the reader's peeks skip it) and moves on. Every failure lands
/// on stderr and in the inbox's `inbox_exec_fail` events. The loop
/// ends only on interrupt or a daemon error.
pub(crate) fn run_inbox_exec(
    state_dir: &Path,
    alias: &str,
    reader: &str,
    argv: &[String],
    retry_base_ms: u64,
    timeout_ms: u64,
    max_failures: u32,
) -> Result<i32> {
    const RETRY_CAP_MS: u64 = 30_000;
    let mut pending: VecDeque<Value> = VecDeque::new();
    let mut attempts: HashMap<i64, u32> = HashMap::new();
    let mut retry_at: Option<Instant> = None;
    loop {
        if let Some(m) = pending.front().cloned() {
            if let Some(t) = retry_at {
                let now = Instant::now();
                if now < t {
                    // Bounded backoff — poll again soon so new arrivals
                    // still queue up behind the failed head.
                    std::thread::sleep((t - now).min(Duration::from_secs(1)));
                    continue;
                }
                retry_at = None;
            }
            let seq = m["seq"].as_i64().unwrap_or_default();
            match inbox_exec_once(argv, &m, timeout_ms) {
                Ok(()) => {
                    client::rpc_relay(
                        state_dir,
                        "agent_inbox_ack",
                        json!({"alias": alias, "through": seq, "reader": reader}),
                    )?;
                    println!("{}", serde_json::to_string(&m).unwrap_or_default());
                    pending.pop_front();
                    attempts.remove(&seq);
                }
                Err(why) => {
                    let a = {
                        let a = attempts.entry(seq).or_insert(0);
                        *a += 1;
                        *a
                    };
                    client::rpc_relay(
                        state_dir,
                        "agent_inbox_ack",
                        json!({"alias": alias, "reader": reader,
                               "fail": {"message": m["id"], "seq": seq,
                                        "attempt": a, "error": why}}),
                    )?;
                    if a >= max_failures {
                        // Poison: park it for this reader — queued and
                        // unread still, but no longer head-of-line.
                        let reason = format!("exec failed {a} times; last: {why}");
                        client::rpc_relay(
                            state_dir,
                            "agent_inbox_ack",
                            json!({"alias": alias, "reader": reader,
                                   "park": m["id"], "reason": reason}),
                        )?;
                        eprintln!(
                            "inbox {alias}: seq {seq} parked after {a} failures \
                             ({why}) — still queued; moving on"
                        );
                        pending.pop_front();
                        attempts.remove(&seq);
                        continue;
                    }
                    let delay = retry_base_ms
                        .saturating_mul(1u64 << a.saturating_sub(1).min(10))
                        .min(RETRY_CAP_MS);
                    eprintln!(
                        "inbox {alias}: exec failed for seq {seq} (attempt {a}): \
                         {why} — the message stays queued; retrying in {delay}ms"
                    );
                    retry_at = Some(Instant::now() + Duration::from_millis(delay));
                }
            }
            continue;
        }
        // Queue empty — long-poll for arrivals. Peek changes nothing:
        // an un-acked message comes back every round until exec
        // succeeds.
        let page = client::rpc_relay(
            state_dir,
            "agent_inbox",
            json!({"alias": alias, "peek": true, "reader": reader, "wait": 25}),
        )?;
        for m in page["messages"].as_array().cloned().unwrap_or_default() {
            let seq = m["seq"].as_i64().unwrap_or_default();
            if !pending.iter().any(|p| p["seq"].as_i64() == Some(seq)) {
                pending.push_back(m);
            }
        }
    }
}

/// Run one `--exec` argv on one message: the message JSON on stdin, no
/// shell. `Ok(())` only on exit 0; `Err` carries the exit status and a
/// stderr tail for the failure report. A run still alive at
/// `timeout_ms` (0 = no limit) is killed — the error names the timeout.
pub(crate) fn inbox_exec_once(
    argv: &[String],
    message: &Value,
    timeout_ms: u64,
) -> std::result::Result<(), String> {
    let mut child = cadence_agent::reaper::spawn(
        Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::piped()),
    )
    .map_err(|e| format!("could not spawn '{}': {e}", argv[0]))?;
    // The write runs on its own thread: a consumer that never reads
    // stdin or exits early must not block the wait on a full pipe.
    let writer = child.stdin.take().map(|mut stdin| {
        let body = serde_json::to_string(message)
            .unwrap_or_default()
            .into_bytes();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&body);
        })
    });
    // Poll try_wait: std has no wait-with-timeout, and a wedged
    // consumer must not block the inbox head-of-line forever.
    let deadline = (timeout_ms > 0).then(|| Instant::now() + Duration::from_millis(timeout_ms));
    let timed_out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) => {
                if let Some(t) = deadline {
                    if Instant::now() >= t {
                        let _ = child.kill();
                        break true;
                    }
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("could not wait on '{}': {e}", argv[0])),
        }
    };
    // Reaped or killed: wait_with_output drains the piped stderr (EOF
    // on exit) and reports the status.
    let out = child
        .wait_with_output()
        .map_err(|e| format!("could not wait on '{}': {e}", argv[0]))?;
    if let Some(w) = writer {
        let _ = w.join();
    }
    if timed_out {
        return Err(format!(
            "'{}' killed after {}ms timeout",
            argv[0], timeout_ms
        ));
    }
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stderr = stderr.trim();
    let tail = if stderr.len() > 500 {
        &stderr[stderr.len() - 500..]
    } else {
        stderr
    };
    Err(format!(
        "'{}' {}: {}",
        argv[0],
        out.status
            .code()
            .map_or_else(|| "died on a signal".to_string(), |c| format!("exited {c}")),
        if tail.is_empty() { "no stderr" } else { tail }
    ))
}
