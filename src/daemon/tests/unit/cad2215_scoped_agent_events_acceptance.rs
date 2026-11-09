//! CAD-2215: independent scoped `agent_events` acceptance.
//!
//! A non-operator reader of an agent's event log gets an opaque page: every
//! visible row carries a stable keyed `id` and its own purpose-bound `cursor`,
//! the page cursor is the last visible cursor (or `null` / the exact supplied
//! token), and no structural `seq`, `event_seq`, keyring fingerprint or
//! watermark is disclosed. Visibility is decided by the reader's project scope
//! and private-job denials, so a hidden project's events neither appear nor
//! move page boundaries when they are inserted between two pages.
//!
//! Tokens are not authority: another registered agent, another alias, the
//! daemon stream, another workspace, a tampered token, a numeric position and
//! a token minted before the alias was re-registered are all refused. Project
//! access withdrawn between two pages refuses the continuation without
//! invalidating the token, so restoring access resumes the same feed — and a
//! proven operator keeps the numeric document and numeric tail.
//!
//! Key material and the keyring record are fixture-owned files outside the
//! tracker; the only setup operation is the real operator
//! `scoped_cursor_initialize` RPC. No production code, harness hook or existing
//! acceptance is modified or duplicated. This does NOT establish OS isolation
//! or private-project availability.

use super::*;
use crate::test_seam::{scoped, Asserted};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const IDENTITY_BYTE: u8 = 0x11;
const KEY_A_BYTE: u8 = 0x22;
const KEY_B_BYTE: u8 = 0x33;
const KEY_ID_A: u8 = 0x0a;
const KEY_ID_B: u8 = 0x0b;

const VISIBLE_MARKER: &str = "CAD2215_AE_VISIBLE_";
const HIDDEN_MARKER: &str = "CAD2215_AE_HIDDEN_";
const QUIET_HIDDEN_MARKER: &str = "CAD2215_AE_QUIET_HIDDEN_";
const VISIBLE_AFTER_MARKER: &str = "CAD2215_AE_VISIBLE_AFTER";
const OPERATOR_ONLY_MARKER: &str = "CAD2215_AE_OPERATOR_SURVIVOR";

/// More than one forward page (the handler reads 100 rows at a time).
const VISIBLE_COUNT: usize = 105;
const HIDDEN_COUNT: usize = 12;
const QUIET_HIDDEN_COUNT: usize = 60;

fn hex_id(byte: u8) -> String {
    format!("{byte:02x}").repeat(16)
}

fn private_dir(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn write_private(path: &Path, bytes: &[u8]) {
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

/// The one cursor format a scoped reader names. The authortitative value is
/// `opaque-v1`; a scoped reader that omits it or names another value is
/// refused.
const CURSOR_FORMAT: &str = "opaque-v1";

/// Owner-provisioned key material and the fixed record content. The directory
/// is shared by every world in a test so a token can only differ by the
/// codec's own workspace binding.
struct Keys {
    _dir: tempfile::TempDir,
    config: Value,
}

fn keys() -> Keys {
    let dir = tempfile::tempdir().unwrap();
    private_dir(dir.path());
    for (name, byte) in [
        ("identity.key", IDENTITY_BYTE),
        ("cursor-a.key", KEY_A_BYTE),
        ("cursor-b.key", KEY_B_BYTE),
    ] {
        write_private(&dir.path().join(name), &[byte; 32]);
    }
    let config = json!({
        "version": 1,
        "identity_key_file": dir.path().join("identity.key").to_str().unwrap(),
        "active_key_id": hex_id(KEY_ID_A),
        "keys": [
            { "id": hex_id(KEY_ID_A), "key_file": dir.path().join("cursor-a.key").to_str().unwrap() },
            { "id": hex_id(KEY_ID_B), "key_file": dir.path().join("cursor-b.key").to_str().unwrap() },
        ],
    });
    Keys { _dir: dir, config }
}

fn write_config(state: &Path, config: &Value) {
    write_private(
        &state.join("scoped-cursors.json"),
        &serde_json::to_vec(config).unwrap(),
    );
}

fn write_project(pm: &Path, key: &str, prefix: &str, issue: &str, policy: &str) {
    let project = pm.join(key);
    let issue_dir = project.join(issue);
    std::fs::create_dir_all(&issue_dir).unwrap();
    std::fs::write(
        project.join("project.yaml"),
        format!("key: {key}\nprefix: {prefix}\n{policy}"),
    )
    .unwrap();
    std::fs::write(
        issue_dir.join("issue.md"),
        format!(
            "---\nid: {issue}\ntitle: {issue} fixture\nstatus: backlog\n\
             priority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n"
        ),
    )
    .unwrap();
}

/// The policy block that makes a project private for a scoped reader.
const PRIVATE_POLICY: &str = "policy:\n  version: 1\n  visibility: private\n";

fn visible_project_policy() -> &'static str {
    "key: open\nprefix: VIS\n"
}

/// The readable project made private: a revocation of the same project, not a
/// replacement with another project's identity.
fn private_open_policy() -> &'static str {
    "key: open\nprefix: VIS\npolicy:\n  version: 1\n  visibility: private\n"
}

/// One real workspace: a tracker with a readable and a private project, a
/// registered agent set, jobs/tasks the scoped event query joins on, and the
/// fixture events. Reads are made by a freshly booted `Shared`, the same way a
/// daemon restart rebinds to the same workspace.
struct World {
    _state: tempfile::TempDir,
    pm: PathBuf,
}

impl World {
    fn state(&self) -> &Path {
        self._state.path()
    }

    fn boot(&self) -> Arc<Shared> {
        self.boot_with(|_| {})
    }

    /// Boot with a test-only read hook installed through `ServeOptions`.
    fn boot_with(&self, configure: impl FnOnce(&mut ServeOptions)) -> Arc<Shared> {
        let mut opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", self.pm.to_str().unwrap());
        configure(&mut opts);
        Shared::new(self.state(), &opts).unwrap()
    }

    fn visible_policy_path(&self) -> PathBuf {
        self.pm.join("open").join("project.yaml")
    }
}

/// Build a workspace with `visible` readable events, `hidden` private-project
/// events interleaved between them, and `quiet_hidden` private-project events
/// on the quiet agent's own stream. `Some(keys)` writes the keyring record and
/// binds it with the real operator initialization RPC.
fn build_world(keys: Option<&Keys>, visible: usize, hidden: usize, quiet_hidden: usize) -> World {
    let state = tempfile::tempdir().unwrap();
    private_dir(state.path());
    let pm = state.path().join("tracker");
    crate::issue::Pm::init(&pm).unwrap();
    std::fs::set_permissions(&pm, std::fs::Permissions::from_mode(0o700)).unwrap();
    write_project(&pm, "open", "VIS", "VIS-1", "");
    write_project(&pm, "hidden", "HID", "HID-1", PRIVATE_POLICY);
    if let Some(keys) = keys {
        write_config(state.path(), &keys.config);
    }
    let world = World { _state: state, pm };
    {
        let shared = world.boot();
        for alias in ["reader", "peer", "quiet", "manager"] {
            register(&shared, world.state(), alias);
        }
        let mut db = rusqlite::Connection::open(world.state().join("cadence.sqlite3")).unwrap();
        db.execute_batch(
            "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
             VALUES ('open-job','Open','spec.md','manager','VIS-1','open',1,1),
                    ('hidden-job','Hidden','spec.md','manager','HID-1','open',2,2);
             INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
             VALUES ('open-task','open-job','Open','reader','running',1,1),
                    ('hidden-task','hidden-job','Hidden','reader','running',2,2);",
        )
        .unwrap();
        {
            let tx = db.transaction().unwrap();
            let mut hidden_left = hidden;
            for index in 0..visible {
                if index % 9 == 0 && hidden_left > 0 {
                    insert_event(
                        &tx,
                        "reader",
                        "hidden_progress",
                        &json!({"marker": format!("{HIDDEN_MARKER}{}", hidden - hidden_left)}),
                        "hidden-job",
                        "hidden-task",
                        500.0 + index as f64,
                    );
                    hidden_left -= 1;
                }
                insert_event(
                    &tx,
                    "reader",
                    "fixture_progress",
                    &json!({"n": index, "marker": format!("{VISIBLE_MARKER}{index}")}),
                    "open-job",
                    "open-task",
                    1000.0 + index as f64,
                );
            }
            for index in 0..hidden_left {
                insert_event(
                    &tx,
                    "reader",
                    "hidden_progress",
                    &json!({"marker": format!("{HIDDEN_MARKER}{}", hidden - hidden_left + index)}),
                    "hidden-job",
                    "hidden-task",
                    4000.0 + index as f64,
                );
            }
            // The quiet agent's whole stream is private-project activity.
            for index in 0..quiet_hidden {
                insert_event(
                    &tx,
                    "quiet",
                    "hidden_progress",
                    &json!({"marker": format!("{QUIET_HIDDEN_MARKER}{index}")}),
                    "hidden-job",
                    "hidden-task",
                    2000.0 + index as f64,
                );
            }
            // A readable row on the daemon stream: a different resource from
            // any agent alias.
            insert_event(
                &tx,
                "daemon",
                "fixture_daemon",
                &json!({"marker": OPERATOR_ONLY_MARKER}),
                "open-job",
                "open-task",
                3000.0,
            );
            tx.commit().unwrap();
        }
        drop(db);
        if keys.is_some() {
            let response = scoped(Asserted::Operator, || {
                shared.dispatch("scoped_cursor_initialize", &json!({}), std::process::id())
            })
            .expect("the operator must initialize the keyring");
            assert_eq!(response["initialized"], json!(true), "{response}");
        }
    }
    world
}

fn insert_event(
    tx: &rusqlite::Transaction<'_>,
    alias: &str,
    kind: &str,
    payload: &Value,
    job: &str,
    task: &str,
    at: f64,
) {
    tx.execute(
        "INSERT INTO events(alias,kind,payload,job_id,task_id,at) VALUES(?,?,?,?,?,?)",
        rusqlite::params![alias, kind, payload.to_string(), job, task, at],
    )
    .unwrap();
}

/// Append one new event after a page was read, on an already-open connection.
fn append_event(
    state: &Path,
    alias: &str,
    kind: &str,
    payload: &Value,
    job: &str,
    task: &str,
    at: f64,
) {
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO events(alias,kind,payload,job_id,task_id,at) VALUES(?,?,?,?,?,?)",
        rusqlite::params![alias, kind, payload.to_string(), job, task, at],
    )
    .unwrap();
}

fn page(shared: &Arc<Shared>, who: Asserted, params: &Value) -> Result<Value> {
    let shared = Arc::clone(shared);
    scoped(who, || {
        shared.dispatch("agent_events", params, std::process::id())
    })
}

fn scoped_page(shared: &Arc<Shared>, who: &str, params: Value) -> Value {
    let page = page(shared, Asserted::Agent(who.to_string()), &params)
        .unwrap_or_else(|error| panic!("scoped agent-events page for {who}: {error:?}"));
    assert_scoped_page(&page, who);
    page
}

fn rows(page: &Value) -> &Vec<Value> {
    page["events"]
        .as_array()
        .unwrap_or_else(|| panic!("agent-events page has no events array: {page}"))
}

fn ids(page: &Value) -> Vec<String> {
    rows(page)
        .iter()
        .map(|row| {
            row["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| panic!("scoped event row has no stable id: {row}"))
                .to_string()
        })
        .collect()
}

fn markers(page: &Value) -> Vec<String> {
    rows(page)
        .iter()
        .map(|row| {
            row["payload"]["marker"]
                .as_str()
                .unwrap_or_else(|| panic!("fixture row has no marker: {row}"))
                .to_string()
        })
        .collect()
}

fn cursor(page: &Value) -> Option<String> {
    match &page["cursor"] {
        Value::Null => None,
        Value::String(token) if !token.is_empty() && token.parse::<i64>().is_err() => {
            Some(token.clone())
        }
        other => panic!("scoped page cursor is not an opaque token or null: {other:?}"),
    }
}

fn assert_scoped_page(page: &Value, what: &str) {
    assert_eq!(
        page[cadence_core::board_api::READER_SCOPE_FIELD],
        json!(cadence_core::board_api::AGENT_EVENTS_SCOPE_ACK),
        "{what}: page did not acknowledge the scoped agent-events protocol: {page}"
    );
    let body = page
        .as_object()
        .unwrap_or_else(|| panic!("{what}: page is not an object: {page}"));
    for field in [
        "seq",
        "event_seq",
        "fingerprint",
        "keyring_fingerprint",
        "watermark",
        "position",
    ] {
        assert!(
            !body.contains_key(field),
            "{what}: page exposed structural {field}: {page}"
        );
    }
    // A page may only carry a string cursor (a string token, or the exact
    // supplied token on an empty continuation) or null.
    match &page["cursor"] {
        Value::Null => {}
        Value::String(token) => {
            assert!(!token.is_empty(), "{what}: empty page cursor: {page}");
            assert!(
                token.parse::<i64>().is_err(),
                "{what}: page cursor is a numeric position: {token}"
            );
        }
        other => panic!("{what}: page cursor is not a string or null: {other:?}"),
    }
    let mut unique = Vec::new();
    for row in rows(page) {
        let row = row
            .as_object()
            .unwrap_or_else(|| panic!("{what}: row is not an object: {row}"));
        for field in ["seq", "event_seq", "fingerprint", "watermark", "position"] {
            assert!(
                !row.contains_key(field),
                "{what}: row exposed structural {field}: {row:?}"
            );
        }
        let id = row["id"].as_str().unwrap_or_default();
        assert!(!id.is_empty(), "{what}: row lost its stable id: {row:?}");
        let row_cursor = row["cursor"].as_str().unwrap_or_default();
        assert!(
            !row_cursor.is_empty() && row_cursor.parse::<i64>().is_err(),
            "{what}: row cursor is not opaque: {row:?}"
        );
        assert!(
            !unique.contains(&id.to_string()),
            "{what}: duplicate stable id {id} in one page"
        );
        unique.push(id.to_string());
    }
    let rendered = page.to_string();
    for needle in ["csfk_", "keyring_fingerprint"] {
        assert!(
            !rendered.contains(needle),
            "{what}: page leaked {needle}: {rendered}"
        );
    }
    // Payloads are caller content: fixture payloads carry no seq here, so any
    // occurrence of a bare numeric position would still be structural.
}

/// A refusal must name neither content nor key material, and must not be an
/// internal failure.
fn assert_refusal(error: Error, what: &str) {
    assert_ne!(error.kind(), "internal", "{what}: {error:?}");
    let rendered = format!("{error} {error:?}");
    for needle in [
        VISIBLE_MARKER,
        HIDDEN_MARKER,
        QUIET_HIDDEN_MARKER,
        "csc1_",
        "csrid_",
        "csfk_",
    ] {
        assert!(
            !rendered.contains(needle),
            "{what} leaked {needle}: {rendered}"
        );
    }
}

fn altered(token: &str) -> String {
    let mut bytes = token.as_bytes().to_vec();
    let at = bytes.len() / 2;
    bytes[at] = if bytes[at] == b'A' { b'B' } else { b'A' };
    String::from_utf8(bytes).unwrap()
}

fn visible_params(after: Option<&str>) -> Value {
    let mut params = json!({
        "alias": "reader",
        "cursor_format": CURSOR_FORMAT,
    });
    if let Some(token) = after {
        params["after"] = json!(token);
    }
    params
}

#[test]
fn scoped_agent_events_are_opaque_and_continue_across_hidden_insertion_with_stable_ids() {
    let keys = keys();
    let world = build_world(Some(&keys), VISIBLE_COUNT, HIDDEN_COUNT, QUIET_HIDDEN_COUNT);
    let shared = world.boot();

    let first = scoped_page(&shared, "reader", visible_params(None));
    let first_rows = rows(&first);
    assert!(!first_rows.is_empty(), "{first}");
    assert!(
        first_rows.len() < VISIBLE_COUNT,
        "the fixture did not exercise continuation: {} rows",
        first_rows.len()
    );
    let first_ids = ids(&first);
    let first_markers = markers(&first);
    for (index, marker) in first_markers.iter().enumerate() {
        assert_eq!(
            marker,
            &format!("{VISIBLE_MARKER}{index}"),
            "scoped order changed"
        );
    }
    let rendered = first.to_string();
    assert!(
        !rendered.contains(HIDDEN_MARKER),
        "the scoped page exposed hidden-project activity: {first}"
    );
    let token = cursor(&first).expect("a non-empty scoped page must carry its forward cursor");

    // Hidden activity and new visible activity arrive after the first page.
    append_event(
        world.state(),
        "reader",
        "hidden_progress",
        &json!({"marker": format!("{HIDDEN_MARKER}between-pages")}),
        "hidden-job",
        "hidden-task",
        5000.0,
    );
    append_event(
        world.state(),
        "reader",
        "fixture_progress",
        &json!({"marker": VISIBLE_AFTER_MARKER}),
        "open-job",
        "open-task",
        5001.0,
    );

    // The continuation resumes exactly after the first page: no hidden row,
    // no repeated visible row, and the newly visible row appears once.
    let second = scoped_page(&shared, "reader", visible_params(Some(&token)));
    let second_markers = markers(&second);
    assert!(
        second_markers
            .iter()
            .any(|marker| marker.as_str() == VISIBLE_AFTER_MARKER),
        "the continuation skipped the event inserted after the first page: {second}"
    );
    let rendered = second.to_string();
    assert!(
        !rendered.contains(HIDDEN_MARKER),
        "hidden insertion changed the continuation: {second}"
    );
    for marker in &second_markers {
        assert!(
            !first_markers.contains(marker),
            "the continuation repeated a row from the first page: {marker}"
        );
    }

    // Walk to the terminal empty page: every visible fixture row appears
    // exactly once, no hidden row appears, and the empty page echoes the token.
    let mut seen: Vec<String> = first_markers.clone();
    seen.extend(second_markers);
    let mut after = cursor(&second).expect("the inserted visible row must be continuable");
    let mut terminal = None;
    for _ in 0..8 {
        let page = scoped_page(&shared, "reader", visible_params(Some(&after)));
        assert!(
            !page.to_string().contains(HIDDEN_MARKER),
            "a later page exposed hidden activity: {page}"
        );
        if rows(&page).is_empty() {
            terminal = Some((page, after.clone()));
            break;
        }
        for marker in markers(&page) {
            assert!(
                !seen.contains(&marker),
                "scoped pagination repeated {marker}"
            );
            seen.push(marker);
        }
        after = cursor(&page).expect("a non-empty page must carry its forward cursor");
    }
    let (terminal, supplied) = terminal.expect("scoped pagination did not reach an empty page");
    assert_eq!(
        terminal["cursor"],
        json!(supplied),
        "the terminal empty page did not echo the supplied token: {terminal}"
    );
    let mut expected: Vec<String> = (0..VISIBLE_COUNT)
        .map(|index| format!("{VISIBLE_MARKER}{index}"))
        .collect();
    expected.push(VISIBLE_AFTER_MARKER.to_string());
    assert_eq!(
        seen, expected,
        "the scoped feed did not preserve visible order/content"
    );

    // Repeating the terminal poll changes nothing and still echoes the token.
    let repeated = scoped_page(&shared, "reader", visible_params(Some(&supplied)));
    assert!(rows(&repeated).is_empty(), "{repeated}");
    assert_eq!(repeated["cursor"], json!(supplied), "{repeated}");

    // Stable identity: re-reading the first page (after the insertions) yields
    // the same ids for the same rows.
    let reread = scoped_page(&shared, "reader", visible_params(None));
    assert_eq!(
        ids(&reread),
        first_ids,
        "re-reading a page changed its stable ids"
    );
}

#[test]
fn agent_event_tokens_refuse_other_readers_aliases_workspaces_and_reregistration() {
    let keys = keys();
    let world = build_world(Some(&keys), 8, 2, 0);
    let shared = world.boot();
    let first = scoped_page(&shared, "reader", visible_params(None));
    let token = cursor(&first).expect("the first page must carry a token");
    assert!(!rows(&first).is_empty(), "{first}");

    // Another registered reader cannot use this reader's cursor.
    let other_reader = page(
        &shared,
        Asserted::Agent("peer".to_string()),
        &visible_params(Some(&token)),
    )
    .expect_err("a peer reader must not open another reader's cursor");
    assert_refusal(other_reader, "cross-reader cursor");

    // The cursor is bound to the alias it was minted for.
    let cross_alias = page(
        &shared,
        Asserted::Agent("reader".to_string()),
        &json!({
            "alias": "peer",
            "cursor_format": CURSOR_FORMAT,
            "after": token,
        }),
    )
    .expect_err("a cursor minted for one alias must not read another alias");
    assert_refusal(cross_alias, "cross-alias cursor");

    // The daemon stream is a different resource.
    let daemon = page(
        &shared,
        Asserted::Agent("reader".to_string()),
        &json!({
            "alias": "daemon",
            "cursor_format": CURSOR_FORMAT,
            "after": token,
        }),
    )
    .expect_err("an agent cursor must not read the daemon stream");
    assert_refusal(daemon, "daemon-stream cursor");

    // A scoped read must name the opaque format: omitting it is not a way to
    // fall back to numeric positions.
    let missing_format = page(
        &shared,
        Asserted::Agent("reader".to_string()),
        &json!({"alias": "reader"}),
    )
    .expect_err("a scoped read without cursor_format must be refused");
    assert_refusal(missing_format, "missing cursor format");

    // A numeric position is refused for a scoped reader regardless of format.
    for params in [
        json!({
            "alias": "reader",
            "cursor_format": CURSOR_FORMAT,
            "after": 1,
        }),
        json!({"alias": "reader", "after": 1}),
    ] {
        let numeric = page(&shared, Asserted::Agent("reader".to_string()), &params)
            .expect_err("a scoped reader must never send a numeric position");
        assert_refusal(numeric, "numeric scoped position");
    }

    // An unsupported format is not a downgrade.
    let bad_format = page(
        &shared,
        Asserted::Agent("reader".to_string()),
        &json!({"alias": "reader", "cursor_format": "numeric"}),
    )
    .expect_err("an unknown cursor format must be refused");
    assert_refusal(bad_format, "unknown cursor format");

    // Non-string `after` shapes are refused rather than coerced.
    for after in [json!(null), json!(true), json!([1]), json!({"seq": 1})] {
        let shape = page(
            &shared,
            Asserted::Agent("reader".to_string()),
            &json!({
                "alias": "reader",
                "cursor_format": CURSOR_FORMAT,
                "after": after,
            }),
        )
        .expect_err("a non-string after must be refused");
        assert_refusal(shape, "non-string after");
    }

    // Tampering is one generic refusal.
    let tampered = page(
        &shared,
        Asserted::Agent("reader".to_string()),
        &visible_params(Some(&altered(&token))),
    )
    .expect_err("an altered cursor must be refused");
    assert_refusal(tampered, "altered cursor");

    // The untampered token still continues its own feed.
    let continued = scoped_page(&shared, "reader", visible_params(Some(&token)));
    assert!(
        rows(&continued).is_empty(),
        "a continuation past the last visible row must be empty: {continued}"
    );
    assert_eq!(continued["cursor"], json!(token), "{continued}");

    // Another workspace with the same key material and alias set refuses it.
    let other_world = build_world(Some(&keys), 8, 2, 0);
    let other_shared = other_world.boot();
    let cross_workspace = page(
        &other_shared,
        Asserted::Agent("reader".to_string()),
        &visible_params(Some(&token)),
    )
    .expect_err("a cursor must not cross workspaces");
    assert_refusal(cross_workspace, "cross-workspace cursor");
    // Its own first page works, so the refusal is the binding, not a lost
    // project or alias.
    let own_page = scoped_page(&other_shared, "reader", visible_params(None));
    assert!(!rows(&own_page).is_empty(), "{own_page}");

    // Re-registering the alias mints a new durable uid: the old cursor is
    // refused, a fresh read still works.
    {
        let db = rusqlite::Connection::open(world.state().join("cadence.sqlite3")).unwrap();
        db.execute(
            "UPDATE agents SET state='stopped', endpoint=NULL WHERE alias='reader'",
            [],
        )
        .unwrap();
    }
    shared
        .store
        .remove_agent(
            "reader",
            true,
            &json!({"by": "operator", "by_kind": "operator"}),
        )
        .expect("the fixture agent must be removable");
    register(&shared, world.state(), "reader");
    let replay = page(
        &shared,
        Asserted::Agent("reader".to_string()),
        &visible_params(Some(&token)),
    )
    .expect_err("a re-registered alias must not inherit an old cursor");
    assert_refusal(replay, "re-registered alias cursor");
    let fresh = scoped_page(&shared, "reader", visible_params(None));
    assert!(!rows(&fresh).is_empty(), "{fresh}");
}

#[test]
fn withdrawn_project_access_refuses_the_continuation_without_invalidating_the_token() {
    let keys = keys();
    let world = build_world(Some(&keys), VISIBLE_COUNT, 4, 0);
    let shared = world.boot();

    let first = scoped_page(&shared, "reader", visible_params(None));
    let token = cursor(&first).expect("the first page must carry a token");
    let first_count = rows(&first).len();
    assert!(first_count < VISIBLE_COUNT, "{first}");

    // Withdraw the readable project's visibility between two pages.
    std::fs::write(world.visible_policy_path(), private_open_policy()).unwrap();
    let withdrawn = page(
        &shared,
        Asserted::Agent("reader".to_string()),
        &visible_params(Some(&token)),
    )
    .expect_err("a withdrawn reader must not continue its feed");
    assert_refusal(withdrawn, "withdrawn continuation");
    // A fresh read carries no token, so the daemon cannot tell it apart from
    // a genuinely projectless reader: it gets a valid empty scoped page, never
    // a numeric fallback. The refused old token above is what proves the
    // withdrawal took effect.
    let fresh = scoped_page(&shared, "reader", visible_params(None));
    assert!(
        rows(&fresh).is_empty(),
        "a withdrawn reader saw events on a fresh read: {fresh}"
    );
    assert_eq!(fresh["cursor"], Value::Null, "{fresh}");

    // Restoring access neither resets nor invalidates the already-issued
    // token: the same continuation resumes the same visible feed.
    std::fs::write(world.visible_policy_path(), visible_project_policy()).unwrap();
    let resumed = scoped_page(&shared, "reader", visible_params(Some(&token)));
    let resumed_markers = markers(&resumed);
    assert_eq!(
        resumed_markers.first().map(String::as_str),
        Some(format!("{VISIBLE_MARKER}{first_count}").as_str()),
        "the restored continuation did not resume after the first page: {resumed}"
    );
    for marker in &resumed_markers {
        assert!(
            !marker.contains(HIDDEN_MARKER),
            "restored page exposed hidden activity"
        );
    }
}

#[test]
fn empty_pages_tails_and_operator_numeric_controls_keep_their_contracts() {
    let keys = keys();
    let world = build_world(Some(&keys), VISIBLE_COUNT, HIDDEN_COUNT, QUIET_HIDDEN_COUNT);
    let shared = world.boot();

    // An agent with only hidden-project activity has an empty scoped feed:
    // cursor null (not 0), no rows, and a named scoped reader.
    let quiet = scoped_page(
        &shared,
        "quiet",
        json!({
            "alias": "quiet",
            "cursor_format": CURSOR_FORMAT,
        }),
    );
    assert!(rows(&quiet).is_empty(), "{quiet}");
    assert_eq!(quiet["cursor"], Value::Null, "{quiet}");
    assert!(
        !quiet.to_string().contains(QUIET_HIDDEN_MARKER),
        "hidden activity reached the empty scoped page: {quiet}"
    );

    // The quiet tail is empty too: `has_older` is visible-derived, so hidden
    // history must not flip it.
    let quiet_tail = scoped_page(
        &shared,
        "quiet",
        json!({
            "alias": "quiet",
            "tail": true,
            "cursor_format": CURSOR_FORMAT,
        }),
    );
    assert!(rows(&quiet_tail).is_empty(), "{quiet_tail}");
    assert_eq!(quiet_tail["has_older"], json!(false), "{quiet_tail}");
    assert_eq!(quiet_tail["cursor"], Value::Null, "{quiet_tail}");

    // The reader's tail is the newest visible page, bounded and opaque.
    let tail = scoped_page(
        &shared,
        "reader",
        json!({
            "alias": "reader",
            "tail": true,
            "cursor_format": CURSOR_FORMAT,
        }),
    );
    let tail_rows = rows(&tail);
    assert_eq!(
        tail_rows.len(),
        50,
        "the scoped tail page is not bounded: {tail}"
    );
    assert_eq!(tail["has_older"], json!(true), "{tail}");
    let tail_markers = markers(&tail);
    assert_eq!(
        tail_markers.first().map(String::as_str),
        Some(format!("{VISIBLE_MARKER}{}", VISIBLE_COUNT - 50).as_str()),
        "the scoped tail did not end on the newest visible rows: {tail}"
    );
    assert_eq!(
        tail_markers.last().map(String::as_str),
        Some(format!("{VISIBLE_MARKER}{}", VISIBLE_COUNT - 1).as_str()),
        "{tail}"
    );
    assert!(
        !tail.to_string().contains(HIDDEN_MARKER),
        "the tail exposed hidden activity: {tail}"
    );
    assert!(
        cursor(&tail).is_some(),
        "a non-empty tail must carry a cursor"
    );

    // A proven operator keeps the legacy numeric document: numeric seq rows,
    // numeric cursor, and no scoped reader acknowledgement when it did not ask
    // for the new format.
    let operator = page(&shared, Asserted::Operator, &json!({"alias": "reader"}))
        .expect("operator agent events");
    let operator_rows = rows(&operator);
    assert!(!operator_rows.is_empty(), "{operator}");
    for row in operator_rows {
        assert!(
            row["seq"].as_i64().is_some(),
            "the operator page lost its numeric position: {row}"
        );
        assert!(
            row["seq"].as_i64().is_some() && !row["id"].is_string(),
            "the operator row was converted to a scoped shape: {row}"
        );
    }
    assert!(
        operator["cursor"].as_i64().is_some(),
        "the operator page has no numeric cursor: {operator}"
    );
    assert!(
        operator
            .get(cadence_core::board_api::READER_SCOPE_FIELD)
            .is_none(),
        "a legacy operator page carried a reader acknowledgement: {operator}"
    );

    // The operator tail stays numeric as well.
    let operator_tail = page(
        &shared,
        Asserted::Operator,
        &json!({"alias": "reader", "tail": true}),
    )
    .expect("operator tail");
    assert_eq!(operator_tail["has_older"], json!(true), "{operator_tail}");
    assert!(
        operator_tail["cursor"].as_i64().is_some(),
        "the operator tail cursor is not numeric: {operator_tail}"
    );
    for row in rows(&operator_tail) {
        assert!(row["seq"].as_i64().is_some(), "{row}");
    }

    // An operator that asks for the scoped format is still answered
    // numerically: the format never downgrades a scoped reader and never
    // converts the operator's own document into opaque rows.
    let operator_formatted = page(
        &shared,
        Asserted::Operator,
        &json!({
            "alias": "reader",
            "cursor_format": CURSOR_FORMAT,
        }),
    )
    .expect("operator agent events with cursor_format");
    for row in rows(&operator_formatted) {
        assert!(row["seq"].as_i64().is_some(), "{row}");
    }
    assert!(
        operator_formatted["cursor"].as_i64().is_some(),
        "{operator_formatted}"
    );
    assert_ne!(
        operator_formatted[cadence_core::board_api::READER_SCOPE_FIELD],
        json!(cadence_core::board_api::AGENT_EVENTS_SCOPE_ACK),
        "an operator page claimed the scoped reader acknowledgement: {operator_formatted}"
    );
}

/// Missing key material refuses a scoped agent-events read outright — never a
/// numeric fallback, never an empty success — while a proven operator keeps
/// its numeric document.
#[test]
fn missing_keyring_refuses_scoped_agent_events_without_a_numeric_fallback() {
    let world = build_world(None, 4, 0, 0);
    let shared = world.boot();

    let refused = page(
        &shared,
        Asserted::Agent("reader".to_string()),
        &visible_params(None),
    )
    .expect_err("a scoped read without a keyring must fail closed");
    assert_refusal(refused, "unconfigured scoped read");

    // The operator is unaffected: its numeric document needs no key material.
    let operator = page(&shared, Asserted::Operator, &json!({"alias": "reader"}))
        .expect("the operator keeps its numeric agent-events read");
    assert!(
        operator["cursor"].as_i64().is_some(),
        "the operator page lost its numeric cursor: {operator}"
    );
    for row in rows(&operator) {
        assert!(row["seq"].as_i64().is_some(), "{row}");
    }
}

/// A scoped reader that holds no readable-project grant is not refused: it
/// receives an ordinary empty opaque feed (no rows, `null` cursor), never a
/// numeric fallback and never hidden activity. The world's only project is
/// private before the first read, so this reader never held a grant.
#[test]
fn projectless_scoped_reader_receives_an_empty_opaque_feed() {
    let keys = keys();
    let world = build_world(Some(&keys), 4, 0, 0);
    std::fs::write(world.visible_policy_path(), private_open_policy()).unwrap();
    let shared = world.boot();

    let empty = scoped_page(&shared, "reader", visible_params(None));
    assert!(
        rows(&empty).is_empty(),
        "a projectless reader saw hidden events: {empty}"
    );
    assert_eq!(empty["cursor"], Value::Null, "{empty}");
    assert!(
        !empty.to_string().contains(VISIBLE_MARKER),
        "the empty scoped feed exposed readable-project content: {empty}"
    );

    // The scoped tail is empty too, with visible-derived has_older and a null
    // cursor — never a numeric 0.
    let tail = scoped_page(
        &shared,
        "reader",
        json!({
            "alias": "reader",
            "tail": true,
            "cursor_format": CURSOR_FORMAT,
        }),
    );
    assert!(rows(&tail).is_empty(), "{tail}");
    assert_eq!(tail["has_older"], json!(false), "{tail}");
    assert_eq!(tail["cursor"], Value::Null, "{tail}");

    // Opacity is not relaxed for an empty feed: a numeric position is still a
    // downgrade and is refused.
    let numeric = page(
        &shared,
        Asserted::Agent("reader".to_string()),
        &json!({
            "alias": "reader",
            "cursor_format": CURSOR_FORMAT,
            "after": 1,
        }),
    )
    .expect_err("a projectless scoped reader must not send a numeric position");
    assert_refusal(numeric, "projectless numeric position");

    // The proven operator still recovers the private stream numerically.
    let operator = page(&shared, Asserted::Operator, &json!({"alias": "reader"}))
        .expect("operator recovery of the private stream");
    assert!(
        operator["events"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()),
        "the operator lost the private stream recovery: {operator}"
    );
    for row in rows(&operator) {
        assert!(row["seq"].as_i64().is_some(), "{row}");
    }
}

const INSERTED_HIDDEN_MARKER: &str = "CAD2215_AE_INSERTED_HIDDEN";

/// A selected row whose event is rebound to a hidden project after selection
/// must not be returned: the per-selected-row re-admission refuses the page.
/// The later read returns the still-readable rows only.
#[test]
fn selected_event_rebound_to_a_private_project_is_not_returned() {
    let keys = keys();
    let world = build_world(Some(&keys), 8, 0, 0);
    let state = world.state().to_path_buf();
    let fired = Arc::new(AtomicBool::new(false));
    let shared = world.boot_with(|opts| {
        let fired = Arc::clone(&fired);
        let state = state.clone();
        opts.agent_events_selected_hook = Some(Arc::new(
            move |alias: &str, rows: &[crate::store::Event]| {
                if alias != "reader" || rows.is_empty() || fired.swap(true, Ordering::SeqCst) {
                    return;
                }
                let seq = rows[0].seq;
                let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
                db.execute(
                    "UPDATE events SET job_id='hidden-job', task_id='hidden-task' WHERE seq=?1",
                    [seq],
                )
                .unwrap();
            },
        ));
    });

    let refused = page(
        &shared,
        Asserted::Agent("reader".to_string()),
        &visible_params(None),
    )
    .expect_err("a selected row rebound to a private project must refuse the page");
    assert_refusal(refused, "selected row rebound private");
    assert!(
        fired.load(Ordering::SeqCst),
        "the selection hook never fired"
    );

    // The rebound row is gone; every still-readable row is returned.
    let fresh = scoped_page(&shared, "reader", visible_params(None));
    let fresh_markers = markers(&fresh);
    assert!(
        !fresh_markers.contains(&format!("{VISIBLE_MARKER}0")),
        "the rebound private row was returned: {fresh}"
    );
    assert_eq!(fresh_markers.len(), 7, "{fresh}");
    assert!(
        !fresh.to_string().contains(HIDDEN_MARKER),
        "a later read exposed hidden activity: {fresh}"
    );
}

/// Inserting an unrelated hidden issue/job (with a hidden event on the
/// reader's own stream) between selection and response must not refuse the
/// page or change the visible rows or their continuation.
#[test]
fn unrelated_hidden_issue_and_job_insertion_does_not_refuse_the_visible_page() {
    let keys = keys();
    let world = build_world(Some(&keys), VISIBLE_COUNT, 0, 0);
    let pm = world.pm.clone();
    let state = world.state().to_path_buf();
    let fired = Arc::new(AtomicBool::new(false));
    let shared = world.boot_with(|opts| {
        let fired = Arc::clone(&fired);
        let pm = pm.clone();
        let state = state.clone();
        opts.agent_events_selected_hook = Some(Arc::new(
            move |alias: &str, _rows: &[crate::store::Event]| {
                if alias != "reader" || fired.swap(true, Ordering::SeqCst) {
                    return;
                }
                let issue = pm.join("hidden").join("HID-2");
                std::fs::create_dir_all(&issue).unwrap();
                std::fs::write(
                    issue.join("issue.md"),
                    "---\nid: HID-2\ntitle: HID-2 fixture\nstatus: backlog\n\
                     priority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n",
                )
                .unwrap();
                let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
                db.execute_batch(
                    "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
                     VALUES('hidden-job-2','Hidden 2','spec.md','manager','HID-2','open',9,9);
                     INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
                     VALUES('hidden-task-2','hidden-job-2','Hidden 2','reader','running',9,9);",
                )
                .unwrap();
                db.execute(
                    "INSERT INTO events(alias,kind,payload,job_id,task_id,at) VALUES(?,?,?,?,?,?)",
                    rusqlite::params![
                        "reader",
                        "hidden_progress",
                        json!({"marker": INSERTED_HIDDEN_MARKER}).to_string(),
                        "hidden-job-2",
                        "hidden-task-2",
                        9.0,
                    ],
                )
                .unwrap();
            },
        ));
    });

    let first = scoped_page(&shared, "reader", visible_params(None));
    let first_markers = markers(&first);
    assert_eq!(first_markers.len(), 100, "{first}");
    for (index, marker) in first_markers.iter().enumerate() {
        assert_eq!(
            marker,
            &format!("{VISIBLE_MARKER}{index}"),
            "unrelated hidden insertion changed the visible page"
        );
    }
    for needle in [INSERTED_HIDDEN_MARKER, HIDDEN_MARKER] {
        assert!(
            !first.to_string().contains(needle),
            "the visible page exposed {needle}: {first}"
        );
    }
    let token = cursor(&first).expect("the first page must carry a token");
    let second = scoped_page(&shared, "reader", visible_params(Some(&token)));
    let expected: Vec<String> = (100..VISIBLE_COUNT)
        .map(|index| format!("{VISIBLE_MARKER}{index}"))
        .collect();
    assert_eq!(
        markers(&second),
        expected,
        "the continuation changed after an unrelated hidden insertion: {second}"
    );
}

/// The scoped tail keeps one older visible row as the `has_older` witness
/// through revalidation, then removes it before the response. If that witness
/// becomes private between selection and response, the tail must refuse rather
/// than reporting a stale `has_older` computed from a row it can no longer
/// admit. The witness is deliberately not one of the 50 returned rows.
#[test]
fn tail_refuses_when_the_older_has_older_witness_becomes_private() {
    let keys = keys();
    // Control: the same fixture tail is 50 returned rows with has_older true.
    let control = build_world(Some(&keys), VISIBLE_COUNT, 0, 0);
    let control_shared = control.boot();
    let control_tail = scoped_page(
        &control_shared,
        "reader",
        json!({"alias": "reader", "tail": true, "cursor_format": CURSOR_FORMAT}),
    );
    assert_eq!(rows(&control_tail).len(), 50, "{control_tail}");
    assert_eq!(control_tail["has_older"], json!(true), "{control_tail}");

    let world = build_world(Some(&keys), VISIBLE_COUNT, 0, 0);
    let state = world.state().to_path_buf();
    let fired = Arc::new(AtomicBool::new(false));
    let witness = Arc::new(std::sync::Mutex::new(None));
    let shared = world.boot_with(|opts| {
        let fired = Arc::clone(&fired);
        let witness = Arc::clone(&witness);
        let state = state.clone();
        opts.agent_events_selected_hook = Some(Arc::new(
            move |alias: &str, rows: &[crate::store::Event]| {
                if alias != "reader" || fired.swap(true, Ordering::SeqCst) {
                    return;
                }
                assert_eq!(
                    rows.len(),
                    51,
                    "the tail hook must see 50 returned rows plus one older witness: {} rows",
                    rows.len()
                );
                // rows[0] is the older witness, never a returned row.
                let seq = rows[0].seq;
                *witness.lock().unwrap() = Some(seq);
                let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
                db.execute(
                    "UPDATE events SET job_id='hidden-job', task_id='hidden-task' WHERE seq=?1",
                    [seq],
                )
                .unwrap();
            },
        ));
    });

    let refused = page(
        &shared,
        Asserted::Agent("reader".to_string()),
        &json!({"alias": "reader", "tail": true, "cursor_format": CURSOR_FORMAT}),
    )
    .expect_err("a tail whose has_older witness became private must refuse");
    assert_refusal(refused, "tail witness moved private");
    assert!(
        fired.load(Ordering::SeqCst),
        "the selection hook never fired"
    );
    assert!(
        witness.lock().unwrap().is_some(),
        "the hook did not record the older witness"
    );
}
