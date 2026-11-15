//! CAD-2298 independent refusal acceptance. Authored from the ticket, not the
//! implementation: `master_ask_form` is made only on the master's live
//! assigned turn in its Home conversation. A turn running in the Recruit
//! conversation, a turn running in an app conversation and a turn whose
//! token is not current are each refused by the real dispatch guard, and no
//! refusal writes a `surface` thread entry. The same request succeeds on a
//! live Home turn, so each refusal is the guard and not a bad request.

use crate::daemon::Shared;
use crate::store::{ConversationKind, NewAgent};
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};

const GENERATION: &str = "2298aabbccddeeff2298aabbccddeeff";
const OTHER_GENERATION: &str = "2298ffeeddccbbaa2298ffeeddccbbaa";

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

fn db(dir: &std::path::Path) -> rusqlite::Connection {
    rusqlite::Connection::open(dir.join("cadence.sqlite3")).unwrap()
}

fn surface_entries(dir: &std::path::Path) -> i64 {
    db(dir)
        .query_row(
            "SELECT COUNT(*) FROM thread_entries WHERE kind='surface'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn master_ask_form_is_refused_outside_a_live_home_turn() {
    let dir = tempfile::Builder::new().prefix("c2298").tempdir().unwrap();
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
    let alias = crate::master::ALIAS;
    shared
        .store
        .register_agent(&NewAgent {
            alias,
            provider: "claude",
            endpoint_kind: "managed",
            role: "worker",
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
                thread_id: "master-thread".into(),
                session_id: "master-session".into(),
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
    let pid = std::process::id();
    let params = json!({"form": form()});
    let operator =
        |method: &str, p: Value| scoped(Asserted::Operator, || shared.dispatch(method, &p, pid));
    let ask = || {
        scoped(Asserted::Agent(alias.into()), || {
            shared.dispatch("master_ask_form", &params, pid)
        })
        .map_err(|e| e.to_string())
    };
    // An operator message becomes the master's running turn, in the Home
    // conversation or in the one named by `conversation`.
    let go_live = |message: &str, conversation: Option<&str>, token: &str| {
        let mut send = json!({"alias": alias, "text": "plan the week", "message": message});
        if let Some(id) = conversation {
            send["conversation"] = json!(id);
        }
        operator("thread_send", send).expect("operator message to the master");
        db(dir.path())
            .execute(
                "UPDATE messages SET state='submitting', started=1.0 WHERE id=?1",
                [message],
            )
            .unwrap();
        shared.store.mark_running(message, token).unwrap();
    };
    let finish = |message: &str| {
        db(dir.path())
            .execute(
                "UPDATE messages SET state='done', completed=2.0 WHERE id=?1",
                [message],
            )
            .unwrap();
    };
    let current = || crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint(GENERATION);

    // Control: the same request on a live Home turn succeeds.
    go_live("cad2298-home", None, &current());
    let asked = ask().expect("the same form asks on a live Home turn");
    assert_eq!(asked["state"], "asked", "{asked}");
    assert_eq!(surface_entries(dir.path()), 1);
    finish("cad2298-home");

    // (a) The master's live turn in its Recruit conversation.
    let recruit = operator(
        "conversation_create",
        json!({"alias": alias, "kind": "recruit"}),
    )
    .expect("the Recruit conversation");
    let recruit_id = recruit["conversation"]["id"].as_str().unwrap().to_string();
    go_live("cad2298-recruit", Some(&recruit_id), &current());
    let err = ask().expect_err("a Recruit turn must not ask a form");
    assert!(err.contains("Recruit"), "not the Home-only guard: {err}");
    assert_eq!(surface_entries(dir.path()), 1, "a Recruit refusal wrote");
    finish("cad2298-recruit");

    // (b) The master's live turn in an app conversation: the turn's own
    // message is recorded in that conversation's thread.
    let (app_thread, _) = shared
        .store
        .conversation_create(alias, "cad2298-app", None, ConversationKind::Fresh)
        .unwrap();
    go_live("cad2298-app-turn", None, &current());
    db(dir.path())
        .execute(
            "UPDATE thread_entries SET thread_id=?1 WHERE message_id='cad2298-app-turn'",
            [&app_thread.id],
        )
        .unwrap();
    let err = ask().expect_err("an app conversation turn must not ask a form");
    assert!(
        err.contains(
            "master ask-form is made in the master's Home conversation, not in an app conversation"
        ),
        "not the scope guard: {err}"
    );
    assert_eq!(surface_entries(dir.path()), 1, "an app refusal wrote");
    finish("cad2298-app-turn");

    // (c) A Home turn whose token is not the current generation's.
    let stale = crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint(OTHER_GENERATION);
    go_live("cad2298-stale", None, &stale);
    let err = ask().expect_err("a stale turn token must not ask a form");
    assert!(
        err.contains("no longer current"),
        "not the token guard: {err}"
    );
    assert_eq!(surface_entries(dir.path()), 1, "a stale refusal wrote");

    // The same turn with a current token asks: the token was the only cause.
    db(dir.path())
        .execute(
            "UPDATE messages SET turn_id=?1 WHERE id='cad2298-stale'",
            [&current()],
        )
        .unwrap();
    ask().expect("the same turn with a current token asks");
    assert_eq!(surface_entries(dir.path()), 2);
}
