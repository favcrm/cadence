//! CAD-782 versioned email content at the store layer.
//!
//! Guard-first: every refusal below is asserted before the behavior
//! it protects — invalid HTML/URL/token/oversize input refuses
//! without mutation, stale and concurrent CAS writes refuse, renders
//! derive deterministically from one exact revision, proposals are
//! inert until Apply, Discard changes nothing, test preparation
//! shares the content hash with labelled preview-only sender
//! material, and final-send preparation always refuses until
//! CAD-785/786 supply host-verified evidence.
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
        .app_content_render("ctx-1", "launch-1", None, Some("Amina"), None)
        .unwrap();
    let second = store
        .app_content_render("ctx-1", "launch-1", Some(1), Some("Amina"), None)
        .unwrap();
    assert_eq!(first["render"], second["render"]);
    let html = first["render"]["html"].as_str().unwrap();
    let text = first["render"]["text"].as_str().unwrap();
    // Sample personalization substitutes; the fallback never leaks in.
    assert!(html.contains("Hello Amina"));
    assert!(!html.contains("friend"));
    assert!(text.contains("Hello Amina"));
    // The button URL survives escaped; the preview sender material
    // is present and labelled preview-only, never send-ready.
    assert!(html.contains("https://example.com/posts/welcome"));
    assert!(html.contains("noreply@cadence.invalid"));
    assert!(html.contains("Unsubscribe"));
    assert!(text.contains("noreply@cadence.invalid"));
    assert!(text.contains("Unsubscribe:"));
    assert_eq!(first["render"]["preview_only"], true);
    assert_eq!(first["render"]["send_ready"], false);
    assert_eq!(first["render"]["binding"]["binding_id"], "preview");
    // A saved binding renders its own sender material with a
    // distinct render digest, but stays preview-only: operator text
    // is never verification, so readiness never follows the binding.
    // The content digest stays identical.
    save_binding(
        &store,
        "bind-1",
        "news@example.com",
        "https://example.com/unsub",
    );
    let verified = store
        .app_content_render("ctx-1", "launch-1", None, Some("Amina"), Some("bind-1"))
        .unwrap();
    assert_eq!(verified["render"]["preview_only"], true);
    assert_eq!(verified["render"]["send_ready"], false);
    assert!(verified["render"]["html"]
        .as_str()
        .unwrap()
        .contains("news@example.com"));
    assert_eq!(
        verified["render"]["content_digest"],
        first["render"]["content_digest"]
    );
    assert_ne!(
        verified["render"]["render_digest"],
        first["render"]["render_digest"]
    );
    assert!(store
        .app_content_render("ctx-1", "launch-1", None, None, Some("bind-missing"))
        .is_err());
    // No editable content can inject markup: angle brackets are escaped.
    assert!(!html.contains("<script"));
    // Fallback rendering uses the declared fallback.
    let fallback = store
        .app_content_render("ctx-1", "launch-1", None, None, None)
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
        .app_content_render("ctx-1", "launch-1", Some(9), None, None)
        .is_err());
    assert!(store
        .app_content_render("ctx-1", "launch-1", None, Some("<b>x</b>"), None)
        .is_err());
    assert!(store
        .app_content_render("ctx-1", "missing", None, None, None)
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
    // Honest attribution: the operator submitted this draft, so it
    // reads as operator work — never as assistant output. Assistant
    // attribution stays reserved for receipt-backed writes.
    assert_eq!(proposed["proposal"]["actor"], "operator");
    assert_eq!(proposed["proposal"]["origin"], "operator-direct");
    assert_eq!(proposed["proposal"]["assistant_receipt"], Value::Null);
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

fn binding_draft<'a>(
    name: &'a str,
    address: &'a str,
    base: &'a str,
    connection: Option<&'a str>,
) -> super::super::app_content::BindingDraft<'a> {
    super::super::app_content::BindingDraft {
        sender_name: name,
        sender_address: address,
        unsubscribe_base: base,
        connection_id: connection,
    }
}

fn save_binding(store: &RecordStore, binding: &str, address: &str, base: &str) -> Value {
    store
        .app_sender_binding_save(
            "ctx-1",
            binding,
            None,
            &binding_draft("News", address, base, Some("conn-smtp-1")),
        )
        .unwrap()
}

#[test]
fn cad782_sender_binding_round_trip_with_cas() {
    let dir = TempDir::new().unwrap();
    let store = content_file(&dir, "install-a");
    let created = save_binding(
        &store,
        "bind-1",
        "news@example.com",
        "https://example.com/unsub",
    );
    assert_eq!(created["binding"]["revision"], 1);
    // Every saved binding is preview-only: operator text and domain
    // syntax are never verification. CAD-785/786 own the verified
    // seam; until then readiness never follows a save.
    assert_eq!(created["binding"]["preview_only"], true);
    assert!(created["binding"]["binding_digest"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    let shown = store.app_sender_binding_show("ctx-1", "bind-1").unwrap();
    assert_eq!(shown["binding"], created["binding"]);
    let listed = store.app_sender_binding_list("ctx-1").unwrap();
    assert_eq!(listed["bindings"].as_array().unwrap().len(), 1);
    // `.invalid` material and real-looking domains resolve identically:
    // every saved binding is preview-only until host verification exists.
    let preview_sender = save_binding(
        &store,
        "bind-prev-a",
        "news@cadence.invalid",
        "https://example.com/unsub",
    );
    assert_eq!(preview_sender["binding"]["preview_only"], true);
    let preview_unsub = save_binding(
        &store,
        "bind-prev-b",
        "news@example.com",
        "https://cadence.invalid/unsub",
    );
    assert_eq!(preview_unsub["binding"]["preview_only"], true);
    // Blind, stale and reserved-ID saves refuse without mutation.
    assert!(store
        .app_sender_binding_save(
            "ctx-1",
            "bind-1",
            None,
            &binding_draft(
                "News",
                "news@example.com",
                "https://example.com/unsub",
                None
            )
        )
        .is_err());
    assert!(store
        .app_sender_binding_save(
            "ctx-1",
            "bind-1",
            Some(7),
            &binding_draft(
                "News",
                "news@example.com",
                "https://example.com/unsub",
                None
            )
        )
        .is_err());
    assert!(store
        .app_sender_binding_save(
            "ctx-1",
            "preview",
            None,
            &binding_draft(
                "News",
                "news@example.com",
                "https://example.com/unsub",
                None
            )
        )
        .is_err());
    let updated = store
        .app_sender_binding_save(
            "ctx-1",
            "bind-1",
            Some(1),
            &binding_draft(
                "News v2",
                "news@example.com",
                "https://example.com/unsub",
                None,
            ),
        )
        .unwrap();
    assert_eq!(updated["binding"]["revision"], 2);
    assert_eq!(updated["binding"]["sender"]["name"], "News v2");
    // Invalid sender material refuses: bad address, bad base,
    // bad connection, bad name, unknown binding reads.
    for (name, address, base, connection) in [
        ("News", "not-an-email", "https://example.com/unsub", None),
        (
            "News",
            "news@example.com<script>",
            "https://example.com/unsub",
            None,
        ),
        ("News", "news@example.com", "http://example.com/unsub", None),
        (
            "News",
            "news@example.com",
            "https://no-dot-host/unsub",
            None,
        ),
        ("News", "news@example.com", "javascript:alert(1)", None),
        (
            "News",
            "news@example.com",
            "https://example.com/unsub",
            Some("bad conn!"),
        ),
        ("", "news@example.com", "https://example.com/unsub", None),
        (
            "<b>News</b>",
            "news@example.com",
            "https://example.com/unsub",
            None,
        ),
    ] {
        assert!(
            store
                .app_sender_binding_save(
                    "ctx-1",
                    "bind-bad",
                    None,
                    &binding_draft(name, address, base, connection)
                )
                .is_err(),
            "invalid binding admitted: {address} {base}"
        );
    }
    assert!(store
        .app_sender_binding_show("ctx-1", "bind-missing")
        .is_err());
    // Nothing above mutated the revision-2 row.
    assert_eq!(
        store.app_sender_binding_show("ctx-1", "bind-1").unwrap()["binding"],
        updated["binding"]
    );
}

#[test]
fn cad782_send_prepare_refuses_until_verified_binding() {
    let dir = TempDir::new().unwrap();
    let store = content_file(&dir, "install-a");
    save_basic(&store);
    save_binding(
        &store,
        "bind-1",
        "news@example.com",
        "https://example.com/unsub",
    );
    save_binding(
        &store,
        "bind-prev",
        "news@cadence.invalid",
        "https://cadence.invalid/unsub",
    );
    // Final-send preparation always refuses in this ticket: unknown
    // bindings refuse as unavailable, and every saved binding —
    // real-looking or `.invalid` — refuses as unverified. No
    // operator-typed byte pattern is send-ready.
    for binding in ["bind-missing", "preview", "bind-prev", "bind-1"] {
        assert!(
            store
                .app_content_send_prepare("ctx-1", "launch-1", binding, None)
                .is_err(),
            "send prepared behind {binding}"
        );
    }
    // Test preparation still works and is always labelled preview-only,
    // never send-ready — behind the default placeholders and behind a
    // named binding. The named binding changes the bytes and the
    // digest while the content digest stands independent.
    let test = store
        .app_content_test_prepare("ctx-1", "launch-1", "op@example.com", None)
        .unwrap()["test_send"]
        .clone();
    assert_eq!(test["preview_only"], true);
    assert_eq!(test["send_ready"], false);
    assert!(test["html"].as_str().unwrap().contains(".invalid"));
    let test_named = store
        .app_content_test_prepare("ctx-1", "launch-1", "op@example.com", Some("bind-1"))
        .unwrap()["test_send"]
        .clone();
    assert_eq!(test_named["preview_only"], true);
    assert_eq!(test_named["send_ready"], false);
    assert!(test_named["html"]
        .as_str()
        .unwrap()
        .contains("news@example.com"));
    assert_eq!(test_named["content_digest"], test["content_digest"]);
    assert_ne!(test_named["payload_digest"], test["payload_digest"]);
    // Unknown bindings refuse test preparation too.
    assert!(store
        .app_content_test_prepare("ctx-1", "launch-1", "op@example.com", Some("bind-missing"))
        .is_err());
    // The test recipient shapes refuse; nothing sends here — the
    // payload is preparation only, with no SMTP credential or call.
    assert!(store
        .app_content_test_prepare("ctx-1", "launch-1", "not-an-email", None)
        .is_err());
    assert!(store
        .app_content_test_prepare("ctx-1", "launch-1", "op@example.com<script>", None)
        .is_err());
}

#[test]
fn cad782_operator_text_and_fictitious_connection_never_send_ready() {
    // The operator precheck counterexample: `app_sender_binding_save`
    // accepts arbitrary sender bytes plus an absent or fictitious
    // connection_id, and domain syntax must not promote either to
    // send-ready. Every variant below stays preview-only and every
    // final-send preparation refuses.
    let dir = TempDir::new().unwrap();
    let store = content_file(&dir, "install-a");
    save_basic(&store);
    let variants: Vec<(&str, Option<&str>)> = vec![
        ("bind-none", None),
        ("bind-fiction", Some("conn-fictitious-1")),
        ("bind-real-conn", Some("conn-smtp-1")),
    ];
    for (binding, connection) in &variants {
        let saved = store
            .app_sender_binding_save(
                "ctx-1",
                binding,
                None,
                &binding_draft(
                    "News",
                    "news@example.com",
                    "https://example.com/unsub",
                    *connection,
                ),
            )
            .unwrap();
        assert_eq!(
            saved["binding"]["preview_only"], true,
            "saved binding reads send-ready: {binding}"
        );
        let rendered = store
            .app_content_render("ctx-1", "launch-1", None, None, Some(binding))
            .unwrap();
        assert_eq!(rendered["render"]["preview_only"], true);
        assert_eq!(rendered["render"]["send_ready"], false);
        assert!(rendered["render"]["html"]
            .as_str()
            .unwrap()
            .contains("news@example.com"));
        // The per-recipient token is still the literal placeholder
        // marker: CAD-786 mints real tokens, never this string.
        assert!(rendered["render"]["unsubscribe_url"]
            .as_str()
            .unwrap()
            .contains("token=RECIPIENT"));
        let test = store
            .app_content_test_prepare("ctx-1", "launch-1", "op@example.com", Some(binding))
            .unwrap()["test_send"]
            .clone();
        assert_eq!(test["preview_only"], true);
        assert_eq!(test["send_ready"], false);
        assert!(
            store
                .app_content_send_prepare("ctx-1", "launch-1", binding, None)
                .is_err(),
            "final send prepared behind operator text: {binding}"
        );
    }
    // Positive controls: the default preview placeholders render
    // labelled preview-only, and saved bindings stay readable and
    // listable as preview-only rows.
    let preview = store
        .app_content_render("ctx-1", "launch-1", None, Some("Amina"), None)
        .unwrap();
    assert_eq!(preview["render"]["preview_only"], true);
    assert_eq!(preview["render"]["send_ready"], false);
    assert_eq!(preview["render"]["binding"]["binding_id"], "preview");
    let listed = store.app_sender_binding_list("ctx-1").unwrap();
    assert_eq!(listed["bindings"].as_array().unwrap().len(), 3);
    for binding in listed["bindings"].as_array().unwrap() {
        assert_eq!(binding["preview_only"], true);
    }
}

#[test]
fn cad782_binding_rotation_invalidates_send_not_content_approval() {
    let dir = TempDir::new().unwrap();
    let store = content_file(&dir, "install-a");
    save_basic(&store);
    save_binding(
        &store,
        "bind-1",
        "news@example.com",
        "https://example.com/unsub",
    );
    store.app_content_approve("ctx-1", "launch-1", 1).unwrap();
    // Test preparation pins the binding digest: rotating the binding
    // changes the prepared digest while the content digest — and the
    // content-only approval — stand. Final-send preparation refuses
    // throughout (no verified binding exists yet), so rotation is
    // proven through test preparation.
    let first = store
        .app_content_test_prepare("ctx-1", "launch-1", "op@example.com", Some("bind-1"))
        .unwrap()["test_send"]
        .clone();
    store
        .app_sender_binding_save(
            "ctx-1",
            "bind-1",
            Some(1),
            &binding_draft(
                "News v2",
                "news@example.com",
                "https://example.com/unsub",
                None,
            ),
        )
        .unwrap();
    let second = store
        .app_content_test_prepare("ctx-1", "launch-1", "op@example.com", Some("bind-1"))
        .unwrap()["test_send"]
        .clone();
    assert_ne!(first["payload_digest"], second["payload_digest"]);
    assert_eq!(first["content_digest"], second["content_digest"]);
    assert_eq!(
        second["sender_binding"]["revision"], 2,
        "test prepare did not pin the rotated binding"
    );
    let shown = store.app_content_show("ctx-1", "launch-1").unwrap();
    assert_eq!(shown["content"]["approval"]["valid"], true);
    assert_eq!(shown["content"]["approval"]["scope"], "content-only");
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
    // Test-send preparations behind the default and named bindings
    // share the content hash; sender material follows the binding and
    // every payload stays preview-only. Final-send preparation refuses
    // until CAD-785/786 supply verified evidence.
    save_binding(
        &store,
        "bind-1",
        "news@example.com",
        "https://example.com/unsub",
    );
    let test = store
        .app_content_test_prepare("ctx-1", "launch-1", "op@example.com", Some("bind-1"))
        .unwrap();
    let test_default = store
        .app_content_test_prepare("ctx-1", "launch-1", "op@example.com", None)
        .unwrap();
    assert_eq!(
        test["test_send"]["content_digest"],
        test_default["test_send"]["content_digest"]
    );
    assert_eq!(test["test_send"]["preview_only"], true);
    assert_eq!(test["test_send"]["send_ready"], false);
    assert!(store
        .app_content_send_prepare("ctx-1", "launch-1", "bind-1", None)
        .is_err());
    assert!(test["test_send"]["headers"]["List-Unsubscribe"]
        .as_str()
        .unwrap()
        .contains("https://example.com/unsub"));
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
        .app_content_render("ctx-2", "launch-1", None, None, None)
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
    // Sender bindings are scoped the same way: ctx-1's binding is
    // unusable from ctx-2 and vice versa. (Final-send preparation
    // refuses everywhere in this ticket; the cross-context refusal
    // below holds independently of that gate.)
    save_binding(
        &store,
        "bind-1",
        "news@example.com",
        "https://example.com/unsub",
    );
    assert!(store
        .app_content_send_prepare("ctx-2", "launch-1", "bind-1", None)
        .is_err());
    assert!(store.app_sender_binding_show("ctx-2", "bind-1").is_err());
    assert_eq!(
        store.app_sender_binding_list("ctx-2").unwrap()["bindings"],
        Value::Array(vec![])
    );
    assert!(other.app_sender_binding_show("ctx-1", "bind-1").is_err());
    // Malformed identifiers refuse before any file read.
    for bad in ["", "UPPER", "has space", "a/b", "x".repeat(200).as_str()] {
        assert!(store.app_content_show("ctx-1", bad).is_err());
    }
}

#[test]
fn cad782_origin_column_migrates_older_files() {
    use super::super::app_records::record_db_path;
    let dir = TempDir::new().unwrap();
    let store = content_file(&dir, "install-a");
    save_basic(&store);
    store
        .app_content_propose("ctx-1", "launch-1", "prop-1", &draft("Old", blocks_basic()))
        .unwrap();
    drop(store);
    // Simulate a file from between the two landings: proposals
    // without the origin column. Reopen must backfill exactly the
    // one value those rows can carry, without touching content.
    let path = record_db_path(dir.path(), "install-a").unwrap();
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch("ALTER TABLE app_content_proposals DROP COLUMN origin")
        .unwrap();
    drop(conn);
    let store = content_file(&dir, "install-a");
    let shown = store.app_content_proposal_show("ctx-1", "prop-1").unwrap();
    assert_eq!(shown["proposal"]["origin"], "operator-direct");
    assert_eq!(shown["proposal"]["actor"], "operator");
    assert_eq!(
        store.app_content_show("ctx-1", "launch-1").unwrap()["content"]["revision"],
        1
    );
    // New writes carry the column after migration.
    store
        .app_content_propose("ctx-1", "launch-1", "prop-2", &draft("New", blocks_basic()))
        .unwrap();
    assert_eq!(
        store.app_content_proposal_show("ctx-1", "prop-2").unwrap()["proposal"]["origin"],
        "operator-direct"
    );
}

#[test]
fn cad1058_name_column_migrates_older_files_idempotently() {
    use super::super::app_records::record_db_path;
    let dir = TempDir::new().unwrap();
    let store = content_file(&dir, "install-a");
    save_basic(&store);
    drop(store);
    // A file from before the name column: reopen adds it, keeps the
    // saved content, and reopening again is a no-op.
    let path = record_db_path(dir.path(), "install-a").unwrap();
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch("ALTER TABLE app_content_docs DROP COLUMN name")
        .unwrap();
    drop(conn);
    for _ in 0..2 {
        let store = content_file(&dir, "install-a");
        let shown = store.app_content_show("ctx-1", "launch-1").unwrap();
        assert_eq!(shown["content"]["revision"], 1);
        assert!(shown["content"]["name"].is_null());
        drop(store);
    }
    let store = content_file(&dir, "install-a");
    let named = draft("Named", blocks_basic())
        .with_name(Some("Spring launch"))
        .unwrap();
    store
        .app_content_save("ctx-1", "launch-1", Some(1), &named)
        .unwrap();
    assert_eq!(
        store.app_content_show("ctx-1", "launch-1").unwrap()["content"]["name"],
        "Spring launch"
    );
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

#[test]
fn cad782_nonempty_proposal_list_releases_the_record_lock() {
    // Regression: proposal_list held the record-file mutex while
    // re-entering proposal_show (which locks it again), so any
    // nonempty list deadlocked. Run the list on a worker thread with
    // a bounded wait: without the fix this times out instead of
    // hanging the suite forever.
    let dir = TempDir::new().unwrap();
    let store = content_file(&dir, "install-a");
    save_basic(&store);
    store
        .app_content_propose(
            "ctx-1",
            "launch-1",
            "prop-1",
            &draft("Listed", blocks_basic()),
        )
        .unwrap();
    let store = std::sync::Arc::new(store);
    let probe = std::sync::Arc::clone(&store);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = probe.app_content_proposal_list("ctx-1", None).map(|value| {
            value["proposals"]
                .as_array()
                .map(std::vec::Vec::len)
                .unwrap_or(usize::MAX)
        });
        let _ = done_tx.send(out);
    });
    let listed = done_rx
        .recv_timeout(std::time::Duration::from_secs(15))
        .expect("nonempty proposal list hung holding the record lock");
    let count = listed.expect("nonempty proposal list refused");
    assert_eq!(count, 1, "nonempty proposal list dropped its row");
}
