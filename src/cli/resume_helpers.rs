//! CAD-1002: resume/stop lifecycle helpers — moved verbatim from
//! `cli/mod.rs` (CAD-984 PR-2).

use super::*;

/// `cadence agent resume`: provider-launch treatment for a reopen —
/// bounded wait for the endpoint, then attach this terminal by default.
/// `--detach` or a non-TTY/nested-tmux context prints the attach command
/// instead. Kinds with nothing attachable (managed stdio, fake) return
/// the resume receipt immediately — there is no endpoint to wait for.
pub(crate) fn resume_agent(state_dir: &Path, alias: &str, detach: bool) -> Result<i32> {
    let result = client::rpc(state_dir, "agent_resume", json!({"alias": alias}))?;
    let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))?;
    let agent = &show["agent"];
    let (provider, kind) = (
        agent["provider"].as_str().unwrap_or_default(),
        agent["endpoint_kind"].as_str().unwrap_or_default(),
    );
    if !registry::attachable(provider, kind) {
        // Non-attachable actors prove their open by reaching a live
        // state — finish_resume waits for that (bounded) before any
        // briefing/AGENTS.md housekeeping.
        print_json(&finish_resume(state_dir, alias, result));
        return Ok(0);
    }
    // Poll until the endpoint is live or the actor gives up.
    let deadline = Instant::now() + Duration::from_secs(30);
    let (agent, unknown) = loop {
        let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))?;
        let agent = show["agent"].clone();
        let state = agent["state"].as_str().unwrap_or_default();
        if agent["endpoint"].is_string() || matches!(state, "stopped" | "offline" | "attention") {
            break (agent, show["unknown"].as_i64().unwrap_or(0));
        }
        if Instant::now() >= deadline {
            return Err(Error::rejected(format!(
                "Agent '{alias}' opened no endpoint within 30s — inspect \
                 `cadence agent show {alias}` and attach when it is live"
            )));
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let state = agent["state"].as_str().unwrap_or_default();
    // A fenced agent (attention, no endpoint) gets the recovery hint —
    // unreconciled unknowns reconcile first — anything else attaches.
    let next = if state == "attention" && agent["endpoint"].is_null() {
        fenced_next(alias, agent["error"].as_str().unwrap_or_default(), unknown)
    } else {
        json!({"attach": format!("cadence agent attach {alias}")})
    };
    print_json(&finish_resume(
        state_dir,
        alias,
        json!({
            "alias": alias, "state": state,
            "endpoint": agent["endpoint"], "next": next,
        }),
    ));
    if detach || agent["endpoint"].is_null() {
        return Ok(0);
    }
    if atty_stdin() && std::env::var_os("TMUX").is_none() {
        return attach_agent(state_dir, alias, true);
    }
    attach_agent(state_dir, alias, false)
}

/// The two provider errors that mean "the pane bound a different native
/// session" — resume can never converge on these, so they get the
/// remove-and-rejoin hint.
pub(crate) fn session_mismatch(error: &str) -> bool {
    error.contains("owns session") || error.contains("acquired session")
}

/// `next` hint for a fenced agent (`attention`, no endpoint). An
/// unreconciled `unknown` is an inspection problem: the hint does not
/// hand out an unfence-then-resume command chain. A session-mismatch
/// can never converge — remove and rejoin (each retried resume mints a
/// fresh provider session); anything else retries `agent resume`.
pub(crate) fn fenced_next(alias: &str, error: &str, unknown: i64) -> Value {
    if session_mismatch(error) {
        json!({
            "remove": format!("cadence agent remove {alias}"),
            "rejoin": "cadence join <pm> <provider> -r <session>",
            "note": "retrying resume mints a new provider session each time",
        })
    } else if unknown > 0 {
        json!({
            "inspect": cadence_agent::daemon::unknown_inspect_lead(),
            "decision": cadence_agent::daemon::unknown_recovery_note(),
        })
    } else {
        json!({"resume": format!("cadence agent resume {alias}")})
    }
}

/// Resume one registered agent, waiting — bounded — for the endpoint
/// when the kind has one. Live agents are skipped; terminal-state or
/// RPC failures land in the per-member `error`. The unrecoverable case
/// (the pane adopted a different native session) gets an explicit
/// remove-and-rejoin hint.
pub(crate) fn resume_one(state_dir: &Path, alias: &str) -> Value {
    let (agent, unknown) = match client::rpc(state_dir, "agent_show", json!({"alias": alias})) {
        Ok(show) => (show["agent"].clone(), show["unknown"].as_i64().unwrap_or(0)),
        Err(e) => return json!({"alias": alias, "resumed": false, "error": e.to_string()}),
    };
    // Live = a live actor (idle/running/waiting_input/starting) or a
    // live endpoint address — fake/managed actors never expose one, so
    // endpoint alone cannot detect "already up".
    // A mailbox has nothing to resume — its queue survives regardless.
    let (provider, kind) = (
        agent["provider"].as_str().unwrap_or_default(),
        agent["endpoint_kind"].as_str().unwrap_or_default(),
    );
    if !registry::has_actor(provider, kind) {
        return json!({"alias": alias, "resumed": false, "skipped": "mailbox"});
    }
    let live = agent["endpoint"].is_string()
        || matches!(
            agent["state"].as_str(),
            Some("idle") | Some("running") | Some("waiting_input") | Some("starting")
        );
    if live {
        return finish_resume(
            state_dir,
            alias,
            json!({"alias": alias, "resumed": false, "skipped": "live"}),
        );
    }
    // Fenced by an unreconciled `unknown` — never attempted; the sweep
    // reports it under `fenced` with the reconcile-first commands.
    if unknown > 0 {
        return json!({"alias": alias, "resumed": false, "fenced": true,
        "state": agent["state"],
        "hint": format!(
            "fenced by an unreconciled unknown message — not resumed. {} {}",
            cadence_agent::daemon::unknown_inspect_lead(),
            cadence_agent::daemon::unknown_recovery_note()
        )});
    }
    // Any other `attention` fence gates resume exactly the way it gates
    // startup relaunch — the recorded cause is the operator's context;
    // a session-mismatch can never converge (each retried resume mints
    // a new provider session), anything else may retry once cleared.
    if agent["state"].as_str() == Some("attention") {
        let error = agent["error"].as_str().unwrap_or_default().to_string();
        let hint = if session_mismatch(&error) {
            format!(
                "unrecoverable — `cadence agent remove {alias}` then rejoin with \
                     `cadence join <pm> <provider> -r <session>`; each retried resume \
                     mints a new provider session"
            )
        } else {
            format!(
                "fenced (attention) — `cadence agent show {alias}` records the \
                     cause; retry `cadence agent resume {alias}` once it is cleared"
            )
        };
        return json!({"alias": alias, "resumed": false, "fenced": true,
                      "state": "attention", "error": error, "hint": hint});
    }
    let attachable = registry::attachable(provider, kind);
    if let Err(e) = client::rpc(state_dir, "agent_resume", json!({"alias": alias})) {
        return json!({"alias": alias, "resumed": false, "error": e.to_string()});
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let agent = client::rpc(state_dir, "agent_show", json!({"alias": alias}))
            .map(|s| s["agent"].clone())
            .unwrap_or_default();
        let state = agent["state"].as_str().unwrap_or_default();
        if agent["endpoint"].is_string() {
            return finish_resume(
                state_dir,
                alias,
                json!({"alias": alias, "resumed": true,
                          "endpoint": agent["endpoint"]}),
            );
        }
        // Kinds without an attachable endpoint are done once the actor
        // is back — there is nothing to wait on.
        if !attachable && matches!(state, "idle" | "running") {
            return finish_resume(state_dir, alias, json!({"alias": alias, "resumed": true}));
        }
        if matches!(state, "attention" | "stopped" | "offline") {
            let error = agent["error"].as_str().unwrap_or("unknown").to_string();
            let mut out = json!({"alias": alias, "resumed": false,
                                 "state": state, "error": error});
            if session_mismatch(&error) {
                // The pane bound a different native session — resume
                // can never converge; rebuild the member instead.
                out["hint"] = json!(format!(
                    "unrecoverable — `cadence agent remove {alias}` then \
                     rejoin with `cadence join <pm> <provider> -r <session>`; \
                     retrying resume mints a new provider session each time"
                ));
                out["unrecoverable"] = json!(true);
            }
            return out;
        }
        if Instant::now() >= deadline {
            return json!({"alias": alias, "resumed": false,
                          "error": "endpoint did not open within 15s"});
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Post-resume housekeeping on a successfully resumed agent: regenerate
/// a missing briefing in its state-dir location (agents registered
/// before briefings moved out of the repo have none there) and re-apply
/// the opt-in AGENTS.md block — the persisted `agents_md` param replays
/// here like the other launch params. Best-effort: the resume already
/// succeeded, so a failure surfaces as a warning field, not an error.
pub(crate) fn finish_resume(state_dir: &Path, alias: &str, mut out: Value) -> Value {
    match refresh_briefing(state_dir, alias) {
        Ok(Some(file)) => out["briefing"] = json!(file),
        Ok(None) => {}
        Err(e) => out["briefing_warning"] = json!(e.to_string()),
    }
    out
}

/// `Some(path)` when a briefing was (re)written, `None` when nothing
/// was needed. Post-open only, same rule as launch: the actor must
/// prove it is live before anything is written — a resume whose open
/// stalls or fences writes nothing (terminal states exit early, a slow
/// open gives up after 15s).
pub(crate) fn refresh_briefing(state_dir: &Path, alias: &str) -> Result<Option<PathBuf>> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let agent = loop {
        let agent = client::rpc(state_dir, "agent_show", json!({"alias": alias}))
            .map(|s| s["agent"].clone())
            .unwrap_or_default();
        let state = agent["state"].as_str().unwrap_or_default();
        if agent["endpoint"].is_string() || matches!(state, "idle" | "running" | "waiting_input") {
            break agent;
        }
        if matches!(state, "stopped" | "offline" | "attention") || Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    if !registry::has_actor(
        agent["provider"].as_str().unwrap_or_default(),
        agent["endpoint_kind"].as_str().unwrap_or_default(),
    ) {
        return Ok(None);
    }
    let file = client::briefing_path(state_dir, &agent["params"], alias);
    let opted_in = agent["params"]["agents_md"].as_bool() == Some(true);
    let split_pty = agent["endpoint_kind"].as_str() == Some("pty")
        && cadence_agent::agent_uid::config::configured_uid(state_dir)?.is_some();
    if file.exists() && !opted_in && !split_pty {
        return Ok(None);
    }
    brief_agent(state_dir, alias, false, None).map(Some)
}

/// Resume a list of aliases in order, printing a per-member status line
/// and returning the `{resumed, skipped, fenced, failed}` summary.
/// Fenced members are never attempted — they land in `fenced` with the
/// reconcile-first hint.
pub(crate) fn resume_sweep(state_dir: &Path, aliases: &[String]) -> Value {
    let (mut resumed, mut skipped, mut fenced, mut failed) = (vec![], vec![], vec![], vec![]);
    for alias in aliases {
        let r = resume_one(state_dir, alias);
        if r["resumed"].as_bool() == Some(true) {
            eprintln!("resume {alias}: up");
            resumed.push(r);
        } else if r["skipped"].is_string() {
            eprintln!(
                "resume {alias}: skipped ({})",
                r["skipped"].as_str().unwrap_or("")
            );
            skipped.push(r);
        } else if r["fenced"].as_bool() == Some(true) {
            eprintln!(
                "resume {alias}: FENCED — {}",
                r["hint"]
                    .as_str()
                    .unwrap_or("reconcile its unknown messages")
            );
            fenced.push(r);
        } else {
            let reason = r["error"].as_str().unwrap_or("unknown");
            eprintln!("resume {alias}: FAILED — {reason}");
            failed.push(r);
        }
    }
    json!({"resumed": resumed, "skipped": skipped,
           "fenced": fenced, "failed": failed})
}

/// `cadence resume <group>` / `cadence resume --all`.
pub(crate) fn resume_command(
    state_dir: &Path,
    group: Option<String>,
    all: bool,
    detach: bool,
) -> Result<i32> {
    if all {
        let targets = client::rpc(state_dir, "agent_list", json!({}))?["agents"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter(|a| {
                a["endpoint"].is_null()
                    && (a["thread_id"].is_string() || a["session_id"].is_string())
            })
            .filter_map(|a| a["alias"].as_str().map(str::to_string))
            .collect::<Vec<_>>();
        print_json(&resume_sweep(state_dir, &targets));
        return Ok(0);
    }
    let group = group.expect("clap requires group unless --all");
    // Resolve like join: alias or provider-native id → the PM agent.
    let pm = client::rpc(state_dir, "agent_show", json!({"alias": group})).map_err(|_| {
        Error::rejected(format!(
            "Unknown group '{group}' — no such agent; \
                 `cadence agent list` shows registered aliases"
        ))
    })?["agent"]
        .clone();
    let pm_alias = pm["alias"].as_str().unwrap_or_default().to_string();
    // PM first, then members — only those without a live endpoint
    // (resume_one re-checks and reports the live ones as skipped).
    let mut order = vec![pm_alias.clone()];
    order.extend(group_members(state_dir, &pm_alias)?);
    print_json(&resume_sweep(state_dir, &order));
    if detach {
        return Ok(0);
    }
    // Attach to the PM pane by default — same rules as provider_launch:
    // exec only where this terminal can.
    let pm = client::rpc(state_dir, "agent_show", json!({"alias": pm_alias}))?["agent"].clone();
    if pm["endpoint"].is_null() {
        return Ok(0);
    }
    if atty_stdin() && std::env::var_os("TMUX").is_none() {
        return attach_agent(state_dir, &pm_alias, true);
    }
    attach_agent(state_dir, &pm_alias, false)
}

/// `cadence stop <group>`: members first, then the PM — agents stay
/// registered and resumable. Per-member outcomes are reported, never
/// silently dropped.
pub(crate) fn stop_group(state_dir: &Path, group: &str) -> Result<i32> {
    let pm = client::rpc(state_dir, "agent_show", json!({"alias": group})).map_err(|_| {
        Error::rejected(format!(
            "Unknown group '{group}' — no such agent; \
                 `cadence agent list` shows registered aliases"
        ))
    })?["agent"]
        .clone();
    let pm_alias = pm["alias"].as_str().unwrap_or_default().to_string();
    let mut order = group_members(state_dir, &pm_alias)?;
    order.push(pm_alias);
    let mut stopped = vec![];
    let mut skipped = vec![];
    let mut failed = vec![];
    for alias in &order {
        // A mailbox is never "stopped" — it has no actor and its queue
        // is the point. Removing it is the only lifecycle action.
        let is_inbox = client::rpc(state_dir, "agent_show", json!({"alias": alias}))
            .map(|s| {
                !registry::has_actor(
                    s["agent"]["provider"].as_str().unwrap_or_default(),
                    s["agent"]["endpoint_kind"].as_str().unwrap_or_default(),
                )
            })
            .unwrap_or(false);
        if is_inbox {
            eprintln!("stop {alias}: skipped (inbox — durable mailbox)");
            skipped.push(json!({"alias": alias}));
            continue;
        }
        match client::rpc(state_dir, "agent_stop", json!({"alias": alias})) {
            Ok(r) => {
                eprintln!("stop {alias}: {}", r["state"].as_str().unwrap_or("ok"));
                stopped.push(json!({"alias": alias, "state": r["state"]}));
            }
            Err(e) => {
                eprintln!("stop {alias}: FAILED — {e}");
                failed.push(json!({"alias": alias, "error": e.to_string()}));
            }
        }
    }
    print_json(&json!({"stopped": stopped, "skipped": skipped, "failed": failed}));
    Ok(0)
}

/// Email-style flags callers guess for `message send` / `send`.
pub(crate) const EMAIL_FLAGS: [&str; 4] = ["--to", "--subject", "--body", "--cc"];

/// `message send --to …` would get clap's `to pass '--to' as a value,
/// use '-- --to'` tip, which steers the caller to smuggle the flag in as
/// text. Swap that for the real usage line; every other parse error
/// (help and version included) passes through untouched.
pub(crate) fn email_flag_error(err: &clap::Error) -> Option<clap::Error> {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    if err.kind() != ErrorKind::UnknownArgument {
        return None;
    }
    let Some(ContextValue::String(arg)) = err.get(ContextKind::InvalidArg) else {
        return None;
    };
    let flag = arg.split('=').next().unwrap_or_default();
    if !EMAIL_FLAGS.contains(&flag) {
        return None;
    }
    // The usage line names the subcommand the parse failed in.
    let Some(ContextValue::StyledStr(usage)) = err.get(ContextKind::Usage) else {
        return None;
    };
    let usage = usage.to_string();
    let verb = if usage.contains(" message send ") {
        "message send"
    } else if usage.contains(" send ") {
        "send"
    } else {
        return None;
    };
    // CAD-888: top-level `send` accepts `--to` as the recipient.
    let (banned, recipient) = if verb == "send" {
        (
            "--subject/--body/--cc",
            "the positional alias or --to <alias>",
        )
    } else {
        ("--to/--subject/--body/--cc", "the positional alias")
    };
    Some(clap::Error::raw(
        ErrorKind::UnknownArgument,
        format!(
            "`cadence {verb}` has no `{flag}` flag — it takes no email-style \
             {banned}; the recipient is {recipient}\n\n\
             Usage: cadence {verb} <ALIAS> --text <body>\n\n\
             \x20 body: --text <body>, -m <body> or --file <path>\n\
             \x20 multi-topic report: open the body with `SUBJECT: <topic>`\n\n\
             For more information, try 'cadence {verb} --help'.\n"
        ),
    ))
}
