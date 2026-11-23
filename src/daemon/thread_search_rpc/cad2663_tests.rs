//! CAD-2663: `thread_search` through the daemon's own dispatch, as an
//! asserted native worker `w1` holding one running turn. Thread rows are
//! synthetic fixtures appended to the store.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::daemon::{ServeOptions, Shared};
use crate::store::{ConversationKind, Message, NewAgent, NewEntry, Sender, Steer, Take};
use crate::test_seam::{scoped, Asserted};

struct Fx {
    _dir: tempfile::TempDir,
    _keys: tempfile::TempDir,
    shared: Arc<Shared>,
    quoted: String,
}

fn private(path: &std::path::Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn write_private(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

fn register(shared: &Shared, alias: &str, cwd: &str) {
    shared
        .store
        .register_agent(&NewAgent {
            alias,
            provider: "claude",
            endpoint_kind: "managed",
            role: "worker",
            cwd,
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    shared
        .store
        .set_identity(
            alias,
            &crate::adapter::Identity {
                thread_id: format!("{alias}-thread"),
                session_id: format!("{alias}-session"),
                model: None,
                effort: None,
                pid: std::process::id(),
                endpoint: None,
                generation: Some("g1".into()),
                attach: None,
            },
        )
        .unwrap();
    shared.store.ensure_thread(alias).unwrap();
}

fn enqueue(shared: &Shared, alias: &str, id: &str, issue: Option<&str>) {
    enqueue_in(shared, alias, id, issue, None);
}

/// Enqueue into `conversation` (an app conversation's thread id), or the
/// home thread.
fn enqueue_in(
    shared: &Shared,
    alias: &str,
    id: &str,
    issue: Option<&str>,
    conversation: Option<&str>,
) {
    shared
        .store
        .enqueue_steered_for_with_conversation(
            alias,
            None,
            "kickoff",
            None,
            id,
            "test",
            None,
            issue,
            None,
            &Sender::Unattributed,
            &Steer::NONE,
            None,
            None,
            None,
            conversation,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
}

/// Claim the alias's next queued message as its running turn.
fn run(shared: &Shared, alias: &str, token: &str) -> Message {
    let Take::Message(taken) = shared.store.take_queued(alias).unwrap() else {
        panic!("{alias} must claim its queued turn");
    };
    shared.store.mark_running(&taken.id, token).unwrap();
    *taken
}

fn say(shared: &Shared, alias: &str, message: &str, role: &str, text: &str) {
    let (role, kind) = if role == "user" {
        ("operator", "message")
    } else {
        ("agent", "turn_result")
    };
    shared
        .store
        .thread_append(
            alias,
            NewEntry {
                role,
                kind,
                text,
                payload: None,
                message_id: Some(message),
            },
        )
        .unwrap();
}

/// `w1`, with in its home thread:
/// - a completed turn `m-old` whose answer holds the fact, buried under
///   many later completed turns so it is far out of any verbatim window;
/// - a cancelled message and a still-queued one, each quoting the phrase;
/// - the running turn `m-now`, also quoting it.
///
/// `w2` holds its own completed turn quoting the phrase. Projects `alpha`
/// holds issue A-1, which every turn of w1 except the plain ones is bound
/// to. An app conversation of w1 holds a completed turn quoting the phrase;
/// with `now_in_conversation` the running turn belongs to it instead of the
/// home thread.
fn fx() -> Fx {
    fx_in(false)
}

fn fx_in(now_in_conversation: bool) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let pm_dir = dir.path().join("pm");
    let pm = crate::issue::Pm::init(&pm_dir).unwrap();
    let project = pm_dir.join("alpha");
    std::fs::create_dir_all(project.join("A-1")).unwrap();
    std::fs::write(project.join("project.yaml"), "key: alpha\nprefix: A\n").unwrap();
    let front = crate::issue::model::Front::new("A-1", "t", "2026-11-23T00:00:00Z");
    std::fs::write(
        project.join("A-1").join("issue.md"),
        format!(
            "---\n{}---\n\nbody\n",
            serde_yaml::to_string(&front).unwrap()
        ),
    )
    .unwrap();
    pm.commit(
        &[
            project.join("project.yaml"),
            project.join("A-1").join("issue.md"),
        ],
        "CAD-2663 fixture\n\nActor: test\n",
    )
    .unwrap();
    private(dir.path());
    private(&pm_dir);
    let keys = tempfile::tempdir().unwrap();
    private(keys.path());
    for (name, byte) in [("identity.key", 0xa1u8), ("cursor-a.key", 0xb2u8)] {
        write_private(&keys.path().join(name), &[byte; 32]);
    }
    let id = "01".repeat(16);
    write_private(
        &dir.path().join("scoped-cursors.json"),
        &serde_json::to_vec(&json!({
            "version": 1,
            "identity_key_file": keys.path().join("identity.key").to_str().unwrap(),
            "active_key_id": &id,
            "keys": [{"id": &id, "key_file": keys.path().join("cursor-a.key").to_str().unwrap()}],
        }))
        .unwrap(),
    );
    let opts = ServeOptions {
        provider_env: crate::adapter::ProviderEnv::refusing_providers(),
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    opts.provider_env
        .set("CADENCE_HOME", dir.path().to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let answer = scoped(Asserted::Operator, || {
        shared.dispatch("scoped_cursor_initialize", &json!({}), std::process::id())
    })
    .expect("the operator initializes the keyring");
    assert_eq!(answer["initialized"], json!(true), "{answer}");

    let cwd = dir.path().to_str().unwrap().to_string();
    register(&shared, "w1", &cwd);
    register(&shared, "w2", &cwd);
    let token = crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("g1");
    // A live-shaped turn token quoted in prose (a real generation is hex).
    let quoted = crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("0123456789ab");

    // The old fact, then 40 later turns: far beyond the verbatim window.
    enqueue(&shared, "w1", "m-old", Some("A-1"));
    let old = run(&shared, "w1", &token);
    say(
        &shared,
        "w1",
        "m-old",
        "user",
        "What retry budget did we settle on?",
    );
    say(
        &shared,
        "w1",
        "m-old",
        "agent",
        &format!("Decided: the retry budget is 42 requests per minute. (turn {quoted})"),
    );
    shared
        .store
        .finish(&old, "completed", &json!({}), None)
        .unwrap();
    for i in 0..40 {
        let id = format!("m-later-{i}");
        enqueue(&shared, "w1", &id, Some("A-1"));
        let later = run(&shared, "w1", &token);
        say(&shared, "w1", &id, "user", &format!("later question {i}"));
        say(
            &shared,
            "w1",
            &id,
            "agent",
            &format!("later answer {i} about the budget"),
        );
        shared
            .store
            .finish(&later, "completed", &json!({}), None)
            .unwrap();
    }
    // A plain conversation turn with no issue: the pack carries it back.
    enqueue(&shared, "w1", "m-plain", None);
    let plain = run(&shared, "w1", &token);
    say(
        &shared,
        "w1",
        "m-plain",
        "agent",
        "PLAIN_TURN_FACT the retry budget is 42 there too",
    );
    shared
        .store
        .finish(&plain, "completed", &json!({}), None)
        .unwrap();

    let (conversation, _) = shared
        .store
        .conversation_create("w1", "install-1", None, ConversationKind::Fresh)
        .unwrap();
    let in_conversation = |id: &str, text: &str| {
        shared
            .store
            .thread_append_to(
                &conversation.id,
                NewEntry {
                    role: "agent",
                    kind: "turn_result",
                    text,
                    payload: None,
                    message_id: Some(id),
                },
            )
            .unwrap();
    };
    enqueue_in(&shared, "w1", "m-conv", None, Some(&conversation.id));
    let conv = run(&shared, "w1", &token);
    in_conversation("m-conv", "CONVERSATION_FACT the retry budget is 42");
    shared
        .store
        .finish(&conv, "completed", &json!({}), None)
        .unwrap();

    enqueue(&shared, "w1", "m-cancel", Some("A-1"));
    say(
        &shared,
        "w1",
        "m-cancel",
        "user",
        "CANCELLED_FACT the retry budget is 42",
    );
    shared.store.cancel("m-cancel", "operator", None).unwrap();

    // A turn belongs to the conversation its first thread entry is in (an
    // app send's enqueue note); place it there for the conversation case.
    if now_in_conversation {
        in_conversation("m-now", "CURRENT_FACT the retry budget is 42");
    }
    enqueue(&shared, "w1", "m-now", Some("A-1"));
    run(&shared, "w1", &token);
    say(
        &shared,
        "w1",
        "m-now",
        "user",
        "CURRENT_FACT the retry budget is 42",
    );
    enqueue(&shared, "w1", "m-queued", Some("A-1"));
    say(
        &shared,
        "w1",
        "m-queued",
        "user",
        "QUEUED_FACT the retry budget is 42",
    );

    enqueue(&shared, "w2", "m-w2", Some("A-1"));
    let other = run(&shared, "w2", &token);
    say(
        &shared,
        "w2",
        "m-w2",
        "agent",
        "OTHER_AGENT_FACT the retry budget is 42",
    );
    shared
        .store
        .finish(&other, "completed", &json!({}), None)
        .unwrap();

    Fx {
        _dir: dir,
        _keys: keys,
        shared,
        quoted,
    }
}

fn search(fx: &Fx, params: Value) -> crate::error::Result<Value> {
    scoped(Asserted::Agent("w1".into()), || {
        fx.shared
            .dispatch("thread_search", &params, std::process::id())
    })
}

fn snippets(answer: &Value) -> Vec<String> {
    answer["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["snippet"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn cad2663_finds_an_old_fact_and_reads_around_it() {
    let fx = fx();
    let answer = search(&fx, json!({"query": "retry budget is 42"})).unwrap();
    let found = snippets(&answer);
    // Phrase hits first (newest first among them), the old fact included.
    assert!(
        found[..2]
            .iter()
            .any(|s| s.contains("Decided: the retry budget is 42")),
        "{answer}"
    );
    assert!(answer["hits"][0]["phrase"].as_bool().unwrap());
    assert!(answer["hits"][1]["phrase"].as_bool().unwrap());
    // Scattered words rank after every phrase hit.
    let first_scattered = answer["hits"]
        .as_array()
        .unwrap()
        .iter()
        .position(|hit| !hit["phrase"].as_bool().unwrap());
    if let Some(at) = first_scattered {
        assert!(answer["hits"].as_array().unwrap()[at..]
            .iter()
            .all(|hit| !hit["phrase"].as_bool().unwrap()));
    }
    assert!(answer.to_string().find("\"seq\"").is_none(), "{answer}");

    let handle = answer["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["snippet"].as_str().unwrap().contains("Decided:"))
        .unwrap()["handle"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(handle.starts_with("csc1_"), "{handle}");
    let window = search(&fx, json!({"around": handle, "before": 1, "after": 3})).unwrap();
    let entries = window["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 5, "{window}");
    assert_eq!(entries[0]["text"], "What retry budget did we settle on?");
    assert!(entries[1]["hit"].as_bool().unwrap(), "{window}");
    assert!(
        entries[2..]
            .iter()
            .any(|e| e["text"].as_str().unwrap().contains("later question 0")),
        "{window}"
    );
    assert_eq!(window["more_after"], json!(true));
    assert!(window.to_string().find("\"seq\"").is_none(), "{window}");
}

#[test]
fn cad2663_searches_only_what_reached_this_agent() {
    let fx = fx();
    let answer = search(&fx, json!({"query": "retry budget is 42", "limit": 10})).unwrap();
    let text = answer.to_string();
    for absent in [
        "CANCELLED_FACT",
        "QUEUED_FACT",
        "CURRENT_FACT",
        "OTHER_AGENT_FACT",
    ] {
        assert!(!text.contains(absent), "{absent}: {text}");
    }
    assert!(text.contains("PLAIN_TURN_FACT"), "{text}");
    // A live turn token quoted in prose never leaves the daemon.
    assert!(!text.contains(&fx.quoted), "{text}");
    assert!(text.contains("[redacted]"), "{text}");
}

#[test]
fn cad2663_answers_are_bounded() {
    let fx = fx();
    let answer = search(&fx, json!({"query": "later answer budget"})).unwrap();
    assert_eq!(answer["hits"].as_array().unwrap().len(), 5, "{answer}");
    assert!(answer["matches"].as_u64().unwrap() > 5);
    assert_eq!(answer["truncated"], json!(true));
    assert_eq!(answer["complete"], json!(true));
    for bad in [
        json!({"query": "x", "limit": 0}),
        json!({"query": "x", "limit": 11}),
    ] {
        assert!(search(&fx, bad).is_err());
    }
    assert!(search(&fx, json!({"query": "a b c d e f g"})).is_err());

    // A huge entry is clipped in a window, and the window stays in budget.
    let big = "HUGE_ENTRY ".repeat(2_000);
    for i in 0..8 {
        fx.shared
            .store
            .thread_append(
                "w1",
                NewEntry {
                    role: "agent",
                    kind: "assistant_text",
                    text: &format!("{i} {big}"),
                    payload: None,
                    message_id: Some("m-old"),
                },
            )
            .unwrap();
    }
    let answer = search(&fx, json!({"query": "HUGE_ENTRY"})).unwrap();
    let handle = answer["hits"][0]["handle"].as_str().unwrap();
    assert!(answer["hits"][0]["snippet"].as_str().unwrap().len() <= 410);
    let window = search(&fx, json!({"around": handle, "before": 6, "after": 6})).unwrap();
    let bytes: usize = window["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["text"].as_str().unwrap().len())
        .sum();
    assert!(bytes <= crate::thread_search::WINDOW_BYTES_MAX, "{bytes}");
    assert_eq!(window["truncated"], json!(true), "{window}");
    // The budget never drops the hit itself, and what is kept is in order.
    let entries = window["entries"].as_array().unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|e| e["hit"].as_bool().unwrap())
            .count(),
        1,
        "{window}"
    );
    let created: Vec<f64> = entries
        .iter()
        .map(|e| e["created"].as_f64().unwrap())
        .collect();
    assert!(created.windows(2).all(|w| w[0] <= w[1]), "{created:?}");
}

#[test]
fn cad2663_a_turn_searches_only_its_own_conversation() {
    // A home turn never sees the app conversation.
    let home = search(&fx(), json!({"query": "retry budget is 42", "limit": 10})).unwrap();
    let text = home.to_string();
    assert!(!text.contains("CONVERSATION_FACT"), "{text}");
    assert!(text.contains("Decided:"), "{text}");

    // A turn of the app conversation sees that conversation only.
    let fx = fx_in(true);
    let conv = search(&fx, json!({"query": "retry budget is 42", "limit": 10})).unwrap();
    let text = conv.to_string();
    assert!(text.contains("CONVERSATION_FACT"), "{text}");
    for absent in ["Decided:", "PLAIN_TURN_FACT", "CURRENT_FACT"] {
        assert!(!text.contains(absent), "{absent}: {text}");
    }
}
