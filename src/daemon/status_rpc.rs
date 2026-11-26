use serde_json::{json, Value};

use super::{caller_rule, Shared};
use crate::error::Result;
use crate::proto;

impl Shared {
    pub(super) fn rpc_health(&self, peer_pid: u32) -> Result<Value> {
        let redact_inflight = match self.connection_caller(peer_pid) {
            Ok(caller_rule::Who::Agent(alias)) => {
                match self.store.running_channel_authority(&alias) {
                    Ok(Some(t)) => t.authority_mode == "member",
                    Ok(None) => false,
                    Err(_) => true,
                }
            }
            Ok(_) => false,
            Err(_) => true,
        };
        let (adopted_live, adopted_oldest, adopted_reaped_total) = crate::reaper::adopted_report(5);
        Ok(json!({
            "state": "ready",
            "pid": std::process::id(),
            // CAD-694: this run's instance id — `daemon restart`
            // binds the recovery record to the daemon it actually
            // started, not whatever a later boot left on disk.
            "instance": self.instance,
            // The board compares this boot-pinned UID to the private
            // record before attributing any session-bearing peer.
            "agent_uid": self.agent_uid,
            "sandbox": crate::sandbox::profile(),
            "protocol": proto::PROTOCOL_VERSION,
            "capabilities": proto::capabilities(),
            "agent_gc_timer": self.agent_gc.status(),
            "agent_auto_stop": self.auto_stop.status(),
            // CAD-308: whether orphans of what this daemon launched
            // re-parent to it (true for `daemon run`).
            "child_subreaper": crate::reaper::is_subreaper(),
            // Adopted orphans still running (never killed) and the
            // adopted children reaped so far — CAD-308.
            "adopted_live": adopted_live,
            "adopted_oldest": if redact_inflight {
                json!([])
            } else {
                json!(adopted_oldest
                    .iter()
                    .map(|a| json!({"pid": a.pid, "comm": a.comm, "age_secs": a.age_secs}))
                    .collect::<Vec<_>>())
            },
            "adopted_reaped_total": adopted_reaped_total,
            // CAD-538: the hosted lease, when held — provider, epoch,
            // expiry and the fence reason after a loss.
            "lease": self.lease.as_ref().map(|l| l.status_json()),
            // CAD-1020: the publish driver's last/next tick, status
            // and last error — `sender_not_configured` when no send
            // transport is attached.
            "social_publish_driver": self.social_publish_driver.status_json(),
            // CAD-561: a pending update and what it waits on, so
            // `cadence daemon status` and the board's banner show it.
            "pending_update": self.pending_update().map(|p| p.to_json()),
            "update_waiting": if redact_inflight {
                json!([])
            } else {
                json!(self.inflight_turns().unwrap_or_default())
            },
        }))
    }

    pub(super) fn rpc_daemon_info(&self) -> Result<Value> {
        // Registered connection (platform adapter) names — an
        // app slot's binding is checked against these (CAD-547).
        let mut connections: Vec<&str> = self.platforms.keys().map(String::as_str).collect();
        connections.sort_unstable();
        Ok(json!({
            "build_commit": crate::overview::BUILD_COMMIT,
            "build_time": crate::overview::BUILD_TIME,
            "started_at": self.started_at,
            "connections": connections,
            // CAD-1193: the lock-holding daemon's own pid — the
            // same fact `health` reports, without the fleet-wide
            // adopted-process and inflight-turn collection
            // `health` runs first. The board's operator proof
            // reads it here so a courtesy read never waits on
            // the heavyweight probe. Daemon-owned and computed
            // per reply, never caller-supplied.
            "pid": std::process::id(),
        }))
    }
}
