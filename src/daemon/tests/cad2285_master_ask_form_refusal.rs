//! CAD-2285 independent refusal acceptance. Authored from the ticket, not the
//! implementation: `master_ask_form` is the master's, on its live assigned
//! Home turn, and asks the same form at most once per turn. The operator and
//! any other agent are refused by the real dispatch guard, a replay on the
//! same live turn is refused, and no refusal leaves a `surface` thread entry.

use crate::daemon::Shared;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};

const GENERATION: &str = "2285aabbccddeeff2285aabbccddeeff";
const MESSAGE: &str = "cad2285-home-turn";

fn form() -> Value {
    json!({"version": "1.0", "createSurface": {
        "surfaceId": "s-pick",
        "catalogId": "https://a2ui.org/specification/v1_0/catalogs/basic/catalog.json",
        "components": [
            {"id": "root", "component": "Card", "child": "col"},
            {"id": "col", "component": "Column", "children": ["title", "day", "send"]},
            {"id": "title", "component": "Text", "text": "Pick a day"},
            {"id": "day", "component": "ChoicePicker", "label": "Day",
             "variant": "mutuallyExclusive",
             "options": [{"label": "Monday", "value": "mon"},
                         {"label": "Friday", "value": "fri"}],
             "value": {"@path": "/day"}},
            {"id": "send", "component": "Button", "variant": "primary", "child": "send_label",
             "action": {"event": {"name": "pick.answer",
                                  "context": {"day": {"@path": "/day"}}}}},
            {"id": "send_label", "component": "Text", "text": "Send answer"}
        ],
        "dataModel": {"day": ["mon"]}
    }})
}

fn surface_entries(dir: &std::path::Path) -> i64 {
    rusqlite::Connection::open(dir.join("cadence.sqlite3"))
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM thread_entries WHERE kind='surface'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn only_the_masters_live_home_turn_asks_a_form_once() {
    let dir = tempfile::Builder::new().prefix("c2285").tempdir().unwrap();
    let pm = dir.path().join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let opts = crate::daemon::ServeOptions {
        test_seam: true,
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    for (alias, role) in [(crate::master::ALIAS, "pm"), ("worker", "worker")] {
        shared
            .store
            .register_agent(&NewAgent {
                alias,
                provider: "pi",
                endpoint_kind: "managed",
                role,
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        shared
            .store
            .set_identity_with_quota(
                alias,
                &crate::adapter::Identity {
                    thread_id: format!("{alias}-thread"),
                    session_id: format!("{alias}-session"),
                    model: None,
                    effort: None,
                    pid: std::process::id(),
                    endpoint: None,
                    generation: Some(GENERATION.into()),
                    attach: None,
                },
                None,
            )
            .unwrap();
    }
    let pid = std::process::id();
    let params = json!({"form": form()});
    let ask = |who: Asserted| {
        scoped(who, || shared.dispatch("master_ask_form", &params, pid)).map_err(|e| e.to_string())
    };

    // No live turn yet: even the master is refused, and the operator and
    // another agent are refused as callers.
    for (who, why) in [
        (Asserted::Operator, "operator"),
        (Asserted::Agent("worker".into()), "other agent"),
        (Asserted::Agent(crate::master::ALIAS.into()), "no live turn"),
    ] {
        let err = ask(who).expect_err(why);
        assert!(!err.is_empty(), "{why}: empty refusal");
    }
    assert_eq!(surface_entries(dir.path()), 0, "a refusal wrote an entry");

    // The master's Home turn goes live.
    scoped(Asserted::Operator, || {
        shared.dispatch(
            "thread_send",
            &json!({"alias": crate::master::ALIAS, "text": "plan the week", "message": MESSAGE}),
            pid,
        )
    })
    .expect("operator message to the master's Home");
    rusqlite::Connection::open(dir.path().join("cadence.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE messages SET state='submitting', started=1.0 WHERE id=?1",
            [MESSAGE],
        )
        .unwrap();
    let token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(GENERATION);
    shared.store.mark_running(MESSAGE, &token).unwrap();

    // Still refused for the operator and another agent while the turn is live.
    for who in [Asserted::Operator, Asserted::Agent("worker".into())] {
        ask(who).expect_err("only the master's own turn may ask");
    }
    assert_eq!(surface_entries(dir.path()), 0, "a refusal wrote an entry");

    // The master asks once: recorded. The same form again on the same live
    // turn is refused and adds nothing.
    let asked = ask(Asserted::Agent(crate::master::ALIAS.into()))
        .expect("the master's first ask on its live Home turn");
    assert_eq!(asked["state"], "asked", "{asked}");
    assert_eq!(surface_entries(dir.path()), 1);
    let replay = ask(Asserted::Agent(crate::master::ALIAS.into()))
        .expect_err("the same form twice on one turn");
    assert!(replay.contains("already asked"), "{replay}");
    assert_eq!(surface_entries(dir.path()), 1, "the replay wrote an entry");
}
