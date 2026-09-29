//! CAD-782 versioned email content at the store layer.
//!
//! Guard-first: every refusal below is asserted before the behavior
//! it protects — invalid HTML/URL/token/oversize input refuses
//! without mutation, stale and concurrent CAS writes refuse, renders
//! derive deterministically from one exact revision, proposals are
//! inert until Apply, Discard changes nothing, and test/final
//! preparation share the content hash with locked host-owned sender
//! material.
use super::super::app_content::{Block, Draft};
use super::super::app_records::RecordStore;
use super::*;

fn content_file(dir: &TempDir, install: &str) -> RecordStore {
    RecordStore::open(dir.path(), install).unwrap()
}

fn draft(subject: &str, blocks: Vec<Value>) -> Draft {
    Draft::parse(subject, "", &blocks).unwrap()
}

fn blocks_basic() -> Vec<Value> {
    vec![
        json!({"type": "heading", "text": "Hello {{first_name|friend}}"}),
        json!({"type": "paragraph", "text": "A calm first line.\nA calm second line."}),
        json!({"type": "button", "label": "Read more", "url": "https://example.com/posts/welcome"}),
    ]
}

fn save_basic(store: &RecordStore) -> Value {
    store
        .app_content_save(
            "ctx-1",
            "launch-1",
            None,
            &draft("Spring launch", blocks_basic()),
        )
        .unwrap()
}

#[test]
fn cad782_save_show_round_trip_with_cas() {
    let dir = TempDir::new().unwrap();
    let store = content_file(&dir, "install-a");
    let created = save_basic(&store);
    assert_eq!(created["content"]["revision"], 1);
    assert_eq!(created["content"]["approval"]["valid"], false);
    let shown = store.app_content_show("ctx-1", "launch-1").unwrap();
    assert_eq!(shown["content"], created["content"]);
    // Saving without the observed revision refuses on an existing doc.
    assert!(store
        .app_content_save("ctx-1", "launch-1", None, &draft("Other", blocks_basic()))
        .is_err());
    // Saving with a wrong expected revision refuses without mutation.
    assert!(store
        .app_content_save(
            "ctx-1",
            "launch-1",
            Some(7),
            &draft("Other", blocks_basic())
        )
        .is_err());
    let updated = store
        .app_content_save(
            "ctx-1",
            "launch-1",
            Some(1),
            &draft("Spring launch v2", blocks_basic()),
        )
        .unwrap();
    assert_eq!(updated["content"]["revision"], 2);
    assert_eq!(updated["content"]["subject"], "Spring launch v2");
}

#[test]
fn cad782_invalid_content_refuses_without_mutation() {
    let dir = TempDir::new().unwrap();
    let store = content_file(&dir, "install-a");
    save_basic(&store);
    let before = store.app_content_show("ctx-1", "launch-1").unwrap();
    // Each invalid draft is built to fail parsing: the refusal names
    // the grammar, never the content.
    let invalid: Vec<(String, Vec<Value>)> = vec![
        ("<b>Bold</b> launch".to_string(), blocks_basic()),
        (
            "Launch <script>alert(1)</script>".to_string(),
            blocks_basic(),
        ),
        (
            "Launch".to_string(),
            vec![json!({"type": "heading", "text": "<img src=x>"})],
        ),
        (
            "Launch".to_string(),
            vec![json!({"type": "button", "label": "Go", "url": "javascript:alert(1)"})],
        ),
        (
            "Launch".to_string(),
            vec![json!({"type": "button", "label": "Go", "url": "http://example.com/x"})],
        ),
        (
            "Launch".to_string(),
            vec![json!({"type": "button", "label": "Go", "url": "https://no-dot-host/x"})],
        ),
        (
            "Launch".to_string(),
            vec![json!({"type": "button", "label": "Go", "url": "https://example.com/<x>"})],
        ),
        (
            "Launch".to_string(),
            vec![json!({"type": "button", "label": "Go"})],
        ),
        (
            "Launch".to_string(),
            vec![json!({"type": "unknown", "text": "x"})],
        ),
        (
            "Launch".to_string(),
            vec![json!({"type": "heading", "text": "x", "extra": 1})],
        ),
        ("Launch".to_string(), vec![]),
        (
            "Launch".to_string(),
            vec![json!({"type": "paragraph", "text": "Hello {{last_name|x}}"})],
        ),
        (
            "Launch".to_string(),
            vec![json!({"type": "paragraph", "text": "Hello {{first_name|}}"})],
        ),
        (
            "Launch".to_string(),
            vec![json!({"type": "paragraph", "text": "Hello {{first_name|x"})],
        ),
        (
            "Launch".to_string(),
            vec![json!({"type": "paragraph", "text": "Hi {{first_name|<b>x</b>}}"})],
        ),
        (
            "Launch".to_string(),
            vec![json!({"type": "paragraph", "text": "onclick=evil"})],
        ),
        (
            "Launch".to_string(),
            vec![json!({"type": "heading", "text": "x".repeat(200).as_str()})],
        ),
        (
            "x".repeat(300),
            vec![json!({"type": "heading", "text": "ok"})],
        ),
        (
            "Launch".to_string(),
            vec![json!({"type": "paragraph", "text": "x".repeat(5000).as_str()})],
        ),
    ];
    for (subject, blocks) in &invalid {
        let raw: Vec<Value> = blocks.clone();
        assert!(
            Draft::parse(subject, "", &raw).is_err(),
            "invalid draft admitted: {subject} {raw:?}"
        );
    }
    // Oversized block list refuses.
    let many: Vec<Value> = (0..20)
        .map(|i| json!({"type": "paragraph", "text": format!("para {i}")}))
        .collect();
    assert!(Draft::parse("Launch", "", &many).is_err());
    // Too many tokens refuse.
    let tokeny: Vec<Value> = (0..12)
        .map(|_| json!({"type": "paragraph", "text": "Hi {{first_name|pal}}"}))
        .collect();
    assert!(Draft::parse("Launch", "", &tokeny).is_err());
    // Nothing mutated: same revision and digest as before.
    let after = store.app_content_show("ctx-1", "launch-1").unwrap();
    assert_eq!(after["content"], before["content"]);
}

#[test]
fn cad782_render_is_deterministic_and_personalized() {
    let dir = TempDir::new().unwrap();
    let store = content_file(&dir, "install-a");
    save_basic(&store);
    let first = store
        .app_content_render("ctx-1", "launch-1", None, Some("Amina"))
        .unwrap();
    let second = store
        .app_content_render("ctx-1", "launch-1", Some(1), Some("Amina"))
        .unwrap();
    assert_eq!(first["render"], second["render"]);
    let html = first["render"]["html"].as_str().unwrap();
    let text = first["render"]["text"].as_str().unwrap();
    // Sample personalization substitutes; the fallback never leaks in.
    assert!(html.contains("Hello Amina"));
    assert!(!html.contains("friend"));
    assert!(text.contains("Hello Amina"));
    // The button URL survives escaped; the locked footer is present.
    assert!(html.contains("https://example.com/posts/welcome"));
    assert!(html.contains("noreply@cadence.invalid"));
    assert!(html.contains("Unsubscribe"));
    assert!(text.contains("noreply@cadence.invalid"));
    assert!(text.contains("Unsubscribe:"));
    // No editable content can inject markup: angle brackets are escaped.
    assert!(!html.contains("<script"));
    // Fallback rendering uses the declared fallback.
    let fallback = store
        .app_content_render("ctx-1", "launch-1", None, None)
        .unwrap();
    assert!(fallback["render"]["html"]
        .as_str()
        .unwrap()
        .contains("Hello friend"));
    assert_ne!(
        fallback["render"]["render_digest"],
        first["render"]["render_digest"]
    );
    // Unknown revisions and bad sample names refuse.
    assert!(store
        .app_content_render("ctx-1", "launch-1", Some(9), None)
        .is_err());
    assert!(store
        .app_content_render("ctx-1", "launch-1", None, Some("<b>x</b>"))
        .is_err());
    assert!(store
        .app_content_render("ctx-1", "missing", None, None)
        .is_err());
}

#[test]
fn cad782_proposal_apply_discard_semantics() {
    let dir = TempDir::new().unwrap();
    let store = content_file(&dir, "install-a");
    save_basic(&store);
    let before = store.app_content_show("ctx-1", "launch-1").unwrap();
    // Proposing changes nothing.
    let proposed = store
        .app_content_propose(
            "ctx-1",
            "launch-1",
            "prop-1",
            &draft("Spring launch, proposed", blocks_basic()),
        )
        .unwrap();
    assert_eq!(proposed["proposal"]["state"], "pending");
    assert_eq!(proposed["proposal"]["actor"], "assistant");
    assert_eq!(proposed["proposal"]["source_revision"], 1);
    assert_eq!(
        store.app_content_show("ctx-1", "launch-1").unwrap()["content"],
        before["content"]
    );
    // Discard changes nothing: same digest before and after.
    let discarded = store
        .app_content_proposal_discard("ctx-1", "prop-1")
        .unwrap();
    assert_eq!(discarded["proposal"]["state"], "discarded");
    assert_eq!(
        store.app_content_show("ctx-1", "launch-1").unwrap()["content"],
        before["content"]
    );
    // A decided proposal cannot be applied or re-discarded.
    assert!(store
        .app_content_proposal_apply("ctx-1", "prop-1", None)
        .is_err());
    assert!(store
        .app_content_proposal_discard("ctx-1", "prop-1")
        .is_err());
    // Apply creates a new attributed revision and invalidates approval.
    store.app_content_approve("ctx-1", "launch-1", 1).unwrap();
    let approved = store.app_content_show("ctx-1", "launch-1").unwrap();
    assert_eq!(approved["content"]["approval"]["valid"], true);
    store
        .app_content_propose(
            "ctx-1",
            "launch-1",
            "prop-2",
            &draft("Spring launch, applied", blocks_basic()),
        )
        .unwrap();
    let applied = store
        .app_content_proposal_apply("ctx-1", "prop-2", Some(1))
        .unwrap();
    assert_eq!(applied["content"]["revision"], 2);
    assert_eq!(applied["content"]["subject"], "Spring launch, applied");
    assert_eq!(applied["content"]["approval"]["valid"], false);
    let decided = store.app_content_proposal_show("ctx-1", "prop-2").unwrap();
    assert_eq!(decided["proposal"]["state"], "applied");
    // A stale proposal (source drifted) refuses to apply.
    store
        .app_content_propose(
            "ctx-1",
            "launch-1",
            "prop-3",
            &draft("Stale idea", blocks_basic()),
        )
        .unwrap();
    store
        .app_content_save(
            "ctx-1",
            "launch-1",
            Some(2),
            &draft("Newer edit", blocks_basic()),
        )
        .unwrap();
    assert!(store
        .app_content_proposal_apply("ctx-1", "prop-3", None)
        .is_err());
    // Proposal IDs never replay behind different bytes.
    assert!(store
        .app_content_propose(
            "ctx-1",
            "launch-1",
            "prop-2",
            &draft("Different bytes", blocks_basic())
        )
        .is_err());
}

#[test]
fn cad782_approval_lifecycle_and_send_parity() {
    let dir = TempDir::new().unwrap();
    let store = content_file(&dir, "install-a");
    save_basic(&store);
    // Approving a stale revision refuses.
    assert!(store.app_content_approve("ctx-1", "launch-1", 7).is_err());
    store.app_content_approve("ctx-1", "launch-1", 1).unwrap();
    assert_eq!(
        store.app_content_show("ctx-1", "launch-1").unwrap()["content"]["approval"]["valid"],
        true
    );
    // Any operator edit invalidates the approval.
    store
        .app_content_save(
            "ctx-1",
            "launch-1",
            Some(1),
            &draft("Edited", blocks_basic()),
        )
        .unwrap();
    assert_eq!(
        store.app_content_show("ctx-1", "launch-1").unwrap()["content"]["approval"]["valid"],
        false
    );
    // Test-send and final-send preparation share the content hash,
    // renderer output and locked sender/unsubscribe material.
    let test = store
        .app_content_test_prepare("ctx-1", "launch-1", "op@example.com")
        .unwrap();
    let send = store
        .app_content_send_prepare("ctx-1", "launch-1", None)
        .unwrap();
    assert_eq!(
        test["test_send"]["content_digest"],
        send["send"]["content_digest"]
    );
    assert_eq!(test["test_send"]["html"], send["send"]["html"]);
    assert_eq!(test["test_send"]["text"], send["send"]["text"]);
    assert_eq!(
        test["test_send"]["sender"],
        json!({"name": "Cadence CRM", "address": "noreply@cadence.invalid"})
    );
    assert!(test["test_send"]["headers"]["List-Unsubscribe"]
        .as_str()
        .unwrap()
        .contains("unsubscribe"));
    // The test recipient shapes refuse; nothing sends here — the
    // payload is preparation only, with no SMTP credential or call.
    assert!(store
        .app_content_test_prepare("ctx-1", "launch-1", "not-an-email")
        .is_err());
    assert!(store
        .app_content_test_prepare("ctx-1", "launch-1", "op@example.com<script>")
        .is_err());
}

#[test]
fn cad782_concurrent_saves_one_wins() {
    let dir = TempDir::new().unwrap();
    let store = content_file(&dir, "install-a");
    save_basic(&store);
    let barrier = std::sync::Barrier::new(2);
    let (first, second) = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            store.app_content_save(
                "ctx-1",
                "launch-1",
                Some(1),
                &draft("Racer A", blocks_basic()),
            )
        });
        let b = scope.spawn(|| {
            barrier.wait();
            store.app_content_save(
                "ctx-1",
                "launch-1",
                Some(1),
                &draft("Racer B", blocks_basic()),
            )
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    let wins = [&first, &second].iter().filter(|done| done.is_ok()).count();
    assert_eq!(wins, 1, "content race planted two revisions");
    assert_eq!(
        store.app_content_show("ctx-1", "launch-1").unwrap()["content"]["revision"],
        2
    );
}

#[test]
fn cad782_contexts_and_installs_are_isolated() {
    let dir = TempDir::new().unwrap();
    let store = content_file(&dir, "install-a");
    save_basic(&store);
    // A sibling context sees nothing of ctx-1.
    assert!(store.app_content_show("ctx-2", "launch-1").is_err());
    assert!(store
        .app_content_render("ctx-2", "launch-1", None, None)
        .is_err());
    assert_eq!(
        store.app_content_list("ctx-2").unwrap()["contents"],
        Value::Array(vec![])
    );
    // Same campaign ID in another context is independent.
    store
        .app_content_save(
            "ctx-2",
            "launch-1",
            None,
            &draft("Other context", blocks_basic()),
        )
        .unwrap();
    assert_eq!(
        store.app_content_show("ctx-2", "launch-1").unwrap()["content"]["revision"],
        1
    );
    assert_eq!(
        store.app_content_show("ctx-1", "launch-1").unwrap()["content"]["revision"],
        1
    );
    // A sibling installation file is independent too.
    let other = content_file(&dir, "install-b");
    assert!(other.app_content_show("ctx-1", "launch-1").is_err());
    // Malformed identifiers refuse before any file read.
    for bad in ["", "UPPER", "has space", "a/b", "x".repeat(200).as_str()] {
        assert!(store.app_content_show("ctx-1", bad).is_err());
    }
}

#[test]
fn cad782_block_parse_is_exact() {
    // Unknown fields, unknown types and wrong shapes refuse.
    for body in [
        json!({"type": "heading"}),
        json!({"type": "heading", "text": 1}),
        json!({"type": "heading", "text": "x", "label": "y"}),
        json!({"type": "button", "label": "x", "url": "https://example.com/a", "text": "y"}),
        json!({"kind": "heading", "text": "x"}),
        json!({"text": "x"}),
        json!("heading"),
    ] {
        assert!(Block::parse(&body).is_err(), "block admitted {body}");
    }
    assert!(Block::parse(&json!({"type": "heading", "text": "Hi"})).is_ok());
    assert!(Block::parse(
        &json!({"type": "button", "label": "Go", "url": "https://example.com/a"})
    )
    .is_ok());
}
