//! CAD-535: `cadence upgrade` — moved verbatim from src/main.rs.

use super::*;

/// `cadence upgrade`: verify and install (see `cadence_agent::upgrade`),
/// then either print the restart command or, with `--restart`, run the
/// NEW binary's `daemon restart --when-idle --ui`. The restart must run
/// from the installed binary: `daemon restart` respawns `current_exe()`,
/// so restarting from this (old) process would bring the old build back.
pub(super) fn run_upgrade(state_dir: &Path, args: UpgradeArgs) -> Result<i32> {
    use cadence_agent::upgrade;
    // A restart without a rollout identity would fail after the install;
    // refuse before touching anything instead.
    if args.restart {
        cadence_agent::rollout::resolve_caller(args.as_identity.as_deref()).map_err(|e| {
            Error::rejected(format!("upgrade --restart refused before installing: {e}"))
        })?;
    }
    let target = match (args.sha, args.latest_main) {
        (Some(sha), false) => upgrade::Target::Sha(sha),
        (None, true) => upgrade::Target::LatestMain,
        _ => {
            return Err(Error::rejected(
                "pass exactly one of --sha <40-hex> or --latest-main",
            ))
        }
    };
    let layout = upgrade::Layout::detect(args.link, args.releases_dir)?;
    let source = upgrade::Gh::new(&args.repo);
    let mut report = upgrade::run(
        &source,
        &layout,
        &upgrade::Request {
            target,
            dry_run: args.dry_run,
            allow_unattested: args.allow_unattested,
            backup_state_dir: Some(state_dir.to_path_buf()),
        },
    )?;
    // CAD-379: an unattested install is said on stderr too, where a human
    // reads it; stdout stays the JSON report.
    if let Some(warning) = report["warning"].as_str() {
        eprintln!("warning: {warning}");
    }
    let command = upgrade::restart_command(args.as_identity.as_deref());
    if !args.restart {
        report["restart_command"] = json!(command);
        report["next"] = json!(if args.dry_run {
            "dry run: nothing installed; rerun without --dry-run to install".to_string()
        } else {
            format!("the daemon still runs its old build; when ready, run `{command}`")
        });
        print_json(&report);
        return Ok(0);
    }
    let installed = PathBuf::from(report["installed_path"].as_str().unwrap_or_default());
    let mut cmd = Command::new(&installed);
    cmd.arg("--state-dir")
        .arg(state_dir)
        .args(upgrade::RESTART_ARGS);
    if let Some(id) = &args.as_identity {
        cmd.arg("--as").arg(id);
    }
    // stderr (the when-idle progress) streams through; the before/after
    // table is captured into the report.
    let out = cadence_agent::reaper::spawn(
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit()),
    )
    .and_then(|child| child.wait_with_output())
    .map_err(|e| Error::internal(format!("could not run {}: {e}", installed.display())))?;
    let ok = out.status.success();
    report["restarted"] = json!(ok);
    report["restart"] = json!({
        "command": format!("{} --state-dir {} {}{}", installed.display(), state_dir.display(),
            upgrade::RESTART_ARGS.join(" "),
            args.as_identity.as_deref().map(|id| format!(" --as {id}")).unwrap_or_default()),
        "exit_code": out.status.code(),
        "output": String::from_utf8_lossy(&out.stdout),
    });
    if !ok {
        report["next"] = json!(format!(
            "installed, but the restart did not complete cleanly — read restart.output; \
             the daemon may still run its old build. Retry with `{command}` once resolved"
        ));
    }
    print_json(&report);
    Ok(if ok { 0 } else { 1 })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    state_dir: PathBuf,
    sha: Option<String>,
    latest_main: bool,
    dry_run: bool,
    restart: bool,
    as_identity: Option<String>,
    allow_unattested: bool,
    repo: String,
    link: Option<PathBuf>,
    releases_dir: Option<PathBuf>,
) -> Result<i32> {
    run_upgrade(
        &state_dir,
        UpgradeArgs {
            sha,
            latest_main,
            dry_run,
            restart,
            as_identity,
            allow_unattested,
            repo,
            link,
            releases_dir,
        },
    )
}
