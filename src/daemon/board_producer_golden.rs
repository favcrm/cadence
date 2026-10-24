//! CAD-1795: the daemon's `master_state` and `thread_read` replies, pinned
//! byte for byte. The goldens under `src/ui/board_api_golden/` were captured
//! from the code that built these replies with `json!` literals;
//! `CADENCE_WRITE_GOLDEN=1` rewrites them. The member views of the same
//! replies are pinned by `ui::board_api_golden` from these files.

use super::*;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use std::path::PathBuf;

fn golden(name: &str, actual: &Value) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/ui/board_api_golden")
        .join(name);
    let text = serde_json::to_string_pretty(actual).unwrap() + "\n";
    if std::env::var_os("CADENCE_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, &text).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(committed, text, "{name} differs from the committed golden");
}

fn fixture() -> (tempfile::TempDir, Arc<Shared>) {
    let dir = tempfile::Builder::new().prefix("c95g").tempdir().unwrap();
    let pm = dir.path().join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let opts = ServeOptions {
        provider_env: crate::adapter::ProviderEnv::refusing_providers(),
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    opts.provider_env
        .set("CADENCE_HOME", dir.path().to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    shared
        .store
        .register_agent(&NewAgent {
            alias: "master",
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: &cwd,
            sandbox: "read-only",
            instructions: None,
            params: Some(r#"{"model":"fixture/model","effort":"high"}"#),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    crate::test_seam::seed_pi_fixture_policy(&shared.store, "fixture/model").unwrap();
    (dir, shared)
}

fn call(shared: &Arc<Shared>, method: &str, params: Value) -> Value {
    let mut out = scoped(Asserted::Operator, || {
        shared.dispatch(method, &params, std::process::id())
    })
    .unwrap();
    // The store mints the conversation id and stamps wall-clock times; the
    // golden keeps their presence and type, not their value.
    if let Some(thread) = shared.store.thread("master").unwrap() {
        scrub(&mut out, &thread.id);
    }
    out
}

fn db(dir: &tempfile::TempDir) -> rusqlite::Connection {
    rusqlite::Connection::open(crate::rollout::db_file(dir.path())).unwrap()
}

#[test]
fn master_state_and_thread_read_bytes_are_unchanged() {
    let (dir, shared) = fixture();
    // An idle master: no turn, no queue.
    golden(
        "master_state_idle.json",
        &call(&shared, "master_state", json!({})),
    );

    for id in ["m-run", "m-q1", "m-q2"] {
        call(
            &shared,
            "thread_send",
            json!({"alias": "master", "text": format!("hello {id}"), "message": id}),
        );
    }
    // Queued: the head waits.
    let queued = call(&shared, "master_state", json!({}));
    // Pin the clocks the replies carry.
    let pin = |sql: &str| db(&dir).execute(sql, []).unwrap();
    pin("UPDATE messages SET created = 100.5 WHERE id = 'm-run'");
    pin("UPDATE messages SET created = 101.5 WHERE id = 'm-q1'");
    pin("UPDATE messages SET created = 102.5 WHERE id = 'm-q2'");
    let queued_pinned = call(&shared, "master_state", json!({}));
    assert_eq!(queued["queued"], queued_pinned["queued"]);
    golden("master_state_queued.json", &queued_pinned);

    pin("UPDATE messages SET state='submitting', started=110.25 WHERE id='m-run'");
    let token =
        crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint("0123456789abcdef0123456789abcdef");
    shared.store.mark_running("m-run", &token).unwrap();
    pin("UPDATE messages SET started=110.25 WHERE id='m-run'");
    golden(
        "master_state_running.json",
        &call(&shared, "master_state", json!({})),
    );

    // The thread page: forward, with a cursor, and backward.
    let page = call(&shared, "thread_read", json!({"alias": "master"}));
    golden("thread_page_forward.json", &page);
    let tail = call(
        &shared,
        "thread_read",
        json!({"alias": "master", "tail": true, "limit": 2}),
    );
    golden("thread_page_tail.json", &tail);
}

fn scrub(v: &mut Value, thread_id: &str) {
    match v {
        Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if matches!(k.as_str(), "created" | "at" | "updated") && val.is_number() {
                    *val = json!(0);
                } else {
                    scrub(val, thread_id);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| scrub(i, thread_id)),
        Value::String(text) if text == thread_id => *text = "THREAD".into(),
        _ => {}
    }
}
