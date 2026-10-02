//! CAD-1024: `cadence staging` — the staging-delegation allowlist and grant
//! store. `register`/`delegate`/`revoke` relay daemon RPCs — operator-only at
//! the daemon (`operator_connection`); `delegations` is a read. `refresh` is
//! the delegate round-trip an agent runs under `--as delegate:<alias>` — each
//! step (claim, daemon stop/start, ui stop/start, release) is admitted only by
//! a live `staging_grants` row for that op on this registered dir (PR-3/4).

use super::*;

#[derive(Subcommand)]
pub(crate) enum StagingAction {
    /// Register this state dir as a staging instance (operator only).
    /// `--board-port` is the board's port and must be in 3110–3199; the
    /// production state/PM paths can never be registered.
    Register {
        /// The board's port for this staging instance.
        #[arg(long)]
        board_port: u16,
    },
    /// Grant `alias` the listed ops on this staging dir for `--ttl`
    /// (operator only). The dir must be registered staging first.
    Delegate {
        /// The agent alias granted.
        #[arg(long)]
        alias: String,
        /// The ops the grant covers — repeatable; any of rollout_claim,
        /// daemon_start, daemon_stop, ui_start, ui_stop.
        #[arg(long = "op", required = true)]
        ops: Vec<String>,
        /// How long the grant lasts: 90s, 30m, 12h, 1d [required, ≤ 7d].
        #[arg(long)]
        ttl: String,
        /// Why this grant exists (recorded).
        #[arg(long)]
        reason: Option<String>,
    },
    /// End `alias`'s live grant on this staging dir (operator only).
    Revoke {
        /// The agent alias whose grant ends.
        #[arg(long)]
        alias: String,
    },
    /// List the live grants on this staging dir (read-only).
    Delegations,
    /// CAD-1024: refresh this staging instance to `--to` — the delegate
    /// round-trip an agent runs: `rollout claim`, `daemon stop`,
    /// `daemon start`, `ui stop`, `ui start`, `rollout release`, each under
    /// `--as delegate:<alias>` and admitted only by a live grant for the op
    /// on this registered staging dir. The operator path is unchanged; a
    /// non-`delegate:` `--as` runs the same steps under that identity.
    Refresh {
        /// The delegate identity — `--as delegate:<alias>`, granted by
        /// `staging delegate` for the ops this run needs.
        #[arg(long = "as")]
        as_identity: String,
        /// Build commit this refresh intends to run (recorded on the lease).
        #[arg(long)]
        to: Option<String>,
        /// Skip the board bounce — restart the daemon only.
        #[arg(long)]
        no_ui: bool,
    },
}

pub(super) fn run(state_dir: PathBuf, action: StagingAction) -> Result<i32> {
    let result = match action {
        StagingAction::Register { board_port } => client::rpc(
            &state_dir,
            "staging_register",
            json!({"board_port": board_port}),
        )?,
        StagingAction::Delegate {
            alias,
            ops,
            ttl,
            reason,
        } => {
            let ttl = cadence_agent::rollout::parse_ttl(&ttl)?;
            client::rpc(
                &state_dir,
                "staging_delegate",
                json!({
                    "agent": alias,
                    "ops": ops,
                    "ttl_secs": ttl.as_secs(),
                    "reason": reason,
                }),
            )?
        }
        StagingAction::Revoke { alias } => {
            client::rpc(&state_dir, "staging_revoke", json!({"agent": alias}))?
        }
        StagingAction::Delegations => client::rpc(&state_dir, "staging_delegations", json!({}))?,
        StagingAction::Refresh {
            as_identity,
            to,
            no_ui,
        } => return staging_refresh(&state_dir, &as_identity, to.as_deref(), no_ui),
    };
    print_json(&result);
    Ok(0)
}

/// `cadence staging refresh` (CAD-1024, contract PR-4): the delegate
/// round-trip. Every op runs under the `--as delegate:<alias>` caller, so
/// each is admitted only by a live `staging_grants` row for that op on this
/// registered dir — never by operator proof. The lease is released in a
/// drop-safe `finally` so a mid-refresh failure never strands it.
fn staging_refresh(
    state_dir: &Path,
    as_identity: &str,
    to: Option<&str>,
    no_ui: bool,
) -> Result<i32> {
    use cadence_agent::rollout::ClaimRequest;
    let caller = cadence_agent::rollout::resolve_caller(Some(as_identity))?;
    let mut out = json!({"delegate": caller.identity});
    // 1. Lease — `claim` runs `require_delegate_grant(.., "rollout_claim")`.
    let claim = cadence_agent::rollout::claim(
        state_dir,
        &ClaimRequest {
            caller: &caller,
            reason: "staging refresh",
            target: to,
            ttl: cadence_agent::rollout::parse_ttl("30m")?,
            takeover: false,
            now: cadence_agent::rollout::unix_now(),
        },
    )?;
    out["claim"] = json!({"holder": claim["holder"]});
    // 2..5 the body; 6 releases the lease in a finally so it is never stranded.
    let body = (|out: &mut Value| -> Result<()> {
        // daemon stop — `shutdown` admits the `delegate:` lease holder via
        // `granted_lease_holder` + a `daemon_stop` grant. A daemon that does
        // not answer is already stopped — refresh tolerates it (the run's
        // whole point is to bring the dir to `--to`, running or not).
        match client::rpc_answer(state_dir, "shutdown", json!({})) {
            Ok(Ok(_)) => {
                if !wait_daemon_exit(state_dir, 30) {
                    return Err(Error::rejected("daemon did not exit within 30s"));
                }
                out["daemon"] = json!({"stopped": true});
            }
            Ok(Err(refused)) => return Err(refused),
            Err(_) => out["daemon"] = json!({"stopped": false, "was_running": false}),
        }
        // daemon start — `authorize_daemon_spawn` gates on the `daemon_start`
        // grant for a `delegate:` caller.
        let started = client::daemon_start_as(state_dir, Some(as_identity))?;
        out["daemon"] = json!({"stopped": true, "start": started["state"]});
        if !no_ui {
            // ui stop + ui start — `ui::run_cli`'s delegate gate.
            cadence_agent::ui::run_cli(
                state_dir,
                &cadence_agent::ui::UiAction::Stop {
                    tailscale_off: false,
                    as_identity: Some(as_identity.to_string()),
                },
            )?;
            cadence_agent::ui::run_cli(
                state_dir,
                &cadence_agent::ui::UiAction::Start {
                    flags: cadence_agent::ui::UiFlags::default(),
                    reset: false,
                    as_identity: Some(as_identity.to_string()),
                },
            )?;
            out["ui"] = json!({"bounced": true});
        }
        Ok(())
    })(&mut out);
    // 6. Release the lease — the delegate holder releases its own.
    let release = cadence_agent::rollout::release(state_dir, &caller);
    out["release"] = json!(release
        .as_ref()
        .map(|r| r["released"].clone())
        .unwrap_or(json!(false)));
    print_json(&out);
    body?;
    release.map(|_| ()).map_err(|e| e)?;
    Ok(0)
}
