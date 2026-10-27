//! CAD-535: `cadence update` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum UpdateAction {
    /// Show the pending update, its phase and exactly what it waits on
    /// (the same view `cadence daemon status` carries).
    Status {
        /// The same release/identity flags the parent takes, so
        /// `cadence update status --as operator:ada` works as written.
        #[command(flatten)]
        target: UpdateTargetArgs,
    },
}

/// The operator identity `cadence update` runs under. Inside a pane the
/// command is refused outright (an agent can never push a build); the
/// process proof is the same one `rollout claim --as` runs.
///
/// CAD-1406: `--as` is optional. Without it the audit label is derived
/// from the proven local OS identity (real uid and pid — never `$USER`,
/// `$CADENCE_ALIAS`, forwarded or client fields), and only after
/// `require_operator` passes. An explicit `--as` stays a caller-supplied
/// label that never substitutes for the proof.
pub(super) fn update_caller(
    state_dir: &Path,
    as_identity: Option<&str>,
) -> Result<cadence_agent::rollout::Caller> {
    use cadence_agent::rollout;
    // CAD-482: an armed fixture's in-band operator assertion answers
    // before the pane check — the same order the proof itself uses, so
    // the pane/`--as` consistency rule does not apply to it. An explicit
    // `--as` stays verbatim; without one the caller falls through to the
    // same proof-derived label as the real path.
    let asserted_operator = matches!(
        cadence_agent::test_seam::process_asserted(state_dir),
        Ok(Some(cadence_agent::test_seam::Asserted::Operator))
    );
    if asserted_operator && as_identity.is_some() {
        return rollout::resolve_caller_with(None, as_identity);
    }
    if let Some(alias) = std::env::var("CADENCE_ALIAS")
        .ok()
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty() && !asserted_operator)
    {
        return Err(Error::rejected(format!(
            "cadence update is an operator action — this shell is cadence pane '{alias}'. \
             Run it from an operator shell outside every pane and managed endpoint"
        )));
    }
    // The proof decides before any identity is accepted — a managed
    // descendant, a pane's tree, or an unproven caller is refused here
    // regardless of what `--as` claims.
    rollout::require_operator(state_dir, "cadence update")?;
    let caller = match as_identity {
        // An explicit label is still restricted to the operator `as`
        // shape — `--as delegate:<alias>` stays refused as before.
        Some(_) => {
            let caller = rollout::resolve_caller(as_identity)?;
            if caller.source != "as" {
                return Err(Error::rejected(
                    "cadence update is an operator action — `--as` names the \
                     operator, never delegate:<alias>",
                ));
            }
            caller
        }
        // `source` marks the provenance (`native`, not `as`): nothing
        // here came from a client field.
        None => rollout::Caller {
            identity: format!(
                "operator:uid{}-pid{}",
                unsafe { libc::getuid() },
                std::process::id()
            ),
            source: "native",
        },
    };
    Ok(caller)
}

/// Refuse the native updater inside an established image-managed
/// runtime. The only proof is the operator's own `hosted:` lifecycle
/// contract in the tracker's pm.yaml — the same table and tracker
/// resolution `daemon run` applies in `hosted_config`. `none`/`off`/
/// absent is hosted lifecycle off; a bare `CADENCE_AGENTICOS_URL` or a
/// container marker alone is not a managed runtime (a native host may
/// integrate with AgenticOS without being image-managed). A present
/// lifecycle config that cannot be read fails closed — the updater
/// must not guess. No lease is acquired or renewed to detect this.
fn refuse_managed_runtime() -> Result<()> {
    let pm_dir = match std::env::var("CADENCE_PM_DIR") {
        Ok(dir) if !dir.trim().is_empty() => {
            cadence_agent::home::guard_tracker(PathBuf::from(dir))?
        }
        _ => cadence_agent::issue::default_dir()?,
    };
    // Same loader as daemon startup: missing file/table is off; every
    // other read or parse failure refuses. No lease is taken.
    let Some(hosted) = cadence_agent::lease::read_hosted(&pm_dir)? else {
        return Ok(());
    };
    if matches!(
        hosted.lease.as_deref().map(str::trim),
        Some(lease) if !lease.is_empty() && lease != "none" && lease != "off"
    ) {
        return Err(Error::rejected(
            "cadence update does not run inside this runtime — the tracker's \
             hosted: lease config marks it image-managed, so AgenticOS owns the \
             binary lifecycle and ships updates by replacing the container image. \
             To update a native installation, run `cadence update` on a host \
             without a hosted lease",
        ));
    }
    Ok(())
}

/// The native updater proves its operator with Linux procfs, which macOS
/// does not provide. Refuse before any proof, config, layout, network or
/// state work; this is not a fallback proof and grants no bypass.
#[cfg(target_os = "macos")]
fn native_update_platform_guard() -> Result<()> {
    Err(Error::rejected(
        "native daemon updates are unsupported on macOS — the native updater \
         cannot prove the operator on this platform. Upgrade a public client \
         with the documented installer-based release flow (docs/INSTALL-AGENT.md); \
         `--as` and environment variables do not bypass this",
    ))
}

#[cfg(not(target_os = "macos"))]
fn native_update_platform_guard() -> Result<()> {
    Ok(())
}

/// `cadence update`: the everyday path (CAD-561). `status`, `--check`
/// and `--rollback` are the other faces of the same pipeline.
pub(super) fn run_update(state_dir: &Path, args: UpdateArgs) -> Result<i32> {
    use cadence_agent::update;
    native_update_platform_guard()?;
    let caller = update_caller(state_dir, args.as_identity.as_deref())?;
    // An established image-managed runtime refuses the native updater
    // outright — no install, restart, drain or marker work.
    refuse_managed_runtime()?;
    let layout =
        cadence_agent::upgrade::Layout::detect(args.link.clone(), args.releases_dir.clone())?;
    // CAD-1479: the owned catalogue is the default release source;
    // `--release-source` is the advanced https:// override, and a
    // non-default `--repo` still selects the GitHub route — see
    // `UpdateSource::new`.
    let source = UpdateSource::new(args.release_source.as_deref(), &args.repo)?;
    let host = RealUpdateHost {
        state_dir,
        layout,
        source,
        label: caller.identity.clone(),
        collect: args.json.then(|| std::cell::RefCell::new(Vec::new())),
        pending: std::cell::RefCell::new(None),
        progress_log: args.progress.clone(),
        pin: args.to.clone(),
    };
    if args.status {
        let status = client::rpc(state_dir, "update_status", json!({})).unwrap_or_else(
            |_| json!({"pending_update": Value::Null, "waiting": [], "waiting_count": 0}),
        );
        let guidance = home_dir()
            .map(|home| guidance::readonly(&home))
            .unwrap_or_else(|error| {
                json!({
                    "guidance": "not-checked",
                    "wrote": false,
                    "error": error.to_string(),
                    "live_session": "not-checked",
                })
            });
        if args.json {
            let mut status = status;
            status["guidance"] = guidance;
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
            for line in guidance::lines(&guidance) {
                println!("{line}");
            }
        }
        return Ok(0);
    }
    if args.check {
        let report = update::check(&host)?;
        if args.to.is_some() && !report.up_to_date && report.refusals.is_empty() {
            // A pinned check proves the build installs, not only that
            // the sha is on main: a refusal names the failed check.
            update::verify_pinned(&host, &report.target)?;
        }
        let guidance = home_dir()
            .map(|home| guidance::readonly(&home))
            .unwrap_or_else(|error| {
                json!({
                    "guidance": "not-checked",
                    "wrote": false,
                    "error": error.to_string(),
                    "target_guidance": "not-verified",
                    "live_session": "not-checked",
                })
            });
        if args.json {
            let mut body = report.to_json();
            body["guidance"] = guidance;
            print_json(&body);
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
            for line in guidance::lines(&guidance) {
                println!("{line}");
            }
        }
        return Ok(0);
    }
    if args.rollback {
        let report = update::rollback(&host)?;
        let guidance = guidance_after(state_dir, &host.layout, &report);
        if args.json {
            let mut body = report.to_json();
            body["guidance"] = guidance;
            print_json(&body);
        } else {
            for line in guidance::lines(&guidance) {
                println!("{line}");
            }
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
    let guidance = guidance_after(state_dir, &host.layout, &report);
    if args.json {
        let mut body = report.to_json();
        body["guidance"] = guidance;
        print_json(&body);
    } else {
        for line in guidance::lines(&guidance) {
            println!("{line}");
        }
    }
    if report.rolled_back {
        return Ok(1);
    }
    Ok(0)
}

fn guidance_after(
    state_dir: &Path,
    layout: &cadence_agent::upgrade::Layout,
    report: &cadence_agent::update::RunReport,
) -> Value {
    match home_dir() {
        Ok(home) => guidance::after_update(state_dir, layout, report, &home),
        Err(error) => json!({
            "guidance": "not-checked",
            "wrote": false,
            "error": error.to_string(),
            "live_session": "not-checked",
            "daemon_restarted_for_skills": false,
        }),
    }
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
    progress: Option<PathBuf>,
    to: Option<String>,
    target: UpdateTargetArgs,
) -> Result<i32> {
    let (status, target) = match action {
        Some(UpdateAction::Status { target: own }) => (true, own.or(target)),
        None => (false, target),
    };
    run_update(
        &state_dir,
        UpdateArgs {
            status,
            check,
            rollback,
            drain,
            now,
            keep,
            backup_dir,
            json,
            progress,
            to,
            as_identity: target.as_identity,
            repo: target.repo,
            link: target.link,
            releases_dir: target.releases_dir,
            release_source: target.release_source,
        },
    )
}
