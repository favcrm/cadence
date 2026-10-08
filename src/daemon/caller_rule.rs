//! The one caller rule for every daemon RPC (CAD-384).
//!
//! [`RULES`] names a rule for EVERY method of `Shared::dispatch`; a test
//! parses the method table from the source, so a method added without
//! a rule fails the build's tests. `Shared::caller_gate` applies the
//! rule before the method runs — a refusal leaves no write.
//!
//! The caller ([`Who`]) comes from the connection alone: the nearest
//! registered pane or enrolled managed endpoint on the peer's `/proc`
//! ancestry IS that agent; deriving none, the peer is the operator only
//! on positive proof ([`crate::peer::operator_proof`], CAD-276). A
//! detached child of an agent — `setsid` + double fork, no agent
//! identity — is [`Who::Unproven`] and is refused by every rule that
//! checks the connection. Request fields never name the caller: an
//! agent is attributed to itself, and a request field naming anyone
//! else (the operator included) is refused.

use serde_json::{json, Value};

use super::IDENTITY_FIELDS;
use crate::peer::{may_mutate_agent, AgentCaller, AgentMutation};

/// Who is on the other end of the connection, for [`admit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Who {
    /// Positive operator proof.
    Operator,
    /// A registered agent, by alias.
    Agent(String),
    /// No agent identity and no operator proof — `why` names the check
    /// that failed.
    Unproven(String),
}

/// Which agent a request acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    /// The `alias` param.
    Alias,
    /// The recipient of the message named by the `message` param.
    Message,
    /// The `job` param.
    Job,
    /// The job of the task named by the `task` param.
    Task,
}

/// A method's caller rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rule {
    /// Writes nothing — no caller check.
    Read,
    /// Authorized by a secret the request carries (a turn token or a
    /// request handle), not by the connection.
    Bearer,
    /// The handler binds the caller from the connection itself — named
    /// so the reader can find it.
    Handler(&'static str),
    /// Acts on an agent: [`may_mutate_agent`] decides with the target's
    /// own PM. Stamps `by` with the caller.
    OnAgent(Target, AgentMutation),
    /// Acts on a job or one of its tasks: the operator, or the job's
    /// PM bound to a registration that predates the job — a PM removed
    /// and re-registered under the same alias inherits nothing
    /// (CAD-422). Stamps `by` with the caller.
    OnJob(Target),
    /// A write attributed to its caller in `field`: an agent is
    /// attributed to itself; the operator's own value is kept, else
    /// `default`.
    Attributed {
        field: &'static str,
        default: Option<&'static str>,
    },
    /// `shutdown`: the operator, or the agent holding the live rollout
    /// lease under a live operator grant (the rollout owner's `daemon
    /// restart` from its pane); in a sandbox, also a caller tied to none
    /// of its agents.
    Shutdown,
}

use AgentMutation::{Controlled, SelfService};

const BY_OPERATOR: Rule = Rule::Attributed {
    field: "by",
    default: Some("operator"),
};

/// Every `Shared::dispatch` method and its rule.
pub(crate) const RULES: &[(&str, Rule)] = &[
    ("health", Rule::Read),
    ("daemon_info", Rule::Read),
    ("shutdown", Rule::Shutdown),
    // CAD-561: the update's drain gate is an operator action — an agent
    // can never stop the fleet's work to push a build. `update_status`
    // is the read side (the board's banner, `cadence update status`).
    (
        "update_drain",
        Rule::Handler("operator_connection (CAD-561)"),
    ),
    ("update_status", Rule::Read),
    (
        "agent_register",
        Rule::Handler(
            "authorize_register: agent callers by CAD-149; an unattributed caller needs proven_operator (CAD-431)",
        ),
    ),
    ("model_defaults_get", Rule::Read),
    (
        "model_defaults_set",
        Rule::Handler("operator_connection (CAD-337)"),
    ),
    ("agent_list", Rule::Read),
    ("agent_show", Rule::Read),
    // CAD-886: `agent_wait` answers a strict subset of what `agent_show`
    // discloses (state/reason/message/turn/waited_secs) — same visibility.
    ("agent_wait", Rule::Read),
    (
        "agent_identity",
        Rule::Handler("caller_identity (verified agent endpoint only, CAD-744)"),
    ),
    (
        "agent_send",
        Rule::Handler(
            "thread_sender: the operator's thread write needs proof (CAD-384); \
             send_with: --priority/--supersedes need agent_caller + may_mutate_agent \
             Steer — the operator or the recipient's PM (CAD-158); \
             issue/worktree are refused for every caller — only dispatch_send \
             and task_dispatch write lane tags (CAD-378 R6)",
        ),
    ),
    (
        "agent_ask",
        Rule::Handler(
            "thread_sender: the operator's thread write needs proof (CAD-384); \
             send_with: --priority/--supersedes need agent_caller + may_mutate_agent \
             Steer — the operator or the recipient's PM (CAD-158)",
        ),
    ),
    ("thread_read", Rule::Read),
    (
        "thread_send",
        Rule::Handler("rpc_thread_send: agents refused, operator on proof (CAD-384)"),
    ),
    // CAD-1098: the app conversations are the operator's chat, like
    // `thread_send` — one proof (`operator_chat`), identical on the board.
    (
        "conversation_list",
        Rule::Handler("operator_chat: agents refused, operator on proof (CAD-1098 I1)"),
    ),
    (
        "conversation_create",
        Rule::Handler("operator_chat: agents refused, operator on proof (CAD-1098 I1)"),
    ),
    ("agent_events", Rule::Read),
    (
        "agent_requests",
        Rule::Handler(
            "agent_caller: pending rows disclose to the operator, the owning \
             agent and its PM — a peer sees none (CAD-506)",
        ),
    ),
    (
        "agent_respond",
        Rule::Handler("authorize_respond: the operator or the requester's PM (CAD-370)"),
    ),
    (
        "request_open",
        Rule::Handler(
            "request_caller: the named agent alone opens its own requests \
             (CAD-452; the stale Unguarded entry corrected, CAD-422)",
        ),
    ),
    ("request_wait", Rule::Bearer),
    ("request_close", Rule::Bearer),
    (
        "agent_ready",
        Rule::Attributed {
            field: "by",
            default: Some("operator"),
        },
    ),
    ("agent_capture", Rule::Handler("app endpoint operator transcript proof (CAD-631)")),
    ("agent_probe", Rule::Handler("app endpoint operator transcript proof (CAD-631)")),
    (
        "agent_answer",
        Rule::Handler("derived_caller: PeerTies (CAD-102)"),
    ),
    (
        "agent_set",
        Rule::Handler("authorize_agent_mutation (CAD-149)"),
    ),
    (
        "agent_recover_submit",
        Rule::Handler("authorize_agent_mutation: the operator or the target's PM (CAD-152)"),
    ),
    (
        "agent_inbox",
        Rule::Handler(
            "rpc_inbox: peek is an unguarded read — a mailbox's consumer has no \
             verifiable identity (CAD-251); a drain consumes, so a proven agent \
             caller may drain only its own inbox (CAD-480)",
        ),
    ),
    (
        "agent_inbox_ack",
        Rule::Handler(
            "rpc_inbox_ack: consumers pass unauthenticated as on the read \
             (CAD-251); an agent caller may ack, park or report only on its own \
             alias, and cursor resets are operator-only (CAD-480)",
        ),
    ),
    (
        "message_read",
        Rule::Handler(
            "rpc_message_read: an agent caller may read only a message \
             addressed to its own alias (CAD-565)",
        ),
    ),
    (
        "message_report",
        Rule::Handler(
            "rpc_message_report: an explicit id+token reports by bearer possession, \
             with id prefixes enumerating the caller's own alias (operator: \
             global; unproven: exact id only); the id-less default resolves \
             the connection caller's own single held turn (CAD-880)"
        ),
    ),
    (
        "message_reconcile",
        Rule::Handler("operator_connection (CAD-374)"),
    ),
    ("message_cancel", Rule::OnAgent(Target::Message, Controlled)),
    (
        "job_new",
        Rule::Handler(
            "rpc_job_new: the operator, or the agent the job names as its PM \
             creating its own job (CAD-422)",
        ),
    ),
    ("job_list", Rule::Read),
    ("job_show", Rule::Read),
    ("job_events", Rule::Read),
    ("job_cancel", Rule::OnJob(Target::Job)),
    ("job_close", Rule::OnJob(Target::Job)),
    ("task_new", Rule::OnJob(Target::Job)),
    ("task_show", Rule::Read),
    ("task_dispatch", Rule::OnJob(Target::Task)),
    (
        "task_verdict",
        Rule::Handler("agent_caller: any agent but the assignee/author, or the operator (CAD-372)"),
    ),
    ("task_accept", Rule::OnJob(Target::Task)),
    ("task_sha", Rule::OnJob(Target::Task)),
    ("task_fail", Rule::OnJob(Target::Task)),
    (
        "task_reopen",
        Rule::OnJob(Target::Task),
    ),
    ("task_cancel", Rule::OnJob(Target::Task)),
    (
        "memory_propose",
        Rule::Handler("memory_actor: caller_identity (CAD-381)"),
    ),
    (
        "memory_review",
        Rule::Handler("memory_actor: caller_identity (CAD-381)"),
    ),
    (
        "memory_finalize",
        Rule::Handler("memory_actor: caller_identity (CAD-381)"),
    ),
    (
        "monitor_register",
        Rule::Attributed {
            field: "owner",
            default: Some("operator"),
        },
    ),
    ("monitor_list", Rule::Read),
    ("monitor_show", Rule::Read),
    (
        "monitor_heartbeat",
        Rule::Handler(
            "rpc_monitor_heartbeat: the operator, or the monitor's owner bound \
             to a registration that predates the monitor (CAD-422)",
        ),
    ),
    ("monitor_alerts", Rule::Read),
    ("monitor_alert_ack", BY_OPERATOR),
    (
        "monitor_stop",
        Rule::Handler("operator_connection (CAD-373)"),
    ),
    (
        "monitor_dispatch",
        Rule::Handler("operator_connection (CAD-373)"),
    ),
    (
        "agent_unfence",
        Rule::Handler("operator_connection_on_agent (CAD-374)"),
    ),
    ("agent_stop", Rule::OnAgent(Target::Alias, SelfService)),
    (
        "agent_remove",
        Rule::Handler("authorize_agent_mutation (CAD-304 S3)"),
    ),
    (
        "agent_gc",
        Rule::Handler("agent_caller + gc_partition (CAD-304 S3)"),
    ),
    ("agent_gc_plan", Rule::Read),
    ("agent_resume", Rule::OnAgent(Target::Alias, SelfService)),
    (
        "slot_acquire",
        Rule::Handler("slot_or_unregistered: build/test run only for the unregistered label (CAD-113/230/1021)"),
    ),
    (
        "slot_release",
        Rule::Handler("slot_caller: no release for an unregistered caller (CAD-113/230/1021)"),
    ),
    (
        "slot_status",
        Rule::Handler("slot_or_unregistered: read-only, open to the unregistered label (CAD-1021)"),
    ),
    ("slot_reconcile", Rule::Handler("proven_operator (CAD-276)")),
    (
        "slot_launch",
        Rule::Handler("launch_requester: pane, endpoint, operator, or the unregistered label for env-less build/test recipes (CAD-230b/1021)"),
    ),
    (
        "slot_runner",
        Rule::Handler("connection_caller: a derived agent or the operator; the unregistered label reads only its own runner (CAD-230b/422/1021)"),
    ),
    (
        "approval_record",
        Rule::Handler("operator_connection (CAD-217)"),
    ),
    (
        "approval_revoke",
        Rule::Handler("operator_connection (CAD-217)"),
    ),
    (
        "approval_designate",
        Rule::Handler("operator_connection_on_agent (CAD-918)"),
    ),
    ("approval_designations", Rule::Read),
    (
        "approval_scope",
        Rule::Handler("operator_connection (CAD-918)"),
    ),
    (
        "approval_delegate",
        Rule::Handler(
            "connection_caller: a designated agent only — the operator and unproven \
             callers are refused (CAD-918)",
        ),
    ),
    (
        "plan_propose",
        Rule::Handler("connection_caller: a derived agent or the operator (CAD-422)"),
    ),
    ("plan_approve", Rule::Handler("operator_connection")),
    (
        "idea_decide",
        Rule::Handler("operator_connection (CAD-139)"),
    ),
    ("plan_reject", Rule::Handler("operator_connection")),
    (
        "epic_stage",
        Rule::Handler(
            "connection_caller: a derived agent or the operator (CAD-422); \
             operator_connection into an operator stage or with operator_decision \
             (CAD-432: every board-relayed move)",
        ),
    ),
    ("project_work_approve", Rule::Handler("operator_connection")),
    (
        "area_ack",
        Rule::Handler("rpc_area_ack: the operator, or the area's owner PM by its connection (CAD-378)"),
    ),
    (
        "dispatch_record",
        Rule::Handler(
            "rpc_dispatch_record: the kickoff's attributed sender or the \
             operator; lane + pm bind the message row's daemon-written \
             fields, never a param or the tracker (CAD-378, CAD-467)",
        ),
    ),
    (
        "dispatch_send",
        Rule::Handler(
            "rpc_dispatch_send: the target's PM or the operator (steer gate); \
             an agent caller must also share the issue's holders when it is \
             doing/review; the lane tags come from the daemon's own \
             resolution, never caller fields (CAD-378 R6)",
        ),
    ),
    (
        "issue_kickoff",
        Rule::Handler("operator_connection (CAD-606)"),
    ),
    (
        "issue_kickoff_options",
        Rule::Handler("operator_connection (CAD-606)"),
    ),
    // CAD-608: the issue page's lane card. `lane_show` is a read of the
    // issue's own lane. Every mutation is operator-only inside the
    // handler — an agent, its detached child, and a forged identity
    // field never reach the worktree.
    ("lane_show", Rule::Read),
    (
        "lane_ask",
        Rule::Handler("operator_connection (CAD-608)"),
    ),
    (
        "lane_instruct",
        Rule::Handler("operator_connection (CAD-608)"),
    ),
    (
        "lane_interrupt",
        Rule::Handler("operator_connection (CAD-608)"),
    ),
    (
        "lane_stop",
        Rule::Handler("operator_connection (CAD-608)"),
    ),
    (
        "lane_unfence",
        Rule::Handler("operator_connection (CAD-608)"),
    ),
    (
        "lane_reassign",
        Rule::Handler("operator_connection (CAD-608)"),
    ),
    ("app_workspace_migration_recover", Rule::Handler("operator_connection (CAD-667)")),
    ("app_chat_descriptor", Rule::Handler("operator_connection (CAD-1110)")),
    ("app_assistant_actions", Rule::Handler("scoped_chat_assistant: connection-derived agent and live turn (CAD-1184)")),
    ("app_assistant_invoke", Rule::Handler("scoped_chat_assistant: connection-derived agent and live turn (CAD-1184)")),
    ("app_assistant_operation_show", Rule::Handler("scoped_chat_assistant: connection-derived agent and live turn (CAD-1184)")),
    ("app_assistant_actions_operator", Rule::Handler("operator_connection (CAD-1184)")),
    ("app_assistant_operations", Rule::Handler("operator_connection (CAD-1184)")),
    ("app_assistant_operation_operator_show", Rule::Handler("operator_connection (CAD-1184)")),
    ("app_assistant_decision", Rule::Handler("operator_connection (CAD-1184)")),
    ("app_assistant_permissions", Rule::Handler("operator_connection (CAD-1184)")),
    ("app_assistant_permission_revoke", Rule::Handler("operator_connection (CAD-1184)")),
    ("app_assistant_permission_block", Rule::Handler("operator_connection (CAD-1184)")),
    ("app_screen_mint", Rule::Handler("operator_connection (CAD-1006)")),
    // The frame-GET peer: authority is the burned one-use nonce, not a
    // connection class — the handler binds it to the consuming session.
    ("app_screen_consume", Rule::Handler("operator_connection + burned one-use frame capability + stored-session liveness (CAD-1006)")),
    ("project_work_approvals", Rule::Read),
    ("app_local_install_approve", Rule::Handler("operator_connection (CAD-631)")),
    ("app_local_install_revoke", Rule::Handler("operator_connection (CAD-631)")),
    ("app_run_create", Rule::Handler("operator_connection (CAD-631)")),
    ("app_run_approve", Rule::Handler("operator_connection (CAD-631)")),
    ("app_run_start", Rule::Handler("operator_connection (CAD-1123): create+approve+dispatch from the install team")),
    ("app_install_team_set", Rule::Handler("operator_connection (CAD-1123)")),
    ("app_install_team_show", Rule::Handler("operator_connection (CAD-1123)")),
    ("app_run_cancel", Rule::Handler("operator_connection (CAD-631)")),
    ("app_run_dispatch", Rule::Handler("operator_connection (CAD-631)")),
    ("app_run_show", Rule::Handler("operator_connection (CAD-631)")),
    ("app_run_list", Rule::Handler("operator_connection (CAD-631)")),
    ("app_run_artifact", Rule::Handler("assigned turn or operator (CAD-631)")),
    ("app_binding_quote", Rule::Handler("operator_connection and exact current binding (CAD-632)")),
    ("app_run_capability_call", Rule::Handler("active assigned app turn and exact frozen binding (CAD-632)")),
    ("app_run_capability_results", Rule::Handler("operator_connection (CAD-632)")),
    ("app_run_capability_result", Rule::Handler("operator_connection (CAD-632)")),
    ("app_run_capability_asset", Rule::Handler("operator_connection (CAD-632)")),
    ("app_binding_create", Rule::Handler("operator_connection (CAD-692)")),
    ("app_binding_update", Rule::Handler("operator_connection (CAD-692)")),
    ("app_binding_revoke", Rule::Handler("operator_connection (CAD-692)")),
    ("app_binding_show", Rule::Handler("operator_connection (CAD-692)")),
    ("app_binding_list", Rule::Handler("operator_connection (CAD-692)")),
    ("app_binding_publish_set", Rule::Handler("operator_connection (CAD-1123)")),
    ("app_effect_stage", Rule::Handler("operator_connection (CAD-692)")),
    ("app_effect_show", Rule::Handler("operator_connection (CAD-692)")),
    ("app_effect_list", Rule::Handler("operator_connection (CAD-692)")),
    ("app_effect_decide", Rule::Handler("operator_connection (CAD-692)")),
    ("app_effect_resolve", Rule::Handler("operator_connection (CAD-692)")),
    ("social_publish_media_import", Rule::Handler("operator_connection (CAD-979)")),
    ("social_publish_schedule", Rule::Handler("operator_connection (CAD-771)")),
    ("social_publish_cancel", Rule::Handler("operator_connection (CAD-771)")),
    ("social_publish_show", Rule::Handler("operator_connection (CAD-771)")),
    ("social_publish_list", Rule::Handler("operator_connection (CAD-771)")),
    ("social_publish_claim_due", Rule::Handler("operator_connection (CAD-771)")),
    ("social_publish_send_now", Rule::Handler("operator_connection (CAD-1041)")),
    ("social_publish_reconcile", Rule::Handler("operator_connection (CAD-771)")),
    ("social_publish_report", Rule::Handler("operator_connection (CAD-771)")),
    ("social_publish_start", Rule::Handler("operator_connection (CAD-1123)")),
    ("social_publish_reschedule", Rule::Handler("operator_connection (CAD-1123)")),
    ("app_context_create", Rule::Handler("operator_connection (CAD-690)")),
    ("app_context_list", Rule::Handler("operator_connection (CAD-690)")),
    ("app_context_show", Rule::Handler("operator_connection (CAD-690)")),
    ("app_context_update", Rule::Handler("operator_connection (CAD-690)")),
    ("app_context_archive", Rule::Handler("operator_connection (CAD-690)")),
    ("app_record_create", Rule::Handler("operator_connection (CAD-753)")),
    ("app_record_list", Rule::Handler("operator_connection (CAD-753)")),
    ("app_record_show", Rule::Handler("operator_connection (CAD-753)")),
    ("app_record_update", Rule::Handler("operator_connection (CAD-753)")),
    ("app_record_csv_preview", Rule::Handler("operator_connection (CAD-779)")),
    ("app_record_csv_import", Rule::Handler("operator_connection (CAD-779)")),
    ("app_record_csv_confirm", Rule::Handler("operator_connection (CAD-1014: host-bound confirm receipt mint)")),
    ("app_segment_save", Rule::Handler("operator_connection (CAD-780)")),
    ("app_segment_show", Rule::Handler("operator_connection (CAD-780)")),
    ("app_segment_list", Rule::Handler("operator_connection (CAD-780)")),
    ("app_exclusion_save", Rule::Handler("operator_connection (CAD-780)")),
    ("app_exclusion_show", Rule::Handler("operator_connection (CAD-780)")),
    ("app_exclusion_list", Rule::Handler("operator_connection (CAD-780)")),
    ("app_suppression_add", Rule::Handler("operator_connection (CAD-780)")),
    ("app_suppression_remove", Rule::Handler("operator_connection (CAD-780)")),
    ("app_suppression_list", Rule::Handler("operator_connection (CAD-780)")),
    ("app_audience_preview", Rule::Handler("operator_connection (CAD-780)")),
    ("app_audience_prepare", Rule::Handler("operator_connection (CAD-780)")),
    ("app_audience_show", Rule::Handler("operator_connection (CAD-780)")),
    ("app_sender_binding_save", Rule::Handler("operator_connection (CAD-782)"),
    ),
    ("app_sender_binding_show", Rule::Handler("operator_connection (CAD-782)"),
    ),
    ("app_sender_binding_list", Rule::Handler("operator_connection (CAD-782)"),
    ),
    ("app_content_save", Rule::Handler("operator_connection (CAD-782)")),
    ("app_content_show", Rule::Handler("operator_connection (CAD-782)")),
    ("app_content_clone", Rule::Handler("operator_connection (CAD-1182)")),
    ("app_content_list", Rule::Handler("operator_connection (CAD-782)")),
    ("app_content_render", Rule::Handler("operator_connection (CAD-782)")),
    ("app_content_propose", Rule::Handler("operator_connection (CAD-782)")),
    ("app_content_proposal_request", Rule::Handler("operator_connection (CAD-813)")),
    ("app_content_assistant_propose", Rule::Handler("active assigned chat turn and verified App binding (CAD-813)")),
    ("app_content_assistant_draft", Rule::Handler("active assigned scoped chat turn and verified App binding; one turn one draft, host-derived source, inert pending (CAD-1014)")),
    ("app_record_csv_assistant_import", Rule::Handler("active assigned scoped chat turn and verified App binding (CAD-1014)")),
    ("app_segment_assistant_save", Rule::Handler("active assigned scoped chat turn and verified App binding (CAD-1014)")),
    ("app_segment_assistant_list", Rule::Handler("active assigned scoped chat turn and verified App binding (CAD-1014); read-only, no claim")),
    ("app_segment_assistant_show", Rule::Handler("active assigned scoped chat turn and verified App binding (CAD-1014); read-only, no claim")),
    ("app_record_csv_assistant_preview", Rule::Handler("active assigned scoped chat turn and verified App binding (CAD-1014); read-only preview, no claim")),
    ("app_segment_assistant_preview", Rule::Handler("active assigned scoped chat turn and verified App binding (CAD-1014); bounded membership read, no freeze/send")),
    ("app_content_assistant_proposals", Rule::Handler("active assigned scoped chat turn and verified App binding (CAD-1014); read-only proposal list, no claim")),
    ("app_content_assistant_proposal_show", Rule::Handler("active assigned scoped chat turn and verified App binding (CAD-1014); read-only proposal show, no claim")),
    ("app_content_proposal_show", Rule::Handler("operator_connection (CAD-782)")),
    ("app_content_proposal_render", Rule::Handler("operator_connection (CAD-782); before-Apply preview read (CAD-1014)")),
    ("app_content_proposal_list", Rule::Handler("operator_connection (CAD-782)")),
    ("app_content_proposal_apply", Rule::Handler("operator_connection (CAD-782)")),
    ("app_content_proposal_discard", Rule::Handler("operator_connection (CAD-782)")),
    ("app_content_approve", Rule::Handler("operator_connection (CAD-782)")),
    ("app_content_test_prepare", Rule::Handler("operator_connection (CAD-782)")),
    ("app_content_send_prepare", Rule::Handler("operator_connection (CAD-782)")),
    ("app_workspace_install", Rule::Handler("operator_connection (CAD-667)")),
    ("app_workspace_upgrade", Rule::Handler("operator_connection (CAD-743)")),
    ("app_workspace_install_check", Rule::Handler("operator_connection (CAD-1186)")),
    ("app_workspace_upgrade_check", Rule::Handler("operator_connection (CAD-743)")),
    ("app_workspace_upgrade_recover", Rule::Handler("operator_connection (CAD-743)")),
    ("app_workspace_list", Rule::Handler("operator_connection (CAD-667)")),
    ("app_workspace_show", Rule::Handler("operator_connection (CAD-667)")),
    ("app_workspace_migrate", Rule::Handler("operator_connection (CAD-667)")),
    ("app_workspace_recover", Rule::Handler("operator_connection (CAD-667)")),
    // CAD-1129: the apps Explorer. Reads and member verbs take the
    // operator connection OR a `member_as` the daemon re-proves against
    // a live public member session; writes stay the operator's.
    (
        "app_catalog_list",
        Rule::Handler("operator_connection (+ re-proven member_as for member scope) (CAD-1129)"),
    ),
    (
        "app_catalog_show",
        Rule::Handler("operator_connection (+ re-proven member_as for member scope) (CAD-1129)"),
    ),
    (
        "app_catalog_git_check",
        Rule::Handler("operator_connection (CAD-1129)"),
    ),
    ("app_home", Rule::Handler("operator_connection (+ re-proven member_as for member scope) (CAD-1129)")),
    (
        "app_favorites_get",
        Rule::Handler("operator_connection (+ re-proven member_as for member scope) (CAD-1129)"),
    ),
    (
        "app_favorites_put",
        Rule::Handler("operator_connection (+ re-proven member_as for member scope) (CAD-1129)"),
    ),
    (
        "app_favorites_put_default",
        Rule::Handler("operator_connection (CAD-1129)"),
    ),
    (
        "app_favorites_opened",
        Rule::Handler("operator_connection (+ re-proven member_as for member scope) (CAD-1129)"),
    ),
    (
        "app_install_request",
        Rule::Handler("operator_connection + re-proven member_as requester (CAD-1129)"),
    ),
    (
        "app_install_requests_list",
        Rule::Handler("operator_connection (CAD-1129)"),
    ),
    (
        "app_install_request_dismiss",
        Rule::Handler("operator_connection (CAD-1129)"),
    ),
    (
        "app_workspace_install_entry",
        Rule::Handler("operator_connection (CAD-1129)"),
    ),
    (
        "app_workspace_update_check",
        Rule::Handler("operator_connection (CAD-1129)"),
    ),
    (
        "app_workspace_remove_preview",
        Rule::Handler("operator_connection (CAD-1129)"),
    ),
    (
        "app_workspace_remove",
        Rule::Handler("operator_connection (CAD-1129)"),
    ),
    (
        "app_workspace_restore",
        Rule::Handler("operator_connection (CAD-1129)"),
    ),
    (
        "workflow_approve",
        Rule::Handler("operator_connection (CAD-487)"),
    ),
    (
        "app_approve",
        Rule::Handler("operator_connection (CAD-547)"),
    ),
    (
        "app_revoke",
        Rule::Handler("operator_connection (CAD-577)"),
    ),
    (
        "app_set_team",
        Rule::Handler("operator_connection (CAD-577)"),
    ),
    (
        "app_add_worker",
        Rule::Handler("operator_connection (CAD-577)"),
    ),
    // CAD-339: the master agent's verbs. `Shared::master_policy` runs
    // before this table for every method (the master's allowlist).
    (
        "master_dispatch",
        Rule::Handler("caller_is_master (CAD-339)"),
    ),
    (
        "question_escalate",
        Rule::Handler("master_or_operator (CAD-339)"),
    ),
    (
        "agent_file_write",
        Rule::Handler("operator_connection (CAD-339)"),
    ),
    (
        "master_start",
        Rule::Handler("operator_connection (CAD-339)"),
    ),
    (
        "master_summary",
        Rule::Handler("reads; `post` needs master_or_operator (CAD-339)"),
    ),
    ("master_state", Rule::Read),
    (
        "master_models",
        Rule::Handler(
            "operator_connection (CAD-575): the master's model vocabulary, cost \
             tiers and effort levels for the board's model picker",
        ),
    ),
    (
        "master_command",
        Rule::Handler(
            "operator_connection (CAD-551): the operator's allowlisted provider-session \
             commands for the master — the verb list is the daemon's, never raw input",
        ),
    ),
    // CAD-615: the master files and retries; the operator decides.
    (
        "master_ask_permission",
        Rule::Handler("require_master_caller (CAD-615)"),
    ),
    (
        "master_peek_grant",
        Rule::Handler("require_master_caller (CAD-615)"),
    ),
    (
        "master_permission_use",
        Rule::Handler("require_master_caller (CAD-615)"),
    ),
    (
        "master_permission_allow_once",
        Rule::Handler("operator_connection (CAD-615)"),
    ),
    (
        "master_permission_always",
        Rule::Handler("operator_connection (CAD-615)"),
    ),
    (
        "master_permission_reject",
        Rule::Handler("operator_connection (CAD-615)"),
    ),
    (
        "master_permission_revoke",
        Rule::Handler("operator_connection (CAD-615)"),
    ),
    (
        "master_permission_list",
        Rule::Handler("operator_connection (CAD-615)"),
    ),
    // CAD-574: a Needs-you row suppression is the operator's call —
    // `needs_dismissed.json` is daemon-owned like `area_acks.json`.
    (
        "needs_dismiss",
        Rule::Handler("operator_connection (CAD-574)"),
    ),
    (
        "reports_changed",
        Rule::Handler("proven_operator (CAD-339)"),
    ),
    // CAD-431: the worker loop's review and merge verbs.
    (
        "report_verdict",
        Rule::Handler("agent_caller: the report's assigned reviewer (CAD-431)"),
    ),
    // CAD-447: an answer reaches the question's author.
    (
        "answer_route",
        Rule::Handler("agent_caller: the answer's own recorded author (CAD-447)"),
    ),
    ("delivery_list", Rule::Read),
    (
        "delivery_review_evidence",
        Rule::Handler("operator_connection (CAD-120)"),
    ),
    (
        "delivery_observe",
        Rule::Handler("operator_connection (CAD-431)"),
    ),
    (
        "delivery_merge",
        Rule::Handler("operator_connection (CAD-431)"),
    ),
    (
        "delivery_approve",
        Rule::Handler("operator_connection (CAD-140)"),
    ),
    (
        "delivery_decline",
        Rule::Handler("operator_connection (CAD-431)"),
    ),
    (
        "interrupt",
        Rule::Handler("authorize_agent_mutation / master_dispatched (CAD-323)"),
    ),
    (
        "rollout_grant",
        Rule::Handler("operator_connection (CAD-384)"),
    ),
    (
        "rollout_revoke",
        Rule::Handler("operator_connection (CAD-384)"),
    ),
    // CAD-1024: the staging allowlist and grant store. Every write is the
    // operator's alone — an agent, a detached child or a forged operator
    // field never reaches `staging_delegate`/`staging_revoke`/
    // `staging_register`. `staging_delegations` is read-only.
    (
        "staging_register",
        Rule::Handler("operator_connection (CAD-1024)"),
    ),
    (
        "staging_delegate",
        Rule::Handler("operator_connection (CAD-1024)"),
    ),
    (
        "staging_revoke",
        Rule::Handler("operator_connection (CAD-1024)"),
    ),
    ("staging_delegations", Rule::Read),
    (
        "project_new",
        Rule::Handler("caller_is_master or operator_connection (CAD-358)"),
    ),
    (
        "operator_link_mint",
        Rule::Handler("operator_with_secret: operator proof AND the operator secret (CAD-313)"),
    ),
    // CAD-313: the board's session verbs — the nonce or the session
    // token the request carries is the credential.
    (
        "operator_session_open",
        Rule::Handler("nonce bearer; a connection that derives an agent spends it and is refused (CAD-313)"),
    ),
    (
        "operator_session_open_device",
        Rule::Handler("issuer bearer, verified live against the daemon-owned device-login config; agent callers refused pre-mint (CAD-777, CAD-841)"),
    ),
    ("operator_session_check", Rule::Bearer),
    ("operator_session_logout", Rule::Bearer),
    ("operator_session_stolen", Rule::Bearer),
    // CAD-841: the daemon owns the device-login config — boards read
    // issuer+org through the open read; only the operator-secret verbs
    // write it or disclose the allowlist.
    ("device_login_config", Rule::Read),
    (
        "operator_device_login_set",
        Rule::Handler("operator_with_secret: operator proof AND the operator secret (CAD-841)"),
    ),
    (
        "operator_device_login_clear",
        Rule::Handler("operator_with_secret: operator proof AND the operator secret (CAD-841)"),
    ),
    (
        "operator_device_login_show",
        Rule::Handler("operator_with_secret: operator proof AND the operator secret (CAD-841)"),
    ),
    // CAD-526: the platform sign-in exchange — the compact JWS is the
    // credential; a connection that derives an agent is refused before
    // the `jti` is consumed (the handler checks).
    (
        "board_session_open",
        Rule::Handler(
            "assertion bearer; agent callers refused pre-verification (CAD-526)",
        ),
    ),
    ("board_session_check", Rule::Bearer),
    // CAD-1129: a member's public session the board opens for
    // `member_as` re-proof. The board relays it over its own operator
    // connection, so it is gated by `operator_connection` — a bearer
    // alone never admits it.
    ("board_session_member", Rule::Handler("operator_connection (CAD-1129)")),
    (
        "operator_sessions",
        Rule::Handler("operator_with_secret: operator proof AND the operator secret (CAD-313)"),
    ),
    (
        "operator_secret_rotate",
        Rule::Handler("operator_with_secret: operator proof AND the operator secret (CAD-313)"),
    ),
    // CAD-366: platform custody and grants (ADR 0006 §5.3) — every
    // custody write is the operator's alone; the reads bind an agent
    // caller to its own grants.
    (
        "platform_enroll",
        Rule::Handler("operator_connection (CAD-366)"),
    ),
    (
        "platform_rotate",
        Rule::Handler("operator_connection (CAD-366)"),
    ),
    (
        "platform_revoke",
        Rule::Handler("operator_connection (CAD-366)"),
    ),
    (
        "platform_grant",
        Rule::Handler("operator_connection (CAD-366)"),
    ),
    (
        "platform_ungrant",
        Rule::Handler("operator_connection (CAD-366)"),
    ),
    (
        "platform_default_set",
        Rule::Handler("operator_connection (CAD-366)"),
    ),
    ("connection_providers", Rule::Handler("rpc_connection")),
    ("connection_list", Rule::Handler("rpc_connection")),
    ("connection_show", Rule::Handler("rpc_connection")),
    ("connection_check", Rule::Handler("rpc_connection")),
    ("connection_create", Rule::Handler("rpc_connection")),
    ("connection_rotate", Rule::Handler("rpc_connection")),
    ("connection_revoke", Rule::Handler("rpc_connection")),
    (
        "connection_test",
        Rule::Handler("rpc_connection: operator_connection (CAD-1065)"),
    ),
    ("crm_smtp_bind", Rule::Handler("rpc_crm_smtp")),
    ("crm_smtp_rebind", Rule::Handler("rpc_crm_smtp")),
    ("crm_smtp_revoke", Rule::Handler("rpc_crm_smtp")),
    ("crm_smtp_show", Rule::Handler("rpc_crm_smtp")),
    ("crm_smtp_test_send", Rule::Handler("rpc_crm_smtp")),
    // CAD-786: the send verbs all enter `rpc_crm_send`, whose first
    // act is `operator_connection`.
    ("crm_send_prepare", Rule::Handler("rpc_crm_send")),
    ("crm_send_approve", Rule::Handler("rpc_crm_send")),
    ("crm_send_show", Rule::Handler("rpc_crm_send")),
    ("crm_send_list", Rule::Handler("rpc_crm_send")),
    ("crm_send_resolve", Rule::Handler("rpc_crm_send")),
    ("crm_send_origin_set", Rule::Handler("rpc_crm_send")),
    ("crm_send_origin_show", Rule::Handler("rpc_crm_send")),
    // CAD-786: the unsubscribe token is the credential — the verb
    // can only add a suppression.
    ("crm_unsubscribe_redeem", Rule::Bearer),
    ("platform_accounts", Rule::Read),
    ("platform_defaults", Rule::Read),
    (
        "platform_grants",
        Rule::Handler(
            "grant_caller: an agent reads its own grants; the operator reads \
             any (CAD-366)",
        ),
    ),
    (
        "platform_check",
        Rule::Handler(
            "grant_caller: an agent checks its own grants; the operator names \
             the holder (CAD-366)",
        ),
    ),
    // CAD-506: the effect gate (ADR 0006 §5.2, §5.4).
    (
        "platform_call",
        Rule::Handler(
            "request_caller: the calling agent is connection-derived; the \
             record's agent is never a request field (CAD-506)",
        ),
    ),
    (
        "platform_effects",
        Rule::Handler(
            "effect_caller: an agent reads its own pending effects; the \
             operator reads all (CAD-506)",
        ),
    ),
    (
        "platform_effect_close",
        Rule::Handler(
            "effect_caller: the operator closes any closeable row; an agent \
             closes only its own waiting one (CAD-506)",
        ),
    ),
    // CAD-546: the `local` platform's outbox ledger — the operator's
    // read alone (the board's `/api/outbox` relays it, gated the same).
    (
        "platform_outbox",
        Rule::Handler("operator_connection (CAD-546)"),
    ),
    // CAD-580: the wiki v1 store. Every method binds `wiki_caller` —
    // agent_caller on the connection (unproven refused), reconciled
    // with `wiki_as` (operator connections only; an agent's claim must
    // name itself). `wiki::allowed` then decides per path prefix.
    (
        "wiki_ls",
        Rule::Handler("wiki_caller: agent_caller + wiki_as (CAD-580)"),
    ),
    (
        "wiki_read",
        Rule::Handler("wiki_caller: agent_caller + wiki_as (CAD-580)"),
    ),
    (
        "wiki_write",
        Rule::Handler("wiki_caller: agent_caller + wiki_as (CAD-580)"),
    ),
    (
        "wiki_put_blob",
        Rule::Handler("wiki_caller: agent_caller + wiki_as (CAD-580)"),
    ),
    (
        "wiki_mkdir",
        Rule::Handler("wiki_caller: agent_caller + wiki_as (CAD-580)"),
    ),
    (
        "wiki_mv",
        Rule::Handler("wiki_caller: agent_caller + wiki_as (CAD-580)"),
    ),
    (
        "wiki_rm",
        Rule::Handler("wiki_caller: agent_caller + wiki_as (CAD-580)"),
    ),
    (
        "wiki_search",
        Rule::Handler("wiki_caller: agent_caller + wiki_as (CAD-580)"),
    ),
    (
        "wiki_history",
        Rule::Handler("wiki_caller: agent_caller + wiki_as (CAD-580)"),
    ),
    // CAD-719: index health is operator-only — a read-only status and
    // an explicit rebuild. These do not bind `wiki_caller`: they gate
    // on `operator_connection` like the other operator actions, and an
    // agent or relayed caller can never read or kick the index.
    (
        "wiki_index_status",
        Rule::Handler("operator_connection (CAD-719)"),
    ),
    (
        "wiki_index_refresh",
        Rule::Handler("operator_connection (CAD-719)"),
    ),
    // CAD-129: submitting a test run is a mutation attributed to the
    // caller. Reading a job, its log, or the queue is not.
    (
        "test_submit",
        Rule::Attributed {
            field: "by",
            default: Some("operator"),
        },
    ),
    ("test_status", Rule::Read),
    ("test_log", Rule::Read),
    ("test_queue", Rule::Read),
];

/// The rule for `method`; `None` for a method the daemon does not
/// serve (dispatch refuses it as unknown).
pub(crate) fn rule_of(method: &str) -> Option<Rule> {
    RULES.iter().find(|(m, _)| *m == method).map(|(_, r)| *r)
}

impl Rule {
    /// Does [`admit`] need the connection's caller for this rule.
    pub(crate) fn checks_connection(self) -> bool {
        matches!(
            self,
            Rule::OnAgent(..) | Rule::OnJob(..) | Rule::Attributed { .. } | Rule::Shutdown
        )
    }
}

/// What [`admit`] needs besides the caller — gathered by the daemon
/// only for the rule and caller that need it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Facts {
    /// [`Rule::OnAgent`]: the target agent and its own PM.
    pub(crate) target: Option<(String, Option<String>)>,
    /// [`Rule::OnJob`]: `(job id, its stored pm_alias, the bound PM)` —
    /// the PM row predating the job, or `None` when none does.
    pub(crate) job: Option<(String, String, Option<String>)>,
    /// [`Rule::Shutdown`]: the live rollout lease holder, when it also
    /// holds a live operator grant (`rollout::granted_lease_holder`).
    pub(crate) lease_holder: Option<String>,
    /// [`Rule::Shutdown`], [`Rule::OnAgent`]: this daemon serves a
    /// sandbox (CAD-310) and the caller is tied to none of its agents
    /// (`Shared::sandbox_outsider`) — e.g. `sandbox down` run from a
    /// production agent's pane.
    pub(crate) sandbox_outsider: bool,
}

/// The verb as refusals name it: `agent_stop` → `agent stop`.
pub(crate) fn verb(method: &str) -> String {
    method.replace('_', " ")
}

/// Decide one request. `Ok(Some((field, value)))` admits it with
/// `field` stamped to the caller's attribution; `Ok(None)` admits it
/// unchanged; `Err` is the refusal text, naming the rule.
pub(crate) fn admit(
    method: &str,
    rule: Rule,
    who: &Who,
    params: &Value,
    facts: &Facts,
) -> Result<Option<(&'static str, Value)>, String> {
    if !rule.checks_connection() {
        return Ok(None);
    }
    let verb = verb(method);
    let alias = match who {
        Who::Unproven(why) => {
            // A sandbox's owner running `sandbox down` from a production
            // pane: tied to none of the sandbox's agents (CAD-310/384).
            if facts.sandbox_outsider {
                match rule {
                    Rule::Shutdown => return Ok(None),
                    Rule::OnAgent(..) | Rule::OnJob(..) => {
                        return Ok(Some(("by", json!("operator (sandbox)"))));
                    }
                    _ => {}
                }
            }
            return Err(format!(
                "{verb} refused: this connection derives no agent identity and is \
                 not provably the operator: {why}. Run it from the calling agent's \
                 own pane, or from an attached operator shell outside every pane \
                 and managed endpoint (caller rule, CAD-384)"
            ));
        }
        Who::Operator => {
            return Ok(match rule {
                Rule::OnAgent(..) | Rule::OnJob(..) => {
                    Some(("by", keep_or(params, "by", Some("operator"))))
                }
                Rule::Attributed { field, default } => {
                    Some((field, keep_or(params, field, default)))
                }
                _ => None,
            });
        }
        Who::Agent(alias) => alias.as_str(),
    };
    // The daemon's identity-shaped fields (CAD-149): an agent may carry
    // one only when it names the agent itself.
    for field in IDENTITY_FIELDS {
        match params.get(*field) {
            None | Some(Value::Null) => {}
            Some(v) if v.as_str() == Some(alias) => {}
            Some(v) => {
                // The CLI of an agent whose env lost `CADENCE_ALIAS`
                // defaults `--by`/`--owner` to the operator.
                let hint = if v.as_str() == Some("operator") {
                    format!(
                        "; drop `--{field} operator` (and set CADENCE_ALIAS={alias}) — the \
                         daemon records the caller itself"
                    )
                } else {
                    String::new()
                };
                return Err(format!(
                    "{verb} refused: agent '{alias}' is attributed to itself — request \
                     field '{field}' names {v}, and an agent never acts as another \
                     agent or as the operator{hint} (caller rule, CAD-384)"
                ));
            }
        }
    }
    match rule {
        Rule::Shutdown => match facts.lease_holder.as_deref() {
            Some(holder) if holder == alias => Ok(None),
            holder => Err(format!(
                "{verb} refused: agent '{alias}' does not hold the rollout lease under \
                 a live operator grant (granted holder: {}) — the daemon is stopped by \
                 the operator, or by the rollout owner the operator granted \
                 (`cadence rollout grant {alias}`, then `cadence rollout claim`) \
                 (caller rule, CAD-384)",
                holder.unwrap_or("none")
            )),
        },
        Rule::OnAgent(_, mutation) => {
            let (target, pm) = facts
                .target
                .as_ref()
                .ok_or_else(|| format!("{verb} refused: its target agent is unknown"))?;
            may_mutate_agent(
                &AgentCaller::Agent(alias.to_string()),
                target,
                pm.as_deref(),
                mutation,
                &verb,
            )?;
            Ok(Some(("by", json!(alias))))
        }
        Rule::OnJob(_) => match &facts.job {
            Some((_, _, Some(pm))) if pm == alias => Ok(Some(("by", json!(alias)))),
            Some((job_id, stored, bound)) => {
                let bound_note = match bound {
                    Some(b) => format!("bound to '{b}'"),
                    None => format!("'{stored}' names no registration predating the job"),
                };
                Err(format!(
                    "{verb} refused: agent '{alias}' is not job '{job_id}'s PM \
                     ({bound_note}) — the operator or that PM runs it (caller \
                     rule, CAD-422)"
                ))
            }
            None => Err(format!(
                "{verb} refused: agent '{alias}' — the job or task is unknown"
            )),
        },
        Rule::Attributed { field, .. } => Ok(Some((field, json!(alias)))),
        _ => Ok(None),
    }
}

/// The operator's own value for `field` when it carries one, else
/// `default` (JSON null when there is none).
fn keep_or(params: &Value, field: &str, default: Option<&str>) -> Value {
    match params.get(field) {
        Some(v) if !v.is_null() => v.clone(),
        _ => default.map_or(Value::Null, |d| json!(d)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every method name in `Shared::dispatch`'s match — parsed from the
    /// source so the test sees methods added later (the CAD-339 table
    /// parse).
    pub(crate) fn dispatch_methods() -> Vec<String> {
        let src = include_str!("../daemon.rs");
        let start = src.find("    fn dispatch_method(\n").expect("dispatch fn");
        let body = &src[start..];
        let body = &body[body.find("match method {").expect("match")..];
        let body = &body[..body
            .find("other => Err(Error::rejected(format!(\"Unknown method")
            .expect("end")];
        // rustfmt splits a long `"a" | "b" | "c" =>` arm across lines,
        // so join each arm's continuation lines (they start with `|`)
        // before reading its names — a wrapped arm still counts once.
        let mut out = Vec::new();
        let mut arm_text = String::new();
        let mut in_arm = false;
        for line in body.lines() {
            let t = line.trim_start();
            let indent = line.len() - t.len();
            if in_arm && t.starts_with('|') {
                // A wrapped `| "next"` continuation — same 12-space
                // indent as the arm's first line.
                arm_text.push_str(t);
            } else {
                in_arm = false;
            }
            if indent == 12 && t.starts_with('"') {
                arm_text.clear();
                arm_text.push_str(t);
                in_arm = true;
            }
            if in_arm && arm_text.contains("=>") {
                let (arms, _) = arm_text.split_once("=>").unwrap();
                for arm in arms.split('|') {
                    let name = arm.trim().trim_matches('"');
                    if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                    {
                        out.push(name.to_string());
                    }
                }
                in_arm = false;
            }
        }
        out
    }

    fn agent(a: &str) -> Who {
        Who::Agent(a.to_string())
    }

    fn detached() -> Who {
        Who::Unproven("pid 7 on its ancestry carries CADENCE_ALIAS".into())
    }

    fn p_facts() -> Facts {
        Facts::default()
    }

    /// Facts for a request on `tgt`, a worker in pm's group.
    fn on_tgt() -> Facts {
        Facts {
            target: Some(("tgt".into(), Some("pm".into()))),
            ..Facts::default()
        }
    }

    /// CAD-384 acceptance 4: the table covers the whole method table, so
    /// a method added without a rule fails here; and every mutating
    /// method's rule is asserted for an agent (a stranger, the target
    /// itself, the target's PM), a detached child and the operator.
    #[test]
    fn every_dispatch_method_has_a_caller_rule() {
        let table = dispatch_methods();
        assert!(table.len() > 50, "method table parse: {table:?}");
        for m in &table {
            assert!(
                rule_of(m).is_some(),
                "method {m} has no caller rule (CAD-384)"
            );
        }
        for (m, _) in RULES {
            assert!(table.iter().any(|t| t == m), "rule for unknown method {m}");
        }
        assert!(rule_of("a_method_added_tomorrow").is_none());

        let p = json!({});
        for (m, rule) in RULES {
            let facts = on_tgt();
            let stranger = admit(m, *rule, &agent("pm2"), &p, &facts);
            let own = admit(m, *rule, &agent("tgt"), &p, &facts);
            let pm = admit(m, *rule, &agent("pm"), &p, &facts);
            let det = admit(m, *rule, &detached(), &p, &facts);
            let op = admit(m, *rule, &Who::Operator, &p, &facts);
            match rule {
                Rule::Read | Rule::Bearer | Rule::Handler(_) => {
                    for r in [&stranger, &own, &pm, &det, &op] {
                        assert_eq!(r, &Ok(None), "{m}: the gate never checks a {rule:?}");
                    }
                }
                Rule::OnAgent(_, mutation) => {
                    assert!(
                        stranger
                            .unwrap_err()
                            .contains("cannot change another agent"),
                        "{m}"
                    );
                    assert!(
                        det.unwrap_err().contains("not provably the operator"),
                        "{m}"
                    );
                    assert_eq!(pm, Ok(Some(("by", json!("pm")))), "{m}");
                    assert_eq!(op, Ok(Some(("by", json!("operator")))), "{m}");
                    match mutation {
                        SelfService => assert_eq!(own, Ok(Some(("by", json!("tgt")))), "{m}"),
                        Controlled | AgentMutation::Steer => {
                            assert!(own.is_err(), "{m}: an agent on itself")
                        }
                    }
                }
                Rule::Attributed { field, default } => {
                    assert_eq!(stranger, Ok(Some((*field, json!("pm2")))), "{m}");
                    assert!(
                        det.unwrap_err().contains("not provably the operator"),
                        "{m}"
                    );
                    let want = default.map_or(Value::Null, |d| json!(d));
                    assert_eq!(op, Ok(Some((*field, want))), "{m}");
                    // An agent naming the operator, or another agent, is refused.
                    for forged in [json!({*field: "operator"}), json!({*field: "pm"})] {
                        let r = admit(m, *rule, &agent("pm2"), &forged, &facts);
                        assert!(r.unwrap_err().contains("attributed to itself"), "{m}");
                    }
                    // Naming itself is fine; the operator's own value is kept.
                    let own_name = json!({*field: "pm2"});
                    assert!(admit(m, *rule, &agent("pm2"), &own_name, &facts).is_ok());
                    let named = json!({*field: "pm"});
                    assert_eq!(
                        admit(m, *rule, &Who::Operator, &named, &facts),
                        Ok(Some((*field, json!("pm")))),
                        "{m}"
                    );
                }
                Rule::OnJob(_) => {
                    // `on_tgt` binds no job, so every agent is refused
                    // with the unknown-job refusal; only the operator
                    // passes, stamped `by`.
                    for r in [&stranger, &own, &pm] {
                        assert!(
                            r.as_ref()
                                .unwrap_err()
                                .contains("the job or task is unknown"),
                            "{m}: {r:?}"
                        );
                    }
                    assert!(
                        det.unwrap_err().contains("not provably the operator"),
                        "{m}"
                    );
                    assert_eq!(op, Ok(Some(("by", json!("operator")))), "{m}");
                }
                Rule::Shutdown => {
                    for r in [&stranger, &own, &pm, &det] {
                        assert!(
                            r.as_ref().unwrap_err().contains("caller rule"),
                            "{m}: {r:?}"
                        );
                    }
                    assert_eq!(op, Ok(None), "{m}");
                }
            }
        }
    }

    /// The methods named in CAD-384's acceptance, and the operator-only
    /// and operator-attributed verbs, carry the rule the issue asks for —
    /// never a pass-through one.
    #[test]
    fn cad384_methods_carry_their_rule() {
        for m in ["agent_stop", "agent_resume", "message_cancel"] {
            assert!(matches!(rule_of(m), Some(Rule::OnAgent(..))), "{m}");
        }
        // Gated in their handlers by #221 (CAD-370/372/373/374), on the
        // same derivation (`agent_caller`) and operator proof.
        for m in [
            "agent_unfence",
            "message_reconcile",
            "agent_respond",
            "task_verdict",
            "monitor_stop",
            "monitor_dispatch",
            "job_new",
            "monitor_heartbeat",
            "request_open",
        ] {
            assert!(matches!(rule_of(m), Some(Rule::Handler(_))), "{m}");
        }
        // CAD-422: a job's PM alone runs its job and task verbs; a
        // stranger agent is refused at the rule layer.
        for m in ["job_cancel", "job_close", "task_new"] {
            assert_eq!(rule_of(m), Some(Rule::OnJob(Target::Job)), "{m}");
        }
        for m in [
            "task_dispatch",
            "task_accept",
            "task_sha",
            "task_fail",
            "task_reopen",
            "task_cancel",
        ] {
            assert_eq!(rule_of(m), Some(Rule::OnJob(Target::Task)), "{m}");
        }
        for m in ["monitor_alert_ack", "monitor_register", "agent_ready"] {
            assert!(matches!(rule_of(m), Some(Rule::Attributed { .. })), "{m}");
        }
        assert_eq!(rule_of("shutdown"), Some(Rule::Shutdown));
        // CAD-422 closed the last `Unguarded` entries: the variant is
        // gone, so a new method that needs no connection check can only
        // be a `Read` or a `Handler` that names its own proof — both a
        // deliberate edit here, never a silent default.
    }

    /// Shutdown: the rollout lease holder's pane may stop the daemon; a
    /// sandbox also admits a caller tied to none of its agents (an agent
    /// of production running `cadence sandbox down`); nothing else.
    #[test]
    fn shutdown_admits_the_operator_and_the_lease_holder() {
        let holder = Facts {
            lease_holder: Some("pm".into()),
            ..Facts::default()
        };
        let p = json!({});
        assert_eq!(
            admit("shutdown", Rule::Shutdown, &agent("pm"), &p, &holder),
            Ok(None)
        );
        let e = admit("shutdown", Rule::Shutdown, &agent("w1"), &p, &holder).unwrap_err();
        assert!(e.contains("holder: pm"), "{e}");
        assert!(admit("shutdown", Rule::Shutdown, &detached(), &p, &holder).is_err());
        let sandbox = Facts {
            sandbox_outsider: true,
            ..Facts::default()
        };
        assert_eq!(
            admit("shutdown", Rule::Shutdown, &detached(), &p, &sandbox),
            Ok(None)
        );
        assert!(admit("shutdown", Rule::Shutdown, &agent("w1"), &p, &sandbox).is_err());
        // The sandbox outsider may also stop and resume the sandbox's
        // agents (`sandbox down`), recorded as the sandbox's operator;
        // an unproven caller outside a sandbox still may not.
        let stop = Rule::OnAgent(Target::Alias, SelfService);
        assert_eq!(
            admit("agent_stop", stop, &detached(), &p, &sandbox),
            Ok(Some(("by", json!("operator (sandbox)"))))
        );
        assert!(admit("agent_stop", stop, &detached(), &p, &Facts::default()).is_err());
        // Attributed and handler rules get no sandbox exemption.
        assert!(admit("task_fail", BY_OPERATOR, &detached(), &p, &sandbox).is_err());
        // An agent naming the operator is told to drop the flag.
        let e = admit(
            "task_fail",
            BY_OPERATOR,
            &agent("w1"),
            &json!({"by": "operator"}),
            &p_facts(),
        )
        .unwrap_err();
        assert!(e.contains("drop `--by operator`"), "{e}");
        assert_eq!(
            admit(
                "shutdown",
                Rule::Shutdown,
                &Who::Operator,
                &p,
                &Facts::default()
            ),
            Ok(None)
        );
    }

    /// CAD-561: `update_drain` is the update's fleet gate — an agent can
    /// never stop the fleet's work to push a build, and a detached
    /// (unproven) caller cannot either; only the operator passes. The
    /// read side, `update_status`, is open like every other read.
    #[test]
    fn update_drain_is_operator_only_and_never_an_agent() {
        assert_eq!(
            rule_of("update_drain"),
            Some(Rule::Handler("operator_connection (CAD-561)"))
        );
        assert_eq!(rule_of("update_status"), Some(Rule::Read));
        assert!(!rule_of("update_status").unwrap().checks_connection());
        // `Handler` means the table admits nobody: the handler itself
        // proves the connection (`daemon.rs`'s arm runs
        // `operator_connection`, pinned by
        // `cad561_update_drain_is_operator_gated_in_the_dispatch`), so
        // an agent can never pass by being an agent. A table rule that
        // checked the connection would be the weaker shape.
        assert!(!rule_of("update_drain").unwrap().checks_connection());
        assert!(!rule_of("update_status").unwrap().checks_connection());
    }
}
