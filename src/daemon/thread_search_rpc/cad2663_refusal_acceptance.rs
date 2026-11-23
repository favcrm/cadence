//! CAD-2663 independent refusal acceptance. Authored from the ticket and the
//! `thread_search` RPC contract by a session other than the implementer; the
//! implementer's own tests were not read. The implementer must not edit or
//! weaken this file (AGENTS.md "Gates and security work").
//!
//! Every call goes through the real `Shared::dispatch("thread_search", ..)`
//! with a daemon-asserted caller. The fixture holds:
//! - `w1`'s home thread: one delivered turn quoting the shared word with
//!   `W1HOMEMARK`, a cancelled message and a still-queued message quoting the
//!   same word (`W1CANCELLEDMARK`, `W1QUEUEDMARK`), and a delivered secret;
//! - `w1`'s app conversation: one delivered turn quoting the word with
//!   `W1CONVMARK`;
//! - `w2`'s home thread: one delivered turn quoting the word with `W2MARK`.
//!
//! A refused call must refuse, and its error must carry none of the marks. A
//! permitted call must carry only the caller's own reached entries of the
//! thread its running turn belongs to, and never a `seq`.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::daemon::{ServeOptions, Shared};
use crate::store::{ConversationKind, NewAgent, NewEntry, Sender, Steer, Take};
use crate::test_seam::{scoped, Asserted};

/// The one word every planted entry quotes.
const WORD: &str = "quokkaharbor";
const W1_HOME: &str = "W1HOMEMARK";
const W1_CONV: &str = "W1CONVMARK";
const W1_QUEUED: &str = "W1QUEUEDMARK";
const W1_CANCELLED: &str = "W1CANCELLEDMARK";
const W2: &str = "W2MARK";
const ALL_MARKS: [&str; 5] = [W1_HOME, W1_CONV, W1_QUEUED, W1_CANCELLED, W2];
/// The word the secret entry quotes.
const SECRET_WORD: &str = "walrussecret";

struct Turn {
    message: crate::store::Message,
    token: String,
}

struct Fx {
    _dir: tempfile::TempDir,
    _keys: tempfile::TempDir,
    shared: Arc<Shared>,
    /// `w1`'s running search turn in its home thread.
    w1_home: Turn,
    /// `w2`'s running search turn in its home thread.
    w2_home: Turn,
    /// `w1`'s finished fact turn (home).
    w1_fact: Turn,
    /// The id of `w1`'s still-queued message.
    w1_queued: String,
    /// `w1`'s app conversation.
    conversation: String,
    /// `w2`'s home thread id.
    w2_thread: String,
    /// The raw secret planted in `w1`'s home thread.
    secret: String,
}

fn private_dir(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn write_private(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    drop(file);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn files_under(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().is_some_and(|name| name == ".git") {
            continue;
        }
        if path.is_dir() {
            files_under(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// A GitHub-PAT-shaped secret built at runtime from seeded noise.
fn secret() -> String {
    use sha2::{Digest, Sha256};
    const ALPHANUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut noise = String::new();
    let mut counter = 0u32;
    while noise.len() < 36 {
        for b in Sha256::digest(format!("cad2663-refusal:{counter}").as_bytes()) {
            if noise.len() == 36 {
                break;
            }
            noise.push(ALPHANUM[b as usize % ALPHANUM.len()] as char);
        }
        counter += 1;
    }
    [&["gh", "p_"].concat(), noise.as_str()].concat()
}

fn register(shared: &Shared, cwd: &str, alias: &str, generation: &str) {
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
                generation: Some(generation.into()),
                attach: None,
            },
        )
        .unwrap();
    shared.store.ensure_thread(alias).unwrap();
}

/// Enqueue a turn. A conversation turn arrives as an app chat message with
/// the daemon-verified binding, so its enqueue note (and with it the turn)
/// lands in that conversation, exactly as the App path routes it.
fn enqueue(shared: &Shared, alias: &str, id: &str, body: &str, conversation: Option<&str>) {
    let app = conversation.map(|conversation| {
        json!({"verified": true, "install_id": "install-1", "conversation": conversation})
    });
    let sender = if conversation.is_some() {
        Sender::OperatorChat
    } else {
        Sender::Unattributed
    };
    shared
        .store
        .enqueue_steered_for_with_conversation(
            alias,
            None,
            body,
            None,
            id,
            "test",
            None,
            None,
            None,
            &sender,
            &Steer::NONE,
            None,
            app.as_ref(),
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

/// Claim `alias`'s next queued message and mark it running.
fn start(shared: &Shared, alias: &str, generation: &str, expect: &str) -> Turn {
    let Take::Message(taken) = shared.store.take_queued(alias).unwrap() else {
        panic!("{alias} must claim its queued turn {expect}");
    };
    assert_eq!(taken.id, expect, "{alias} claimed the wrong turn");
    let token = crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint(generation);
    shared.store.mark_running(&taken.id, &token).unwrap();
    Turn {
        message: *taken,
        token,
    }
}

fn finish(shared: &Shared, turn: &Turn) {
    shared
        .store
        .finish(&turn.message, "completed", &json!({}), None)
        .unwrap();
}

fn agent_entry(shared: &Shared, alias: &str, message: &str, text: &str) {
    shared
        .store
        .thread_append(
            alias,
            NewEntry {
                role: "agent",
                kind: "turn_result",
                text,
                payload: None,
                message_id: Some(message),
            },
        )
        .unwrap()
        .expect("the entry must land in a thread");
}

fn fx() -> Fx {
    let dir = tempfile::Builder::new()
        .prefix("c2663ra")
        .tempdir()
        .unwrap();
    private_dir(dir.path());
    let pm_dir = dir.path().join("pm");
    let pm = crate::issue::Pm::init(&pm_dir).unwrap();
    private_dir(&pm_dir);
    let project = pm_dir.join("alpha");
    let issue_dir = project.join("A-1");
    std::fs::create_dir_all(&issue_dir).unwrap();
    std::fs::write(project.join("project.yaml"), "key: alpha\nprefix: A\n").unwrap();
    let front = crate::issue::model::Front::new("A-1", "t", "2026-11-22T00:00:00Z");
    std::fs::write(
        issue_dir.join("issue.md"),
        format!(
            "---\n{}---\n\nbody\n",
            serde_yaml::to_string(&front).unwrap()
        ),
    )
    .unwrap();
    let mut files = Vec::new();
    files_under(&pm_dir, &mut files);
    pm.commit(&files, "CAD-2663 refusal fixture\n\nActor: test\n")
        .unwrap();

    // Scoped-cursor keyring: owner-only key files outside the state and
    // tracker, and the record in the state dir.
    let keys_dir = tempfile::Builder::new()
        .prefix("c2663rk")
        .tempdir()
        .unwrap();
    let keys = keys_dir.path().to_path_buf();
    private_dir(&keys);
    write_private(&keys.join("identity.key"), &[0x41; 32]);
    write_private(&keys.join("cursor.key"), &[0x52; 32]);
    let key_id = "01".repeat(16);
    write_private(
        &dir.path().join("scoped-cursors.json"),
        &serde_json::to_vec(&json!({
            "version": 1,
            "identity_key_file": keys.join("identity.key").to_str().unwrap(),
            "active_key_id": key_id,
            "keys": [{"id": key_id, "key_file": keys.join("cursor.key").to_str().unwrap()}],
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
    let initialized = scoped(Asserted::Operator, || {
        shared.dispatch("scoped_cursor_initialize", &json!({}), std::process::id())
    })
    .expect("the operator initializes the keyring");
    assert_eq!(initialized["initialized"], json!(true), "{initialized}");

    let cwd = dir.path().to_str().unwrap().to_string();
    register(&shared, &cwd, "w1", "g1");
    register(&shared, &cwd, "w2", "g2");
    let secret = secret();

    // w1 home: a delivered fact turn with neighbours and a secret.
    enqueue(&shared, "w1", "m-w1-fact", "w1 kickoff", None);
    let w1_fact = start(&shared, "w1", "g1", "m-w1-fact");
    agent_entry(&shared, "w1", "m-w1-fact", "neighbour before the fact");
    agent_entry(
        &shared,
        "w1",
        "m-w1-fact",
        &format!("{WORD} {W1_HOME} the deploy port is 4471"),
    );
    agent_entry(
        &shared,
        "w1",
        "m-w1-fact",
        &format!("{SECRET_WORD} the key is {secret} keep it"),
    );
    agent_entry(&shared, "w1", "m-w1-fact", "neighbour after the fact");
    finish(&shared, &w1_fact);

    // w1 home: a message the operator cancelled before it ran.
    let cancelled = format!("{WORD} {W1_CANCELLED} cancelled instruction");
    enqueue(&shared, "w1", "m-w1-cancelled", &cancelled, None);
    agent_entry(&shared, "w1", "m-w1-cancelled", &cancelled);
    shared
        .store
        .cancel("m-w1-cancelled", "operator", Some("test"))
        .unwrap();

    // w1 app conversation: a delivered turn.
    let (conversation, _) = shared
        .store
        .conversation_create("w1", "install-1", None, ConversationKind::Fresh)
        .unwrap();
    enqueue(
        &shared,
        "w1",
        "m-w1-conv",
        "conversation kickoff",
        Some(&conversation.id),
    );
    let conv_turn = start(&shared, "w1", "g1", "m-w1-conv");
    assert_eq!(
        shared
            .store
            .message_conversation("w1", "m-w1-conv")
            .unwrap()
            .map(|thread| thread.id),
        Some(conversation.id.clone()),
        "fixture: the conversation turn belongs to the conversation"
    );
    shared
        .store
        .thread_append_to(
            &conversation.id,
            NewEntry {
                role: "agent",
                kind: "turn_result",
                text: &format!("{WORD} {W1_CONV} conversation-only fact"),
                payload: None,
                message_id: Some("m-w1-conv"),
            },
        )
        .unwrap()
        .expect("the conversation entry lands");
    finish(&shared, &conv_turn);

    // w1's running search turn in its home thread, then a still-queued
    // message behind it.
    enqueue(&shared, "w1", "m-w1-search", "w1 search turn", None);
    let w1_home = start(&shared, "w1", "g1", "m-w1-search");
    let queued = format!("{WORD} {W1_QUEUED} queued instruction");
    enqueue(&shared, "w1", "m-w1-queued", &queued, None);
    agent_entry(&shared, "w1", "m-w1-queued", &queued);

    // w2 home: a delivered turn quoting the same word, then its own
    // running search turn.
    enqueue(&shared, "w2", "m-w2-fact", "w2 kickoff", None);
    let w2_fact = start(&shared, "w2", "g2", "m-w2-fact");
    agent_entry(
        &shared,
        "w2",
        "m-w2-fact",
        &format!("{WORD} {W2} another agent's fact"),
    );
    finish(&shared, &w2_fact);
    enqueue(&shared, "w2", "m-w2-search", "w2 search turn", None);
    let w2_home = start(&shared, "w2", "g2", "m-w2-search");
    let w2_thread = shared.store.thread("w2").unwrap().unwrap().id;

    Fx {
        _dir: dir,
        _keys: keys_dir,
        shared,
        w1_home,
        w2_home,
        w1_fact,
        w1_queued: "m-w1-queued".into(),
        conversation: conversation.id,
        w2_thread,
        secret,
    }
}

fn call(fx: &Fx, who: Asserted, params: &Value) -> crate::error::Result<Value> {
    scoped(who, || {
        fx.shared
            .dispatch("thread_search", params, std::process::id())
    })
}

fn ok(fx: &Fx, alias: &str, params: Value) -> Value {
    let answer = call(fx, Asserted::Agent(alias.into()), &params)
        .unwrap_or_else(|error| panic!("{alias} {params} must be answered: {error:?}"));
    assert_no_seq(&answer, &answer);
    answer
}

/// A refused call's error text; the refusal carries no planted entry.
fn refused(fx: &Fx, who: Asserted, params: Value) -> String {
    match call(fx, who.clone(), &params) {
        Err(error) => {
            let text = format!("{error} {error:?}");
            for mark in ALL_MARKS {
                assert!(
                    !text.contains(mark),
                    "the refusal of {params} as {} leaked {mark}: {text}",
                    who.as_str()
                );
            }
            text
        }
        Ok(answer) => panic!("{params} as {} must be refused, got {answer}", who.as_str()),
    }
}

/// [`refused`], pinned to the guard under test.
fn refused_by(fx: &Fx, who: Asserted, params: Value, guard: &str) {
    let error = refused(fx, who, params.clone());
    assert!(
        error.contains(guard),
        "the refusal of {params} must come from `{guard}`: {error}"
    );
}

/// The refusal a handle that does not open answers.
const SEALED: &str = "no longer readable";

fn assert_no_seq(value: &Value, whole: &Value) {
    match value {
        Value::Object(map) => {
            assert!(
                !map.contains_key("seq"),
                "an answer must never carry `seq`: {whole}"
            );
            for child in map.values() {
                assert_no_seq(child, whole);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| assert_no_seq(item, whole)),
        _ => {}
    }
}

fn hits(answer: &Value) -> &Vec<Value> {
    answer["hits"].as_array().expect("a search answers hits")
}

/// The handle of the single hit whose snippet carries `mark`.
fn handle_of(answer: &Value, mark: &str) -> String {
    let found: Vec<&Value> = hits(answer)
        .iter()
        .filter(|hit| hit["snippet"].as_str().unwrap_or("").contains(mark))
        .collect();
    assert_eq!(found.len(), 1, "one hit carries {mark}: {answer}");
    let handle = found[0]["handle"].as_str().expect("a hit has a handle");
    assert!(handle.starts_with("csc1_"), "an opaque handle: {handle}");
    handle.to_string()
}

fn carries(answer: &Value, mark: &str) -> bool {
    answer.to_string().contains(mark)
}

/// Case 1: Another agent's entries are never returned, in either direction,
/// although both threads quote the same word.
#[test]
fn cad2663_refusal_other_agents_entries_never_returned() {
    let fx = fx();
    let w1 = ok(&fx, "w1", json!({"query": WORD, "limit": 10}));
    assert!(carries(&w1, W1_HOME), "w1 finds its own fact: {w1}");
    assert!(!carries(&w1, W2), "w1 must not see w2's entry: {w1}");

    let w2 = ok(&fx, "w2", json!({"query": WORD, "limit": 10}));
    assert!(carries(&w2, W2), "w2 finds its own fact: {w2}");
    for mark in [W1_HOME, W1_CONV, W1_QUEUED, W1_CANCELLED] {
        assert!(!carries(&w2, mark), "w2 must not see w1's {mark}: {w2}");
    }
    // Searching for the other agent's mark directly finds nothing.
    let probe = ok(&fx, "w1", json!({"query": W2}));
    assert!(hits(&probe).is_empty(), "w1 searching w2's mark: {probe}");
    assert_eq!(probe["matches"], json!(0), "{probe}");
}

/// Case 2: Entries of a queued and of a cancelled message are never returned,
/// while a delivered entry quoting the same word is.
#[test]
fn cad2663_refusal_queued_and_cancelled_never_returned() {
    let fx = fx();
    let all = ok(&fx, "w1", json!({"query": WORD, "limit": 10}));
    assert!(carries(&all, W1_HOME), "the delivered fact is found: {all}");
    assert!(!carries(&all, W1_QUEUED), "queued leaked: {all}");
    assert!(!carries(&all, W1_CANCELLED), "cancelled leaked: {all}");
    assert_eq!(
        all["matches"],
        json!(1),
        "only the delivered home entry matches the shared word: {all}"
    );
    for mark in [W1_QUEUED, W1_CANCELLED] {
        let direct = ok(&fx, "w1", json!({"query": mark}));
        assert!(
            hits(&direct).is_empty(),
            "{mark} must not be found: {direct}"
        );
        assert_eq!(direct["matches"], json!(0), "{direct}");
    }
    // The queued message is still queued: the exclusion is the state rule.
    let still = fx.shared.store.message(&fx.w1_queued).unwrap().unwrap();
    assert_eq!(still.state, "queued", "fixture: the message stays queued");
}

/// Case 3: The same agent's other conversation is never returned, both from the
/// home thread and from inside the conversation.
#[test]
fn cad2663_refusal_other_conversation_never_returned() {
    let fx = fx();
    // From home: the conversation's fact is invisible.
    let home = ok(&fx, "w1", json!({"query": WORD, "limit": 10}));
    assert!(carries(&home, W1_HOME), "{home}");
    assert!(
        !carries(&home, W1_CONV),
        "home turn saw the conversation: {home}"
    );
    let direct = ok(&fx, "w1", json!({"query": W1_CONV}));
    assert!(hits(&direct).is_empty(), "{direct}");
    let home_handle = handle_of(&home, W1_HOME);

    // From inside the conversation: the home fact is invisible.
    finish(&fx.shared, &fx.w1_home);
    fx.shared
        .store
        .cancel(&fx.w1_queued, "operator", Some("test"))
        .unwrap();
    enqueue(
        &fx.shared,
        "w1",
        "m-w1-conv-search",
        "conversation search turn",
        Some(&fx.conversation),
    );
    let _conv = start(&fx.shared, "w1", "g1", "m-w1-conv-search");
    assert_eq!(
        fx.shared
            .store
            .message_conversation("w1", "m-w1-conv-search")
            .unwrap()
            .map(|thread| thread.id),
        Some(fx.conversation.clone()),
        "fixture: the search turn belongs to the conversation"
    );
    let conv = ok(&fx, "w1", json!({"query": WORD, "limit": 10}));
    assert!(carries(&conv, W1_CONV), "the conversation's fact: {conv}");
    for mark in [W1_HOME, W1_QUEUED, W1_CANCELLED, W2] {
        assert!(
            !carries(&conv, mark),
            "conversation turn saw {mark}: {conv}"
        );
    }
    let direct = ok(&fx, "w1", json!({"query": W1_HOME}));
    assert!(hits(&direct).is_empty(), "{direct}");
    // Positive control: the conversation turn opens its own hit.
    let conv_handle = handle_of(&conv, W1_CONV);
    ok(&fx, "w1", json!({"around": conv_handle}));
    // A home hit's handle does not open in the conversation's turn.
    refused_by(
        &fx,
        Asserted::Agent("w1".into()),
        json!({"around": home_handle, "before": 6, "after": 6}),
        SEALED,
    );
}

/// Case 4: A field naming an agent, thread, conversation or project is refused
/// by name and answers no entry.
#[test]
fn cad2663_refusal_naming_fields_refused() {
    let fx = fx();
    let w1 = Asserted::Agent("w1".into());
    for (field, value) in [
        ("alias", json!("w2")),
        ("agent", json!("w2")),
        ("thread", json!(fx.w2_thread)),
        ("thread_id", json!(fx.w2_thread)),
        ("conversation", json!(fx.conversation)),
        ("project", json!("alpha")),
        ("issue", json!("A-1")),
    ] {
        let mut params = json!({"query": WORD});
        params[field] = value;
        let error = refused(&fx, w1.clone(), params);
        assert!(
            error.contains(&format!("`{field}`")),
            "the refusal names `{field}`: {error}"
        );
    }
    // The same request without the extra field is answered: the refusal is
    // the field, not the query.
    let control = ok(&fx, "w1", json!({"query": WORD}));
    assert!(carries(&control, W1_HOME), "{control}");
}

/// Case 5: A handle is sealed to its agent, turn and bytes.
#[test]
fn cad2663_refusal_handles_are_sealed() {
    let fx = fx();
    let w1 = Asserted::Agent("w1".into());
    let w2 = Asserted::Agent("w2".into());
    let found = ok(&fx, "w1", json!({"query": WORD}));
    let handle = handle_of(&found, W1_HOME);

    // Positive control: w1 reads around its own hit.
    let window = ok(
        &fx,
        "w1",
        json!({"around": handle, "before": 1, "after": 1}),
    );
    let entries = window["entries"].as_array().expect("a window");
    let hit: Vec<&Value> = entries.iter().filter(|e| e["hit"] == json!(true)).collect();
    assert_eq!(hit.len(), 1, "{window}");
    assert!(
        hit[0]["text"].as_str().unwrap().contains(W1_HOME),
        "{window}"
    );
    assert!(carries(&window, "neighbour before"), "{window}");

    // Another agent cannot open it, although it opens its own.
    let own = ok(&fx, "w2", json!({"query": WORD}));
    ok(&fx, "w2", json!({"around": handle_of(&own, W2)}));
    refused_by(&fx, w2.clone(), json!({"around": handle}), SEALED);
    refused_by(
        &fx,
        w2,
        json!({"around": handle, "before": 6, "after": 6}),
        SEALED,
    );

    // Tampered handles do not open.
    let mut chars: Vec<char> = handle.chars().collect();
    let middle = chars.len() / 2;
    chars[middle] = if chars[middle] == 'A' { 'B' } else { 'A' };
    let flipped: String = chars.into_iter().collect();
    let last = handle.len() - 1;
    let tail = if handle.ends_with('A') { 'B' } else { 'A' };
    for tampered in [
        flipped,
        format!("{}{tail}", &handle[..last]),
        handle[..handle.len() - 4].to_string(),
        format!("{handle}A"),
        "csc1_".to_string(),
    ] {
        refused_by(&fx, w1.clone(), json!({"around": tampered}), SEALED);
    }

    // A handle from an earlier, finished turn does not open in a later one.
    finish(&fx.shared, &fx.w1_home);
    fx.shared
        .store
        .cancel(&fx.w1_queued, "operator", Some("test"))
        .unwrap();
    enqueue(&fx.shared, "w1", "m-w1-later", "a later home turn", None);
    let _later = start(&fx.shared, "w1", "g1", "m-w1-later");
    refused_by(&fx, w1.clone(), json!({"around": handle}), SEALED);
    // Positive control in the later turn: a fresh search opens.
    let again = ok(&fx, "w1", json!({"query": WORD}));
    let fresh = handle_of(&again, W1_HOME);
    assert_ne!(fresh, handle, "a later turn mints its own handle");
    ok(&fx, "w1", json!({"around": fresh}));
}

/// Case 6: An explicit message/token pair must be the caller's own running turn
/// with its live token.
#[test]
fn cad2663_refusal_explicit_turn_pair_must_be_own_live_turn() {
    let fx = fx();
    let w1 = Asserted::Agent("w1".into());
    let w2_turn = (&fx.w2_home.message.id, &fx.w2_home.token);
    let w1_turn = (&fx.w1_home.message.id, &fx.w1_home.token);
    let wrong = crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("g1");
    for (message, token) in [
        // Another agent's running turn with its live token.
        (w2_turn.0.as_str(), w2_turn.1.as_str()),
        // Own running turn, another agent's token.
        (w1_turn.0.as_str(), w2_turn.1.as_str()),
        // Own running turn, a wrong token.
        (w1_turn.0.as_str(), wrong.as_str()),
        // Own finished turn with the token it ran under.
        (fx.w1_fact.message.id.as_str(), fx.w1_fact.token.as_str()),
        // Own queued message.
        (fx.w1_queued.as_str(), w1_turn.1.as_str()),
    ] {
        refused_by(
            &fx,
            w1.clone(),
            json!({"message": message, "token": token, "query": WORD}),
            "current owned active turn",
        );
    }
    // Half a pair is refused.
    refused_by(
        &fx,
        w1.clone(),
        json!({"message": w1_turn.0, "query": WORD}),
        "message and token together",
    );
    refused_by(
        &fx,
        w1,
        json!({"token": w1_turn.1, "query": WORD}),
        "message and token together",
    );
    // Positive control: the own pair is answered.
    let own = ok(
        &fx,
        "w1",
        json!({"message": w1_turn.0, "token": w1_turn.1, "query": WORD}),
    );
    assert!(carries(&own, W1_HOME), "{own}");
    assert!(!carries(&own, W2), "{own}");
}

/// Case 7: Only a verified native agent may search; an operator or an unproven
/// caller is refused.
#[test]
fn cad2663_refusal_non_agent_caller_refused() {
    let fx = fx();
    refused_by(
        &fx,
        Asserted::Operator,
        json!({"query": WORD}),
        "requires the calling native agent",
    );
    refused_by(
        &fx,
        Asserted::Unproven,
        json!({"query": WORD}),
        "requires the calling native agent",
    );
    refused_by(
        &fx,
        Asserted::Operator,
        json!({"message": fx.w1_home.message.id, "token": fx.w1_home.token, "query": WORD}),
        "requires the calling native agent",
    );
}

/// A redacted secret stays redacted in hits and in the window around them.
#[test]
fn cad2663_refusal_secret_stays_redacted() {
    let fx = fx();
    let found = ok(&fx, "w1", json!({"query": SECRET_WORD}));
    assert_eq!(hits(&found).len(), 1, "{found}");
    assert!(
        !found.to_string().contains(&fx.secret),
        "a hit leaked the secret: {found}"
    );
    assert!(carries(&found, "[redacted"), "{found}");
    let handle = hits(&found)[0]["handle"].as_str().unwrap().to_string();
    let window = ok(
        &fx,
        "w1",
        json!({"around": handle, "before": 6, "after": 6}),
    );
    assert!(
        !window.to_string().contains(&fx.secret),
        "the window leaked the secret: {window}"
    );
}
