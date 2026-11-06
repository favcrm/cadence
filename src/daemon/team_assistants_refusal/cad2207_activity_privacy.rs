//! CAD-2207 — the OWNER-ONLY activity projection must never display a member's
//! private personal-assistant traffic.
//!
//! Independently authored acceptance, deliberately separate from the
//! implementation and from the shared `team_assistants_refusal` checks. This
//! module is a CHILD of that module only to reuse its already-real fixture
//! (`Fx`: a real in-process daemon, a real `operator/board-identity.json` trust
//! root and one loopback authoritative platform answering
//! `/__platform/assistant/revalidate`). The daemon, the store, the caller rule,
//! the live-authority pull and both public verbs are the production ones.
//!
//! WHAT IS EXERCISED (nothing mirrored, nothing simulated):
//!   * `member_assist_state` / `member_assist_send` through the REAL dispatcher,
//!     on a proven operator/board relay with a real signed
//!     `agenticos-assistant-actor/1` `member_envelope`, admitted only after the
//!     fixture platform's live authority pull. That is the production path that
//!     writes the private `queued` event
//!     (`alias:<private assistant alias>/message:<id>`) the operator's
//!     projection used to display.
//!   * `activity_query` and `activity_export` as the REAL local operator
//!     (`Asserted::Operator` → `operator_connection` in `activity_gate`), i.e.
//!     the owner-visible page, the same page the CLI renders.
//!   * ordinary daemon agent activity (`store.enqueue` on an unrelated
//!     registered worker — the same store call the daemon's send path makes) as
//!     the positive control, so a projection that lists nothing cannot pass.
//!
//! WHAT IS ASSERTED (the contract, not an implementation shape):
//!   * no private assistant alias, no private message id, no private body, no
//!     private conversation id and no member subject may appear anywhere in an
//!     operator page — `activity_query` or `activity_export`;
//!   * the private `queued` event's projected `event_id` and its exact `at`
//!     never appear, so the page leaks neither the row's identity nor its
//!     timing;
//!   * `row_count` always equals the rows actually returned, and a real private
//!     send does not move the page at all: the ordinary page is byte-identical
//!     before and after it (no phantom row, no count bump);
//!   * `activity_export` returns exactly the same page as `activity_query` for
//!     the same window, and its one written `activity_export_prepared` receipt
//!     is the only row it adds;
//!   * `message_queued`/`outcome=queued` filters, an exact ordinary `target`
//!     filter and `limit=1` cursor pagination still return exactly the visible
//!     ordinary rows, terminating with `complete:true`;
//!   * a filter naming the private alias/message yields no rows (or a refusal
//!     that does not echo the private facts), and a subject filter naming the
//!     member yields nothing;
//!   * an operator time probe around the private send's own instant returns
//!     nothing (or refuses without naming it);
//!   * after the binding is REMOVED at the platform and REVOKED locally, the
//!     durable provenance keeps the old rows hidden, and a brand-new ordinary
//!     message queued on the retired alias stays hidden too (fail closed on
//!     alias reuse), while a message on a different ordinary alias still
//!     reports.
//!
//! The private rows are asserted to EXIST first (`Store::message` +
//! `Store::events` on the bound alias — fixture ground truth, never a source
//! read), so "hidden" cannot be satisfied by a send that never happened.
//!
//! NOT exercised here (reported, never simulated): the `activity_*` HOSTED
//! route with a `public_session` (the member-role refusal for it lives in the
//! parent module), any app/PM source rows (a fresh fixture has none), and
//! alias reuse by a SECOND member's assistant — `assistant_bind` refuses an
//! alias already registered as an agent, and this fixture's private send
//! requires that agent row, so that reuse shape is not constructible through
//! the public surface available here.
//!
//! Expected red today: the pages contain the private
//! `alias:<alias>/message:<id>` row, so the fingerprint/equality/count
//! assertions fail. They pass only when the projection refuses to project a
//! member assistant's rows (including a retired/reused alias) from durable
//! provenance.
//!
//! Run just this module's acceptances:
//! `scripts/boat-build -- cargo test --locked --features test-seam --lib cad2207_activity -- --test-threads 1`

use super::*;

use std::collections::BTreeSet;

/// The member whose personal assistant is the private subject under test.
const PRIVATE_SUB: &str = "sub-ivy";
/// The message id the member's own send names — a caller-supplied id, so the
/// test can fingerprint exactly the row the daemon queued.
const PRIVATE_MSG: &str = "cad2207-private-send";
/// The private body. The projection carries no body at all, so finding it is a
/// leak.
const PRIVATE_TEXT: &str =
    "cad2207 private body: only this member's assistant conversation may hold it";
/// Ordinary daemon activity — the positive control.
const ORDINARY_ALIAS: &str = "ordinary-worker";
const ORDINARY_MSG: &str = "cad2207-ordinary-a";
const ORDINARY_MSG_2: &str = "cad2207-ordinary-b";
const ORDINARY_TEXT: &str = "cad2207 ordinary worker body";
/// A brand-new message queued on the RETIRED assistant alias.
const REUSED_MSG: &str = "cad2207-reused-b";
/// A far-future bound so both pages capture the same explicit interval and the
/// `from`/`until` in a next-page cursor round-trip exactly.
const WINDOW_UNTIL: f64 = 4_000_000_000.0;

/// The explicit capture window both verbs are asked for: everything this
/// fixture ever writes, so nothing is excluded by the default `until = now`.
fn window() -> Value {
    json!({"from": 0.0, "until": WINDOW_UNTIL})
}

/// One operator `activity_query`. Refuses loudly — an operator page over
/// ordinary-only content must never be refused.
fn query(fx: &Fx, params: Value) -> Value {
    fx.op("activity_query", params)
}

/// The rows of one page, checked to be the array the contract promises.
fn page_rows<'a>(page: &'a Value, label: &str) -> &'a Vec<Value> {
    page["events"]
        .as_array()
        .unwrap_or_else(|| panic!("{label}: the activity page has no events array: {page}"))
}

/// The `target` of every row, as a set — the projection's only alias/message
/// carrier for a queued core row.
fn targets_of(page: &Value, label: &str) -> BTreeSet<String> {
    page_rows(page, label)
        .iter()
        .map(|row| {
            row["target"]
                .as_str()
                .unwrap_or_else(|| panic!("{label}: an activity row has no target: {row}"))
                .to_string()
        })
        .collect()
}

/// The projection's own row/count invariant plus the privacy contract: no
/// private alias, private message id, private body, private conversation id or
/// member subject anywhere in the page.
fn assert_page_hides_private(page: &Value, private_alias: &str, private_thread: &str, label: &str) {
    let rows = page_rows(page, label);
    assert_eq!(
        page["row_count"].as_u64().map(|n| n as usize),
        Some(rows.len()),
        "{label}: activity row_count does not match the rows it returned: {page}"
    );
    let rendered = page.to_string();
    for secret in [
        private_alias,
        private_thread,
        PRIVATE_MSG,
        PRIVATE_TEXT,
        PRIVATE_SUB,
    ] {
        assert!(
            !rendered.contains(secret),
            "{label}: the operator activity page exposed '{secret}': {page}"
        );
    }
    for row in rows {
        let target = row["target"].as_str().unwrap_or_default();
        assert!(
            !target.contains(private_alias) && !target.contains(PRIVATE_MSG),
            "{label}: an activity row names the private assistant or its message: {row}"
        );
        assert!(
            row["at"].as_f64().is_some(),
            "{label}: an activity row carries no captured timestamp: {row}"
        );
    }
}

/// The exact private row's identity and timing are absent. `event_id` is the
/// projection's derived id (`event-<seq>`), and `at` is the store's own
/// timestamp — a page carrying either would leak the row's existence.
fn assert_page_hides_private_event(page: &Value, event_id: &str, at: f64, label: &str) {
    assert!(
        !page.to_string().contains(event_id),
        "{label}: the operator activity page carries the private event id '{event_id}': {page}"
    );
    for row in page_rows(page, label) {
        assert_ne!(
            row.get("at").and_then(Value::as_f64),
            Some(at),
            "{label}: an activity row carries the private event's exact timestamp: {row}"
        );
    }
}

/// A refusal is acceptable only when it is content-free: whatever the caller
/// guessed is theirs, but the answer must not name a private fact.
fn assert_refusal_hides_private(err: &crate::error::Error, secrets: &[&str], label: &str) {
    let text = err.to_string();
    for secret in secrets {
        assert!(
            !text.contains(secret),
            "{label}: a refusal named the private fact '{secret}': {text}"
        );
    }
}

/// Queue one ordinary daemon message on the unrelated worker alias — the same
/// store call the daemon's send path makes, and the visibility control.
fn queue_ordinary(fx: &Fx, id: &str, body: &str) {
    let (duplicate, state) = fx
        .shared
        .store
        .enqueue(ORDINARY_ALIAS, body, None, id, "test")
        .unwrap_or_else(|e| panic!("ordinary enqueue {id}: {e}"));
    assert!(!duplicate, "ordinary message {id} collided");
    assert_eq!(state, "queued", "ordinary message {id} was not queued");
}

/// The member's own bound assistant + one unrelated ordinary worker.
///
/// Deliberately NOT `Fx::member_runtime`: that fixture seeds a foreign message
/// and job whose ids embed the member subject, which would collide with the
/// subject fingerprint asserted below. Every step here is a production call —
/// `assistant_bind` through `provision`, the REAL `member_assist_state` (which
/// registers the assistant's confined runtime and its bound conversation), and
/// `register_agent` for the unrelated ordinary worker.
fn private_assistant(fx: &Fx) -> (String, String) {
    fx.provision(PRIVATE_SUB);
    let state = fx.op(
        "member_assist_state",
        json!({"member_envelope": fx.member_envelope(PRIVATE_SUB)}),
    );
    let alias = state["assistant"]["alias"]
        .as_str()
        .unwrap_or_else(|| panic!("member_assist_state bound no assistant alias: {state}"))
        .to_string();
    let conversation = state["conversation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("member_assist_state bound no conversation: {state}"))
        .to_string();
    let cwd = fx.dir.path().join("cad2207-ordinary-cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: ORDINARY_ALIAS,
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "workspace-write",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap_or_else(|e| panic!("register the ordinary worker: {e}"));
    (alias, conversation)
}

/// The real private send: `member_assist_send` on the proven board relay, with
/// a freshly minted signed member envelope and the caller-supplied message id.
/// The receipt must name the message and fabricate no reply.
fn send_as_member(fx: &Fx, id: &str, text: &str) {
    let receipt = fx.op(
        "member_assist_send",
        json!({
            "member_envelope": fx.member_envelope(PRIVATE_SUB),
            "text": text,
            "message": id,
        }),
    );
    assert!(
        receipt.to_string().contains(id),
        "the member send receipt does not name the message id it was given: {receipt}"
    );
    assert!(
        receipt["reply"].is_null() && receipt["result"].is_null(),
        "the member send receipt fabricated an assistant reply: {receipt}"
    );
}

/// Fixture ground truth: the private message row exists on the bound assistant
/// alias, and the store recorded the `queued` event the projection derives a
/// row from. Returns that event's projected id and its timestamp.
fn private_queued_row(fx: &Fx, alias: &str) -> (String, f64) {
    let message = fx
        .shared
        .store
        .message(PRIVATE_MSG)
        .unwrap_or_else(|e| panic!("private message read: {e}"))
        .unwrap_or_else(|| panic!("the member send never stored {PRIVATE_MSG}"));
    assert_eq!(
        message.alias, alias,
        "the private send landed on an unexpected alias"
    );
    let events = fx
        .shared
        .store
        .events(alias, 0, 200)
        .unwrap_or_else(|e| panic!("private events read: {e}"));
    let queued = events
        .iter()
        .find(|event| {
            event.kind == "queued"
                && event.payload.get("message").and_then(Value::as_str) == Some(PRIVATE_MSG)
        })
        .unwrap_or_else(|| {
            panic!("the member send wrote no queued event for {PRIVATE_MSG} on {alias}")
        });
    (format!("event-{:020}", queued.seq), queued.at)
}

/// The owner-only page is the OPERATOR's. A member's private assistant send —
/// its alias, message id, body, conversation, timing, and its row/count — must
/// not appear in `activity_query` or `activity_export`, while ordinary daemon
/// activity stays visible, filterable and cursor-pageable.
#[test]
fn cad2207_operator_activity_never_shows_a_member_private_assistant_send() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let (private_alias, private_thread) = private_assistant(&fx);

    // Ordinary activity exists BEFORE the private send, so "nothing changed"
    // is a real comparison and the positive control is established first.
    queue_ordinary(&fx, ORDINARY_MSG, ORDINARY_TEXT);
    queue_ordinary(&fx, ORDINARY_MSG_2, ORDINARY_TEXT);
    let ordinary_targets: BTreeSet<String> = [
        format!("alias:{ORDINARY_ALIAS}/message:{ORDINARY_MSG}"),
        format!("alias:{ORDINARY_ALIAS}/message:{ORDINARY_MSG_2}"),
    ]
    .into_iter()
    .collect();

    let before = query(&fx, window());
    assert_page_hides_private(&before, &private_alias, &private_thread, "pre-send");
    let before_targets = targets_of(&before, "pre-send");
    for want in &ordinary_targets {
        assert!(
            before_targets.contains(want),
            "the operator lost a visible ordinary row {want}: {before}"
        );
    }
    let before_count = before["row_count"]
        .as_u64()
        .unwrap_or_else(|| panic!("the activity page has no row_count: {before}"));

    // ---- the real private send, through the real dispatcher ----
    let send_from = crate::store::now_public();
    send_as_member(&fx, PRIVATE_MSG, PRIVATE_TEXT);
    let send_until = crate::store::now_public();
    // Ground truth: the private message row AND its queued event really exist.
    let (private_event_id, private_at) = private_queued_row(&fx, &private_alias);

    // ---- the same page after the send: byte-identical ----
    let after = query(&fx, window());
    assert_page_hides_private(&after, &private_alias, &private_thread, "post-send");
    assert_page_hides_private_event(&after, &private_event_id, private_at, "post-send");
    assert_eq!(
        after["events"], before["events"],
        "a member's private send changed the operator's activity page: before {before} / after {after}"
    );
    assert_eq!(
        after["row_count"], before["row_count"],
        "a member's private send changed the operator's activity row count"
    );

    // ---- activity_export returns the same page ----
    let exported = fx.op("activity_export", window());
    assert_page_hides_private(&exported, &private_alias, &private_thread, "export");
    assert_page_hides_private_event(&exported, &private_event_id, private_at, "export");
    assert_eq!(
        exported["events"], after["events"],
        "activity_export returned a different page than activity_query for the same window"
    );
    assert_eq!(exported["row_count"], after["row_count"]);

    // The export's own prepared receipt is the ONLY row it adds, and it names
    // no private fact.
    let after_export = query(&fx, window());
    assert_page_hides_private(
        &after_export,
        &private_alias,
        &private_thread,
        "post-export",
    );
    assert_page_hides_private_event(&after_export, &private_event_id, private_at, "post-export");
    let receipt_rows: Vec<Value> = page_rows(&after_export, "post-export")
        .iter()
        .filter(|row| row.get("action").and_then(Value::as_str) == Some("activity_export_prepared"))
        .cloned()
        .collect();
    assert_eq!(
        receipt_rows.len(),
        1,
        "activity_export did not record exactly one prepared receipt: {after_export}"
    );
    assert_eq!(
        receipt_rows[0]["target"].as_str(),
        Some("operation:activity_export")
    );
    assert_eq!(receipt_rows[0]["outcome"].as_str(), Some("prepared"));
    let rest: Vec<Value> = page_rows(&after_export, "post-export")
        .iter()
        .filter(|row| row.get("action").and_then(Value::as_str) != Some("activity_export_prepared"))
        .cloned()
        .collect();
    assert_eq!(
        Value::Array(rest),
        after["events"],
        "activity_export's prepared receipt displaced or replaced visible rows"
    );
    assert_eq!(
        after_export["row_count"].as_u64(),
        Some(before_count + 1),
        "the export receipt added a count other than its own single row"
    );

    // ---- filters still select exactly the visible ordinary rows ----
    let queued = query(
        &fx,
        json!({"from": 0.0, "until": WINDOW_UNTIL, "action": "message_queued"}),
    );
    assert_page_hides_private(&queued, &private_alias, &private_thread, "action filter");
    assert_page_hides_private_event(&queued, &private_event_id, private_at, "action filter");
    assert_eq!(
        targets_of(&queued, "action filter"),
        ordinary_targets,
        "the message_queued filter did not return exactly the visible ordinary rows: {queued}"
    );
    assert_eq!(
        queued["row_count"].as_u64(),
        Some(ordinary_targets.len() as u64)
    );

    let outcomes = query(
        &fx,
        json!({"from": 0.0, "until": WINDOW_UNTIL, "outcome": "queued"}),
    );
    assert_page_hides_private(&outcomes, &private_alias, &private_thread, "outcome filter");
    assert_eq!(
        targets_of(&outcomes, "outcome filter"),
        ordinary_targets,
        "the queued outcome filter did not return exactly the visible ordinary rows: {outcomes}"
    );

    let exact_target = format!("alias:{ORDINARY_ALIAS}/message:{ORDINARY_MSG}");
    let one = query(
        &fx,
        json!({"from": 0.0, "until": WINDOW_UNTIL, "target": exact_target}),
    );
    assert_page_hides_private(
        &one,
        &private_alias,
        &private_thread,
        "exact ordinary target",
    );
    assert_eq!(
        page_rows(&one, "exact ordinary target").len(),
        1,
        "an exact ordinary target filter did not return its one row: {one}"
    );
    assert_eq!(
        one["events"][0]["target"].as_str(),
        Some(exact_target.as_str())
    );

    // A caller who guesses the private alias/message gets no confirmation. An
    // empty page and a content-free refusal are both acceptable; a row naming
    // the private message is not.
    let private_target = format!("alias:{private_alias}/message:{PRIVATE_MSG}");
    match fx.op_err(
        "activity_query",
        json!({"from": 0.0, "until": WINDOW_UNTIL, "target": private_target}),
    ) {
        Ok(page) => {
            assert_page_hides_private(
                &page,
                &private_alias,
                &private_thread,
                "private target filter",
            );
            assert_page_hides_private_event(
                &page,
                &private_event_id,
                private_at,
                "private target filter",
            );
            assert_eq!(
                page["row_count"].as_u64(),
                Some(0),
                "a filter naming the private assistant returned rows: {page}"
            );
            assert!(
                page_rows(&page, "private target filter").is_empty(),
                "a filter naming the private assistant returned a row: {page}"
            );
        }
        Err(e) => assert_refusal_hides_private(
            &e,
            &[
                private_alias.as_str(),
                PRIVATE_MSG,
                PRIVATE_TEXT,
                PRIVATE_SUB,
                private_event_id.as_str(),
            ],
            "private target filter",
        ),
    }

    // A subject filter naming the member never surfaces a private row.
    let by_sub = query(
        &fx,
        json!({"from": 0.0, "until": WINDOW_UNTIL, "sub": PRIVATE_SUB}),
    );
    assert_page_hides_private(&by_sub, &private_alias, &private_thread, "subject filter");
    assert_eq!(
        by_sub["row_count"].as_u64(),
        Some(0),
        "a subject filter naming the member returned rows: {by_sub}"
    );

    // ---- cursor pagination pages exactly the visible rows ----
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut pages = 0usize;
    let mut params = json!({
        "from": 0.0, "until": WINDOW_UNTIL, "action": "message_queued", "limit": 1
    });
    loop {
        let page = query(&fx, params.clone());
        assert_page_hides_private(&page, &private_alias, &private_thread, "paged");
        assert_page_hides_private_event(&page, &private_event_id, private_at, "paged");
        let rows = page_rows(&page, "paged");
        assert!(
            rows.len() <= 1,
            "limit=1 returned more than one row: {page}"
        );
        for row in rows {
            seen.insert(row["target"].as_str().unwrap_or_default().to_string());
        }
        pages += 1;
        assert!(
            pages <= 8,
            "cursor pagination over two ordinary rows did not terminate"
        );
        match page["next_cursor"].as_str() {
            Some(cursor) => {
                assert_eq!(
                    page["complete"].as_bool(),
                    Some(false),
                    "a page with a next_cursor claimed to be complete: {page}"
                );
                params = json!({
                    "from": 0.0, "until": WINDOW_UNTIL, "action": "message_queued",
                    "limit": 1, "cursor": cursor
                });
            }
            None => {
                assert_eq!(
                    page["complete"].as_bool(),
                    Some(true),
                    "a final page did not report itself complete: {page}"
                );
                break;
            }
        }
    }
    assert_eq!(
        pages, 2,
        "cursor pagination did not visit both ordinary rows"
    );
    assert_eq!(
        seen, ordinary_targets,
        "cursor pagination did not return exactly the visible ordinary rows"
    );

    // ---- an operator time probe around the private send's own instant ----
    // The window provably contains the private queued event and no ordinary
    // one (the ordinary messages were queued strictly before `send_from`).
    match fx.op_err(
        "activity_query",
        json!({"from": send_from, "until": send_until}),
    ) {
        Ok(page) => {
            assert_page_hides_private(
                &page,
                &private_alias,
                &private_thread,
                "private time window",
            );
            assert_page_hides_private_event(
                &page,
                &private_event_id,
                private_at,
                "private time window",
            );
            assert_eq!(
                page["row_count"].as_u64(),
                Some(0),
                "an operator time probe around the private send returned rows: {page}"
            );
        }
        Err(e) => assert_refusal_hides_private(
            &e,
            &[
                private_alias.as_str(),
                PRIVATE_MSG,
                PRIVATE_TEXT,
                PRIVATE_SUB,
                private_event_id.as_str(),
            ],
            "private time window",
        ),
    }
}

/// Durable provenance: removing the binding at the platform and revoking it
/// locally keeps the OLD private rows hidden, and a brand-new ordinary message
/// queued on the RETIRED alias stays hidden too (fail closed) — while activity
/// on a different ordinary alias keeps reporting normally.
#[test]
fn cad2207_retired_and_reused_assistant_alias_stays_out_of_operator_activity() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let (private_alias, private_thread) = private_assistant(&fx);
    queue_ordinary(&fx, ORDINARY_MSG, ORDINARY_TEXT);
    send_as_member(&fx, PRIVATE_MSG, PRIVATE_TEXT);
    let (private_event_id, private_at) = private_queued_row(&fx, &private_alias);

    let before = query(&fx, window());
    assert_page_hides_private(&before, &private_alias, &private_thread, "before removal");
    assert_page_hides_private_event(&before, &private_event_id, private_at, "before removal");
    let ordinary_target = format!("alias:{ORDINARY_ALIAS}/message:{ORDINARY_MSG}");
    assert!(
        targets_of(&before, "before removal").contains(&ordinary_target),
        "the operator lost its visible ordinary row: {before}"
    );
    let baseline = before["row_count"]
        .as_u64()
        .unwrap_or_else(|| panic!("the activity page has no row_count: {before}"));

    // ---- the member is removed: platform row gone, local binding revoked ----
    // The binding row itself stays for history (`assistant_revoke` only flips
    // its state), which is exactly the durable provenance the page must read.
    fx.remove_member(PRIVATE_SUB);
    fx.revoke(PRIVATE_SUB);
    let revoked = query(&fx, window());
    assert_page_hides_private(&revoked, &private_alias, &private_thread, "after removal");
    assert_page_hides_private_event(&revoked, &private_event_id, private_at, "after removal");
    assert_eq!(
        revoked["events"], before["events"],
        "retiring the assistant binding changed the operator page"
    );
    assert_eq!(revoked["row_count"], before["row_count"]);

    // ---- alias REUSE: brand-new ordinary traffic on the retired alias ----
    let (duplicate, state) = fx
        .shared
        .store
        .enqueue(
            &private_alias,
            "cad2207 reused alias body",
            None,
            REUSED_MSG,
            "test",
        )
        .unwrap_or_else(|e| panic!("reused-alias enqueue: {e}"));
    assert!(!duplicate, "the reused-alias message id collided");
    assert_eq!(state, "queued");
    let stored = fx
        .shared
        .store
        .message(REUSED_MSG)
        .unwrap_or_else(|e| panic!("reused-alias message read: {e}"))
        .unwrap_or_else(|| panic!("the reused-alias message was not stored"));
    assert_eq!(
        stored.alias, private_alias,
        "the reused-alias message did not land on the retired assistant alias"
    );

    let reused = query(&fx, window());
    assert_page_hides_private(
        &reused,
        &private_alias,
        &private_thread,
        "after alias reuse",
    );
    assert_page_hides_private_event(&reused, &private_event_id, private_at, "after alias reuse");
    assert!(
        !reused.to_string().contains(REUSED_MSG),
        "a retired assistant alias leaked newly queued activity: {reused}"
    );
    assert_eq!(
        reused["row_count"].as_u64(),
        Some(baseline),
        "reused activity on a retired assistant alias moved the operator row count"
    );
    assert_eq!(
        reused["events"], before["events"],
        "reused activity on a retired assistant alias changed the operator page"
    );

    // ---- positive control: ordinary activity still reports ----
    queue_ordinary(&fx, "cad2207-ordinary-after", "cad2207 ordinary body after");
    let after = query(&fx, window());
    assert_page_hides_private(
        &after,
        &private_alias,
        &private_thread,
        "after ordinary control",
    );
    assert_page_hides_private_event(
        &after,
        &private_event_id,
        private_at,
        "after ordinary control",
    );
    assert!(
        targets_of(&after, "after ordinary control").contains(&format!(
            "alias:{ORDINARY_ALIAS}/message:cad2207-ordinary-after"
        )),
        "an ordinary message stopped being visible after a private alias was retired: {after}"
    );
    assert_eq!(
        after["row_count"].as_u64(),
        Some(baseline + 1),
        "an ordinary message no longer counted exactly once: {after}"
    );

    // ---- and the export agrees with the query in the retired state ----
    let exported = fx.op("activity_export", window());
    assert_page_hides_private(
        &exported,
        &private_alias,
        &private_thread,
        "export after reuse",
    );
    assert_page_hides_private_event(
        &exported,
        &private_event_id,
        private_at,
        "export after reuse",
    );
    assert_eq!(
        exported["events"], after["events"],
        "activity_export disagreed with activity_query after the binding was retired"
    );
    assert_eq!(exported["row_count"], after["row_count"]);
}
