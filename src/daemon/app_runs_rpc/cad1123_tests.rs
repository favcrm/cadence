//! CAD-1123 HP2 implementation tests: `app_run_start` takes its owner and
//! workers from the install team, checks the board-fetched quotes, is
//! idempotent, and is operator-only.
//!
//! ACCEPTANCE CHECK SPOT (CAD-1123 HP2+HP3): the non-implementer's check
//! (a frame or agent cannot spend without the host slot tap; a forged or
//! replayed slot token is refused; a non-operator is refused) goes in its
//! own file, written by the reviewer or ticket author. Not written here.

use super::cad1120_tests::{pid, refusal, Fx};
use super::*;
use crate::test_seam::{scoped, Asserted};

impl Fx {
    fn set_team(&self) -> Result<Value> {
        self.operator(
            "app_install_team_set",
            json!({"install_id": self.install, "owner_pm": "lead",
                "roles": {"writer": "writer", "reviewer": "reviewer"}, "expected_revision": 0}),
        )
    }
    fn start_params(&self, request: &str) -> Value {
        json!({
            "install_id": self.install,
            "workflow": "email-brief",
            "request_id": request,
            "expected_quotes": {},
            "inputs": {
                "subject": "Renewal",
                "audience": "Customers due a renewal",
                "facts": "Plan renews 1 July. Price stays HK$88/month.",
            },
        })
    }
}

#[test]
fn start_needs_a_team_then_creates_approves_and_dispatches_once() {
    let fx = Fx::new();
    fx.start_team();
    assert!(
        refusal(fx.operator("app_run_start", fx.start_params("s1"))).contains("no default team")
    );
    fx.set_team().unwrap();
    let run = fx.operator("app_run_start", fx.start_params("s1")).unwrap();
    assert_eq!(run["state"], "running", "{run}");
    assert_eq!(run["snapshot"]["owner_pm"], "lead");
    assert_eq!(run["snapshot"]["inputs"]["writer"], "writer");
    // Replay of the same request returns the same run, no second one.
    let again = fx.operator("app_run_start", fx.start_params("s1")).unwrap();
    assert_eq!(again["id"], run["id"]);
    let listed = fx
        .operator("app_run_list", json!({"install_id": fx.install}))
        .unwrap();
    assert_eq!(listed["runs"].as_array().map_or(0, Vec::len), 1, "{listed}");
}

#[test]
fn start_refuses_forged_roles_owner_quotes_and_extra_fields() {
    let fx = Fx::new();
    fx.start_team();
    fx.set_team().unwrap();
    let mut forged = fx.start_params("f1");
    forged["inputs"]["writer"] = json!("reviewer");
    assert!(refusal(fx.operator("app_run_start", forged)).contains("installation team"));
    for (field, value) in [
        ("owner_pm", json!("lead")),
        ("project_link", json!("x")),
        ("assignments", json!({})),
    ] {
        let mut params = fx.start_params("f2");
        params[field] = value;
        assert!(refusal(fx.operator("app_run_start", params)).contains("unsupported fields"));
    }
    // A quote for a slot the workflow does not freeze is a price mismatch.
    let mut quoted = fx.start_params("f3");
    quoted["expected_quotes"] = json!({"image": {"schema": 1}});
    assert!(refusal(fx.operator("app_run_start", quoted)).contains("price_changed"));
    let mut missing = fx.start_params("f4");
    missing.as_object_mut().unwrap().remove("expected_quotes");
    assert!(refusal(fx.operator("app_run_start", missing)).contains("expected_quotes"));
    let listed = fx
        .operator("app_run_list", json!({"install_id": fx.install}))
        .unwrap();
    assert_eq!(
        listed["runs"].as_array().map_or(0, Vec::len),
        0,
        "nothing was created"
    );
}

#[test]
fn start_and_team_are_operator_only() {
    let fx = Fx::new();
    fx.start_team();
    fx.set_team().unwrap();
    for who in [
        Asserted::Agent("writer".into()),
        Asserted::Agent("lead".into()),
        Asserted::Unproven,
    ] {
        for (method, params) in [
            ("app_run_start", fx.start_params("agent")),
            (
                "app_install_team_set",
                json!({"install_id": fx.install, "owner_pm": "lead",
                    "roles": {"writer": "writer"}, "expected_revision": 1}),
            ),
            ("app_install_team_show", json!({"install_id": fx.install})),
        ] {
            let error = scoped(who.clone(), || fx.shared.dispatch(method, &params, pid()))
                .expect_err(&format!("{who:?} {method} was admitted"))
                .to_string();
            assert!(
                error.contains("operator") || error.contains("unproven"),
                "{who:?} {method}: {error}"
            );
        }
    }
    // Stale team revision loses the compare-and-swap.
    assert!(refusal(fx.set_team()).contains("stale"));
}

/// CAD-1230: a replayed start races the run's own completion. The run was
/// running when the replay read it, and is terminal by the time the replay
/// dispatches it. The replay returns the run as it now stands; it does not
/// fail with "approval absent or stale". A first start still fails.
#[test]
fn replayed_start_of_a_run_that_just_finished_returns_it() {
    let fx = Fx::new();
    fx.start_team();
    fx.set_team().unwrap();
    let run = fx.operator("app_run_start", fx.start_params("r1")).unwrap();
    let id = run["id"].as_str().unwrap().to_string();
    fx.operator("app_run_cancel", json!({"run_id": id}))
        .unwrap();
    let replay = fx.shared.dispatch_started_run(&id, true).unwrap();
    assert_eq!(replay["id"], run["id"]);
    assert_eq!(replay["state"], "cancelled", "{replay}");
    let first = fx.shared.dispatch_started_run(&id, false);
    assert!(refusal(first).contains("approval is absent or stale"));
}
