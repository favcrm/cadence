//! CAD-2215: independent scoped job-event acceptance.
//!
//! Two real store workspaces carry the *same* visible `job_events` history but
//! unrelated hidden-project events interleaved at different global positions.
//! Scoped readers must see the same content, order, stable row identity and
//! pagination boundaries in both, and neither a continuation nor an empty poll
//! may expose a global sequence position or hidden activity. Tokens are bound
//! to the admitted principal and job, so tampering, a numeric position, another
//! job, another principal or another workspace is refused; a project policy
//! that withdraws access stops further reads; the proven operator keeps the
//! numeric document and names its mode.
//!
//! Key material and the keyring record are fixture-owned files in separate temp
//! roots; the only setup operation is the real operator
//! `scoped_cursor_initialize` RPC. No production code, harness hook or existing
//! acceptance is modified. This does NOT establish OS isolation or
//! private-project availability.

use super::*;
use crate::test_seam::{scoped, Asserted};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const IDENTITY_BYTE: u8 = 0xa1;
const KEY_A_BYTE: u8 = 0xb2;
const KEY_B_BYTE: u8 = 0xc3;
const KEY_ID_A: u8 = 0x01;
const KEY_ID_B: u8 = 0x02;

const VISIBLE_JOB: &str = "cad2215-visible-job";
const SIBLING_JOB: &str = "cad2215-sibling-job";
const HIDDEN_JOB: &str = "cad2215-hidden-job";
const HIDDEN_MARKER: &str = "CAD2215_HIDDEN_EVENT_";
const VISIBLE_MARKER: &str = "CAD2215_VISIBLE_EVENT_";
const PAYLOAD_SURVIVOR_MARKER: &str = "CAD2215_PAYLOAD_SURVIVOR";
const VISIBLE_COUNT: usize = 205;

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

/// Owner-provisioned key material plus the fixed keyring record content. The
/// same record is placed in each workspace so a token can only differ by the
/// codec's own workspace binding.
struct Keys {
    dir: tempfile::TempDir,
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
    Keys { dir, config }
}

impl Keys {
    fn key_file(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

fn write_config(state: &Path, config: &Value) {
    let path = state.join("scoped-cursors.json");
    write_private(&path, &serde_json::to_vec(config).unwrap());
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

/// One real workspace: a tracker, a registered agent set and a store. Both
/// histories are built by the same code; only the hidden interleaving differs.
struct History {
    _state: tempfile::TempDir,
    shared: Arc<Shared>,
    pm: PathBuf,
}

impl History {
    fn scoped(&self, who: &str, params: &Value) -> Result<Value> {
        let who = Asserted::Agent(who.to_string());
        scoped(who, || {
            self.shared
                .dispatch("job_events", params, std::process::id())
        })
    }

    fn operator(&self, params: &Value) -> Result<Value> {
        scoped(Asserted::Operator, || {
            self.shared
                .dispatch("job_events", params, std::process::id())
        })
    }

    fn visible_project_policy(&self) -> PathBuf {
        self.pm.join("open").join("project.yaml")
    }

    fn state_dir(&self) -> &Path {
        self._state.path()
    }
}

fn insert_event(
    tx: &rusqlite::Transaction<'_>,
    alias: &str,
    kind: &str,
    payload: Value,
    job: &str,
    at: f64,
) {
    tx.execute(
        "INSERT INTO events(alias,kind,payload,job_id,task_id,at) VALUES(?,?,?,?,?,?)",
        rusqlite::params![
            alias,
            kind,
            payload.to_string(),
            job,
            Option::<String>::None,
            at
        ],
    )
    .unwrap();
}

/// Build a real workspace with `visible_count` visible events on
/// [`VISIBLE_JOB`] and hidden-project events interleaved per `plan`
/// (`(before_visible_index, count)`) plus `trailing_hidden` at the end.
/// `Some(keys)` writes the keyring record and binds it with the real operator
/// initialization RPC; `None` leaves the workspace unconfigured.
fn build_history(
    keys: Option<&Keys>,
    visible_count: usize,
    plan: &[(usize, usize)],
    trailing_hidden: usize,
) -> History {
    let state = tempfile::tempdir().unwrap();
    // tempfile creates directories with 0o777 & !umask (0775 under umask 0002);
    // the keyring record admits the state root only at exactly 0700.
    private_dir(state.path());
    let state_dir = state.path().to_path_buf();
    let pm = state_dir.join("tracker");
    crate::issue::Pm::init(&pm).unwrap();
    // `create_dir_all` also inherits the umask; `admit_tracker` requires an
    // existing tracker root to be owner-only.
    std::fs::set_permissions(&pm, std::fs::Permissions::from_mode(0o700)).unwrap();
    write_project(&pm, "open", "VIS", "VIS-1", "");
    write_project(
        &pm,
        "hidden",
        "HID",
        "HID-1",
        "policy:\n  version: 1\n  visibility: private\n",
    );
    if let Some(keys) = keys {
        write_config(&state_dir, &keys.config);
    }
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let shared = Shared::new(&state_dir, &opts).unwrap();
    for alias in ["manager", "reader", "peer"] {
        register(&shared, &state_dir, alias);
    }
    let mut db = rusqlite::Connection::open(state_dir.join("cadence.sqlite3")).unwrap();
    db.execute_batch(&format!(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
         VALUES ('{VISIBLE_JOB}','Visible','spec.md','manager','VIS-1','open',1,1),
                ('{SIBLING_JOB}','Sibling','spec.md','manager','VIS-1','open',2,2),
                ('{HIDDEN_JOB}','Hidden','spec.md','manager','HID-1','open',3,3);"
    ))
    .unwrap();
    {
        let tx = db.transaction().unwrap();
        let mut hidden = 0usize;
        let hidden_event = |tx: &rusqlite::Transaction<'_>, hidden: &mut usize| {
            insert_event(
                tx,
                "manager",
                "hidden_progress",
                json!({"marker": format!("{HIDDEN_MARKER}{}", *hidden)}),
                HIDDEN_JOB,
                500.0 + *hidden as f64,
            );
            *hidden += 1;
        };
        for index in 0..visible_count {
            for (before, count) in plan {
                if *before == index {
                    for _ in 0..*count {
                        hidden_event(&tx, &mut hidden);
                    }
                }
            }
            insert_event(
                &tx,
                "manager",
                "fixture_progress",
                json!({"n": index, "marker": format!("{VISIBLE_MARKER}{index}")}),
                VISIBLE_JOB,
                1000.0 + index as f64,
            );
        }
        // A real visible event whose payload legitimately contains the words
        // `seq` and `event:<n>`. The scoped projection must preserve the
        // payload verbatim while removing only the row's global position.
        insert_event(
            &tx,
            "manager",
            "fixture_progress",
            json!({
                "n": visible_count,
                "seq": 7,
                "text": "event:123",
                "marker": PAYLOAD_SURVIVOR_MARKER,
            }),
            VISIBLE_JOB,
            1000.0 + visible_count as f64,
        );
        for _ in 0..trailing_hidden {
            hidden_event(&tx, &mut hidden);
        }
        tx.commit().unwrap();
    }
    drop(db);
    if let Some(keys) = keys {
        let response = scoped(Asserted::Operator, || {
            shared.dispatch("scoped_cursor_initialize", &json!({}), std::process::id())
        })
        .expect("the operator must initialize the keyring");
        assert_eq!(response["initialized"], json!(true), "{response}");
        // The key files stay outside the workspace.
        assert!(!state_dir.join("cursor-a.key").exists());
        assert!(keys.key_file("cursor-a.key").exists());
    }
    History {
        _state: state,
        shared,
        pm,
    }
}

fn scoped_page(history: &History, who: &str, job: &str, after: Option<&str>) -> Value {
    let mut params = json!({
        "job": job,
        "cursor_format": cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT,
    });
    if let Some(token) = after {
        params["after"] = json!(token);
    }
    let page = history.scoped(who, &params).expect("scoped job-event page");
    assert_eq!(
        page[cadence_core::board_api::READER_SCOPE_FIELD],
        json!(cadence_core::board_api::JOB_EVENTS_SCOPE_ACK),
        "{who} page did not acknowledge the scoped job-event protocol: {page}"
    );
    page
}

/// Walk a scoped feed to its terminal empty page. Every page must name the
/// scoped reader, and an empty continue-a-page must echo the supplied token.
fn scoped_pages(history: &History, who: &str, job: &str) -> Vec<Value> {
    let mut after: Option<String> = None;
    let mut pages = Vec::new();
    loop {
        let page = scoped_page(history, who, job, after.as_deref());
        let events = page["events"]
            .as_array()
            .unwrap_or_else(|| panic!("scoped page has no events array: {page}"));
        assert_no_structural_seq(&page, "scoped page");
        if let Some(flag) = page.get("has_older") {
            assert!(
                flag.is_boolean(),
                "a scoped page's has_older is not a boolean: {page}"
            );
        }
        if let Some(token) = after.as_deref() {
            if events.is_empty() {
                assert_eq!(
                    page["cursor"],
                    json!(token),
                    "empty poll did not echo the supplied cursor: {page}"
                );
            }
        }
        let next = page["cursor"]
            .as_str()
            .filter(|cursor| !cursor.is_empty())
            .map(str::to_string);
        let empty = events.is_empty();
        pages.push(page);
        if empty {
            break;
        }
        after = Some(next.expect("a non-empty scoped page must carry its forward cursor"));
        assert!(pages.len() < 16, "scoped pagination did not terminate");
    }
    pages
}

fn operator_pages(history: &History, job: &str) -> Vec<Value> {
    let mut after: i64 = 0;
    let mut pages = Vec::new();
    loop {
        let page = history
            .operator(&json!({
                "job": job,
                "cursor_format": cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT,
                "after": after,
            }))
            .expect("operator job-event page");
        assert_eq!(
            page[cadence_core::board_api::READER_SCOPE_FIELD],
            json!(cadence_core::board_api::JOB_EVENTS_OPERATOR_SCOPE_ACK),
            "operator page did not name the numeric reader mode: {page}"
        );
        let events = page["events"].as_array().expect("operator events array");
        for event in events {
            assert!(
                event["seq"].as_i64().is_some(),
                "operator page lost its numeric event position: {page}"
            );
        }
        let cursor = page["cursor"]
            .as_i64()
            .unwrap_or_else(|| panic!("operator page has no numeric cursor: {page}"));
        if events.is_empty() {
            assert_eq!(
                cursor, after,
                "operator empty page did not echo its position"
            );
            pages.push(page);
            break;
        }
        after = cursor;
        pages.push(page);
        assert!(pages.len() < 16, "operator pagination did not terminate");
    }
    pages
}

fn page_sizes(pages: &[Value]) -> Vec<usize> {
    pages
        .iter()
        .map(|page| page["events"].as_array().unwrap().len())
        .collect()
}

/// The visible projection a scoped reader may legitimately compare: no id and
/// no cursor (those are keyed/randomized), only the authorized content.
fn visible_rows(pages: &[Value]) -> Vec<Value> {
    pages
        .iter()
        .flat_map(|page| {
            page["events"].as_array().unwrap().iter().map(|event| {
                json!({
                    "alias": event["alias"],
                    "kind": event["kind"],
                    "payload": event["payload"],
                    "job_id": event["job_id"],
                    "task_id": event["task_id"],
                    "at": event["at"],
                })
            })
        })
        .collect()
}

fn event_ids(page: &Value) -> Vec<String> {
    page["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| {
            event["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| panic!("scoped event has no stable id: {event}"))
                .to_string()
        })
        .collect()
}

/// Only the response's structural metadata and each event row may carry a
/// global position. Payloads are caller/provider content and are preserved
/// verbatim — `seq`/`event:<n>` inside a payload is real content.
fn assert_no_structural_seq(page: &Value, what: &str) {
    let body = page
        .as_object()
        .unwrap_or_else(|| panic!("{what} is not an object: {page}"));
    for field in ["seq", "event_seq"] {
        assert!(
            !body.contains_key(field),
            "{what} exposed a structural {field}: {page}"
        );
    }
    for row in body
        .get("events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let row = row
            .as_object()
            .unwrap_or_else(|| panic!("{what} row is not an object: {row}"));
        for field in ["seq", "event_seq"] {
            assert!(
                !row.contains_key(field),
                "{what} row exposed a structural {field}: {row:?}"
            );
        }
    }
}

/// The fixture event whose payload legitimately contains `seq`/`event:<n>`.
fn survivor_row(pages: &[Value]) -> &Value {
    for page in pages {
        for row in page["events"].as_array().unwrap() {
            if row["payload"]["marker"] == json!(PAYLOAD_SURVIVOR_MARKER) {
                return row;
            }
        }
    }
    panic!("the verbatim-payload event is missing from the scoped pages");
}

fn assert_no_hidden(page: &Value, what: &str) {
    let rendered = page.to_string();
    assert!(
        !rendered.contains(HIDDEN_MARKER),
        "{what} exposed hidden-project activity: {page}"
    );
    assert!(
        !rendered.contains(HIDDEN_JOB),
        "{what} exposed the hidden job: {page}"
    );
}

fn assert_refusal(error: Error, expected: &str, what: &str) {
    assert_eq!(error.code(), Some(expected), "{what}: {error:?}");
    let rendered = format!("{error:?} {error}");
    for needle in [VISIBLE_MARKER, HIDDEN_MARKER, "csc1_"] {
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

#[test]
fn identical_visible_histories_paginate_identically_without_global_or_hidden_activity() {
    let keys = keys();
    // History A: hidden events spread through the visible feed.
    let a = build_history(
        Some(&keys),
        VISIBLE_COUNT,
        &[(0, 5), (50, 20), (120, 20)],
        0,
    );
    // History B: a different count, at different global positions and at the end.
    let b = build_history(Some(&keys), VISIBLE_COUNT, &[(0, 40)], 80);

    let a_pages = scoped_pages(&a, "reader", VISIBLE_JOB);
    let b_pages = scoped_pages(&b, "reader", VISIBLE_JOB);
    for page in a_pages.iter().chain(b_pages.iter()) {
        assert_no_hidden(page, "scoped page");
    }

    // Content and order are identical regardless of hidden interleaving.
    let a_rows = visible_rows(&a_pages);
    let b_rows = visible_rows(&b_pages);
    assert_eq!(
        a_rows, b_rows,
        "hidden interleaving changed the visible projection"
    );
    assert_eq!(a_rows.len(), VISIBLE_COUNT + 1);

    // The payload is caller/provider content: `seq`/`event:<n>` inside it are
    // preserved verbatim, while the row's own global position is absent.
    let survivor = survivor_row(&a_pages);
    assert_eq!(
        survivor["payload"]["seq"],
        json!(7),
        "the scoped projection rewrote payload seq: {survivor}"
    );
    assert_eq!(
        survivor["payload"]["text"],
        json!("event:123"),
        "the scoped projection rewrote a payload event reference: {survivor}"
    );
    assert_eq!(survivor["payload"]["n"], json!(VISIBLE_COUNT));
    assert!(
        !survivor.as_object().unwrap().contains_key("seq"),
        "the verbatim event row carried a structural seq: {survivor}"
    );
    // Page boundaries derive from eligible events only, so they agree too.
    assert_eq!(
        page_sizes(&a_pages),
        page_sizes(&b_pages),
        "hidden interleaving changed eligible page boundaries"
    );
    assert!(
        a_pages.len() >= 2,
        "the fixture did not exercise continuation"
    );

    // Eligible pagination matches the numeric operator view event for event.
    let operator = operator_pages(&a, VISIBLE_JOB);
    assert_eq!(
        page_sizes(&a_pages),
        page_sizes(&operator),
        "scoped pages do not follow the eligible numerical page boundaries"
    );
    let operator_rows = visible_rows(&operator);
    assert_eq!(
        a_rows, operator_rows,
        "scoped projection diverged from the operator view"
    );

    // Stable identity: re-reading the same page yields the same ids, unique
    // within the page — and still no global position.
    let first_ids = event_ids(&a_pages[0]);
    assert_eq!(first_ids.len(), page_sizes(&a_pages)[0]);
    let mut unique = first_ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        first_ids.len(),
        "duplicate stable ids in one page"
    );
    let reread = scoped_page(&a, "reader", VISIBLE_JOB, None);
    assert_eq!(
        event_ids(&reread),
        first_ids,
        "re-reading a page changed stable ids"
    );
    assert_no_structural_seq(&reread, "re-read scoped page");

    // Continuation stays on eligible events: the terminal empty page echoes
    // the supplied token and discloses nothing else.
    let terminal = a_pages.last().unwrap();
    assert!(terminal["events"].as_array().unwrap().is_empty());
    assert!(
        terminal["cursor"]
            .as_str()
            .is_some_and(|token| !token.is_empty()),
        "terminal scoped page lost its echoed cursor: {terminal}"
    );
    assert_no_structural_seq(terminal, "terminal scoped page");
}

#[test]
fn scoped_tokens_refuse_tampering_numeric_positions_and_foreign_bindings() {
    let keys = keys();
    let a = build_history(Some(&keys), 8, &[(0, 3), (4, 2)], 0);
    let b = build_history(Some(&keys), 8, &[(0, 1)], 0);
    let first = scoped_page(&a, "reader", VISIBLE_JOB, None);
    let token = first["cursor"].as_str().unwrap().to_string();
    assert!(!token.is_empty());
    assert!(
        token.parse::<i64>().is_err(),
        "a scoped cursor is a numeric position: {token}"
    );

    // A numeric position from a scoped reader is refused, never read as seq.
    let numeric = a
        .scoped(
            "reader",
            &json!({
                "job": VISIBLE_JOB,
                "cursor_format": cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT,
                "after": 1,
            }),
        )
        .expect_err("numeric scoped cursor must be refused");
    assert_refusal(
        numeric,
        "job_events_cursor_numeric",
        "numeric scoped cursor",
    );

    // A scoped read without the requested opaque mode is refused.
    let missing = a
        .scoped("reader", &json!({"job": VISIBLE_JOB}))
        .expect_err("scoped read without cursor_format must be refused");
    assert_refusal(missing, "job_events_cursor_format", "missing cursor_format");
    let unknown = a
        .scoped(
            "reader",
            &json!({"job": VISIBLE_JOB, "cursor_format": "numeric"}),
        )
        .expect_err("unknown cursor_format must be refused");
    assert_refusal(unknown, "job_events_cursor_format", "unknown cursor_format");

    // Tampered, cross-job, cross-principal and cross-workspace tokens are all
    // the same generic refusal: none of them reveals a position.
    let tampered = a
        .scoped(
            "reader",
            &json!({
                "job": VISIBLE_JOB,
                "cursor_format": cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT,
                "after": altered(&token),
            }),
        )
        .expect_err("tampered token must be refused");
    assert_refusal(tampered, "job_events_cursor_invalid", "tampered token");

    let wrong_job = a
        .scoped(
            "reader",
            &json!({
                "job": SIBLING_JOB,
                "cursor_format": cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT,
                "after": token,
            }),
        )
        .expect_err("cross-job token must be refused");
    assert_refusal(wrong_job, "job_events_cursor_invalid", "cross-job token");

    let wrong_principal = a
        .scoped(
            "peer",
            &json!({
                "job": VISIBLE_JOB,
                "cursor_format": cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT,
                "after": token,
            }),
        )
        .expect_err("cross-principal token must be refused");
    assert_refusal(
        wrong_principal,
        "job_events_cursor_invalid",
        "cross-principal token",
    );

    let wrong_workspace = b
        .scoped(
            "reader",
            &json!({
                "job": VISIBLE_JOB,
                "cursor_format": cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT,
                "after": token,
            }),
        )
        .expect_err("cross-workspace token must be refused");
    assert_refusal(
        wrong_workspace,
        "job_events_cursor_invalid",
        "cross-workspace token",
    );

    // The untouched token still works for its own reader and job: the first
    // page re-reads with identical stable ids, and continuing from its own
    // position is a legal empty poll that echoes the token.
    let again = scoped_page(&a, "reader", VISIBLE_JOB, None);
    assert_eq!(
        event_ids(&again),
        event_ids(&first),
        "the untampered token's page changed"
    );
    let end = scoped_page(&a, "reader", VISIBLE_JOB, Some(&token));
    assert_eq!(end["cursor"], json!(token));
    assert!(end["events"].as_array().unwrap().is_empty());
}

#[test]
fn missing_keyring_and_withdrawn_project_access_refuse_further_reads() {
    // No keyring record and no operator initialization: the scoped feed is
    // unavailable, never a numeric fallback and never an empty success.
    let unconfigured = build_history(None, 4, &[], 0);
    let error = unconfigured
        .scoped(
            "reader",
            &json!({
                "job": VISIBLE_JOB,
                "cursor_format": cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT,
            }),
        )
        .expect_err("a scoped read without a keyring must fail closed");
    assert_refusal(error, "scoped_cursor_unavailable", "unconfigured keyring");

    // A configured workspace where the reader's project access is withdrawn
    // between pages: the already-issued token must not read further.
    let keys = keys();
    let history = build_history(Some(&keys), 8, &[(0, 2)], 0);
    let first = scoped_page(&history, "reader", VISIBLE_JOB, None);
    let token = first["cursor"].as_str().unwrap().to_string();
    assert!(!first["events"].as_array().unwrap().is_empty());

    std::fs::write(
        history.visible_project_policy(),
        "key: open\nprefix: VIS\npolicy:\n  version: 1\n  visibility: private\n",
    )
    .unwrap();

    let continued = history
        .scoped(
            "reader",
            &json!({
                "job": VISIBLE_JOB,
                "cursor_format": cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT,
                "after": token,
            }),
        )
        .expect_err("a withdrawn reader must not continue its feed");
    assert_eq!(continued.kind(), "rejected", "{continued:?}");
    let rendered = format!("{continued:?} {continued}");
    assert!(
        !rendered.contains(VISIBLE_MARKER),
        "revocation refusal leaked a visible event: {rendered}"
    );
    let fresh = history
        .scoped(
            "reader",
            &json!({
                "job": VISIBLE_JOB,
                "cursor_format": cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT,
            }),
        )
        .expect_err("a fresh scoped read after revocation must be refused");
    assert_eq!(fresh.kind(), "rejected", "{fresh:?}");

    // The proven operator still recovers the same events numerically.
    let recovered = operator_pages(&history, VISIBLE_JOB);
    let rows = visible_rows(&recovered);
    assert_eq!(
        rows.len(),
        9,
        "operator recovery lost the job's events: {recovered:?}"
    );
}

#[test]
fn proven_operator_keeps_numeric_recovery_and_names_its_mode() {
    let keys = keys();
    let history = build_history(Some(&keys), VISIBLE_COUNT, &[(10, 5), (100, 7)], 0);

    let operator = operator_pages(&history, VISIBLE_JOB);
    assert!(
        operator.len() >= 2,
        "the operator fixture did not span pages"
    );
    let rows = visible_rows(&operator);
    assert_eq!(rows.len(), VISIBLE_COUNT + 1);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row["payload"]["n"], json!(index), "operator order changed");
    }

    // The same workspace also answers the scoped reader without handing it the
    // numeric document, so the two modes are distinguishable per response.
    let constrained = scoped_page(&history, "reader", VISIBLE_JOB, None);
    assert_eq!(
        constrained[cadence_core::board_api::READER_SCOPE_FIELD],
        json!(cadence_core::board_api::JOB_EVENTS_SCOPE_ACK)
    );
    assert_no_structural_seq(&constrained, "scoped page beside the operator read");
}

/// Operator-proven relayed read for `read_as`: the daemon re-proves the
/// connection as the operator, then resolves the named actor.
fn read_as_page(history: &History, actor: &str, job: &str, after: Option<&str>) -> Result<Value> {
    let mut params = json!({
        "job": job,
        "read_as": actor,
        "cursor_format": cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT,
    });
    if let Some(token) = after {
        params["after"] = json!(token);
    }
    scoped(Asserted::Operator, || {
        history
            .shared
            .dispatch("job_events", &params, std::process::id())
    })
}

#[test]
fn relayed_agent_read_as_cursor_is_bound_to_the_registration_uid() {
    let keys = keys();
    let history = build_history(Some(&keys), 8, &[], 0);
    let old_uid = history.shared.store.agent("manager").unwrap().uid;

    let first = read_as_page(&history, "agents/manager", VISIBLE_JOB, None)
        .expect("operator-proven read_as page");
    assert_eq!(
        first[cadence_core::board_api::READER_SCOPE_FIELD],
        json!(cadence_core::board_api::JOB_EVENTS_SCOPE_ACK),
        "{first}"
    );
    assert_no_structural_seq(&first, "read_as page");
    let token = first["cursor"].as_str().unwrap().to_string();
    assert!(!first["events"].as_array().unwrap().is_empty());

    // A different relayed agent principal cannot open this reader's token.
    let peer = read_as_page(&history, "agents/peer", VISIBLE_JOB, Some(&token))
        .expect_err("cross-agent read_as token must be refused");
    assert_refusal(
        peer,
        "job_events_cursor_invalid",
        "cross-agent read_as token",
    );

    // An unresolvable alias refuses the read outright instead of binding a
    // bare name.
    let ghost = read_as_page(&history, "agents/ghost", VISIBLE_JOB, None)
        .expect_err("an unregistered relayed agent must be refused");
    assert_eq!(ghost.kind(), "rejected", "{ghost:?}");

    // Delete and re-register the same alias: the durable uid changes while the
    // project and the job stay readable.
    {
        let db = rusqlite::Connection::open(history.state_dir().join("cadence.sqlite3")).unwrap();
        db.execute(
            "UPDATE agents SET state='stopped', endpoint=NULL WHERE alias='manager'",
            [],
        )
        .unwrap();
    }
    history
        .shared
        .store
        .remove_agent(
            "manager",
            true,
            &json!({"by": "operator", "by_kind": "operator"}),
        )
        .expect("the fixture agent must be removable");
    register(&history.shared, history.state_dir(), "manager");
    let new_uid = history.shared.store.agent("manager").unwrap().uid;
    assert_ne!(
        old_uid, new_uid,
        "re-registering the alias reused its durable uid"
    );

    // The old token is bound to the OLD registration, not the alias name.
    let replay = read_as_page(&history, "agents/manager", VISIBLE_JOB, Some(&token))
        .expect_err("a re-registered alias must not inherit an old cursor");
    assert_refusal(
        replay,
        "job_events_cursor_invalid",
        "re-registered alias cursor",
    );
    // A new read still works: the refusal above is the identity change, not a
    // lost project or job.
    let fresh = read_as_page(&history, "agents/manager", VISIBLE_JOB, None)
        .expect("the re-registered alias keeps its readable job");
    assert!(!fresh["events"].as_array().unwrap().is_empty());
    assert_eq!(
        fresh[cadence_core::board_api::READER_SCOPE_FIELD],
        json!(cadence_core::board_api::JOB_EVENTS_SCOPE_ACK)
    );
}

/// A real verified reader identity may be longer than 256 bytes and may
/// contain control bytes: an actor spelling is `users/<id>` with no
/// handle-length bound, and the codec hashes the exact bounded principal
/// bytes. A scoped page and its opaque resume must work for such an identity,
/// while a different long identity remains a different principal.
#[test]
fn long_project_readable_read_as_identity_pages_and_resumes() {
    let base = "a".repeat(250);
    let actor = format!("users/{base}\u{0}b");
    let other = format!("users/{base}\u{0}c");
    assert!(
        actor.len() > 256,
        "fixture identity is not over the 256-byte bound: {}",
        actor.len()
    );
    assert!(
        actor.contains('\u{0}'),
        "fixture identity carries no NUL byte: {actor:?}"
    );

    let keys = keys();
    let history = build_history(Some(&keys), 8, &[], 0);

    let first = read_as_page(&history, &actor, VISIBLE_JOB, None)
        .expect("a project-readable long read_as identity must receive a scoped page");
    assert_eq!(
        first[cadence_core::board_api::READER_SCOPE_FIELD],
        json!(cadence_core::board_api::JOB_EVENTS_SCOPE_ACK),
        "{first}"
    );
    assert_no_structural_seq(&first, "long read_as page");
    assert!(!first["events"].as_array().unwrap().is_empty(), "{first}");
    let token = first["cursor"]
        .as_str()
        .filter(|token| !token.is_empty())
        .expect("long read_as page has no forward cursor")
        .to_string();

    // The same long identity resumes from its opaque cursor.
    let resumed = read_as_page(&history, &actor, VISIBLE_JOB, Some(&token))
        .expect("opaque resume must work for the same long identity");
    assert!(
        resumed["events"].as_array().unwrap().is_empty(),
        "{resumed}"
    );
    assert_eq!(resumed["cursor"], json!(token), "{resumed}");

    // A different long identity is still a different principal.
    let crossed = read_as_page(&history, &other, VISIBLE_JOB, Some(&token))
        .expect_err("another long identity must not open this cursor");
    assert_refusal(
        crossed,
        "job_events_cursor_invalid",
        "cross-identity long read_as token",
    );

    // The principal bound is still hard: an identity over it refuses outright
    // with no page, never a partial one.
    let over = format!("users/{}", "z".repeat(8190));
    let refused = read_as_page(&history, &over, VISIBLE_JOB, None)
        .expect_err("an over-bound identity must be refused");
    assert_refusal(
        refused,
        "job_events_cursor_invalid",
        "over-bound read_as identity",
    );
}
