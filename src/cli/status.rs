//! CAD-535: `cadence status` — moved verbatim from src/main.rs.

use super::*;

/// `cadence status` — build the one-screen overview: per-agent rows
/// (state, running message age+head, queued/unknown, dead/resumable,
/// pane verdict, owned issues) plus the footer. `--group` scopes to
/// one root, `--all` to nothing smaller than the install; unset scopes
/// like `agent list` (the caller's group inside a pane, everything
/// otherwise) — `scope` in the payload names which applied.
pub(super) fn status_view(state_dir: &Path, group: Option<&str>, all: bool) -> Result<Value> {
    let list = list_agents(state_dir, &[], &[], &[], &[], group.is_some() || all, false)?;
    let mut agents = list["agents"].as_array().cloned().unwrap_or_default();
    let scope = if let Some(root) = group {
        agents.retain(|a| {
            a["alias"].as_str() == Some(root) || a["params"]["upstream"].as_str() == Some(root)
        });
        json!({"group": root})
    } else {
        list["scope"].clone()
    };
    // Tracker issues per owner — only when a tracker is reachable.
    // `views` gives the derived status the board shows; `load_all`
    // stays a filesystem read, and claim ages come from the tracker's
    // cached line times (CAD-403) — never a git walk per issue.
    let pm = cadence_agent::issue::Pm::open_default().ok();
    let tracker = pm.is_some();
    let mut owned_issues: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    // CAD-383: every doing/review issue with a holder, and its claim age.
    let mut claims: Vec<Value> = Vec::new();
    if let Some(pm) = &pm {
        use cadence_agent::issue::claim;
        let issues = cadence_agent::issue::board::load_all(&pm.dir, None).unwrap_or_default();
        let views = cadence_agent::issue::board::views(&pm.config.notes_dir(), issues);
        let times =
            cadence_agent::issue::line_times::LineTimes::load(&pm.dir, Duration::from_secs(3)).ok();
        let clock = claim::Clock::new(times.as_ref());
        let now = cadence_agent::issue::time::now_epoch();
        for v in views {
            if !matches!(v.status.as_str(), "doing" | "review") {
                continue;
            }
            let front = &v.issue.front;
            if let Some(owner) = &front.owner {
                owned_issues
                    .entry(owner.clone())
                    .or_default()
                    .push(front.id.clone());
            }
            if !claim::holders(front).is_empty() {
                let since = clock.since(&v.issue.project, front);
                let mut row = claim::row(&v.issue.project, front, since, now);
                row["status"] = json!(v.status);
                claims.push(row);
            }
        }
        claims.sort_by(|a, b| a["issue"].as_str().cmp(&b["issue"].as_str()));
        for ids in owned_issues.values_mut() {
            ids.sort();
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let mut rows = Vec::new();
    let mut unread_inboxes = Vec::new();
    // CAD-251: mailboxes past their unread threshold with no recent
    // `inbox_read` — named with count, oldest age and owner.
    let mut stale_inboxes = Vec::new();
    for a in &agents {
        let alias = a["alias"].as_str().unwrap_or_default().to_string();
        let provider = a["provider"].as_str().unwrap_or_default();
        let kind = a["endpoint_kind"].as_str().unwrap_or_default();
        let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))
            .unwrap_or_else(|_| json!({"messages": [], "queued": 0, "unknown": 0}));
        let queued = show["queued"].as_i64().unwrap_or(0);
        let unknown = show["unknown"].as_i64().unwrap_or(0);
        if provider == registry::INBOX && queued > 0 {
            unread_inboxes.push(alias.clone());
        }
        let health = &a["inbox_health"];
        if health["stale"].as_bool().unwrap_or(false) {
            stale_inboxes.push(json!({
                "alias": alias,
                "unread": health["unread"],
                "oldest_unread_age_secs": health["oldest_unread_age_secs"],
                "last_read_at": health["last_read_at"],
                "owner": health["owner"],
            }));
        }
        // The in-flight message: `running` (managed turn live) or
        // `submitted` (pty paste acknowledged, report pending). Age
        // reads from `started` — the dispatch time — falling back to
        // `created` for a queued-claim race.
        let running = show["messages"]
            .as_array()
            .and_then(|ms| {
                ms.iter().find(|m| {
                    matches!(
                        m["state"].as_str().unwrap_or_default(),
                        "running" | "submitted"
                    )
                })
            })
            .map(|m| {
                let started = m["started"]
                    .as_f64()
                    .or(m["created"].as_f64())
                    .unwrap_or(now);
                let head = m["body"]
                    .as_str()
                    .unwrap_or_default()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(50)
                    .collect::<String>();
                // CAD-250: a delivered pty turn still owed its report is
                // named as such, not as an ordinary running turn.
                json!({"id": m["id"], "age_secs": (now - started).max(0.0) as u64,
                       "text": head,
                       "awaiting_report": m["awaiting_report"].as_bool().unwrap_or(false)})
            });
        // One probe per pty agent per invocation — and only for an
        // agent that actually has a pane (a live endpoint); a stopped
        // or paneless pty agent skips the tmux call entirely. The
        // verdict names the pane states that need a human first: an
        // approval menu (`approval: <menu line>`), then a pane idle on
        // a still-running message (`ended?: <age>` — the daemon's
        // sampled streak, not this one probe), then ordinary verdicts.
        let pane = if kind == "pty" && a["endpoint"].is_string() {
            client::rpc(state_dir, "agent_probe", json!({"alias": alias}))
                .ok()
                .map(|p| {
                    let idle = p["idle"].as_bool().unwrap_or(false);
                    let menu = p["approval_menu"].as_bool().unwrap_or(false);
                    let ended = a["ended_secs"].as_u64();
                    let verdict = if menu {
                        format!("approval: {}", p["reason"].as_str().unwrap_or(""))
                    } else if idle {
                        match ended {
                            Some(secs) if running.is_some() => {
                                format!("ended?: {}", fmt_age(secs as i64))
                            }
                            _ => "idle".to_string(),
                        }
                    } else {
                        format!("busy: {}", p["reason"].as_str().unwrap_or(""))
                    };
                    json!({"idle": p["idle"], "reason": p["reason"],
                           "verdict": verdict})
                })
        } else {
            None
        };
        let issues = owned_issues.get(&alias).cloned().unwrap_or_default();
        let held: Vec<Value> = claims
            .iter()
            .filter(|c| c["by"] == alias || c["owner"] == alias)
            .cloned()
            .collect();
        rows.push(json!({
            "alias": alias,
            "provider": provider,
            "endpoint_kind": kind,
            "state": a["state"].as_str().unwrap_or_default(),
            // CAD-96: `stopped (auto, idle 72m)` for an idle auto-stop.
            "state_label": a["state_label"],
            "auto_stopped": a["auto_stopped"],
            "dead": a["dead"].as_bool().unwrap_or(false),
            "resumable": a["resumable"].as_bool().unwrap_or(false),
            "running": running,
            // CAD-250: the daemon's `awaiting_report` view (wait, bound,
            // queued behind it) — null when no turn awaits a report.
            "awaiting_report": a["awaiting_report"].clone(),
            "queued": queued,
            "unknown": unknown,
            "pane": pane,
            "issues": issues,
            // CAD-383: doing/review issues this agent owns or claims,
            // with the claim age.
            "claims": held,
            // CAD-202: the pane's cwd was deleted — delivery refuses.
            "cwd_deleted": show["agent"]["cwd_deleted"].as_bool().unwrap_or(false),
        }));
    }
    let mut states: serde_json::Map<String, Value> = serde_json::Map::new();
    for r in &rows {
        let state = r["state"].as_str().unwrap_or("?");
        let count = states.get(state).and_then(Value::as_i64).unwrap_or(0) + 1;
        states.insert(state.to_string(), json!(count));
    }
    // CAD-113: slot occupancy rides the footer — best-effort and
    // time-boxed: a wedged daemon must not hang the screen.
    let slots = client::rpc_timeout(
        state_dir,
        "slot_status",
        json!({"lane": cadence_agent::slots::default_lane()}),
        Duration::from_secs(2),
    )
    .ok();
    Ok(json!({
        "agents": rows,
        // CAD-576: which agents the rows (and the footer's counts)
        // cover — "all", or the one group root. A reader never has to
        // guess a count's boundary from an absent field.
        "scope": scope,
        "footer": {
            "states": states,
            "unread_inboxes": unread_inboxes,
            "stale_inboxes": stale_inboxes,
            "slots": slots,
            // CAD-383: every in-flight claim, listed agents or not — a
            // PM whose lanes run outside cadence shows up here.
            "claims": claims,
        },
        "tracker": tracker,
    }))
}

/// Aligned-table rendering of `status_view` — the TTY default.
pub(super) fn print_status_table(view: &Value) {
    let agents = view["agents"].as_array().cloned().unwrap_or_default();
    let rows: Vec<[String; 9]> = agents
        .iter()
        .map(|a| {
            let running = if a["running"].is_object() {
                format!(
                    "{}m {}",
                    (a["running"]["age_secs"].as_u64().unwrap_or(0) + 30) / 60,
                    a["running"]["text"].as_str().unwrap_or_default()
                )
            } else {
                "-".to_string()
            };
            let mut flags = Vec::new();
            if a["state"].as_str() == Some("attention") {
                flags.push("fenced");
            }
            if a["dead"].as_bool().unwrap_or(false) {
                flags.push("dead");
            }
            if a["resumable"].as_bool().unwrap_or(false) {
                flags.push("resumable");
            }
            if a["cwd_deleted"].as_bool().unwrap_or(false) {
                flags.push("cwd_deleted");
            }
            if a["awaiting_report"].is_object() {
                flags.push("awaiting_report");
            }
            let pane = a["pane"]["verdict"].as_str().unwrap_or("-").to_string();
            // CAD-383: each owned or claimed issue with its claim age.
            let issues = a["claims"]
                .as_array()
                .map(|cs| {
                    cs.iter()
                        .map(|c| {
                            let id = c["issue"].as_str().unwrap_or_default();
                            match c["age_secs"].as_i64() {
                                Some(age) => format!("{id}({})", fmt_age(age)),
                                None => id.to_string(),
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            [
                a["alias"].as_str().unwrap_or_default().to_string(),
                format!(
                    "{}/{}",
                    a["provider"].as_str().unwrap_or_default(),
                    a["endpoint_kind"].as_str().unwrap_or_default()
                ),
                a["state_label"]
                    .as_str()
                    .or(a["state"].as_str())
                    .unwrap_or_default()
                    .to_string(),
                running,
                a["queued"].as_i64().unwrap_or(0).to_string(),
                a["unknown"].as_i64().unwrap_or(0).to_string(),
                if flags.is_empty() {
                    "-".to_string()
                } else {
                    flags.join(",")
                },
                pane,
                if issues.is_empty() {
                    "-".to_string()
                } else {
                    issues
                },
            ]
        })
        .collect();
    let headers = [
        "ALIAS", "ENDPOINT", "STATE", "RUNNING", "QUE", "UNK", "FLAGS", "PANE", "ISSUES",
    ];
    let mut widths = headers.map(str::len);
    for r in &rows {
        for (i, cell) in r.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let line = |cells: &[String; 9]| {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{:<width$}", c, width = widths[i]))
            .collect::<Vec<_>>()
            .join("  ")
    };
    let head: [String; 9] = headers.map(|h| h.to_string());
    println!("{}", line(&head));
    for r in &rows {
        println!("{}", line(r));
    }
    // Footer: which agents the rows cover, counts by state, and
    // inboxes holding unread messages.
    let scope = match &view["scope"] {
        Value::Object(o) => format!("group {}", o["group"].as_str().unwrap_or("?")),
        _ => "all".to_string(),
    };
    println!();
    println!("scope: {scope}");
    let states = view["footer"]["states"]
        .as_object()
        .map(|m| {
            let mut pairs: Vec<(&String, &Value)> = m.iter().collect();
            pairs.sort_by(|a, b| a.0.cmp(b.0));
            pairs
                .iter()
                .map(|(k, v)| format!("{k}:{}", v.as_i64().unwrap_or(0)))
                .collect::<Vec<_>>()
                .join("  ")
        })
        .unwrap_or_else(|| "none".to_string());
    println!("agents: {states}");
    let unread = view["footer"]["unread_inboxes"]
        .as_array()
        .map(|v| v.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    if !unread.is_empty() {
        println!("unread: {}", unread.join(", "));
    }
    let stale = view["footer"]["stale_inboxes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !stale.is_empty() {
        let names: Vec<String> = stale
            .iter()
            .map(|s| {
                format!(
                    "{} {} unread, oldest {}, owner {}",
                    s["alias"].as_str().unwrap_or_default(),
                    s["unread"].as_u64().unwrap_or(0),
                    fmt_age(s["oldest_unread_age_secs"].as_i64().unwrap_or(0)),
                    s["owner"].as_str().unwrap_or("operator"),
                )
            })
            .collect();
        println!("stale inboxes (no consumer): {}", names.join("; "));
    }
    // Slot occupancy — the one-line build-queue summary.
    let slots = &view["footer"]["slots"];
    if slots.is_object() {
        let held = |pool: &str| slots["pools"][pool]["held"].as_array().map_or(0, Vec::len);
        let cap = |pool: &str| slots["pools"][pool]["capacity"].as_u64().unwrap_or(0);
        let waiting = slots["waiting"].as_array().map_or(0, Vec::len);
        let longest = slots["waiting"]
            .as_array()
            .map(|w| {
                w.iter()
                    .map(|x| x["wait_secs"].as_f64().unwrap_or(0.0))
                    .fold(0.0, f64::max)
            })
            .unwrap_or(0.0);
        println!(
            "slots: {}/{} build, {}/{} suite; waiting: {}, longest {}",
            held("build"),
            cap("build"),
            held("suite"),
            cap("suite"),
            waiting,
            cadence_agent::slots::fmt_wait(longest)
        );
    }
    // CAD-383: claims held by someone with no row here — a PM whose
    // lanes run outside cadence, or an agent outside the scope.
    let listed: Vec<&str> = agents.iter().filter_map(|a| a["alias"].as_str()).collect();
    let unlisted: Vec<String> = view["footer"]["claims"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| {
            !listed.contains(&c["by"].as_str().unwrap_or_default())
                && !listed.contains(&c["owner"].as_str().unwrap_or_default())
        })
        .map(|c| {
            format!(
                "{} {} {}",
                c["issue"].as_str().unwrap_or_default(),
                c["by"].as_str().unwrap_or("?"),
                c["age_secs"]
                    .as_i64()
                    .map(fmt_age)
                    .unwrap_or_else(|| "age unknown".to_string())
            )
        })
        .collect();
    if !unlisted.is_empty() {
        println!("claims: {}", unlisted.join("; "));
    }
    if !view["tracker"].as_bool().unwrap_or(false) {
        println!("tracker: unreachable (no pm dir) — issue column empty");
    }
}

pub(super) fn run_status(
    state_dir: &Path,
    group: Option<&str>,
    all: bool,
    json_out: bool,
    watch: Option<u64>,
) -> Result<i32> {
    let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    loop {
        let view = status_view(state_dir, group, all)?;
        if json_out {
            print_json(&view);
        } else {
            print_status_table(&view);
        }
        let Some(secs) = watch else {
            return Ok(0);
        };
        std::thread::sleep(Duration::from_secs(secs));
        if tty && !json_out {
            // In-place refresh — a watch is one screen, not a scroll.
            // JSON output must stay a clean stream of documents.
            print!("\x1b[2J\x1b[H");
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
    }
}

pub(super) fn run(
    state_dir: PathBuf,
    group: Option<String>,
    all: bool,
    json: bool,
    watch: Option<u64>,
) -> Result<i32> {
    run_status(&state_dir, group.as_deref(), all, json, watch)
}
