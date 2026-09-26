//! CAD-535: `cadence agent` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum AgentAction {
    /// Register an agent and start its actor.
    Register {
        alias: String,
        /// Provider driver: codex (managed) or fake (test double).
        #[arg(long)]
        provider: String,
        /// Endpoint kind: managed (stdio), managed-ws (official-TUI
        /// attachable WebSocket app-server), pty (official TUI in an
        /// owned tmux session), cloud (Devin Cloud v3 API) or fake
        /// (test double).
        #[arg(long, default_value = registry::DEFAULT_ENDPOINT_KIND)]
        endpoint: String,
        /// pm, worker, or reviewer. `reviewer` is who a delivery review
        /// may be routed to; it does not grant PM authority.
        #[arg(long, default_value = "worker")]
        role: String,
        /// read-only or workspace-write.
        #[arg(long, default_value = "read-only")]
        sandbox: String,
        /// File with reusable provider instructions.
        #[arg(long)]
        instructions_file: Option<PathBuf>,
        /// Endpoint option as key=value (pty: session=<native-id> to
        /// resume an existing Devin session). Repeatable.
        #[arg(long = "param")]
        params: Vec<String>,
        /// Team role used only to look up a model default. Does not
        /// change runtime pm/worker authorization. `ops` is stored as
        /// `devops`.
        #[arg(long)]
        team_role: Option<String>,
        /// Use the provider's native model instead of a daemon default.
        /// Refused together with `--param model=…` and on endpoints
        /// that cannot accept a model.
        #[arg(long)]
        provider_default_model: bool,
        /// Working directory for the provider session [default: current
        /// directory]. Meaningless for `--provider inbox` — a mailbox
        /// has no working directory.
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// List registered agents. Inside a cadence pane (`CADENCE_ALIAS`
    /// resolves to a registered agent) the output is scoped to the
    /// caller's group — the group root plus agents whose
    /// `params.upstream` names it — and the root row is marked
    /// `"group_root": true`. Filters narrow that scope, never widen
    /// it. Value flags repeat and comma-join and match ANY of their
    /// values; different flags AND.
    #[command(after_long_help = cadence_agent::filter::GRAMMAR)]
    List {
        /// Show every agent even inside a cadence pane.
        #[arg(long)]
        all: bool,
        /// Agent state (starting idle busy waiting_input attention
        /// stopping stopped offline); repeatable — daemon-side.
        #[arg(long, value_delimiter = ',')]
        state: Vec<String>,
        /// Provider (codex claude devin cursor fake inbox …);
        /// repeatable — daemon-side.
        #[arg(long, value_delimiter = ',')]
        provider: Vec<String>,
        /// Endpoint kind (managed managed-ws pty cloud fake inbox);
        /// repeatable — daemon-side.
        #[arg(long, value_delimiter = ',')]
        kind: Vec<String>,
        /// Tracker project the agent's `cwd` resolves to; repeatable —
        /// an agent whose cwd maps to no project never matches. Needs
        /// a PM dir.
        #[arg(long, value_delimiter = ',')]
        project: Vec<String>,
        /// Sort by alias state provider kind role project cwd model;
        /// `-KEY` descending.
        #[arg(long, allow_hyphen_values = true)]
        sort: Option<String>,
        /// Keep only the first N rows.
        #[arg(long)]
        limit: Option<usize>,
        /// Keep only these keys in each row (comma-joined).
        #[arg(long, value_delimiter = ',')]
        fields: Vec<String>,
        /// Output is JSON already — accepted for grammar parity.
        #[arg(long)]
        json: bool,
    },
    /// Show one agent, its messages and event cursor.
    Show { alias: String },
    /// List pending provider requests (approvals, input).
    Requests { alias: String },
    /// Answer a pending provider request.
    Respond {
        alias: String,
        /// Request handle from `agent requests`.
        #[arg(long)]
        request: String,
        /// accept or decline for approval requests.
        #[arg(long)]
        decision: Option<String>,
        /// JSON answers file for input requests.
        #[arg(long)]
        answers_file: Option<PathBuf>,
        /// Operator note carried on a brokered decline — handed to the
        /// provider as the denial message.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Reconcile every `unknown` message fencing the agent, then resume
    /// it (`--no-resume` leaves it stopped). Same reconcile rules and
    /// events as `message reconcile`; history is never discarded.
    Unfence {
        alias: String,
        /// Terminal state recorded for each reconciled message
        /// [default: interrupted].
        #[arg(long, value_enum, default_value_t = ReconcileStatus::Interrupted)]
        status: ReconcileStatus,
        /// Single-line note recorded with each reconcile event.
        #[arg(long)]
        note: Option<String>,
        /// Reconcile without restarting the agent.
        #[arg(long)]
        no_resume: bool,
    },
    /// Stop the agent's actor (queued messages are retained).
    Stop { alias: String },
    /// Resume a stopped agent on its saved native thread. Like a
    /// provider launch: waits for the endpoint to open (bounded), then
    /// attaches this terminal by default — `--detach` opts out, and a
    /// non-TTY or nested-tmux context prints the attach command instead.
    Resume {
        alias: String,
        /// Do not attach once the endpoint is up.
        #[arg(long)]
        detach: bool,
    },
    /// Show or run the official attach command for an attachable
    /// endpoint (managed-ws: `codex resume --remote`; pty: tmux attach).
    Attach {
        alias: String,
        /// Execute the attach in this terminal instead of printing it.
        #[arg(long)]
        run: bool,
    },
    /// Claim a gated endpoint is ready for one submission (pty only).
    /// Runs the same screen probe verified auto-ready uses and refuses
    /// a visibly busy pane — `--force` claims anyway and is recorded.
    /// Consumed by a single send, expires quickly.
    /// Claims stack — N claims release N queued messages.
    Ready {
        alias: String,
        /// Claim even when the pane probes busy.
        #[arg(long)]
        force: bool,
    },
    /// Print the current terminal contents of a pty endpoint.
    Capture { alias: String },
    /// Reduce a pty pane to gate facts: `{idle, reason, input_nonempty,
    /// prompt_visible, busy_marker, approval_menu}` — the same probe the
    /// verified auto-ready mode runs before self-claiming.
    Probe { alias: String },
    /// Send one menu-choice keystroke to a pty pane currently probing
    /// `approval_menu` — refuses anything else, like `agent ready`
    /// refuses a busy pane. `<choice>` is the option's printed index;
    /// records `approval_answered` with the answerer and the menu line.
    Answer {
        alias: String,
        /// The option's printed index on the open menu.
        choice: String,
        /// Operator note recorded with the answer event.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Submit a pty message that was pasted but never submitted — the
    /// draft still sits in the input line while the message stays
    /// `running` — with exactly one Enter, never a re-paste, so the
    /// message keeps its turn token and report path (CAD-152).
    ///
    /// Refuses and sends nothing unless every check holds at action
    /// time: the message is the agent's pending pasted turn (running,
    /// unacknowledged, token from the live endpoint generation), the
    /// pane shows a non-empty input with no busy marker and no approval
    /// menu, and the visible draft is the message body (normalised for
    /// wrapping). Each refusal names its check. A second recovery of the
    /// same message refuses. Only the operator or the agent's PM may
    /// run it (caller rule, CAD-149). Records `submit_recovered` or
    /// `submit_recover_refused` — never the message body. Exits 1 when
    /// the Enter went out but the draft did not visibly leave the input
    /// (unconfirmed: inspect, never retried).
    RecoverSubmit {
        alias: String,
        /// The stuck message's id.
        #[arg(long)]
        message: String,
        /// The endpoint generation you inspected (from `agent show`);
        /// refuses if the endpoint has been relaunched since.
        #[arg(long)]
        generation: Option<String>,
    },
    /// Merge `key=value` pairs into an agent's endpoint params — e.g.
    /// `agent set <alias> auto_ready=verified` opts a live agent into
    /// daemon-verified readiness.
    ///
    /// `agent set <alias> auto_stop=off` opts an agent out of the
    /// daemon's idle auto-stop (default ON: an agent with nothing queued,
    /// running, awaiting a report or unknown for 60 minutes is stopped,
    /// resumably); `auto_stop_idle_secs=<n>` sets this agent's own bound
    /// (0 = off). A bare `auto_stop` / `auto_stop_idle_secs` removes the
    /// override.
    ///
    /// `--next-launch` stores launch params (`model`, `effort`) for the
    /// agent's next open instead — the live process is untouched; `agent
    /// stop` + `agent resume` picks them up.
    ///
    /// Caller rule (CAD-149), derived from the calling process, never
    /// from a name: the operator and the agent's own PM may set any
    /// allowed key; an agent may set only its own `--next-launch`
    /// model/effort; a peer is refused. Each change is recorded as
    /// `params_updated` with the caller and old/new values. Residual
    /// (CAD-280): a process detached from every pane (`setsid -f env
    /// -i …`) passes the operator check, so `by: "operator"` is not
    /// proof the operator acted.
    Set {
        alias: String,
        /// key=value pairs; a bare `key` (no `=`) removes it.
        pairs: Vec<String>,
        /// Store `model`/`effort` for the next launch rather than live.
        #[arg(long)]
        next_launch: bool,
    },
    /// Remove a dead agent's registry row — and with it the message and
    /// event history no job references (job kickoffs, verdict messages
    /// and job-scoped events stay). Refuses while an endpoint is live
    /// (`agent stop` first), the actor still owns the alias, or the
    /// alias has open messages or non-terminal assigned tasks. Only the
    /// operator or the agent's own PM may remove it (CAD-304); every
    /// removal records `agent_removed` with the caller.
    Remove {
        alias: String,
        /// Remove despite open messages/tasks: queued messages are
        /// cancelled and running ones interrupted through the normal
        /// finish path (their `reply_to` is notified); recorded as an
        /// `agent_remove_forced` event on the daemon stream. Never
        /// deletes or decides an `unknown` message: refused while one
        /// exists — reconcile it first (`message reconcile`, or `agent
        /// unfence --no-resume` for a fencing one). Non-terminal tasks
        /// assigned to the alias are unassigned (state and history
        /// kept; the job's PM is told to `job dispatch <task> --to
        /// <worker>`), so a later agent under the alias inherits none.
        #[arg(long)]
        force: bool,
    },
    /// Write (or refresh) an agent's briefing file + AGENTS.md block and
    /// enqueue it as a durable message — the retrofit for agents
    /// launched before briefings existed. Refuses an unknown alias;
    /// pty targets still need the usual ready claim.
    Bootstrap { alias: String },
    /// Sweep dead agent records: endpoint NULL and state `attention` or
    /// `stopped`. Prints what it removed. No memory or disk remedy.
    /// Each candidate passes the `agent remove` caller rule: the operator
    /// sweeps all, a PM only its own members (the rest are listed as
    /// `not_permitted`).
    ///
    /// Records only: it deletes registry rows with their message and
    /// event history. It frees no disk and is no memory remedy — the one
    /// process it touches is a fenced pty agent's surviving pane, killed
    /// so no orphan outlives its row. A removed agent can no longer be
    /// resumed.
    ///
    /// The daemon runs the same sweep on a timer ONLY when pm.yaml sets
    /// `[host] agent_gc_older_than_secs` (off by default; 7-day floor; at
    /// most hourly). The timer additionally keeps enabled agents, pty
    /// agents whose pane is up, and any agent with a queued, running or
    /// unknown message, and records `agent_gc_removed` per row on the
    /// daemon event stream. The timer kills nothing: it frees no memory
    /// and no disk. `cadence daemon status` shows the setting.
    Gc {
        /// Only remove agents last updated more than this long ago
        /// (e.g. 30m, 12h, 7d; bare number = seconds).
        #[arg(long)]
        older_than: Option<String>,
    },
}

pub(super) fn run(state_dir: PathBuf, action: AgentAction) -> Result<i32> {
    let result = match action {
        AgentAction::Register {
            alias,
            provider,
            endpoint,
            cwd,
            role,
            sandbox,
            instructions_file,
            params,
            team_role,
            provider_default_model,
        } => {
            let instructions = match instructions_file {
                Some(path) => Some(std::fs::read_to_string(path)?),
                None => None,
            };
            let mut obj = serde_json::Map::new();
            for kv in &params {
                let (k, v) = kv
                    .split_once('=')
                    .ok_or_else(|| Error::rejected("--param entries must be key=value"))?;
                if k.is_empty() {
                    return Err(Error::rejected("--param key must not be empty"));
                }
                obj.insert(k.to_string(), Value::String(v.to_string()));
            }
            let has_explicit_model = obj.contains_key("model");
            let params_json = (!obj.is_empty()).then(|| Value::Object(obj).to_string());
            // `--provider inbox` is the mailbox registration —
            // the endpoint kind follows the provider, and no
            // working directory is involved.
            let inbox = registry::is_inbox_provider(&provider);
            let endpoint = if inbox && endpoint == registry::DEFAULT_ENDPOINT_KIND {
                registry::INBOX.to_string()
            } else {
                endpoint
            };
            if provider_default_model {
                if has_explicit_model {
                    return Err(Error::invalid(
                        "conflicting_model_policy",
                        "an explicit model cannot be combined with --provider-default-model",
                    ));
                }
                if !registry::supports_model(&provider, &endpoint) {
                    return Err(Error::invalid(
                        "unsupported_model_setting",
                        format!(
                            "provider '{provider}' endpoint '{endpoint}' does not accept a model"
                        ),
                    ));
                }
            }
            let cwd = match cwd {
                Some(c) => c,
                None => std::env::current_dir()?,
            };
            client::rpc(
                &state_dir,
                "agent_register",
                json!({
                    "alias": alias, "provider": provider,
                    "endpoint_kind": endpoint, "cwd": cwd,
                    "role": role, "sandbox": sandbox,
                    "instructions": instructions,
                    "params": params_json,
                    "team_role": team_role,
                    "model_policy": if provider_default_model {
                        Some("provider_default")
                    } else {
                        None::<&str>
                    },
                }),
            )?
        }
        AgentAction::List {
            all,
            state,
            provider,
            kind,
            project,
            sort,
            limit,
            fields,
            json: _,
        } => {
            // Stamp rows with their tracker project when the filter or
            // the sort reads it.
            let stamp = !project.is_empty() || sort.as_deref() == Some("project");
            let mut out = list_agents(&state_dir, &state, &provider, &kind, &project, all, stamp)?;
            shape_rows(
                &mut out,
                "agents",
                sort.as_deref(),
                &[
                    ("alias", "alias"),
                    ("state", "state"),
                    ("provider", "provider"),
                    ("kind", "endpoint_kind"),
                    ("role", "role"),
                    ("project", "project"),
                    ("cwd", "cwd"),
                    ("model", "model"),
                ],
                "alias",
                limit,
                &fields,
            )?;
            out
        }
        AgentAction::Show { alias } => {
            client::rpc(&state_dir, "agent_show", json!({"alias": alias}))?
        }
        AgentAction::Requests { alias } => {
            client::rpc(&state_dir, "agent_requests", json!({"alias": alias}))?
        }
        AgentAction::Respond {
            alias,
            request,
            decision,
            answers_file,
            reason,
        } => {
            let answers = match answers_file {
                Some(path) => Some(serde_json::from_str::<Value>(&std::fs::read_to_string(
                    path,
                )?)?),
                None => None,
            };
            client::rpc(
                &state_dir,
                "agent_respond",
                json!({"alias": alias, "request": request,
                       "decision": decision, "answers": answers,
                       "reason": reason}),
            )?
        }
        AgentAction::Unfence {
            alias,
            status,
            note,
            no_resume,
        } => {
            let mut result = client::rpc(
                &state_dir,
                "agent_unfence",
                json!({"alias": alias, "status": status.as_str(),
                       "note": note, "resume": !no_resume}),
            )?;
            if no_resume {
                print_json(&result);
                return Ok(0);
            }
            // The daemon waited for the open; surface what it
            // actually did. A failed resume gets the same
            // recovery hint `agent resume` prints.
            let show = client::rpc(&state_dir, "agent_show", json!({"alias": alias}))?;
            let agent = show["agent"].clone();
            result["endpoint"] = agent["endpoint"].clone();
            if result["resumed"] != json!(true) {
                if result["state"] == "attention" {
                    result["next"] = fenced_next(
                        &alias,
                        agent["error"].as_str().unwrap_or_default(),
                        show["unknown"].as_i64().unwrap_or(0),
                    );
                }
                print_json(&result);
                return Ok(0);
            }
            let (provider, kind) = (
                agent["provider"].as_str().unwrap_or_default(),
                agent["endpoint_kind"].as_str().unwrap_or_default(),
            );
            if !registry::attachable(provider, kind) || agent["endpoint"].is_null() {
                print_json(&finish_resume(&state_dir, &alias, result));
                return Ok(0);
            }
            result["next"] = json!({"attach": format!("cadence agent attach {alias}")});
            print_json(&finish_resume(&state_dir, &alias, result));
            if atty_stdin() && std::env::var_os("TMUX").is_none() {
                return attach_agent(&state_dir, &alias, true);
            }
            return attach_agent(&state_dir, &alias, false);
        }
        AgentAction::Stop { alias } => {
            client::rpc(&state_dir, "agent_stop", json!({"alias": alias}))?
        }
        AgentAction::Resume { alias, detach } => {
            return resume_agent(&state_dir, &alias, detach);
        }
        AgentAction::Attach { alias, run } => {
            return attach_agent(&state_dir, &alias, run);
        }
        AgentAction::Ready { alias, force } => {
            // The claimer identity is recorded for audit —
            // CADENCE_ALIAS when the claim came from a pane.
            let by = std::env::var("CADENCE_ALIAS").ok();
            client::rpc(
                &state_dir,
                "agent_ready",
                json!({"alias": alias, "by": by, "force": force}),
            )?
        }
        AgentAction::Probe { alias } => {
            client::rpc(&state_dir, "agent_probe", json!({"alias": alias}))?
        }
        AgentAction::Answer {
            alias,
            choice,
            reason,
        } => {
            let by = std::env::var("CADENCE_ALIAS").ok();
            client::rpc(
                &state_dir,
                "agent_answer",
                json!({"alias": alias, "choice": choice, "by": by, "note": reason}),
            )?
        }
        AgentAction::RecoverSubmit {
            alias,
            message,
            generation,
        } => {
            let out = client::rpc(
                &state_dir,
                "agent_recover_submit",
                json!({"alias": alias, "message": message, "generation": generation}),
            )?;
            print_json(&out);
            return Ok(if out["state"] == "submitted" { 0 } else { 1 });
        }
        AgentAction::Set {
            alias,
            pairs,
            next_launch,
        } => {
            let mut patch = serde_json::Map::new();
            for kv in &pairs {
                match kv.split_once('=') {
                    Some((k, v)) => patch.insert(k.to_string(), Value::String(v.to_string())),
                    // A bare key deletes it from params.
                    None => patch.insert(kv.clone(), Value::Null),
                };
            }
            if patch.is_empty() {
                return Err(Error::rejected(
                    "agent set needs key=value pairs — e.g. \
                     `cadence agent set <alias> auto_ready=verified`",
                ));
            }
            client::rpc(
                &state_dir,
                "agent_set",
                json!({"alias": alias, "patch": patch, "next_launch": next_launch}),
            )?
        }
        AgentAction::Capture { alias } => {
            let out = client::rpc(&state_dir, "agent_capture", json!({"alias": alias}))?;
            if let Some(text) = out["capture"].as_str() {
                println!("{text}");
                return Ok(0);
            }
            out
        }
        AgentAction::Remove { alias, force } => client::rpc(
            &state_dir,
            "agent_remove",
            json!({"alias": alias, "force": force}),
        )?,
        AgentAction::Bootstrap { alias } => {
            let file = brief_agent(&state_dir, &alias, true, None)?;
            print_json(&json!({"alias": alias, "briefing": file,
                               "message": format!("bootstrap-{alias}")}));
            return Ok(0);
        }
        AgentAction::Gc { older_than } => {
            let secs = older_than.as_deref().map(parse_duration).transpose()?;
            client::rpc(&state_dir, "agent_gc", json!({"older_than": secs}))?
        }
    };
    print_json(&result);
    Ok(0)
}
