/// Command templates for needs-me rows — every `cadence …` shape the
/// screen can emit. `overview_commands_all_parse` in `src/cli/tests.rs`
/// parses most emitted forms with `Cli::try_parse_from`. The `<choice>`
/// answer, attach, and bare delivery-sync shapes are not directly
/// exercised by that test.
pub fn cmd_agent_unfence(alias: &str) -> String {
    format!("cadence agent unfence {alias}")
}

pub fn cmd_agent_show(alias: &str) -> String {
    format!("cadence agent show {alias}")
}

pub fn cmd_agent_resume(alias: &str) -> String {
    format!("cadence agent resume {alias}")
}

/// The menu-answer command for a pty pane probing `approval_menu` —
/// `<choice>` is the option's printed index on the open menu.
pub fn cmd_agent_answer(alias: &str) -> String {
    format!("cadence agent answer {alias} <choice>")
}

/// The recovery for a silently ended turn: reconcile the dead turn
/// from the pane. A plain follow-up `send` would queue behind the
/// unreported turn — the actor holds one report-owing turn at a time —
/// and since CAD-565 a nudge is bound to a live turn, so it can never
/// enter the idle pane a silent-ended turn leaves behind. `attach` is
/// the view into the pane to report or interrupt it.
pub fn cmd_agent_attach(alias: &str) -> String {
    format!("cadence agent attach {alias}")
}

pub fn cmd_inbox(alias: &str) -> String {
    format!("cadence inbox {alias}")
}

/// The respond command for a pending request, by request method:
/// provider input requests want an answers file, the approval shapes
/// (provider `*requestApproval`, `session/request_permission`, and
/// brokered `cadence/*`) take a decision, and anything else gets the
/// inspect command — the daemon rejects a respond it cannot map.
pub fn cmd_agent_respond(alias: &str, handle: &str, method: &str) -> String {
    if method == "item/tool/requestUserInput" {
        format!(
            "cadence agent respond {alias} --request {handle} --answers-file answers-{handle}.json"
        )
    } else if method.starts_with("cadence/")
        || method.ends_with("requestApproval")
        || method == "session/request_permission"
    {
        format!("cadence agent respond {alias} --request {handle} --decision accept")
    } else {
        format!("cadence agent requests {alias}")
    }
}

pub fn cmd_issue_show(id: &str) -> String {
    format!("cadence issue show {id}")
}

/// CAD-431: the operator's merge decision on a PASSed ticket.
pub fn cmd_delivery_merge(id: &str) -> String {
    format!("cadence delivery merge {id}")
}

/// CAD-431: decline it instead (the reason is the operator's).
pub fn cmd_delivery_decline(id: &str) -> String {
    format!("cadence delivery decline {id} --reason \"<why>\"")
}

/// CAD-431: re-read the loop's PRs from GitHub (and turn off auto-merge
/// on a moved head).
pub fn cmd_delivery_sync(id: &str) -> String {
    format!("cadence delivery sync {id}")
}

/// CAD-446: re-read every loop's PR from the operator's own shell.
pub const CMD_DELIVERY_SYNC: &str = "cadence delivery sync";

pub fn cmd_issue_set_ready(id: &str) -> String {
    format!("cadence issue set {id} status=ready")
}

pub const CMD_ISSUE_SYNC: &str = "cadence issue sync";
pub const CMD_RESTART_WHEN_IDLE: &str = "cadence daemon restart --when-idle --ui";
/// Deploy-drift remedy (CAD-334): install the newest tested main build.
/// `upgrade` verifies the CI artifact, repoints the CLI, and prints the
/// lease-gated restart ([`CMD_RESTART_WHEN_IDLE`]) as the explicit next
/// step — it never restarts on its own.
pub const CMD_UPGRADE_LATEST_MAIN: &str = "cadence upgrade --latest-main";
