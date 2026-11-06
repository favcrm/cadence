//! CAD-2207 — the OPERATOR's cross-agent work conversation view must not
//! enumerate, name, preview or serve a member's bound personal assistant.
//!
//! Independently authored negative acceptance, deliberately separate from the
//! implementation and from the shared `team_assistants_refusal` checks. This
//! module is a CHILD of that module only to reuse its already-real fixture
//! (`Fx`: a real in-process daemon, a real `operator/board-identity.json` trust
//! root, one loopback authoritative platform answering
//! `/__platform/assistant/revalidate`, and the real `Store`). Every private
//! row here is a REAL stored row: the mail and system rows go through the
//! store's own writers (`enqueue_sent` with `Sender::Agent`, `enqueue`
//! unattributed, `enqueue_daemon`), and the mixed group's private membership is
//! a `topic_subscriptions` row seeded with the test-only `fixture_write`
//! because `Store::topic_subscribe` now refuses a bound assistant inside its
//! write transaction — it models a subscription that already existed. Every
//! assertion is made against the production `Shared::dispatch` path for
//! `work_conversation_list` / `work_conversation_read` — never a mirrored
//! filter.
//!
//! SCOPE, stated narrowly so it cannot be over-read. `work_conversation_list`
//! is operator-only (`operator_chat` + `caller_rule.rs`) and answers pair
//! rows, the operator's own home-thread rows (`kind: "operator"`), group rows
//! (topic subscribers) and the unknown/system bucket. The ticket's guarantee
//! is that a signed-in member's personal assistant — its existence, alias,
//! profile, conversations, previews, messages, result, error, metadata, counts
//! and IDs — is visible only to its authenticated owner, and that operator
//! authority is not member proof. This module therefore requires that:
//!
//!   * no private alias, uid, profile label, bound-thread id, pair id, preview
//!     or body appears anywhere in the operator's list (both
//!     `include_system: false` and `true`);
//!   * the private pair's DERIVED conversation id (`conv-<digest>`, built from
//!     the two stored uids — the exact id a guesser computes) is refused by
//!     `work_conversation_read`; an empty-but-successful page would still
//!     disclose existence, so a refusal is required;
//!   * the private bound thread id is never a readable work conversation;
//!   * the unknown bucket neither counts nor pages the private assistant's
//!     unattributed mail or its daemon system notices;
//!   * and the ORDINARY work conversations survive unchanged: the pair between
//!     two ordinary agents (with its paging), the operator's home-thread row
//!     for an ordinary agent, an ordinary-only group, the ordinary
//!     unattributed rows and valid system notices to ordinary agents. Each
//!     negative is therefore paired with a positive control in the same
//!     fixture, so a blanket "refuse/hide everything" cannot pass, and a
//!     refusal is never confused with content that was never there.
//!
//! The owner's SIGNED route is the positive control that the private data
//! really exists: `member_assist_state` returns the private profile name and
//! the bound thread, `member_assist_history` returns that thread, and the
//! store preconditions below assert the private pair, unknown rows, group
//! subscription and home-thread row are all stored. Only the operator's
//! view must not serve them.
//!
//! DURABLE PROVENANCE. The second check removes the assistant's `agents` row
//! while the `assistants` binding, the messages (whose `sender`/`recipient`
//! labels are the durable uid→alias record), the thread row and the topic
//! subscriptions survive — the shape a restored/older store or an out-of-band
//! deletion leaves, which the contract names explicitly. Provenance there is
//! the stored label joined to the binding alias; the same rows must still be
//! withheld, including after the labels are additionally cleared, which is the
//! genuinely ambiguous case the contract requires to FAIL CLOSED.
//!
//! EXPECTED RED TODAY (recorded so the result is not mistaken for coverage):
//! `rpc_work_conversation_list` / `rpc_work_conversation_read` filter pairs by
//! `Store::work_conversation_pairs()` and threads/groups/unknown rows with no
//! member-assistant predicate at all (`src/daemon/conversations_rpc.rs`), so
//! today the private pair row, the private home-thread row, the mixed group's
//! participant and the private unknown rows are all served to the operator.
//! Both checks below fail on those assertions and pass only when the operator
//! surfaces withhold the bound assistant.
//!
//! NOT COVERED HERE (reported, never simulated): (a) a SYSTEM-source message
//! between two agents, i.e. a `sys-` row that carries a sender uid — the
//! store's only daemon writer (`enqueue_daemon`) stores an unattributed row
//! and `caller_message` refuses a system source on every other enqueue, so no
//! real fixture can produce that row shape; (b) the production
//! `Store::remove_agent` path, which additionally prunes the alias's events,
//! detaches the home thread (`alias=NULL`) and force-cancels open messages —
//! the durable-provenance check seeds the surviving state with
//! `fixture_write` instead of reproducing that whole cascade, and asserts only
//! what remains after an `agents` row is gone; (c) an alias RE-REGISTERED
//! under a new uid while the old binding exists.

use super::*;

/// A second ordinary worker, so the control pair is between two agents that
/// are both provably ordinary.
const SECOND_WORKER: &str = "cad2207-second-worker";
/// Ordinary-only group: both subscribers must stay visible.
const CONTROL_TOPIC_SOURCE: &str = "cad2207-ordinary-group";
/// Mixed group: one ordinary subscriber plus the private assistant.
const MIXED_TOPIC_SOURCE: &str = "cad2207-mixed-group";
/// The operator view's single legacy/system bucket row id — the production
/// `UNKNOWN_CONVERSATION` constant in `conversations_rpc.rs`, not a helper
/// alias. The private assistant's unattributed and `sys-` rows live here.
const UNKNOWN_WORK_CONVERSATION: &str = "conv-legacy-unknown";

const PRIVATE_PAIR_BODY: &str =
    "cad2207 private pair body: ordinary worker to the member assistant";
const PRIVATE_LEGACY_BODY: &str =
    "cad2207 private legacy body: unattributed mail to the member assistant";
const PRIVATE_SYS_BODY: &str = "cad2207 private system notice body for the member assistant";
const ORDINARY_PAIR_FIRST: &str = "cad2207 ordinary pair body one";
const ORDINARY_PAIR_SECOND: &str = "cad2207 ordinary pair body two";
const ORDINARY_LEGACY_BODY: &str = "cad2207 ordinary legacy body";
const ORDINARY_SYS_BODY: &str = "cad2207 ordinary system notice body";

/// The real stored state one fixture seeds, plus the ids an operator could
/// derive from it. Every id here is read back from the store, never invented.
struct Cad2207Work {
    private_alias: String,
    private_uid: String,
    private_thread: String,
    /// The DERIVED id of the pair conversation between the ordinary worker and
    /// the private assistant — exactly what `work_conversation_pairs` returns.
    private_pair: String,
    ordinary_pair: String,
    /// Unattributed rows the operator MAY still see (recipient not private).
    ordinary_unknown_all: BTreeSet<String>,
    ordinary_unknown_nonsystem: BTreeSet<String>,
    control_topic: String,
    mixed_topic: String,
}

impl Cad2207Work {
    /// Every string that must never appear on the operator's surface.
    fn secrets(&self) -> Vec<&str> {
        vec![
            self.private_alias.as_str(),
            self.private_uid.as_str(),
            self.private_thread.as_str(),
            self.private_pair.as_str(),
            PRIVATE_PAIR_BODY,
            PRIVATE_LEGACY_BODY,
            PRIVATE_SYS_BODY,
            CAD2207_PRIVATE_NAME,
        ]
    }
}

fn cad2207_assert_absent(what: &str, text: &str, secrets: &[&str]) {
    for secret in secrets {
        assert!(
            !text.contains(*secret),
            "{what} exposed member-private {secret:?}: {text}"
        );
    }
}

fn cad2207_operator_list(fx: &Fx, include_system: bool) -> Value {
    fx.call(
        Asserted::Operator,
        "work_conversation_list",
        json!({"include_system": include_system}),
    )
    .unwrap_or_else(|e| {
        panic!("operator work_conversation_list (include_system={include_system}) failed: {e}")
    })
}

fn cad2207_operator_read(fx: &Fx, params: Value) -> Result<Value> {
    fx.call(Asserted::Operator, "work_conversation_read", params)
}

fn cad2207_row<'a>(listed: &'a Value, id: &str) -> Option<&'a Value> {
    listed["conversations"]
        .as_array()?
        .iter()
        .find(|row| row["id"].as_str() == Some(id))
}

fn cad2207_message_ids(read: &Value) -> BTreeSet<String> {
    read["messages"]
        .as_array()
        .expect("a work conversation read carries no messages array")
        .iter()
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect()
}

/// Seed the real work surface for one provisioned member:
///
/// * the private assistant's cross-agent pair mail (ordinary worker → the
///   bound assistant), its unattributed legacy mail and a daemon system
///   notice — three stored rows the operator view must withhold;
/// * the same three shapes for ORDINARY agents — the positive controls;
/// * a control pair between two ordinary agents with two messages (so paging
///   is exercised) and their home threads;
/// * an ordinary-only group, and a mixed group whose private membership is
///   seeded as a pre-existing row (`Store::topic_subscribe` now refuses a bound
///   private assistant in its own transaction; the operator view must still
///   not expose the legacy row).
///
/// Ids (pair conversations, uids) are read back from the store, so the
/// "guessed conversation id" below is the one a real attacker derives.
fn cad2207_seed_work_surface(fx: &Fx, rt: &MemberRuntime) -> Cad2207Work {
    let store = &fx.shared.store;
    let private_alias = rt.alias.clone();
    let private_thread = rt.thread_id.clone();
    let private_uid = store
        .agent(&private_alias)
        .unwrap_or_else(|e| panic!("the bound assistant has no agent row: {e}"))
        .uid;
    let ordinary_uid = store.agent("ordinary-worker").unwrap().uid;

    // The second ordinary worker and its home thread (an operator row that
    // must survive).
    let second_cwd = fx.dir.path().join("cad2207-second-cwd");
    std::fs::create_dir_all(&second_cwd).unwrap();
    if store.agent_opt(SECOND_WORKER).unwrap().is_none() {
        store
            .register_agent(&crate::store::NewAgent {
                alias: SECOND_WORKER,
                provider: "pi",
                endpoint_kind: "managed",
                role: "worker",
                cwd: second_cwd.to_str().unwrap(),
                sandbox: "workspace-write",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap_or_else(|e| panic!("register the second ordinary worker: {e}"));
    }
    let second_uid = store.agent(SECOND_WORKER).unwrap().uid;
    let _ = store.ensure_thread(SECOND_WORKER).unwrap();

    // ---- private rows (real writers, real rows) ----
    let (dup, _) = store
        .enqueue_sent(
            &private_alias,
            PRIVATE_PAIR_BODY,
            None,
            "cad2207-private-pair-mail",
            "send",
            None,
            &crate::store::Sender::Agent("ordinary-worker".to_string()),
        )
        .unwrap_or_else(|e| panic!("seed the private pair mail: {e}"));
    assert!(!dup, "the private pair mail id collided");
    store
        .enqueue(
            &private_alias,
            PRIVATE_LEGACY_BODY,
            None,
            "cad2207-private-legacy-mail",
            "send",
        )
        .unwrap_or_else(|e| panic!("seed the private unattributed mail: {e}"));
    store
        .enqueue_daemon(
            &private_alias,
            PRIVATE_SYS_BODY,
            "sys-wake-cad2207-private",
            "wake",
        )
        .unwrap_or_else(|e| panic!("seed the private daemon notice: {e}"));

    // ---- ordinary controls: the same three shapes, both provable ----
    store
        .enqueue_sent(
            SECOND_WORKER,
            ORDINARY_PAIR_FIRST,
            None,
            "cad2207-ordinary-pair-1",
            "send",
            None,
            &crate::store::Sender::Agent("ordinary-worker".to_string()),
        )
        .unwrap_or_else(|e| panic!("seed the ordinary pair's first mail: {e}"));
    store
        .enqueue_sent(
            "ordinary-worker",
            ORDINARY_PAIR_SECOND,
            None,
            "cad2207-ordinary-pair-2",
            "send",
            None,
            &crate::store::Sender::Agent(SECOND_WORKER.to_string()),
        )
        .unwrap_or_else(|e| panic!("seed the ordinary pair's second mail: {e}"));
    store
        .enqueue(
            "ordinary-worker",
            ORDINARY_LEGACY_BODY,
            None,
            "cad2207-ordinary-legacy-mail",
            "send",
        )
        .unwrap_or_else(|e| panic!("seed the ordinary unattributed mail: {e}"));
    store
        .enqueue_daemon(
            "ordinary-worker",
            ORDINARY_SYS_BODY,
            "sys-wake-cad2207-ordinary",
            "wake",
        )
        .unwrap_or_else(|e| panic!("seed the ordinary daemon notice: {e}"));

    // ---- groups: one ordinary-only, one that also subscribes the private
    // assistant (the store can hold this; the view must not expose it) ----
    let (control, _) = store.topic_ensure("project", CONTROL_TOPIC_SOURCE).unwrap();
    store.topic_subscribe(&control.id, &ordinary_uid).unwrap();
    store.topic_subscribe(&control.id, &second_uid).unwrap();
    let (mixed, _) = store.topic_ensure("project", MIXED_TOPIC_SOURCE).unwrap();
    store.topic_subscribe(&mixed.id, &ordinary_uid).unwrap();
    // CAD-2207: `Store::topic_subscribe` now refuses a bound private assistant
    // inside its own write transaction, so the private membership is seeded as
    // a raw fixture row: it models a subscription that ALREADY existed (a
    // legacy/pre-guard store), which the operator view must still not expose.
    // The ordinary subscription above goes through the real guarded writer, and
    // every assertion below is unchanged.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO topic_subscriptions(topic_id,uid,added) VALUES(?,?,?)",
                rusqlite::params![
                    mixed.id,
                    private_uid,
                    crate::issue::time::now_epoch() as f64
                ],
            )?;
            Ok(())
        })
        .unwrap();

    // ---- read back the derived ids ----
    let pairs = store.work_conversation_pairs().unwrap();
    let private_pair = pairs
        .iter()
        .find(|p| p.a_uid == private_uid || p.b_uid == private_uid)
        .map(|p| p.id.clone())
        .unwrap_or_else(|| panic!("the store holds no pair for the bound assistant"));
    let ordinary_pair = pairs
        .iter()
        .find(|p| {
            (p.a_uid == ordinary_uid && p.b_uid == second_uid)
                || (p.a_uid == second_uid && p.b_uid == ordinary_uid)
        })
        .map(|p| p.id.clone())
        .unwrap_or_else(|| panic!("the store holds no pair for the two ordinary workers"));
    assert_ne!(
        private_pair, ordinary_pair,
        "the private and ordinary pairs resolved to one conversation id"
    );

    // The unknown bucket as the store holds it, split by the requirement: any
    // unattributed row addressed to the private assistant is private, every
    // other unattributed row must remain. `work_unknown_messages` with
    // `include_system: true` is the widest real read of that bucket.
    let all_unknown = store
        .work_unknown_messages(
            &crate::store::conversations::Scope {
                include_system: true,
                ..Default::default()
            },
            None,
            None,
            50,
        )
        .unwrap();
    let mut private_unknown = BTreeSet::new();
    let mut ordinary_unknown_all = BTreeSet::new();
    let mut ordinary_unknown_nonsystem = BTreeSet::new();
    for m in &all_unknown {
        if m.recipient_uid.as_deref() == Some(private_uid.as_str()) {
            private_unknown.insert(m.id.clone());
        } else {
            ordinary_unknown_all.insert(m.id.clone());
            if !crate::store::conversations::is_system(m) {
                ordinary_unknown_nonsystem.insert(m.id.clone());
            }
        }
    }
    // Preconditions: the private rows really exist in these buckets, and the
    // ordinary controls really have their own rows. A fixture that silently
    // failed to seed would otherwise make every "absent" assertion vacuous.
    for id in ["cad2207-private-legacy-mail", "sys-wake-cad2207-private"] {
        assert!(
            private_unknown.contains(id),
            "the fixture did not store private unknown row {id}: {private_unknown:?}"
        );
    }
    for id in ["cad2207-ordinary-legacy-mail", "sys-wake-cad2207-ordinary"] {
        assert!(
            ordinary_unknown_all.contains(id),
            "the fixture did not store ordinary unknown row {id}: {ordinary_unknown_all:?}"
        );
    }
    assert!(
        ordinary_unknown_nonsystem.contains("cad2207-ordinary-legacy-mail"),
        "the ordinary unattributed row is not classified as non-system"
    );

    Cad2207Work {
        private_alias,
        private_uid,
        private_thread,
        private_pair,
        ordinary_pair,
        ordinary_unknown_all,
        ordinary_unknown_nonsystem,
        control_topic: control.id,
        mixed_topic: mixed.id,
    }
}

/// CAD-2207: an operator — the one caller allowed on `work_conversation_list`
/// / `work_conversation_read` — sees the ordinary cross-agent work view and
/// NOTHING of a member's bound personal assistant. The owner's signed route
/// proves the data is there and readable; the private pair's derived
/// conversation id is refused rather than answered with an empty page, so
/// existence is not disclosed either.
#[test]
fn cad2207_operator_work_conversation_view_never_exposes_a_bound_member_assistant() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let rt = fx.member_runtime("sub-alice");
    seed_cad2207_private_profile(&fx, "sub-alice");
    let work = cad2207_seed_work_surface(&fx, &rt);

    // ---- positive control: the OWNER's signed route still holds the private
    // profile and the bound conversation. The content exists; only the
    // operator's cross-agent view must not serve it. ----
    let own = fx.op(
        "member_assist_state",
        json!({"member_envelope": fx.member_envelope("sub-alice")}),
    );
    assert_eq!(
        own["assistant"]["alias"].as_str(),
        Some(work.private_alias.as_str()),
        "the owner's signed envelope resolved no assistant: {own}"
    );
    assert_eq!(
        own["assistant"]["identity"]["name"].as_str(),
        Some(CAD2207_PRIVATE_NAME),
        "the owner could not read its own private profile name: {own}"
    );
    assert_eq!(
        own["conversation_id"].as_str(),
        Some(work.private_thread.as_str()),
        "the owner's profile read did not name its bound conversation"
    );
    let own_history = fx.op(
        "member_assist_history",
        json!({"member_envelope": fx.member_envelope("sub-alice")}),
    );
    assert_eq!(
        own_history["conversation"]["id"].as_str(),
        Some(work.private_thread.as_str()),
        "the owner could not read its own bound conversation: {own_history}"
    );

    // ---- preconditions: the private rows really are stored where the
    // operator's view would find them ----
    let store = &fx.shared.store;
    let private_pair_row = store
        .message("cad2207-private-pair-mail")
        .unwrap()
        .expect("the private pair mail is not stored");
    assert_eq!(
        private_pair_row.body, PRIVATE_PAIR_BODY,
        "the stored private body is not the one seeded"
    );
    assert!(
        store
            .home_threads()
            .unwrap()
            .iter()
            .any(|t| t.id == work.private_thread),
        "the private bound home thread is not in the operator's thread listing"
    );
    assert!(
        store
            .topic_subscribers(&work.mixed_topic)
            .unwrap()
            .contains(&work.private_uid),
        "the mixed group does not subscribe the private assistant"
    );

    let before = fx.tree();

    for include_system in [false, true] {
        let listed = cad2207_operator_list(&fx, include_system);
        let text = listed.to_string();
        cad2207_assert_absent(
            &format!("operator work_conversation_list (include_system={include_system})"),
            &text,
            &work.secrets(),
        );
        let rows = listed["conversations"]
            .as_array()
            .expect("work_conversation_list carries no conversations array");
        assert!(
            !rows
                .iter()
                .any(|r| r["id"].as_str() == Some(work.private_thread.as_str())),
            "the operator list carried the private bound thread id: {text}"
        );
        assert!(
            !rows
                .iter()
                .any(|r| r["id"].as_str() == Some(work.private_pair.as_str())),
            "the operator list carried the private pair conversation row: {text}"
        );

        // ---- ordinary work conversations survive ----
        let ordinary_row = cad2207_row(&listed, &work.ordinary_pair)
            .unwrap_or_else(|| panic!("the operator lost the ordinary pair conversation: {text}"));
        let ordinary_text = ordinary_row.to_string();
        assert!(
            ordinary_text.contains("ordinary-worker") && ordinary_text.contains(SECOND_WORKER),
            "the ordinary pair row no longer names both ordinary participants: {ordinary_text}"
        );
        assert!(
            rows.iter().any(|r| {
                r["kind"] == "operator"
                    && r["participants"][1]["alias"].as_str() == Some("ordinary-worker")
            }),
            "the operator lost its own home-thread row for the ordinary worker: {text}"
        );
        let control = cad2207_row(&listed, &work.control_topic)
            .unwrap_or_else(|| panic!("the operator lost the ordinary-only group: {text}"));
        assert_eq!(
            control["members"].as_i64(),
            Some(2),
            "the ordinary-only group no longer lists both provable members: {control}"
        );

        // A mixed group may be withheld or listed with only the provable
        // members; what it may never do is name or count the private one.
        if let Some(mixed) = cad2207_row(&listed, &work.mixed_topic) {
            assert_eq!(
                mixed["members"].as_i64(),
                Some(1),
                "the mixed group still counts the private assistant: {mixed}"
            );
            let mixed_text = mixed.to_string();
            assert!(
                !mixed_text.contains(&work.private_uid)
                    && !mixed_text.contains(&work.private_alias),
                "the mixed group exposed the private participant: {mixed_text}"
            );
        }

        // ---- the unknown/system bucket: ordinary rows only, exactly ----
        let expected: &BTreeSet<String> = if include_system {
            &work.ordinary_unknown_all
        } else {
            &work.ordinary_unknown_nonsystem
        };
        let unknown = cad2207_row(&listed, UNKNOWN_WORK_CONVERSATION).unwrap_or_else(|| {
            panic!("the operator lost the unknown bucket that holds ordinary rows: {text}")
        });
        assert_eq!(
            unknown["counts"]["unknown"].as_i64(),
            Some(expected.len() as i64),
            "the unknown bucket count does not match the ordinary rows it may \
             count (private rows must not be counted): {unknown}"
        );

        let read = cad2207_operator_read(
            &fx,
            json!({"conversation": UNKNOWN_WORK_CONVERSATION, "include_system": include_system, "limit": 50}),
        )
        .unwrap_or_else(|e| {
            panic!("operator work_conversation_read unknown (include_system={include_system}) failed: {e}")
        });
        cad2207_assert_absent(
            &format!("operator unknown-bucket read (include_system={include_system})"),
            &read.to_string(),
            &work.secrets(),
        );
        assert_eq!(
            &cad2207_message_ids(&read),
            expected,
            "the unknown bucket read is not exactly the ordinary rows: {read}"
        );
    }

    // ---- a guessed private conversation id is refused, not answered empty:
    // an empty page would still disclose that the pair exists ----
    let guessed = cad2207_operator_read(&fx, json!({"conversation": work.private_pair}));
    match &guessed {
        Err(e) => {
            let text = e.to_string();
            cad2207_assert_absent("the private pair refusal", &text, &work.secrets());
            assert!(
                !text.contains("cad2207-private"),
                "the refusal echoed a private row id: {text}"
            );
        }
        Ok(page) => {
            panic!("the operator read the private pair conversation by its derived id: {page}")
        }
    }
    // The bound conversation id is not a work pair either; it must not resolve.
    let thread_read = cad2207_operator_read(&fx, json!({"conversation": work.private_thread}));
    assert!(
        thread_read.is_err(),
        "the operator read the private bound thread as a work conversation: {thread_read:?}"
    );

    // ---- positive control: the ordinary pair reads and pages ----
    let page = fx.op(
        "work_conversation_read",
        json!({"conversation": work.ordinary_pair, "limit": 1}),
    );
    assert_eq!(
        page["messages"]
            .as_array()
            .expect("the ordinary read carries no messages")
            .len(),
        1,
        "the ordinary pair ignored the requested page size: {page}"
    );
    assert_eq!(
        page["more_before"].as_bool(),
        Some(true),
        "the ordinary pair did not report an older page: {page}"
    );
    assert!(
        page.to_string().contains(ORDINARY_PAIR_SECOND),
        "the newest ordinary body is missing from its page: {page}"
    );
    let cursor = page["cursor"].as_i64().expect("the page carried no cursor");
    let earlier = fx.op(
        "work_conversation_read",
        json!({"conversation": work.ordinary_pair, "limit": 1, "before": cursor}),
    );
    assert!(
        earlier.to_string().contains(ORDINARY_PAIR_FIRST),
        "the ordinary pair's older page is missing its body: {earlier}"
    );

    assert_eq!(
        fx.tree(),
        before,
        "a refused private read or a served ordinary read changed the tracker"
    );
}

/// CAD-2207 durable provenance: the assistant's `agents` row is gone while the
/// `assistants` binding, the message rows (with their durable `sender` /
/// `recipient` labels), the thread row and the topic subscriptions remain.
/// The operator view must still withhold those rows — a removed alias is not a
/// licence to serve them — and when the labels are ALSO cleared, leaving
/// genuinely ambiguous provenance, it must fail closed instead of attributing
/// the rows to nobody and serving their bodies.
#[test]
fn cad2207_work_conversation_view_keeps_provenance_after_the_agent_row_is_gone() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let rt = fx.member_runtime("sub-bob");
    let work = cad2207_seed_work_surface(&fx, &rt);

    // ---- the shape under test: only the agents row is gone. The production
    // `remove_agent` additionally prunes events, detaches the home thread and
    // force-cancels open messages; those are reported as a fixture gap and are
    // NOT simulated here. What matters for provenance is that the binding and
    // the stored labels survive, which this write leaves intact. ----
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "DELETE FROM agents WHERE alias=?",
                rusqlite::params![work.private_alias],
            )?;
            Ok(())
        })
        .unwrap();

    let store = &fx.shared.store;
    assert!(
        store.agent_opt(&work.private_alias).unwrap().is_none(),
        "the private alias still has an agents row — the removal did not happen"
    );
    assert!(
        store
            .assistant_for_alias(&work.private_alias)
            .unwrap()
            .is_some(),
        "the durable assistant binding is gone — the fixture no longer proves \
         provenance and the check below would be vacuous"
    );
    assert!(
        store
            .work_conversation_pairs()
            .unwrap()
            .iter()
            .any(|p| p.id == work.private_pair),
        "the private pair vanished with the agents row"
    );
    // The store's own projection STILL resolves the alias from the durable row
    // label. That is the leak this check exists for: the content is reachable
    // in the store, so an "absent" answer below cannot be vacuity.
    let participant = store.participant(&work.private_uid).unwrap();
    assert_eq!(
        participant["alias"].as_str(),
        Some(work.private_alias.as_str()),
        "the stored row label no longer resolves the alias — nothing is left to leak"
    );
    assert_eq!(
        participant["removed"],
        json!(true),
        "the removed agent is not reported as removed: {participant}"
    );

    let listed = cad2207_operator_list(&fx, true);
    let text = listed.to_string();
    cad2207_assert_absent(
        "operator work_conversation_list after the agents row was removed",
        &text,
        &work.secrets(),
    );
    assert!(
        cad2207_row(&listed, &work.private_pair).is_none(),
        "the operator list served the removed assistant's pair row: {text}"
    );
    assert!(
        cad2207_row(&listed, &work.private_thread).is_none(),
        "the operator list served the removed assistant's home-thread row: {text}"
    );
    // Ordinary rows still resolve with no live private alias in the picture.
    assert!(
        cad2207_row(&listed, &work.ordinary_pair).is_some(),
        "the operator lost the ordinary pair after an unrelated row was removed: {text}"
    );
    assert!(
        cad2207_row(&listed, &work.control_topic).is_some(),
        "the operator lost the ordinary-only group: {text}"
    );

    let guessed = cad2207_operator_read(&fx, json!({"conversation": work.private_pair}));
    match &guessed {
        Err(e) => cad2207_assert_absent(
            "the removed assistant's pair refusal",
            &e.to_string(),
            &work.secrets(),
        ),
        Ok(page) => {
            panic!("the operator read the removed assistant's pair by its derived id: {page}")
        }
    }
    let unknown = cad2207_operator_read(
        &fx,
        json!({"conversation": UNKNOWN_WORK_CONVERSATION, "include_system": true, "limit": 50}),
    )
    .expect("the ordinary unknown bucket is still readable");
    cad2207_assert_absent(
        "the unknown bucket after the agents row was removed",
        &unknown.to_string(),
        &work.secrets(),
    );
    assert_eq!(
        &cad2207_message_ids(&unknown),
        &work.ordinary_unknown_all,
        "the unknown bucket after removal is not exactly the ordinary rows: {unknown}"
    );

    // ---- genuinely ambiguous: the durable labels are gone too, so nothing in
    // the store can prove which alias the uid belonged to. The contract says
    // fail closed, which here means withholding the rows and their bodies
    // rather than printing them with an invented `removed` participant. ----
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE messages SET recipient=NULL WHERE recipient_uid=?",
                rusqlite::params![work.private_uid],
            )?;
            Ok(())
        })
        .unwrap();
    let ambiguous = store.participant(&work.private_uid).unwrap();
    assert_eq!(
        ambiguous["alias"],
        Value::Null,
        "the fixture's label clear did not take effect: {ambiguous}"
    );

    let listed = cad2207_operator_list(&fx, true);
    let text = listed.to_string();
    cad2207_assert_absent(
        "operator work_conversation_list with ambiguous provenance",
        &text,
        &work.secrets(),
    );
    assert!(
        cad2207_row(&listed, &work.private_pair).is_none(),
        "an unresolvable pair was listed instead of withheld: {text}"
    );
    let unknown = cad2207_operator_read(
        &fx,
        json!({"conversation": UNKNOWN_WORK_CONVERSATION, "include_system": true, "limit": 50}),
    )
    .expect("the ordinary unknown bucket is still readable");
    cad2207_assert_absent(
        "the ambiguous unknown bucket",
        &unknown.to_string(),
        &work.secrets(),
    );
    assert_eq!(
        &cad2207_message_ids(&unknown),
        &work.ordinary_unknown_all,
        "ambiguous provenance cost the provable ordinary rows: {unknown}"
    );
    assert!(
        cad2207_row(&listed, &work.control_topic).is_some(),
        "ambiguous provenance cost the ordinary-only group: {text}"
    );
}
