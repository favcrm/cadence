//! CAD-535: `cadence daemon` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum DaemonAction {
    /// Run the daemon in the foreground.
    Run {
        /// Holder forwarded by `daemon start` / `daemon restart`.
        /// Not an operator flag — the public switch is `daemon start --as`.
        #[arg(long, hide = true)]
        rollout_as: Option<String>,
    },
    /// Start the daemon detached and print its state.
    Start {
        /// After the daemon is up, resume every resumable registered
        /// agent with no live endpoint.
        #[arg(long)]
        resume: bool,
        /// Identity when this shell is not a cadence pane. Required to
        /// start a binary whose commit differs from the one the daemon
        /// last recorded, and only if that identity holds the lease.
        #[arg(long = "as")]
        as_identity: Option<String>,
    },
    /// Report daemon health, including `agent_gc_timer`: whether the
    /// opt-in agent-gc timer is on (pm.yaml `[host]
    /// agent_gc_older_than_secs`), its effective age, and its last sweep;
    /// and `agent_auto_stop`: the idle auto-stop bound (default ON, 3600s;
    /// `[host] auto_stop_idle_secs`, `auto_stop_idle_secs_by_provider`),
    /// what it last stopped, and why each live agent was kept.
    Status,
    /// Ask the daemon to shut down gracefully, then wait until the
    /// process has actually exited and released the state-dir lock
    /// (bounded, 30s) — `stop && start` no longer races the drain.
    /// `daemon stop` is lease-free. A following start of the same
    /// build is also lease-free.
    Stop,
    /// Stop, wait for exit, start, and report a before/after table of
    /// every agent's state (and pane pid for pty agents). The lease
    /// gates a build change and a schema crossing. This command still
    /// asks for the lease, but a same-build `daemon stop` followed by
    /// `daemon start` (or a crash restart of the same build) stays
    /// lease-free, so that same-build requirement is advisory.
    ///
    /// The verdict also reads the new daemon's recovery record: a
    /// restart that fenced any in-flight turn, whose predecessor's
    /// drain failed, or whose record is missing or unbound exits
    /// "not cleanly" — including a restart without --when-idle that
    /// sweeps stale running rows on stopped agents.
    Restart {
        /// First wait until every pty pane probes idle and no managed
        /// agent has a running message; on timeout nothing is changed.
        #[arg(long)]
        when_idle: bool,
        /// Seconds --when-idle waits for a quiet fleet before giving
        /// up [default: 1800].
        #[arg(long, default_value_t = 1800, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
        /// With --when-idle, carry the restart past stale turns —
        /// `running`/`submitted` messages on stopped or dead agents,
        /// which can never report. Default refuses, naming each stale
        /// turn and its remedy (CAD-503).
        #[arg(long)]
        ignore_stale: bool,
        /// Also restart the detached `cadence ui` server when one is
        /// running for this state dir.
        #[arg(long)]
        ui: bool,
        /// Identity when this shell is not a cadence pane. The restart
        /// is refused unless this identity holds the rollout lease.
        #[arg(long = "as")]
        as_identity: Option<String>,
    },
}

pub(super) fn run(state_dir: PathBuf, action: DaemonAction) -> Result<i32> {
    match action {
        DaemonAction::Run { rollout_as } => {
            // CAD-308: before anything is spawned, so every tree the
            // daemon launches keeps its orphans under the daemon.
            cadence_agent::reaper::enable()?;
            cadence_agent::rollout::set_forwarded_identity(rollout_as);
            // Never keep the holder in the environment, even if the
            // parent shell exported it. Panes inherit the daemon's env.
            std::env::remove_var("CADENCE_ROLLOUT_AS");
            // CAD-1922: a state dir inside the consolidated home is
            // created 0700 along with the home root; elsewhere this is
            // the same create_dir_all. The mode check below stays: an
            // existing state dir is tightened, never left wide.
            cadence_agent::home::create_home_dir(&state_dir)?;
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))?;
            }
            // Every daemon start re-syncs the vendored skill: a
            // rebuilt binary propagates changes. stderr lands in
            // daemon.log for the detached child — stdout stays silent.
            // A sandbox shares $HOME with production, so it never
            // writes the skill there.
            if cadence_agent::sandbox::profile().is_some() {
                eprintln!("skill: skipped (sandbox profile)");
            } else {
                match home_dir().map(|h| cadence_agent::skill::sync(&h, false, None)) {
                    Ok(Ok(report)) if report["wrote"].as_bool().unwrap_or(false) => {
                        eprintln!("skill: refreshed {}", report["root"])
                    }
                    Ok(Err(e)) => eprintln!("skill: refresh failed: {e}"),
                    _ => {}
                }
            }
            cadence_agent::daemon::serve(&state_dir)?;
            Ok(0)
        }
        DaemonAction::Start {
            resume,
            as_identity,
        } => {
            std::fs::create_dir_all(&state_dir)?;
            // CAD-396/407: an interrupted `restore --force` leaves the
            // old store renamed aside; a daemon started now would create
            // an empty store next to it. `serve` refuses too; this says
            // so before anything is spawned.
            cadence_agent::backup::refuse_interrupted_restore(&state_dir)?;
            cadence_agent::backup::portable::refuse_if_inactive(&state_dir)?;
            let mut result = client::daemon_start_as(&state_dir, as_identity.as_deref())?;
            // --resume: once the daemon answers, sweep every agent
            // with a stored thread and no live endpoint.
            if resume {
                let targets = client::rpc(&state_dir, "agent_list", json!({}))?["agents"]
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
                result["resume"] = resume_sweep(&state_dir, &targets);
            }
            print_json(&result);
            Ok(0)
        }
        DaemonAction::Status => {
            let health = client::rpc(&state_dir, "health", json!({}))?;
            print_json(&health);
            Ok(0)
        }
        DaemonAction::Stop => daemon_stop(&state_dir),
        DaemonAction::Restart {
            when_idle,
            timeout,
            ignore_stale,
            ui,
            as_identity,
        } => daemon_restart(
            &state_dir,
            when_idle,
            timeout,
            ignore_stale,
            ui,
            as_identity,
        ),
    }
}

/// Has the daemon released the state-dir singleton? `serve` holds an
/// exclusive `flock` on `cadence.lock` for its whole life; the kernel
/// drops it only when the process exits, so a successful non-blocking
/// lock probe is the exact "old daemon is gone" signal `daemon stop`
/// must wait for before a `daemon start` can win the same lock.
pub(crate) fn daemon_lock_free(state_dir: &Path) -> bool {
    use std::os::unix::io::AsRawFd;
    let Ok(file) = std::fs::OpenOptions::new()
        .write(true)
        .open(state_dir.join("cadence.lock"))
    else {
        // No lock file yet — no daemon ever owned this dir.
        return true;
    };
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    // The File drops here either way — a successful probe must not
    // keep the lock it was only testing.
    rc == 0
}

/// Poll until the daemon releases `cadence.lock`, bounded. Returns
/// false on timeout.
pub(crate) fn wait_daemon_exit(state_dir: &Path, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if daemon_lock_free(state_dir) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    daemon_lock_free(state_dir)
}

/// `daemon stop`: ask for shutdown, then wait until the process has
/// actually exited — the rpc returns while the daemon is still
/// draining actors, and an immediate `daemon start` would lose the
/// singleton race without this wait.
pub(crate) fn daemon_stop(state_dir: &Path) -> Result<i32> {
    let result = client::rpc(state_dir, "shutdown", json!({}))?;
    let exited = wait_daemon_exit(state_dir, 30);
    let mut out = result;
    out["exited"] = json!(exited);
    print_json(&out);
    if exited {
        Ok(0)
    } else {
        Err(Error::rejected(
            "daemon did not exit within 30s — it is still draining; \
             retry `daemon stop` or inspect daemon.log",
        ))
    }
}

/// CAD-503: can an in-flight turn on this agent still report? `stopped`,
/// `offline` and `attention` are settled no-actor states, and `dead` is
/// the daemon's own no-live-endpoint/no-pid verdict — a `running` or
/// `submitted` row under either is stale and can never finish, so waiting
/// on it is a deadlock (F19). `starting`/`stopping` still hold an actor
/// mid-transition — their rows stay live blockers.
pub(crate) fn stale_holder(agent: &Value) -> bool {
    match agent["state"].as_str().unwrap_or_default() {
        "stopped" | "offline" | "attention" => true,
        "starting" | "stopping" => false,
        _ => agent["dead"].as_bool().unwrap_or(false),
    }
}

/// One status line for the restart wait loops, split by whether waiting
/// can help: `busy` agents still holding the fleet (pty panes that probe
/// busy, actor agents with a `running`/`submitted` message on a live
/// actor) and `stale` turns — in-flight rows whose agent has no live
/// actor left to report them (CAD-503). Waiting on a stale turn is a
/// deadlock, so the caller refuses it (or carries it with
/// `--ignore-stale`) instead of blocking until the timeout.
pub(crate) fn busy_agents(
    state_dir: &Path,
    agents: &[Value],
) -> (Vec<String>, Vec<String>, Vec<(String, String)>) {
    let mut busy = Vec::new();
    let mut stale = Vec::new();
    let mut stale_pairs = Vec::new();
    for a in agents {
        let alias = a["alias"].as_str().unwrap_or_default();
        let provider = a["provider"].as_str().unwrap_or_default();
        let kind = a["endpoint_kind"].as_str().unwrap_or_default();
        if !registry::has_actor(provider, kind) {
            continue;
        }
        if kind == "pty" && a["endpoint"].is_string() {
            match client::rpc(state_dir, "agent_probe", json!({"alias": alias})) {
                Ok(probe) if !probe["idle"].as_bool().unwrap_or(false) => busy.push(format!(
                    "{alias}(busy: {})",
                    probe["reason"].as_str().unwrap_or("pane busy")
                )),
                _ => {}
            }
        }
        // In-flight turn on any actor endpoint — the pane probe can
        // read idle between paste and render, so the message row is
        // the authoritative in-flight signal.
        if let Ok(show) = client::rpc(state_dir, "agent_show", json!({"alias": alias})) {
            let inflight: Vec<&Value> = show["messages"]
                .as_array()
                .map(|ms| {
                    ms.iter()
                        .filter(|m| {
                            matches!(
                                m["state"].as_str().unwrap_or_default(),
                                "running" | "submitted"
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            if inflight.is_empty() {
                continue;
            }
            if stale_holder(a) {
                for m in inflight {
                    stale.push(format!(
                        "{alias}: {} ({})",
                        m["id"].as_str().unwrap_or("?"),
                        m["state"].as_str().unwrap_or("?")
                    ));
                    stale_pairs.push((
                        alias.to_string(),
                        m["id"].as_str().unwrap_or("?").to_string(),
                    ));
                }
            } else {
                busy.push(format!("{alias}(running message)"));
            }
        }
    }
    (busy, stale, stale_pairs)
}

/// The detached UI's `ui run` argv from /proc — restarting the board
/// keeps the host/port/dist/allow-hosts it was actually started with
/// rather than assuming the defaults.
pub(crate) fn ui_run_args(pid: i32) -> (String, u16, Option<std::path::PathBuf>, Vec<String>) {
    let mut host = "127.0.0.1".to_string();
    let mut port = 3010u16;
    let mut dist = None;
    let mut allow_hosts = Vec::new();
    let Ok(bytes) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return (host, port, dist, allow_hosts);
    };
    let args: Vec<String> = bytes
        .split(|b| *b == 0)
        .filter_map(|s| String::from_utf8(s.to_vec()).ok())
        .collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--host" if i + 1 < args.len() => host = args[i + 1].clone(),
            "--port" if i + 1 < args.len() => {
                port = args[i + 1].parse().unwrap_or(3010);
            }
            "--dist" if i + 1 < args.len() => {
                dist = Some(std::path::PathBuf::from(&args[i + 1]));
            }
            "--allow-host" if i + 1 < args.len() => allow_hosts.push(args[i + 1].clone()),
            _ => {}
        }
        i += 1;
    }
    (host, port, dist, allow_hosts)
}

/// CAD-424: the replacement daemon refuses to start over an interrupted
/// restore's leftovers, so a restart that shut the old one down first
/// would leave no daemon at all and the recovery only in daemon.log.
/// Refuse with the same recovery message before anything stops.
pub(crate) fn refuse_restart_over_leftovers(state_dir: &Path) -> Result<()> {
    cadence_agent::backup::refuse_interrupted_restore(state_dir).map_err(|error| {
        Error::rejected(format!(
            "daemon restart refused before shutdown; the running daemon is untouched: {error}"
        ))
    })
}

/// `daemon restart`: stop, wait for the process to exit (the
/// singleton lock is the truth), start, wait until every agent that
/// was live before settles out of `starting`/`offline`, then print a
/// before/after table. `--when-idle` gates the whole thing on a
/// quiet fleet first; `--ui` bounces the detached board server too.
/// CAD-503: a `running`/`submitted` row on a stopped or dead agent is
/// stale — no actor exists to report it, so waiting is a deadlock
/// (F19). `--when-idle` refuses such turns early with their remedy
/// instead of timing out; `--ignore-stale` carries them through.
pub(crate) fn daemon_restart(
    state_dir: &Path,
    when_idle: bool,
    timeout: u64,
    ignore_stale: bool,
    ui: bool,
    as_identity: Option<String>,
) -> Result<i32> {
    refuse_restart_over_leftovers(state_dir)?;
    let caller =
        cadence_agent::rollout::resolve_caller(as_identity.as_deref()).map_err(|error| {
            Error::rejected(format!("daemon restart refused before shutdown: {error}"))
        })?;
    let ticket = cadence_agent::rollout::begin_restart(state_dir, &caller)?;
    // A failed replacement can leave no daemon serving the socket.
    // Preserve semantic refusals, but let transport failure reach the
    // shutdown/lock checks below so rollback can start the old release.
    let (before, reachable) = match client::rpc_answer(state_dir, "agent_list", json!({})) {
        Ok(Ok(answer)) => (
            answer["agents"].as_array().cloned().unwrap_or_default(),
            true,
        ),
        Ok(Err(refused)) => return Err(refused),
        Err(_) => {
            // A lost response does not prove the daemon is down. Never
            // skip --when-idle on a live lock holder whose fleet
            // snapshot was unavailable.
            if !daemon_lock_free(state_dir) {
                return Err(Error::rejected(
                    "daemon owns the state-dir lock but does not answer the \
                     socket — inspect daemon.log before restarting",
                ));
            }
            (Vec::new(), false)
        }
    };
    // Stale `(alias, message_id)` pairs observed while waiting — a row
    // swept to `unknown` by the replacement store's recovery fenced a
    // turn without any `turn_adopt_refused` event to catch it.
    let mut stale_turns: Vec<(String, String)> = Vec::new();
    if when_idle && reachable {
        let deadline = Instant::now() + Duration::from_secs(timeout);
        let mut next_report = Instant::now();
        let mut stale_noted = false;
        loop {
            let agents = client::rpc(state_dir, "agent_list", json!({}))?["agents"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let (busy, stale, pairs) = busy_agents(state_dir, &agents);
            if !stale.is_empty() {
                if !ignore_stale {
                    return Err(Error::rejected(format!(
                        "{} stale turn(s) on stopped or dead agents can never \
                         report — restart aborted before touching anything: {}. \
                         Settle each with `cadence agent resume <alias>` (the \
                         report bound then retires it for reconcile) or \
                         `cadence agent remove --force <alias>` (settles it now, \
                         dropping the agent), or pass --ignore-stale to carry \
                         the stale rows through the restart",
                        stale.len(),
                        stale.join(", ")
                    )));
                }
                if !stale_noted {
                    eprintln!(
                        "when-idle: ignoring {} stale turn(s) on stopped or \
                         dead agents: {}",
                        stale.len(),
                        stale.join(", ")
                    );
                    stale_noted = true;
                }
                for p in pairs {
                    if !stale_turns.contains(&p) {
                        stale_turns.push(p);
                    }
                }
            }
            if busy.is_empty() {
                break;
            }
            if Instant::now() >= deadline {
                return Err(Error::rejected(format!(
                    "fleet still busy after {timeout}s — restart aborted \
                     before touching anything: {}",
                    busy.join(", ")
                )));
            }
            if Instant::now() >= next_report {
                eprintln!("when-idle: waiting on {}", busy.join(", "));
                next_report = Instant::now() + Duration::from_secs(30);
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    // --ui restarts the detached board server when one was running —
    // snapshot before the daemon goes down so a ui that dies mid-way
    // isn't "rediscovered".
    let ui_was_running = ui
        .then(|| cadence_agent::ui::detached_pid(state_dir))
        .flatten();
    // Stop: the shutdown rpc returns while the daemon drains; the
    // lock probe is the real exit. A daemon that was never running
    // (lock free, socket dead) skips straight to start.
    // Per-alias event cursors taken now bound what this restart
    // produced — paged forward after the restart they cannot miss a
    // `turn_adopted`/`turn_adopt_refused` to a busy agent's tail page,
    // and an older restart's events can never masquerade as this
    // one's.
    let mut event_cursors: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();
    // Aliases whose pre-stop tail read failed — the verdict cannot
    // bound their event history, so it must fail rather than silently
    // drop the check (CAD-694).
    let mut cursor_misses: Vec<String> = Vec::new();
    for a in &before {
        if a["endpoint_kind"].as_str() != Some("pty") {
            continue;
        }
        let Some(alias) = a["alias"].as_str().map(str::to_string) else {
            continue;
        };
        match client::rpc(
            state_dir,
            "agent_events",
            json!({"alias": alias, "tail": true}),
        ) {
            Ok(v) => {
                event_cursors.insert(alias, v["cursor"].as_i64().unwrap_or(0));
            }
            Err(_) => cursor_misses.push(alias),
        }
    }
    // The lease can be released, handed off, or expire while --when-idle
    // waits. Re-check immediately before shutdown and do not restart
    // when this caller no longer holds it.
    cadence_agent::rollout::recheck_restart(state_dir, &ticket)?;
    // --when-idle can wait for minutes; look again right before shutdown.
    refuse_restart_over_leftovers(state_dir)?;
    // CAD-384: a daemon that ANSWERS is running — a refusal (the caller
    // rule: not the operator, not a granted lease holder) aborts the
    // restart here, before anything is recorded. Only an unreachable
    // socket means "not running".
    let was_running = if reachable {
        match client::rpc_answer(state_dir, "shutdown", json!({})) {
            Ok(Ok(_)) => true,
            Ok(Err(refused)) => return Err(refused),
            Err(_) => false,
        }
    } else {
        // Do not shut down a daemon that appeared after the free-lock
        // proof: it supplied no fleet snapshot or idle proof. The lock
        // is checked again below before attempting an offline start.
        false
    };
    if !was_running && !daemon_lock_free(state_dir) {
        return Err(Error::rejected(
            "daemon owns the state-dir lock but does not answer the \
             socket — inspect daemon.log before restarting",
        ));
    }
    cadence_agent::rollout::note_restart_proceeded(state_dir, &ticket)?;
    if was_running && !wait_daemon_exit(state_dir, 30) {
        return Err(Error::rejected(
            "daemon did not exit within 30s — restart aborted; the old \
             process is still draining (see daemon.log)",
        ));
    }
    let started = client::daemon_start_as(state_dir, Some(&caller.identity))?;
    // CAD-694: the recovery-record verdict binds to the instance THIS
    // start spawned. Only a `started` receipt pid-matches its health
    // answer to the spawned child; an `already_running` receipt carries
    // whichever daemon held the socket (a concurrent restart's), so it
    // binds nothing: the restart fails closed on it, never clean on
    // another run's evidence.
    let started_instance = started_instance(&started);
    let start_unbound = started["state"].as_str() != Some("started");
    if start_unbound {
        eprintln!(
            "restart: the start answered {} instead of `started` — that daemon is not \
             the one this restart spawned, so its recovery evidence is not bound to \
             this restart",
            started["state"].as_str().unwrap_or("without a state")
        );
    }
    // Wait until every agent that was live before leaves the
    // transitional states — `starting` (actor up, endpoint not open)
    // and `offline` (actor exited under shutdown). Stopped and fenced
    // agents keep their state; the wait is about liveness settling.
    let live_before: Vec<String> = before
        .iter()
        .filter(|a| {
            !matches!(
                a["state"].as_str().unwrap_or_default(),
                "stopped" | "attention" | "offline"
            )
        })
        .filter_map(|a| a["alias"].as_str().map(str::to_string))
        .collect();
    let settle_deadline = Instant::now() + Duration::from_secs(120);
    let after = loop {
        let after = client::rpc(state_dir, "agent_list", json!({}))?["agents"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let unsettled = after.iter().any(|a| {
            let alias = a["alias"].as_str().unwrap_or_default();
            live_before.iter().any(|l| l == alias)
                && matches!(
                    a["state"].as_str().unwrap_or_default(),
                    "starting" | "offline"
                )
        });
        if !unsettled || Instant::now() >= settle_deadline {
            break after;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    if let Some(ui_pid) = ui_was_running {
        cadence_agent::ui::run_cli(
            state_dir,
            &cadence_agent::ui::UiAction::Stop {
                tailscale_off: false,
                as_identity: None,
            },
        )?;
        // ui.json is the source of truth now; the /proc argv is only
        // the fallback for a server started before options persisted.
        let flags = if cadence_agent::ui::opts_present(state_dir) {
            cadence_agent::ui::UiFlags::default()
        } else {
            let (host, port, dist, allow_hosts) = ui_run_args(ui_pid);
            cadence_agent::ui::UiFlags {
                host: Some(host),
                port: Some(port),
                dist,
                allow_hosts,
                ..Default::default()
            }
        };
        // CAD-384: the board is the operator's, even when the rollout
        // owner restarts from its pane — a board that inherits the pane's
        // `CADENCE_ALIAS` fails operator proof on every operator write it
        // relays (`monitor_alert_ack`, `thread_send`, `model_defaults_set`).
        // The restart's caller was resolved above; nothing after this
        // reads the alias.
        std::env::remove_var("CADENCE_ALIAS");
        // CAD-1251: a restart's board start proves itself within the
        // update's health budget, not the interactive 10 s (an explicit
        // override wins).
        if std::env::var_os(cadence_agent::ui::START_BUDGET_ENV).is_none() {
            std::env::set_var(
                cadence_agent::ui::START_BUDGET_ENV,
                cadence_agent::ui::START_BUDGET_RESTART
                    .as_secs()
                    .to_string(),
            );
        }
        cadence_agent::ui::run_cli(
            state_dir,
            &cadence_agent::ui::UiAction::Start {
                flags,
                reset: false,
                as_identity: None,
            },
        )?;
        println!("ui: restarted");
    }
    // Before/after table: state then, state now, and for pty agents
    // whether the pane pid survived and whether an in-flight turn was
    // re-adopted (`kept`) or fenced (`fenced`) by the hot restart. A
    // changed pane pid, a fenced turn, a new attention state, or an
    // unsettled previously live agent makes the command exit non-zero.
    // Existing fences stay visible without being blamed on this restart.
    let before_by_alias: std::collections::HashMap<&str, &Value> = before
        .iter()
        .filter_map(|a| a["alias"].as_str().map(|al| (al, a)))
        .collect();
    let mut rows: Vec<(String, String, String, String, String)> = Vec::new();
    let mut bad = start_unbound;
    // CAD-694: the successor's recovery record is drain evidence the
    // cursors cannot always carry — a predecessor already dead gave
    // none, and non-pty endpoints were never scanned. The record is
    // bound to the instance this start returned; missing, malformed or
    // mismatched, a failed drain, or any fenced turn fails closed.
    // An unbound start already reported itself above; its record verdict
    // would only repeat that, so it is judged only for a spawned start.
    if !start_unbound {
        if let Some(reason) = recovery_record_verdict(state_dir, started_instance.as_deref()) {
            eprintln!("restart: {reason}");
            bad = true;
        }
    }
    for alias in &cursor_misses {
        eprintln!("restart: no pre-stop event cursor for {alias} — its verdict is unverified");
        bad = true;
    }
    let mut existing_fences = Vec::new();
    for a in &after {
        let alias = a["alias"].as_str().unwrap_or_default().to_string();
        let b = before_by_alias.get(alias.as_str());
        let before_state = b
            .and_then(|b| b["state"].as_str())
            .unwrap_or("-")
            .to_string();
        let after_state = a["state"].as_str().unwrap_or_default().to_string();
        let mut pane = "-".to_string();
        let mut turn = "-".to_string();
        if a["endpoint_kind"].as_str() == Some("pty") {
            let old = b.and_then(|b| b["pid"].as_u64()).unwrap_or(0);
            let new = a["pid"].as_u64().unwrap_or(0);
            if old == 0 && new == 0 {
                // no pane either side
            } else if old == new {
                pane = "same".to_string();
            } else {
                bad = true;
                pane = format!("CHANGED {old}→{new}");
            }
            // The adopt verdicts are events on the agent — page
            // forward from the cursor taken before the stop so only
            // this restart's events count and none can be missed. A
            // missing cursor means the pre-stop fetch failed; paging
            // from zero could pick up an older restart's verdicts, so
            // the column stays `-` instead.
            if let Some(cursor) = event_cursors.get(alias.as_str()).copied() {
                let mut seq = cursor;
                let mut kinds: Vec<String> = Vec::new();
                // A failed page leaves this restart's verdict unknown —
                // recorded and failed below, never silently skipped.
                let mut pages_failed = false;
                // Bounded: a restart's adopt verdicts land within a few
                // events; 20 pages of 100 is far past any real gap and
                // keeps a pathological event stream from looping.
                for _ in 0..20 {
                    let page = client::rpc(
                        state_dir,
                        "agent_events",
                        serde_json::json!({"alias": alias, "after": seq}),
                    );
                    let Ok(v) = page else {
                        pages_failed = true;
                        break;
                    };
                    let events = v["events"].as_array().cloned().unwrap_or_default();
                    let n = events.len();
                    for e in &events {
                        if let Some(k) = e["kind"].as_str() {
                            kinds.push(k.to_string());
                        }
                    }
                    seq = v["cursor"].as_i64().unwrap_or(seq);
                    if n < 100 {
                        break;
                    }
                }
                if pages_failed {
                    eprintln!(
                        "restart: event history unread for {alias} — its verdict is unverified"
                    );
                    bad = true;
                }
                if kinds.iter().any(|k| k == "turn_adopt_refused") {
                    bad = true;
                    turn = "fenced".to_string();
                } else if kinds.iter().any(|k| k == "turn_adopted") {
                    turn = "kept".to_string();
                }
            }
        }
        // A stale turn ignored with --ignore-stale can be swept to
        // `unknown` by the replacement store's recovery without any
        // adopt attempt — no `turn_adopt_refused` event exists to mark
        // it. Check each carried row directly so a swept stale turn
        // still fails the restart verdict.
        for (sal, mid) in stale_turns.iter().filter(|(sal, _)| sal == &alias) {
            // A carried stale turn the show cannot verify leaves the
            // verdict unproven — fail closed (CAD-694).
            match client::rpc(state_dir, "agent_show", json!({"alias": sal})) {
                Ok(show) => {
                    let swept = show["messages"]
                        .as_array()
                        .map(|ms| {
                            ms.iter().any(|m| {
                                m["id"].as_str() == Some(mid.as_str())
                                    && m["state"].as_str() == Some("unknown")
                            })
                        })
                        .unwrap_or(false);
                    if swept {
                        bad = true;
                        turn = "fenced".to_string();
                    }
                }
                Err(e) => {
                    eprintln!("restart: could not verify carried stale turn {sal}/{mid}: {e}");
                    bad = true;
                }
            }
        }
        if after_state == "attention" {
            if before_state == "attention" {
                existing_fences.push(alias.clone());
            } else {
                bad = true;
            }
        }
        if live_before.contains(&alias) && matches!(after_state.as_str(), "starting" | "offline") {
            bad = true;
        }
        rows.push((alias, before_state, after_state, pane, turn));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let w = rows.iter().map(|r| r.0.len()).max().unwrap_or(5).max(5);
    println!(
        "{:<w$}  {:<12}  {:<12}  {:<18}  TURN",
        "AGENT",
        "BEFORE",
        "AFTER",
        "PANE",
        w = w
    );
    for (alias, before_state, after_state, pane, turn) in &rows {
        println!(
            "{alias:<w$}  {before_state:<12}  {after_state:<12}  {pane:<18}  {turn}",
            w = w
        );
    }
    if !existing_fences.is_empty() {
        existing_fences.sort();
        println!("Existing fences retained: {}", existing_fences.join(", "));
    }
    if bad {
        Err(Error::rejected(
            "restart completed but not cleanly — see the table above \
             (pane pid changed, a turn was fenced, an agent became \
             fenced, a previously live agent did not settle, or the \
             drain evidence was unverifiable)",
        ))
    } else {
        Ok(0)
    }
}

/// The `last-recovery.json` shape the verdict trusts (CAD-694): every
/// field the writer emits is required and typed, so a truncated,
/// stale-format or hand-edited record is a parse failure — never clean
/// evidence. `consumed` stays a raw `Value` because its members are
/// required-but-nullable (serde `Option` accepts a MISSING key, which
/// must fail instead).
#[derive(serde::Deserialize)]
struct RecoveryRecordView {
    instance: String,
    consumed: serde_json::Value,
    fenced: Vec<FencedRowView>,
    /// Sibling of `fenced` — required for shape completeness; the
    /// verdict keys on `fenced` alone.
    #[allow(dead_code)]
    unevidenced: Vec<FencedRowView>,
}

#[derive(serde::Deserialize)]
struct FencedRowView {
    alias: String,
    /// Required for shape completeness; the report lists aliases only.
    #[allow(dead_code)]
    message_id: String,
}

/// The instance id of the daemon a `daemon start` answer spawned.
/// Only the `started` branch pid-matches its health answer to the
/// child it spawned; `already_running` carries whichever daemon held
/// the socket, so it binds nothing and the verdict fails closed.
pub(crate) fn started_instance(started: &Value) -> Option<String> {
    if started["state"].as_str() != Some("started") {
        return None;
    }
    started["health"]["instance"].as_str().map(str::to_string)
}

/// CAD-694: evaluate the successor daemon's recovery record — drain
/// evidence the verdict needs independent of the per-alias event
/// cursors (a predecessor already dead yields none, and non-pty
/// endpoints were never cursor-covered). `started_id` is the instance
/// the restart's own start returned (pid-matched health), NOT a file:
/// a later boot or a pair of failed writes can leave an old, mutually
/// matching (instance, record) pair on disk, so equality between the
/// two files proves nothing — only naming THIS successor counts.
/// `Some(reason)` fails the verdict: the record is absent, unreadable,
/// wrongly shaped, or bound to a different run; the predecessor's drain
/// failed; or this recovery fenced any in-flight turn.
pub(crate) fn recovery_record_verdict(
    state_dir: &Path,
    started_id: Option<&str>,
) -> Option<String> {
    let Some(started_id) = started_id else {
        return Some(
            "the daemon start answer did not name its instance — the predecessor's \
             drain cannot be verified"
                .to_string(),
        );
    };
    let Some(current) =
        std::fs::read_to_string(state_dir.join(cadence_agent::daemon::INSTANCE_FILE))
            .ok()
            .map(|s| s.trim().to_string())
    else {
        return Some(
            "the new daemon left no readable instance file — the predecessor's \
             drain cannot be verified"
                .to_string(),
        );
    };
    let Some(record) =
        std::fs::read_to_string(state_dir.join(cadence_agent::daemon::LAST_RECOVERY_FILE))
            .ok()
            .and_then(|raw| serde_json::from_str::<RecoveryRecordView>(&raw).ok())
    else {
        return Some(
            "the new daemon left no readable recovery record (missing or malformed) — \
             the predecessor's drain cannot be verified"
                .to_string(),
        );
    };
    if current != started_id {
        return Some(
            "the recorded daemon instance does not name the run this restart \
             started — the predecessor's drain cannot be verified"
                .to_string(),
        );
    }
    if record.instance != started_id {
        return Some(
            "the recovery record names a different run than the one this restart \
             started — the predecessor's drain cannot be verified"
                .to_string(),
        );
    }
    // `consumed` is null or an object whose `failed` member is present
    // and null|string — anything else is evidence that did not come
    // from this build's writer.
    let failed = match &record.consumed {
        Value::Null => Ok(None),
        Value::Object(map) => match (
            map.get("instance").is_some_and(|v| v.is_string()),
            map.get("stale")
                .is_some_and(|v| v.is_string() || v.is_null()),
            map.get("failed"),
        ) {
            (true, true, Some(f)) if f.is_string() || f.is_null() => {
                Ok(f.as_str().map(str::to_string))
            }
            _ => Err("recovery record's `consumed` is malformed"),
        },
        _ => Err("recovery record's `consumed` is malformed"),
    };
    match failed {
        Err(why) => Some(format!("{why} — the drain cannot be verified")),
        Ok(Some(failed)) => Some(format!("the previous daemon's drain failed: {failed}")),
        Ok(None) => {
            if record.fenced.is_empty() {
                None
            } else {
                let names = record
                    .fenced
                    .iter()
                    .map(|f| f.alias.as_str())
                    .collect::<Vec<_>>()
                    .join(",");
                Some(format!(
                    "recovery fenced {} in-flight turn(s): {names}",
                    record.fenced.len()
                ))
            }
        }
    }
}
