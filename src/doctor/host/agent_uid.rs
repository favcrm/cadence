//! CAD-536: `cadence doctor host` check `agent_uid` — moved verbatim from src/doctor/host.rs.

use super::*;

/// ADR 0007 T1: the dedicated-agent-uid boundary, folded into one
/// watchdog row. The audit itself reads the live host through
/// `agent_uid::LiveHost` — it takes no `Scan`, so a fabricated fixture
/// cannot make this row lie. On an unprovisioned host with clean
/// negatives the row is `ok` ("not provisioned"); a violation already
/// armed is `warn`; once provisioned, the audit's worst row rules.
pub(super) fn check_agent_uid() -> Check {
    let view = crate::agent_uid::LiveHost::new();
    let (level, detail, remedy, value) =
        crate::agent_uid::audit::host_summary(&view, crate::agent_uid::OPERATOR_USER);
    check(
        "agent-uid",
        match level {
            "warn" => Level::Warn,
            "fail" => Level::Fail,
            _ => Level::Ok,
        },
        value,
        json!("§5 artifacts provisioned per §3; §4 negative assertions hold"),
        detail,
        remedy,
    )
}
