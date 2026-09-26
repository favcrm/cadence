//! CAD-535: `cadence rollout` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum RolloutAction {
    /// Take the rollout lease. Refused while another unexpired lease is
    /// held; the error names that holder, target, and expiry.
    /// `--takeover` is allowed only after the current lease has expired.
    Claim {
        /// Why this rollout is happening.
        #[arg(long)]
        reason: String,
        /// Build commit this rollout intends to run.
        #[arg(long)]
        target: Option<String>,
        /// How long the lease lasts: 90s, 30m, 12h, 1d, or bare seconds
        /// [default: 12h].
        #[arg(long, default_value = "12h")]
        ttl: String,
        /// Identity outside a cadence pane, for example `operator:ada`.
        #[arg(long = "as")]
        as_identity: Option<String>,
        /// Replace an expired lease. Recorded with the previous holder.
        #[arg(long)]
        takeover: bool,
    },
    /// Show the active lease, or report that none is held.
    Status,
    /// Drop the lease. The holder only, unless `--force` with an
    /// operator identity and a reason. `--force` on a live, unexpired
    /// lease also requires `--holder` naming that holder.
    Release {
        /// Identity outside a cadence pane.
        #[arg(long = "as")]
        as_identity: Option<String>,
        /// Release a lease whose holder is gone. Requires `--reason`
        /// and an operator `--as` that is not a registered agent alias.
        /// A lease that has not expired also requires `--holder`.
        #[arg(long)]
        force: bool,
        /// Why the holder is being ousted. Required with `--force`.
        #[arg(long)]
        reason: Option<String>,
        /// Holder being ousted. Required with `--force` while the lease
        /// is still unexpired, and it must match the lease holder.
        #[arg(long)]
        holder: Option<String>,
    },
    /// Pass the lease to another identity. The holder only. The backup
    /// receipt stays with the lease.
    Handoff {
        /// Identity that should hold the lease next.
        #[arg(long)]
        to: String,
        /// Identity outside a cadence pane.
        #[arg(long = "as")]
        as_identity: Option<String>,
    },
    /// Let an agent claim the rollout lease — and, holding it, stop the
    /// daemon from its own pane (`daemon restart`). The operator only,
    /// from a shell outside every pane; recorded as an event. Without a
    /// grant an agent's `rollout claim` is refused (CAD-384).
    Grant {
        /// The agent alias granted, e.g. the rollout owner `ops-1`.
        alias: String,
        /// How long the grant lasts: 90s, 30m, 12h, 1d, or bare seconds
        /// [default: until revoked].
        #[arg(long)]
        until: Option<String>,
    },
    /// End an agent's rollout grant. The operator only. A lease it
    /// holds no longer lets its pane stop the daemon.
    Revoke {
        /// The agent alias whose grant ends.
        alias: String,
    },
    /// Record a backup receipt on the lease. The holder only. The file
    /// must already exist and be a readable SQLite database; this
    /// command does not take the backup.
    Backup {
        /// Path of the SQLite backup to hash and record.
        #[arg(long)]
        path: PathBuf,
        /// Identity outside a cadence pane.
        #[arg(long = "as")]
        as_identity: Option<String>,
    },
}

pub(super) fn run(state_dir: PathBuf, action: RolloutAction) -> Result<i32> {
    use cadence_agent::rollout::{Caller, ClaimRequest};
    let caller = |as_identity: &Option<String>| -> Result<Caller> {
        let caller = cadence_agent::rollout::resolve_caller(as_identity.as_deref())?;
        cadence_agent::rollout::reject_registered_alias(&state_dir, &caller)?;
        Ok(caller)
    };
    let result = match action {
        RolloutAction::Claim {
            reason,
            target,
            ttl,
            as_identity,
            takeover,
        } => {
            let caller = caller(&as_identity)?;
            let ttl = cadence_agent::rollout::parse_ttl(&ttl)?;
            cadence_agent::rollout::claim(
                &state_dir,
                &ClaimRequest {
                    caller: &caller,
                    reason: &reason,
                    target: target.as_deref(),
                    ttl,
                    takeover,
                    now: cadence_agent::rollout::unix_now(),
                },
            )?
        }
        RolloutAction::Status => cadence_agent::rollout::status(&state_dir)?,
        RolloutAction::Release {
            as_identity,
            force,
            reason,
            holder,
        } => {
            let caller = caller(&as_identity)?;
            if force {
                let reason = reason.ok_or_else(|| {
                    Error::rejected("rollout release --force requires --reason \"<why>\"")
                })?;
                cadence_agent::rollout::release_forced(
                    &state_dir,
                    &caller,
                    &reason,
                    holder.as_deref(),
                )?
            } else if reason.is_some() || holder.is_some() {
                return Err(Error::rejected(
                    "--reason and --holder are only used with `rollout release --force`",
                ));
            } else {
                cadence_agent::rollout::release(&state_dir, &caller)?
            }
        }
        RolloutAction::Handoff { to, as_identity } => {
            cadence_agent::rollout::handoff(&state_dir, &caller(&as_identity)?, &to)?
        }
        RolloutAction::Grant { alias, until } => {
            let until_secs = until
                .as_deref()
                .map(cadence_agent::rollout::parse_ttl)
                .transpose()?
                .map(|d| d.as_secs());
            client::rpc(
                &state_dir,
                "rollout_grant",
                json!({"agent": alias, "until_secs": until_secs}),
            )?
        }
        RolloutAction::Revoke { alias } => {
            client::rpc(&state_dir, "rollout_revoke", json!({"agent": alias}))?
        }
        RolloutAction::Backup { path, as_identity } => {
            cadence_agent::rollout::record_backup(&state_dir, &caller(&as_identity)?, &path)?
        }
    };
    print_json(&result);
    Ok(0)
}
