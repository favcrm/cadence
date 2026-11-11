//! CAD-2239 refusal acceptance. Authored from the ticket by its author, not
//! by the implementer: a reviewer never runs in a terminal pane.
//!
//! The real register door (`agent_register` through `Shared::dispatch`) must
//! refuse a reviewer role on a `pty` endpoint for the operator and for an
//! agent caller alike, name the headless form, and leave no agent row behind.
//! The positive control sends the same reviewer request for a headless
//! endpoint, so the refusal is the pane rule and not a bad request or the
//! role. A reviewer pane row that predates the rule is refused when the daemon
//! builds its adapter. Other roles keep their pane.

use crate::adapter::registry::refuse_reviewer_pane;
use crate::adapter::AdapterHooks;
use crate::daemon::Shared;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::json;
use std::sync::Arc;

const PANE_PROVIDERS: [&str; 3] = ["devin", "cursor", "claude"];
/// Every spelling that reads as a review is one role to the pin and the pane
/// rule; a variant must not fall through to a pane.
const REVIEW_ROLES: [&str; 3] = ["reviewer", "Reviewer", "code-reviewer"];
const REFUSAL: &str = "a reviewer does not run in a terminal pane";

fn daemon(dir: &std::path::Path) -> Arc<Shared> {
    let pm = dir.join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let opts = crate::daemon::ServeOptions {
        test_seam: true,
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    Shared::new(dir, &opts).unwrap()
}

fn store_row(
    shared: &Shared,
    alias: &str,
    provider: &str,
    endpoint_kind: &str,
    role: &str,
    cwd: &str,
) {
    shared
        .store
        .register_agent(&NewAgent {
            alias,
            provider,
            endpoint_kind,
            role,
            cwd,
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
}

#[test]
fn the_register_door_refuses_a_reviewer_pane_for_every_caller() {
    let dir = tempfile::Builder::new().prefix("c2239r").tempdir().unwrap();
    let shared = daemon(dir.path());
    let pid = std::process::id();
    let cwd = dir.path().to_str().unwrap().to_string();
    store_row(&shared, "root", "fake", "fake", "pm", &cwd);

    let register = |as_agent: bool, alias: &str, provider: &str, kind: &str, role: &str| {
        let who = if as_agent {
            Asserted::Agent("root".into())
        } else {
            Asserted::Operator
        };
        scoped(who, || {
            shared.dispatch(
                "agent_register",
                &json!({
                    "alias": alias, "provider": provider, "endpoint_kind": kind,
                    "role": role, "cwd": cwd,
                    "params": "{\"upstream\":\"root\"}",
                }),
                pid,
            )
        })
    };

    for provider in PANE_PROVIDERS {
        for (index, role) in REVIEW_ROLES.iter().enumerate() {
            for as_agent in [false, true] {
                let alias = format!("{provider}-r{index}-{as_agent}");
                let refused = register(as_agent, &alias, provider, "pty", role)
                    .expect_err("a reviewer pane must be refused at the register door")
                    .to_string();
                assert!(refused.contains(REFUSAL), "{alias} as {role}: {refused}");
                assert!(
                    refused.contains("cadence join <pm> claude"),
                    "the refusal names the headless form: {refused}"
                );
                assert!(
                    shared.store.agent_opt(&alias).unwrap().is_none(),
                    "a refused reviewer pane must leave no row: {alias}"
                );
            }
        }
    }

    // Positive control: the same reviewer request for a headless endpoint is
    // accepted from both callers, so the refusals above are the pane rule.
    for as_agent in [false, true] {
        let alias = format!("headless-{as_agent}");
        register(as_agent, &alias, "fake", "fake", "reviewer")
            .expect("a headless reviewer still registers");
        assert!(shared.store.agent_opt(&alias).unwrap().is_some());
    }

    // The rule is the reviewer role, not the pane: other roles keep theirs.
    // Asserted on the guard itself, because registering a real pane launches it.
    for provider in PANE_PROVIDERS {
        for role in ["worker", "pm"] {
            assert!(
                refuse_reviewer_pane(provider, "pty", role).is_ok(),
                "{provider} pane as {role} must not be refused"
            );
        }
    }
}

#[test]
fn a_reviewer_pane_row_that_predates_the_rule_is_refused_when_its_adapter_is_built() {
    let dir = tempfile::Builder::new().prefix("c2239b").tempdir().unwrap();
    let shared = daemon(dir.path());
    let cwd = dir.path().to_str().unwrap().to_string();
    for provider in PANE_PROVIDERS {
        // Written straight to the store, as a row registered before the rule.
        let alias = format!("legacy-{provider}");
        store_row(&shared, &alias, provider, "pty", "reviewer", &cwd);
        let agent = shared.store.agent(&alias).unwrap();
        let hooks = AdapterHooks {
            on_event: Box::new(|_, _| panic!("a refused reviewer pane emitted traffic")),
            on_request: Box::new(|_| panic!("a refused reviewer pane requested authority")),
        };
        let refused = crate::adapter::build(
            &agent,
            hooks,
            &dir.path().join(format!("{alias}.log")),
            &shared.provider_env,
            None,
        )
        .err()
        .unwrap_or_else(|| panic!("{alias}: a stored reviewer pane row must not build"))
        .to_string();
        assert!(refused.contains(REFUSAL), "{alias}: {refused}");
    }
}
