//! CAD-535: `cadence update` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum UpdateAction {
    /// Show the pending update, its phase and exactly what it waits on
    /// (the same view `cadence daemon status` carries).
    Status,
}

/// The operator identity `cadence update` runs under. Inside a pane the
/// command is refused outright (an agent can never push a build); the
/// process proof is the same one `rollout claim --as` runs.
pub(super) fn update_caller(
    state_dir: &Path,
    as_identity: Option<&str>,
) -> Result<cadence_agent::rollout::Caller> {
    use cadence_agent::rollout;
    if let Some(alias) = std::env::var("CADENCE_ALIAS")
        .ok()
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
    {
        return Err(Error::rejected(format!(
            "cadence update is an operator action — this shell is cadence pane '{alias}'. \
             Run it from an operator shell outside every pane and managed endpoint, as \
             `cadence update --as operator:<name>`"
        )));
    }
    let caller = rollout::resolve_caller(as_identity)?;
    if caller.source != "as" {
        return Err(Error::rejected(
            "cadence update needs an operator identity outside a pane: pass \
             `--as operator:<name>`",
        ));
    }
    rollout::require_operator(state_dir, "cadence update")?;
    Ok(caller)
}

/// `cadence update`: the everyday path (CAD-561). `status`, `--check`
/// and `--rollback` are the other faces of the same pipeline.
pub(super) fn run_update(state_dir: &Path, args: UpdateArgs) -> Result<i32> {
    use cadence_agent::update;
    let caller = update_caller(state_dir, args.as_identity.as_deref())?;
    let layout =
        cadence_agent::upgrade::Layout::detect(args.link.clone(), args.releases_dir.clone())?;
    let host = RealUpdateHost {
        state_dir,
        layout,
        source: cadence_agent::upgrade::Gh::new(&args.repo),
        label: caller.identity.clone(),
        collect: args.json.then(|| std::cell::RefCell::new(Vec::new())),
        pending: std::cell::RefCell::new(None),
    };
    if args.status {
        let status = client::rpc(state_dir, "update_status", json!({})).unwrap_or_else(
            |_| json!({"pending_update": Value::Null, "waiting": [], "waiting_count": 0}),
        );
        if args.json {
            print_json(&status);
        } else {
            match status["pending_update"].as_object() {
                Some(pending) => {
                    println!(
                        "update in progress: {} → {} ({}) by {} since {}",
                        pending["from"].as_str().unwrap_or("?"),
                        pending["target"].as_str().unwrap_or("?"),
                        pending["phase"].as_str().unwrap_or("?"),
                        pending["by"].as_str().unwrap_or("?"),
                        pending["since"]
                            .as_f64()
                            .map(|s| cadence_agent::issue::time::basic(s as i64))
                            .unwrap_or_else(|| "?".to_string())
                    );
                }
                None => println!("no update in progress"),
            }
            let waiters = status["waiting"].as_array().cloned().unwrap_or_default();
            if waiters.is_empty() {
                println!("waiting on: nothing");
            } else {
                let rendered = waiters
                    .iter()
                    .map(|w| {
                        let alias = w["alias"].as_str().unwrap_or("?");
                        let age = w["age_secs"].as_u64().unwrap_or(0);
                        if age >= 60 {
                            format!("{alias} ({}m)", age / 60)
                        } else {
                            format!("{alias} ({age}s)")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("waiting for {} turns: {rendered}", waiters.len());
            }
        }
        return Ok(0);
    }
    if args.check {
        let report = update::check(&host)?;
        if args.json {
            print_json(&report.to_json());
        } else {
            for line in report.lines() {
                println!("{line}");
            }
            if report.up_to_date {
                println!(
                    "already up to date ({})",
                    report
                        .target_version
                        .clone()
                        .unwrap_or_else(|| report.target.clone())
                );
            }
        }
        return Ok(0);
    }
    if args.rollback {
        let report = update::rollback(&host)?;
        if args.json {
            print_json(&report.to_json());
        }
        return Ok(0);
    }
    let drain = cadence_agent::rollout::parse_ttl(&args.drain)?;
    let opts = update::Options {
        drain,
        now: args.now,
        keep: args.keep as usize,
        backup_dir: args.backup_dir.clone(),
    };
    let report = update::run(&host, &opts)?;
    if args.json {
        print_json(&report.to_json());
    }
    if report.rolled_back {
        return Ok(1);
    }
    Ok(0)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    state_dir: PathBuf,
    action: Option<UpdateAction>,
    check: bool,
    rollback: bool,
    drain: String,
    now: bool,
    keep: u64,
    backup_dir: Option<PathBuf>,
    json: bool,
    as_identity: Option<String>,
    repo: String,
    link: Option<PathBuf>,
    releases_dir: Option<PathBuf>,
) -> Result<i32> {
    run_update(
        &state_dir,
        UpdateArgs {
            status: matches!(action, Some(UpdateAction::Status)),
            check,
            rollback,
            drain,
            now,
            keep,
            backup_dir,
            json,
            as_identity,
            repo,
            link,
            releases_dir,
        },
    )
}
