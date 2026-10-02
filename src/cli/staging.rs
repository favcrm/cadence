//! CAD-1024: `cadence staging` — the staging-delegation allowlist and grant
//! store. Every verb relays a daemon RPC; `register`/`delegate`/`revoke` are
//! operator-only at the daemon (`operator_connection`), `delegations` a read.
//! Nothing here admits a delegate yet — PR-3 adds the `delegate:` caller.

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
                    "alias": alias,
                    "ops": ops,
                    "ttl_secs": ttl.as_secs(),
                    "reason": reason,
                }),
            )?
        }
        StagingAction::Revoke { alias } => {
            client::rpc(&state_dir, "staging_revoke", json!({"alias": alias}))?
        }
        StagingAction::Delegations => client::rpc(&state_dir, "staging_delegations", json!({}))?,
    };
    print_json(&result);
    Ok(0)
}
