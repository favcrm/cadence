//! CAD-535: `cadence build-slot` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum BuildSlotAction {
    /// Take a build/test/suite slot: granted immediately when a slot
    /// is free, else this polls the daemon with a stable request id
    /// until granted or --wait-secs elapses. Prints the slot token.
    Acquire {
        /// build, test or suite. `test` and `suite` can be claimed by
        /// the configured priority lanes ahead of ordinary requests;
        /// `suite` draws on its own pool so a full suite never jams
        /// the build lanes.
        kind: String,
        /// The lane this slot is for (default: $CADENCE_ALIAS, else
        /// $USER, else "unknown").
        #[arg(long)]
        lane: Option<String>,
        /// Pid whose death frees the slot — REQUIRED: the hold must
        /// bind to the process that actually lives for the work (`$$`
        /// in a shell wrapper). `build-slot run` needs no --pid: it
        /// binds the real command itself.
        #[arg(long)]
        pid: u32,
        /// Give up after <secs> waiting in the queue (0 = answer
        /// immediately, granted or not).
        #[arg(long, default_value_t = 0)]
        wait_secs: u64,
        /// Print the grant as JSON ({token, kind, wait_secs}) instead
        /// of the bare token.
        #[arg(long)]
        json: bool,
    },
    /// Hold a slot for exactly one command's lifetime: acquires, then
    /// EXECS the command — the slot's holder is the real cargo/test
    /// process itself, and its exit frees the slot. Wrap gates like
    /// `cadence build-slot run test -- cargo test --lib`.
    Run {
        /// build, test or suite.
        kind: String,
        /// The lane this slot is for (default: $CADENCE_ALIAS, else
        /// $USER, else "unknown").
        #[arg(long)]
        lane: Option<String>,
        /// Give up after <secs> waiting in the queue (0 = fail fast
        /// when nothing is free).
        #[arg(long, default_value_t = 600)]
        wait_secs: u64,
        /// The command to run while holding the slot.
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Return a held slot by token. Release must name the holder —
    /// the default pid is the caller's parent, so a script that
    /// acquired with `--pid $$` releases with a bare `release` from
    /// the same shell; pass --pid to release a slot held by `run`
    /// ($CADENCE_BUILD_SLOT_PID) or another process.
    Release {
        /// The token `acquire` printed.
        token: String,
        /// The lane the slot is held for (default: $CADENCE_ALIAS,
        /// else $USER, else "unknown").
        #[arg(long)]
        lane: Option<String>,
        /// The pid the slot is bound to (default: the caller's
        /// parent — pairing with `acquire --pid $$` in the same
        /// shell).
        #[arg(long)]
        pid: Option<u32>,
    },
    /// Have the daemon run one of the project's RECIPES under a build
    /// slot (CAD-230b) — the admitted path for a caller with no pane of
    /// its own. Recipes (argv, repo-relative cwd, env allowlist, slot
    /// kind) come only from `build.recipes` in the project's
    /// project.yaml; nothing here names a command. The daemon queues,
    /// runs the recipe as its own exec-bound slot holder, logs its
    /// output under the state dir and writes an exit receipt. This
    /// waits, streams the log, and exits with the recipe's exit code.
    Launch {
        /// The recipe name under `build.recipes`.
        recipe: String,
        /// The project (default: the cwd's project, as `issue` resolves it).
        #[arg(long)]
        project: Option<String>,
        /// A checkout of one of the project's registered repos to run in
        /// (default: the cwd when the project was resolved from it, else
        /// the project's main checkout).
        #[arg(long)]
        worktree: Option<PathBuf>,
        /// Give up after <secs> queued for the slot (the recipe then
        /// never starts).
        #[arg(long, default_value_t = 600)]
        wait_secs: u64,
        /// Print the runner id and return at once; `build-slot runner
        /// <id>` shows the receipt later.
        #[arg(long)]
        detach: bool,
    },
    /// One launched runner's receipt: recipe, launch digest, source
    /// HEAD, state, exit code and log path.
    Runner {
        /// The runner id `launch` printed.
        runner_id: String,
    },
    /// Free a managed endpoint's strict hold whose holder is gone —
    /// the one operator path over a strict hold (CAD-230). Run it
    /// outside every pane and managed endpoint. The daemon re-reads
    /// the holder itself and frees the hold only on proven death: a
    /// live or unreadable holder is refused whatever the evidence says.
    Reconcile {
        /// The hold's enrollment (`build-slot status --json`).
        enrollment_id: String,
        /// The hold's token.
        token: String,
        /// Evidence JSON naming the recorded hold exactly:
        /// owner_generation, pid, starttime, uid, plus observed_at,
        /// process_read, command_outcome and side_effect_review.
        #[arg(long)]
        evidence: String,
    },
    /// Who holds and who waits: per-pool capacity, holders, and the
    /// live queue. Your own lane's holds show their tokens; other
    /// lanes' holds show identity only.
    Status {
        /// The lane to view as (default: $CADENCE_ALIAS, else $USER,
        /// else "unknown").
        #[arg(long)]
        lane: Option<String>,
        /// Emit the daemon's slot_status payload as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// Aligned rendering of `slot_status` — the TTY default.
pub(super) fn print_slot_status(s: &Value) {
    println!("{:<6} {:<8} HOLDERS", "POOL", "HELD");
    for pool in ["build", "suite"] {
        let p = &s["pools"][pool];
        let cap = p["capacity"].as_u64().unwrap_or(0);
        let held: Vec<String> = p["held"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|h| {
                let mut line = format!(
                    "{} {} {}",
                    h["lane"].as_str().unwrap_or("?"),
                    h["kind"].as_str().unwrap_or("?"),
                    cadence_agent::slots::fmt_wait(h["age_secs"].as_f64().unwrap_or(0.0))
                );
                // A strict hold names what is not ordinary about it:
                // a non-active enrollment, a holder not provably
                // alive, or accounting past its bound.
                if h["binding"] == "strict" {
                    let flags: Vec<String> = [
                        ("auth_state", "active"),
                        ("liveness", "alive"),
                        ("accounting", "held"),
                    ]
                    .iter()
                    .filter(|(k, ok)| h[*k].as_str().is_some_and(|v| v != *ok))
                    .map(|(k, _)| format!("{k}={}", h[*k].as_str().unwrap_or("?")))
                    .collect();
                    line.push_str(" [strict");
                    for f in flags {
                        line.push(' ');
                        line.push_str(&f);
                    }
                    // What acts on it (CAD-276): reconcile only where
                    // it can free the hold, else the named remedy.
                    if h["reconcile_required"] == true {
                        line.push_str(" reconcile_required");
                    }
                    if let Some(remedy) = h["remedy"].as_str() {
                        line.push_str(" remedy: ");
                        line.push_str(remedy);
                    }
                    line.push(']');
                }
                line
            })
            .collect();
        println!("{pool:<6} {}/{cap:<6} {}", held.len(), held.join(", "));
    }
    if s["strict"]["available"] == false {
        println!(
            "strict admission unavailable: {}",
            s["strict"]["reason"].as_str().unwrap_or("?")
        );
    }
    let waiting = s["waiting"].as_array().cloned().unwrap_or_default();
    if waiting.is_empty() {
        println!("queue: empty");
        return;
    }
    println!(
        "{:<4} {:<14} {:<6} {:<8} FLAGS",
        "#", "LANE", "KIND", "WAITED"
    );
    for (i, w) in waiting.iter().enumerate() {
        let flags = if w["starved"].as_bool().unwrap_or(false) {
            "starved"
        } else if w["priority"].as_bool().unwrap_or(false) {
            "priority"
        } else {
            ""
        };
        println!(
            "{:<4} {:<14} {:<6} {:<8} {}",
            i + 1,
            w["lane"].as_str().unwrap_or("?"),
            w["kind"].as_str().unwrap_or("?"),
            cadence_agent::slots::fmt_wait(w["wait_secs"].as_f64().unwrap_or(0.0)),
            flags
        );
    }
}

/// The shared acquire poll: `request_id` keeps a queued caller's
/// place across polls; `--wait-secs 0` is the read-only probe so a
/// fast-fail never leaves a waiter behind. Returns the grant payload.
pub(super) fn slot_acquire_loop(
    state_dir: &Path,
    kind: &str,
    lane: &str,
    pid: u32,
    request_id: &str,
    wait_secs: u64,
    exec: bool,
) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(wait_secs);
    let probe = wait_secs == 0;
    let mut announced = false;
    loop {
        let mut params = json!({"kind": kind, "lane": lane, "pid": pid,
                                "request_id": request_id, "probe": probe});
        if exec {
            params["exec"] = json!(true);
        }
        let r = client::rpc(state_dir, "slot_acquire", params)?;
        if r["granted"].as_bool().unwrap_or(false) {
            return Ok(r);
        }
        let position = r["position"].as_u64().unwrap_or(0);
        if wait_secs == 0 {
            return Err(Error::rejected(format!(
                "No {kind} slot free — position {position} in the queue. \
                 `cadence build-slot status` shows holders and waiters"
            )));
        }
        if !announced {
            eprintln!("waiting for a {kind} slot (position {position})…");
            announced = true;
        }
        if Instant::now() >= deadline {
            return Err(Error::rejected(format!(
                "Timed out after {wait_secs}s waiting for a {kind} slot \
                 (still position {position})"
            )));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// `cadence build-slot` — acquire polls with a stable request id so a
/// queued caller keeps its place; the daemon mints the token, and
/// release must name the holding (lane, pid).
pub(super) fn run_build_slot(state_dir: &Path, action: &BuildSlotAction) -> Result<i32> {
    match action {
        BuildSlotAction::Acquire {
            kind,
            lane,
            pid,
            wait_secs,
            json: json_out,
        } => {
            // Validate the kind before minting a request id.
            let parsed = cadence_agent::slots::SlotKind::parse(kind)?;
            let lane = lane
                .clone()
                .unwrap_or_else(cadence_agent::slots::default_lane);
            let pid = *pid;
            let request_id = Uuid::new_v4().simple().to_string();
            let r = slot_acquire_loop(state_dir, kind, &lane, pid, &request_id, *wait_secs, false)?;
            if *json_out {
                print_json(&json!({"token": r["token"],
                    "kind": parsed.as_str(),
                    "wait_secs": r["wait_secs"].as_f64().unwrap_or(0.0)}));
            } else {
                println!("{}", r["token"].as_str().unwrap_or_default());
            }
            Ok(0)
        }
        BuildSlotAction::Run {
            kind,
            lane,
            wait_secs,
            cmd,
        } => {
            let lane = lane
                .clone()
                .unwrap_or_else(cadence_agent::slots::default_lane);
            // This process IS the holder — after exec the real
            // command owns the pid the slot is bound to, so the hold
            // lives exactly as long as the work and dies with it.
            let pid = std::process::id();
            let request_id = Uuid::new_v4().simple().to_string();
            // `exec`: the daemon verifies this requester IS the holder
            // it records (CAD-230b) — never an ancestor.
            let r = slot_acquire_loop(state_dir, kind, &lane, pid, &request_id, *wait_secs, true)?;
            let token = r["token"].as_str().unwrap_or_default().to_string();
            eprintln!(
                "slot {token} acquired ({kind}, pid {pid}) — running {}",
                cmd[0]
            );
            use std::os::unix::process::CommandExt;
            let err = std::process::Command::new(&cmd[0])
                .args(&cmd[1..])
                .env("CADENCE_BUILD_SLOT_TOKEN", &token)
                .env("CADENCE_BUILD_SLOT_PID", pid.to_string())
                .env("CADENCE_BUILD_SLOT_LANE", &lane)
                .exec();
            Err(Error::internal(format!("exec {}: {err}", cmd[0])))
        }
        BuildSlotAction::Release { token, lane, pid } => {
            let lane = lane
                .clone()
                .unwrap_or_else(cadence_agent::slots::default_lane);
            let pid = pid.unwrap_or_else(std::os::unix::process::parent_id);
            let r = client::rpc(
                state_dir,
                "slot_release",
                json!({"token": token, "lane": lane, "pid": pid}),
            )?;
            println!("released {}", r["token"].as_str().unwrap_or(token));
            Ok(0)
        }
        BuildSlotAction::Launch {
            recipe,
            project,
            worktree,
            wait_secs,
            detach,
        } => run_build_slot_launch(
            state_dir,
            recipe,
            project.as_deref(),
            worktree.as_deref(),
            *wait_secs,
            *detach,
        ),
        BuildSlotAction::Runner { runner_id } => {
            print_json(&client::rpc(
                state_dir,
                "slot_runner",
                json!({"runner_id": runner_id}),
            )?);
            Ok(0)
        }
        BuildSlotAction::Reconcile {
            enrollment_id,
            token,
            evidence,
        } => {
            let evidence: Value = serde_json::from_str(evidence)
                .map_err(|e| Error::rejected(format!("--evidence is not JSON: {e}")))?;
            let r = client::rpc(
                state_dir,
                "slot_reconcile",
                json!({"enrollment_id": enrollment_id, "token": token,
                       "evidence": evidence}),
            )?;
            print_json(&r);
            Ok(0)
        }
        BuildSlotAction::Status {
            lane,
            json: json_out,
        } => {
            let lane = lane
                .clone()
                .unwrap_or_else(cadence_agent::slots::default_lane);
            let s = client::rpc(state_dir, "slot_status", json!({"lane": lane}))?;
            if *json_out {
                print_json(&s);
            } else {
                print_slot_status(&s);
            }
            Ok(0)
        }
    }
}

/// `cadence build-slot launch` (CAD-230b): the daemon resolves and runs
/// the recipe; this only names it, then follows the receipt and the log.
pub(super) fn run_build_slot_launch(
    state_dir: &Path,
    recipe: &str,
    project: Option<&str>,
    worktree: Option<&Path>,
    wait_secs: u64,
    detach: bool,
) -> Result<i32> {
    let cwd = std::env::current_dir()?;
    let pm_dir = cadence_agent::issue::default_dir()?;
    let key = cadence_agent::issue::project::resolve(&pm_dir, project, &cwd)?.key;
    // The cwd is the checkout only when the project came from it.
    let from_cwd =
        project.is_none() && std::env::var("CADENCE_PROJECT").map_or(true, |p| p.is_empty());
    let worktree = match worktree {
        Some(w) => Some(std::path::absolute(w)?),
        None if from_cwd => Some(cwd.clone()),
        None => None,
    };
    let mut params = json!({"recipe": recipe, "project": key, "wait_secs": wait_secs});
    if let Some(w) = &worktree {
        params["worktree"] = json!(w.display().to_string());
    }
    let r = client::rpc(state_dir, "slot_launch", params)?;
    let id = r["runner_id"].as_str().unwrap_or_default().to_string();
    let short = |k: &str| {
        r[k].as_str()
            .unwrap_or_default()
            .chars()
            .take(12)
            .collect::<String>()
    };
    eprintln!(
        "runner {id} queued — {key}/{recipe} ({}) at {}, intent {} — log {}",
        r["kind"].as_str().unwrap_or("?"),
        short("head_sha"),
        short("digest"),
        r["log_path"].as_str().unwrap_or("?")
    );
    if detach {
        println!("{id}");
        return Ok(0);
    }
    let log = PathBuf::from(r["log_path"].as_str().unwrap_or_default());
    let mut offset = 0u64;
    let stream = |offset: &mut u64| {
        use std::io::{Seek, SeekFrom, Write};
        let Ok(mut f) = std::fs::File::open(&log) else {
            return;
        };
        if f.seek(SeekFrom::Start(*offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if let Ok(n) = f.read_to_end(&mut buf) {
            *offset += n as u64;
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&buf);
            let _ = out.flush();
        }
    };
    loop {
        let receipt = client::rpc(state_dir, "slot_runner", json!({"runner_id": id}))?;
        stream(&mut offset);
        let state = receipt["state"].as_str().unwrap_or_default();
        if ["pending", "queued", "running"].contains(&state) {
            std::thread::sleep(Duration::from_millis(300));
            continue;
        }
        let reason = receipt["reason"].as_str().unwrap_or_default();
        return match (
            state,
            receipt["exit_code"].as_i64(),
            receipt["signal"].as_i64(),
        ) {
            ("exited", Some(code), _) => {
                eprintln!("runner {id} exited {code}");
                Ok(code as i32)
            }
            ("exited", None, Some(sig)) => {
                eprintln!("runner {id} killed by signal {sig}");
                Ok(128 + sig as i32)
            }
            _ => Err(Error::rejected(format!(
                "runner {id} ended '{state}'{}",
                if reason.is_empty() {
                    String::new()
                } else {
                    format!(": {reason}")
                }
            ))),
        };
    }
}

pub(super) fn run(state_dir: PathBuf, action: BuildSlotAction) -> Result<i32> {
    run_build_slot(&state_dir, &action)
}
