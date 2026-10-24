//! CAD-1792: the typed board-API bodies serialize to the same bytes the
//! `json!` builders did. Each golden is the pretty body captured from the
//! pre-typing code; `CADENCE_WRITE_GOLDEN=1` rewrites them.
use super::member_view::redact_agents_payload;
use super::serve::agents_payload_from;
use serde_json::Value;
use std::path::PathBuf;

pub(super) fn golden(name: &str, actual: &Value) {
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

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

/// One inbox row, one worker with a bound task and a running message, one
/// fenced worker missing `quota`/`usage_limit`, and an agent the roster
/// reports without its board slice.
fn roster() -> Value {
    parse(
        r#"{"agents": [
        {"alias": "obs", "provider": "inbox", "endpoint_kind": "inbox",
         "role": null, "team_role": null, "model_selection": null,
         "model_lookup_role": null, "model": null, "model_reported": null,
         "model_configured": null, "model_source": null, "effort": null,
         "effort_reported": null, "effort_source": null, "effort_applicable": null,
         "quota": null, "usage_limit": null, "thread_id": "", "session_id": "",
         "endpoint": "inbox://obs", "params": {"upstream": "lead"}, "dead": false,
         "inbox": {"queued": 3, "oldest_age_secs": 42.0}},
        {"alias": "lead", "provider": "claude", "endpoint_kind": "pty",
         "role": "worker", "team_role": "dev", "model_selection": {"source": "role"},
         "model_lookup_role": "dev", "model": "m-1", "model_reported": "m-1",
         "model_configured": "m-1", "model_source": "reported", "effort": "high",
         "effort_reported": "high", "effort_source": "reported", "effort_applicable": true,
         "quota": {"used": 1}, "usage_limit": null,
         "thread_id": "th-1", "session_id": "s-1", "endpoint": "e",
         "state": "running", "dead": false, "silent_secs": 4, "stalled": false,
         "state_label": null, "tasks": ["t1", "t-unbound"],
         "params": {"catalogue": "pm", "look": "bean", "kind": "hire"},
         "board": {"queued": 2, "unknown": 0, "parked": 1, "event_cursor": 9,
           "messages": [
             {"id": "m1", "state": "running", "task_id": "t1", "created": 1790562108.64,
              "started": "2026-09-29T03:00:00+02:00", "body": "do the thing"},
             {"id": "m2", "state": "done", "result": {"via": "pty_render_miss"},
              "completed": 1790000000.5}]}},
        {"alias": "stuck", "provider": "claude", "endpoint_kind": "pty",
         "state": "attention", "error": "run cadence agent unfence stuck",
         "thread_id": "", "session_id": "", "endpoint": "", "dead": true,
         "tasks": [], "params": null, "board": {"unknown": 1, "messages": []}},
        {"alias": "bare", "provider": "fake", "endpoint_kind": "fake",
         "state": "idle", "params": null}
    ]}"#,
    )
}

fn jobs() -> Value {
    parse(
        r#"{"jobs": [
        {"id": "j1", "issue": "CAD-7", "title": "Ship", "state": "running",
         "task_list": [{"id": "t1", "state": "running", "title": "Build"}]},
        {"id": "j2", "issue": "", "title": "No issue", "state": "idle", "task_list": []}
    ]}"#,
    )
}

#[test]
fn agents_payload_bytes_are_unchanged() {
    let dir = tempfile::TempDir::new().unwrap();
    let full = agents_payload_from(dir.path(), Some(roster()), Some(&jobs()));
    golden("agents_operator.json", &full);
    let mut member = full.clone();
    redact_agents_payload(&mut member);
    golden("agents_member.json", &member);
    golden(
        "agents_unreachable.json",
        &agents_payload_from(dir.path(), None, None),
    );
    golden(
        "agents_no_jobs.json",
        &agents_payload_from(dir.path(), Some(roster()), None),
    );
}

/// `/api/meta`: the typed body equals the `json!` literal it replaced
/// (kept here as the oracle), for a signed-in and an unknown session.
#[test]
fn meta_bytes_are_unchanged() {
    use cadence_core::board_api::{DeliverySyncMeta, MetaDaemon, MetaResponse};
    use serde_json::json;
    let session = json!({"id": "s1", "origin": "loopback", "user": null, "created": 1});
    let proof = json!({"proven": false, "check": "peer", "why": "no proxy"});
    let cases = [
        (Some(true), session.clone(), proof.clone(), Some(true), true),
        (None, Value::Null, Value::Null, None, false),
    ];
    for (i, (signed_in, session, tailnet_proof, operator, full)) in cases.into_iter().enumerate() {
        let daemon = full.then(|| json!({"build_commit": "abc", "build_time": "t", "started_at": 7}));
        let sync = full.then(|| json!({"gh": "/usr/bin/gh", "every_secs": 60}));
        let oracle = json!({
            "read_only": false,
            "anonymous_reads": true,
            "signed_in": signed_in,
            "hosted": false,
            "session": session,
            "login_hint": "cadence ui login",
            "device_login": full,
            "tab_signed_out": false,
            "actor": "ui",
            "tailnet_proof": tailnet_proof,
            "operator": operator,
            "platform_account_configured": false,
            "tailnet_url": full.then(|| "https://x.ts.net"),
            "version": "1.2.3",
            "build_commit": "deadbeef",
            "build_time": "now",
            "daemon": daemon,
            "delivery_sync": sync,
        });
        let typed = MetaResponse {
            read_only: false,
            anonymous_reads: true,
            signed_in,
            hosted: false,
            session,
            login_hint: "cadence ui login".into(),
            device_login: full,
            tab_signed_out: false,
            actor: "ui".into(),
            tailnet_proof,
            operator,
            platform_account_configured: false,
            tailnet_url: full.then(|| "https://x.ts.net".to_string()),
            version: "1.2.3".into(),
            build_commit: "deadbeef".into(),
            build_time: "now".into(),
            daemon: full.then(|| MetaDaemon {
                build_commit: json!("abc"),
                build_time: json!("t"),
                started_at: json!(7),
            }),
            delivery_sync: full.then(|| DeliverySyncMeta {
                gh: Some("/usr/bin/gh".into()),
                gh_error: None,
                every_secs: 60,
            }),
        };
        let typed = serde_json::to_value(typed).unwrap();
        assert_eq!(
            serde_json::to_string_pretty(&typed).unwrap(),
            serde_json::to_string_pretty(&oracle).unwrap(),
            "case {i}"
        );
    }
    // A failing `gh` reports its error and no path.
    let err = DeliverySyncMeta { gh: None, gh_error: Some("no gh".into()), every_secs: 5 };
    assert_eq!(
        serde_json::to_value(err).unwrap(),
        json!({"gh": null, "gh_error": "no gh", "every_secs": 5})
    );
}
