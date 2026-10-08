//! CAD-1002: update/upgrade args + the production `UpdateHost` — moved verbatim
//! from `cli/mod.rs` (CAD-984 PR-2).

use super::*;

/// Where a release lives, which repository is trusted, and the operator
/// identity — shared by `cadence update` and `cadence update status` so
/// the flags parse on either side of the subcommand.
#[derive(clap::Args, Default)]
pub(crate) struct UpdateTargetArgs {
    /// Operator identity outside a cadence pane, for example
    /// `operator:ada`. Required outside a pane; inside one this
    /// command is refused.
    #[arg(long = "as")]
    pub(super) as_identity: Option<String>,
    /// GitHub repository whose CI built and attested the binary.
    #[arg(long, default_value = cadence_agent::upgrade::DEFAULT_REPO)]
    pub(super) repo: String,
    /// Symlink that puts cadence on PATH [default: ~/.local/bin/cadence].
    #[arg(long)]
    pub(super) link: Option<PathBuf>,
    /// Releases directory [default: read off the current link].
    #[arg(long)]
    pub(super) releases_dir: Option<PathBuf>,
}

impl UpdateTargetArgs {
    /// The subcommand's flags when it carried any, else the parent's.
    pub(super) fn or(self, parent: UpdateTargetArgs) -> UpdateTargetArgs {
        UpdateTargetArgs {
            as_identity: self.as_identity.or(parent.as_identity),
            repo: if self.repo.is_empty() {
                parent.repo
            } else {
                self.repo
            },
            link: self.link.or(parent.link),
            releases_dir: self.releases_dir.or(parent.releases_dir),
        }
    }
}

pub(crate) struct UpgradeArgs {
    pub(super) sha: Option<String>,
    pub(super) latest_main: bool,
    pub(super) dry_run: bool,
    pub(super) restart: bool,
    pub(super) as_identity: Option<String>,
    pub(super) allow_unattested: bool,
    pub(super) repo: String,
    pub(super) link: Option<PathBuf>,
    pub(super) releases_dir: Option<PathBuf>,
}

pub(crate) struct UpdateArgs {
    pub(super) status: bool,
    pub(super) check: bool,
    pub(super) rollback: bool,
    pub(super) drain: String,
    pub(super) now: bool,
    pub(super) keep: u64,
    pub(super) backup_dir: Option<PathBuf>,
    pub(super) json: bool,
    pub(super) progress: Option<PathBuf>,
    pub(super) to: Option<String>,
    pub(super) as_identity: Option<String>,
    pub(super) repo: String,
    pub(super) link: Option<PathBuf>,
    pub(super) releases_dir: Option<PathBuf>,
}

/// The production [`cadence_agent::update::UpdateHost`]: the daemon for
/// the drain, the waiters and the health answers; the filesystem for
/// the releases and the marker.
pub(crate) struct RealUpdateHost<'a> {
    pub(super) state_dir: &'a Path,
    pub(super) layout: cadence_agent::upgrade::Layout,
    pub(super) source: cadence_agent::upgrade::Gh,
    pub(super) label: String,
    /// Collect the plain lines for `--json`; `None` prints them as they
    /// happen (the plain form).
    pub(super) collect: Option<std::cell::RefCell<Vec<String>>>,
    /// The marker this run last recorded, for the drain re-assertion.
    pub(super) pending: std::cell::RefCell<Option<cadence_agent::update::PendingUpdate>>,
    /// `--progress`: append every line here too, so the board's card can
    /// read the run while this process is detached from it (CAD-561 r2).
    pub(super) progress_log: Option<PathBuf>,
    /// `--to <sha>`: the pinned target (CAD-1187).
    pub(super) pin: Option<String>,
}

impl RealUpdateHost<'_> {
    fn line(&self, line: &str) {
        let log = self.progress_log.as_deref();
        if let Some(path) = log {
            cadence_agent::update::run_log_line(path, line);
        }
        match &self.collect {
            Some(lines) => lines.borrow_mut().push(line.to_string()),
            // The board starts the helper with stdout pointing at the same
            // progress log; printing there too would write every line
            // twice (CAD-561 r3).
            None if !log.is_some_and(|path| fd_is_path(libc::STDOUT_FILENO, path)) => {
                println!("{line}")
            }
            None => {}
        }
    }
}

/// Is descriptor `fd` the same file as `path`? The helper's stdout is
/// the progress log when the board starts it, so its progress lines must
/// not also be printed there (CAD-561 r3).
pub(crate) fn fd_is_path(fd: i32, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(descriptor), Ok(file)) = (
        std::fs::metadata(format!("/proc/self/fd/{fd}")),
        std::fs::metadata(path),
    ) else {
        return false;
    };
    descriptor.dev() == file.dev() && descriptor.ino() == file.ino()
}

impl cadence_agent::update::UpdateHost for RealUpdateHost<'_> {
    fn pin(&self) -> Option<&str> {
        self.pin.as_deref()
    }
    fn state_dir(&self) -> &Path {
        self.state_dir
    }
    fn layout(&self) -> &cadence_agent::upgrade::Layout {
        &self.layout
    }
    fn source(&self) -> &dyn cadence_agent::upgrade::ReleaseSource {
        &self.source
    }
    fn identity(&self) -> &str {
        &self.label
    }
    fn progress(&self, line: &str) {
        self.line(line);
    }
    fn waiters(&self) -> Result<Vec<cadence_agent::update::Waiter>> {
        // The daemon's own view is authoritative (it reads the message
        // rows); with no daemon running nothing is in flight.
        let status = match client::rpc(self.state_dir, "update_status", json!({})) {
            Ok(status) => status,
            Err(_) => return Ok(Vec::new()),
        };
        Ok(status["waiting"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| {
                        Some(cadence_agent::update::Waiter {
                            alias: r["alias"].as_str()?.to_string(),
                            message: r["message"].as_str().unwrap_or_default().to_string(),
                            state: r["state"].as_str().unwrap_or_default().to_string(),
                            age_secs: r["age_secs"].as_u64().unwrap_or(0),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
    fn set_pending(&self, pending: Option<&cadence_agent::update::PendingUpdate>) -> Result<()> {
        *self.pending.borrow_mut() = pending.cloned();
        match pending {
            Some(pending) => cadence_agent::update::write_pending(self.state_dir, pending),
            None => {
                cadence_agent::update::clear_pending(self.state_dir);
                Ok(())
            }
        }
    }
    fn pending(&self) -> Option<cadence_agent::update::PendingUpdate> {
        self.pending.borrow().clone()
    }
    fn set_drain(&self, on: bool) -> Result<()> {
        let mut params = json!({"on": on, "label": self.label});
        if on {
            // The phase comes from the marker the pipeline just wrote;
            // a daemon that is not running is not an error (there is
            // nothing to drain, and the restart will start it).
            let pending = self.pending.borrow().clone();
            if let Some(pending) = pending {
                params["target"] = json!(pending.target);
                params["phase"] = json!(pending.phase);
                params["from"] = json!(pending.from);
                params["since"] = json!(pending.since);
            }
        }
        match client::rpc(self.state_dir, "update_drain", params) {
            Ok(_) => Ok(()),
            // A daemon that is down mid-update (the switch) has nothing
            // to gate; the marker on disk covers the restart.
            Err(e) if e.to_string().starts_with("Daemon is not reachable") => Ok(()),
            Err(e) => Err(e),
        }
    }
    fn restart(&self, binary: &Path) -> Result<cadence_agent::update::RestartOutcome> {
        use cadence_agent::update::RestartOutcome;
        // Rollback may select an older CLI whose `daemon restart`
        // requires a live fleet RPC. Select its cold-start command
        // before invoking it, with the current updater's guards.
        let caller = update::update_caller(self.state_dir, Some(&self.label))?;
        refuse_restart_over_leftovers(self.state_dir)?;
        let ticket = cadence_agent::rollout::begin_restart(self.state_dir, &caller)?;
        let offline = match client::rpc_answer(self.state_dir, "agent_list", json!({})) {
            Ok(Ok(_)) => false,
            Ok(Err(refused)) => return Err(refused),
            Err(_) => {
                if !daemon_lock_free(self.state_dir) {
                    return Err(Error::rejected(
                        "daemon owns the state-dir lock but its fleet snapshot is unavailable — \
                         refusing updater recovery before invoking the release binary",
                    ));
                }
                true
            }
        };
        let mut commands: Vec<Vec<String>> = if offline {
            vec![vec![
                "daemon".into(),
                "start".into(),
                "--as".into(),
                self.label.clone(),
            ]]
        } else {
            vec![vec![
                "daemon".into(),
                "restart".into(),
                "--ui".into(),
                "--as".into(),
                self.label.clone(),
            ]]
        };
        if let Some(ui_pid) = offline
            .then(|| cadence_agent::ui::detached_pid(self.state_dir))
            .flatten()
        {
            // Preserve the restart's --ui contract when a board survived
            // the failed replacement. Read legacy argv before stopping
            // the process; persisted ui.json remains authoritative.
            let mut ui_start = vec!["ui".into(), "start".into()];
            if !cadence_agent::ui::opts_present(self.state_dir) {
                let (host, port, dist, allow_hosts) = ui_run_args(ui_pid);
                ui_start.extend(["--host".into(), host, "--port".into(), port.to_string()]);
                if let Some(dist) = dist {
                    ui_start.extend(["--dist".into(), dist.to_string_lossy().into_owned()]);
                }
                for host in allow_hosts {
                    ui_start.extend(["--allow-host".into(), host]);
                }
            }
            commands.extend([vec!["ui".into(), "stop".into()], ui_start]);
        }
        for args in commands {
            cadence_agent::rollout::recheck_restart(self.state_dir, &ticket)?;
            refuse_restart_over_leftovers(self.state_dir)?;
            if offline && args[0] == "daemon" {
                // Never shut down a daemon that appeared after the
                // offline proof. Its own singleton is the final start
                // protection; this route never restores a backup or
                // bypasses the selected daemon's schema checks.
                if !daemon_lock_free(self.state_dir) {
                    return Err(Error::rejected(
                        "daemon acquired the state-dir lock before updater recovery — retry",
                    ));
                }
                cadence_agent::rollout::note_restart_proceeded(self.state_dir, &ticket)?;
            }
            let mut cmd = Command::new(binary);
            cmd.arg("--state-dir")
                .arg(self.state_dir)
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            if args[0] == "ui" && args[1] == "start" {
                cmd.env_remove("CADENCE_ALIAS");
            }
            let out = cadence_agent::reaper::spawn(&mut cmd)
                .and_then(|child| child.wait_with_output())
                .map_err(|e| Error::internal(format!("could not run {}: {e}", binary.display())))?;
            if out.status.success() {
                continue;
            }
            // A non-zero exit is the restart's complaint, not a stop
            // (CAD-561 r3): a fenced turn makes `daemon restart` exit
            // non-zero with the new build up, and a failed `daemon start`
            // or `ui start` leaves the daemon or the board down — only the
            // health check that follows can tell, and it rolls back.
            let complaint = String::from_utf8_lossy(&out.stderr);
            let complaint = complaint.trim();
            let exit = out.status.code().unwrap_or(-1);
            return Ok(RestartOutcome::Unclean(if complaint.is_empty() {
                format!("the restart on {} exited {exit}", binary.display())
            } else {
                format!(
                    "the restart on {} exited {exit}: {complaint}",
                    binary.display()
                )
            }));
        }
        Ok(RestartOutcome::Clean)
    }
    fn daemon_build(&self) -> Result<Option<String>> {
        // CAD-598 r4/N2: a health poll must not sit on the default
        // 700s rpc timeout — a daemon that is slow to answer would
        // hold `health_wait` well past HEALTH_TIMEOUT on a single
        // call. A few seconds is the poll's whole budget; the wait
        // retries whatever the short window misses (N1).
        daemon_build_or_absent(client::rpc_timeout(
            self.state_dir,
            "daemon_info",
            json!({}),
            DAEMON_BUILD_TIMEOUT,
        ))
    }
    fn board_build(&self) -> Result<Option<String>> {
        match cadence_agent::ui::health(self.state_dir) {
            None => Ok(None),
            Some((_port, body)) => Ok(serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["build"].as_str().map(str::to_string))),
        }
    }
    fn board_running(&self) -> bool {
        cadence_agent::ui::detached_pid(self.state_dir).is_some()
    }
    fn progress_log(&self) -> Option<PathBuf> {
        self.progress_log.clone()
    }
    fn now(&self) -> f64 {
        cadence_agent::rollout::unix_now()
    }
    fn sleep(&self, duration: std::time::Duration) {
        std::thread::sleep(duration);
    }
}

/// The health poll's per-call bound (CAD-598 r4/N2): a daemon that is
/// slow to answer gets this long per probe, never the default 700s —
/// `health_wait`'s deadline is the real bound.
pub(crate) const DAEMON_BUILD_TIMEOUT: Duration = Duration::from_secs(5);

/// `daemon_build`'s answer classification (CAD-561 r4): only an
/// unreachable daemon reads as "no daemon" — every other RPC failure
/// is real and must fail the run, or `finish_restart` restarts a
/// daemon that was merely slow to answer.
pub(crate) fn daemon_build_or_absent(answer: Result<Value>) -> Result<Option<String>> {
    match answer {
        Ok(info) => Ok(info["build_commit"].as_str().map(str::to_string)),
        Err(e) if e.to_string().starts_with("Daemon is not reachable") => Ok(None),
        Err(e) => Err(e),
    }
}
