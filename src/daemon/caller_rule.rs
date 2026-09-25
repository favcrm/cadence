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
    /// A mutation with no connection check yet — each names why and
    /// its follow-up. The table test pins this list.
    Unguarded(&'static str),
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
    (
        "agent_send",
        Rule::Handler(
            "thread_sender: the operator's thread write needs proof (CAD-384); \
             send_with: --priority/--supersedes need agent_caller + may_mutate_agent \
             Steer — the operator or the recipient's PM (CAD-158)",
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
    ("agent_events", Rule::Read),
    ("agent_requests", Rule::Read),
    (
        "agent_respond",
        Rule::Handler("authorize_respond: the operator or the requester's PM (CAD-370)"),
    ),
    (
        "request_open",
        Rule::Unguarded(
            "the agent's own approval broker opens it; binding it to the agent is a follow-up",
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
    ("agent_capture", Rule::Read),
    ("agent_probe", Rule::Read),
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
    ("message_report", Rule::Bearer),
    (
        "message_reconcile",
        Rule::Handler("operator_connection (CAD-374)"),
    ),
    ("message_cancel", Rule::OnAgent(Target::Message, Controlled)),
    (
        "job_new",
        Rule::Unguarded("bookkeeping; the named PM is not yet bound to the caller"),
    ),
    ("job_list", Rule::Read),
    ("job_show", Rule::Read),
    ("job_events", Rule::Read),
    ("job_cancel", BY_OPERATOR),
    ("job_close", BY_OPERATOR),
    (
        "task_new",
        Rule::Unguarded("bookkeeping on an open job; not yet bound to the job's PM"),
    ),
    ("task_show", Rule::Read),
    ("task_dispatch", BY_OPERATOR),
    (
        "task_verdict",
        Rule::Handler("agent_caller: any agent but the assignee/author, or the operator (CAD-372)"),
    ),
    ("task_accept", BY_OPERATOR),
    ("task_sha", BY_OPERATOR),
    ("task_fail", BY_OPERATOR),
    (
        "task_reopen",
        Rule::Handler("agent_caller: the operator or the job's PM (CAD-373)"),
    ),
    ("task_cancel", BY_OPERATOR),
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
        Rule::Unguarded("liveness ping by the monitor's own loop; carries no caller"),
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
    ("slot_acquire", Rule::Handler("slot_caller (CAD-113/230)")),
    ("slot_release", Rule::Handler("slot_caller (CAD-113/230)")),
    ("slot_status", Rule::Handler("slot_caller (CAD-113/230)")),
    ("slot_reconcile", Rule::Handler("proven_operator (CAD-276)")),
    ("slot_launch", Rule::Handler("launch_requester (CAD-230b)")),
    (
        "slot_runner",
        Rule::Handler("slot_identity or proven_operator (CAD-230b)"),
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
        "plan_propose",
        Rule::Handler("slot_identity or operator_evidence"),
    ),
    ("plan_approve", Rule::Handler("operator_connection")),
    ("plan_reject", Rule::Handler("operator_connection")),
    (
        "epic_stage",
        Rule::Handler(
            "slot_identity or operator_evidence; operator_connection into an operator stage \
             or with operator_decision (CAD-432: every board-relayed move)",
        ),
    ),
    ("project_work_approve", Rule::Handler("operator_connection")),
    ("project_work_approvals", Rule::Read),
    (
        "workflow_approve",
        Rule::Handler("operator_connection (CAD-487)"),
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
        "delivery_observe",
        Rule::Handler("operator_connection (CAD-431)"),
    ),
    (
        "delivery_merge",
        Rule::Handler("operator_connection (CAD-431)"),
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
    ("operator_session_check", Rule::Bearer),
    ("operator_session_logout", Rule::Bearer),
    ("operator_session_stolen", Rule::Bearer),
    (
        "operator_sessions",
        Rule::Handler("operator_with_secret: operator proof AND the operator secret (CAD-313)"),
    ),
    (
        "operator_secret_rotate",
        Rule::Handler("operator_with_secret: operator proof AND the operator secret (CAD-313)"),
    ),
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
            Rule::OnAgent(..) | Rule::Attributed { .. } | Rule::Shutdown
        )
    }
}

/// What [`admit`] needs besides the caller — gathered by the daemon
/// only for the rule and caller that need it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Facts {
    /// [`Rule::OnAgent`]: the target agent and its own PM.
    pub(crate) target: Option<(String, Option<String>)>,
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
                    Rule::OnAgent(..) => {
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
                Rule::OnAgent(..) => Some(("by", keep_or(params, "by", Some("operator")))),
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
        let mut out = Vec::new();
        for line in body.lines() {
            let t = line.trim_start();
            if line.len() - t.len() != 12 || !t.starts_with('"') {
                continue;
            }
            let Some((arms, _)) = t.split_once("=>") else {
                continue;
            };
            for arm in arms.split('|') {
                let name = arm.trim().trim_matches('"');
                if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                    out.push(name.to_string());
                }
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
                Rule::Read | Rule::Bearer | Rule::Handler(_) | Rule::Unguarded(_) => {
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
            "task_reopen",
            "monitor_stop",
            "monitor_dispatch",
        ] {
            assert!(matches!(rule_of(m), Some(Rule::Handler(_))), "{m}");
        }
        for m in [
            "task_fail",
            "task_cancel",
            "job_cancel",
            "job_close",
            "task_dispatch",
            "task_accept",
            "task_sha",
            "monitor_alert_ack",
            "monitor_register",
            "agent_ready",
        ] {
            assert!(matches!(rule_of(m), Some(Rule::Attributed { .. })), "{m}");
        }
        assert_eq!(rule_of("shutdown"), Some(Rule::Shutdown));
        // The known gaps are pinned: a new unguarded method is a
        // deliberate edit here, never a silent default.
        let unguarded: Vec<&str> = RULES
            .iter()
            .filter(|(_, r)| matches!(r, Rule::Unguarded(_)))
            .map(|(m, _)| *m)
            .collect();
        assert_eq!(
            unguarded,
            ["request_open", "job_new", "task_new", "monitor_heartbeat"]
        );
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
}
