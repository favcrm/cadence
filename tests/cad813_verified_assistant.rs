//! CAD-813 verified assistant email proposals over the real daemon socket.
//!
//! ADVERSARIAL-FIRST (RED): the operator mints a one-time,
//! host-stamped proposal request (campaign + content source revision)
//! against a chat message carrying the server-verified App binding;
//! the assigned assistant turn redeems it exactly once across ALL
//! proposal ids. A real scoped turn can propose email copy the host
//! binds to installation, context, campaign, source revision and
//! turn/message/request identity with truthful assistant attribution;
//! the browser can never mint that provenance. Same-context wrong
//! campaign, omitted/unknown/stale request or source, fresh-ID replay
//! with the same message, concurrent fresh IDs, forged
//! `by`/`actor`/`turn_id`/`nonce`/`assistant_receipt`/
//! `source_revision`/routing fields, cross-install/context probes and
//! detached children are all refused without changing any proposal,
//! request or draft. Agent turns may propose only:
//! Apply/Discard/approve/test-send/send and request minting stay
//! operator-only. No SMTP send happens anywhere.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::path::PathBuf;

fn blocks() -> Value {
    json!([
        {"type": "heading", "text": "Hello {{first_name|friend}}"},
        {"type": "paragraph", "text": "A calm first line."},
        {"type": "button", "label": "Read more", "url": "https://example.com/posts/welcome"},
    ])
}

struct Crm {
    _root: tempfile::TempDir,
    _pm: Pm,
    daemon: TestDaemon,
}

impl Crm {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        Self::copy_source(&root.path().join("source"), "blog-post");
        let opts = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let daemon = TestDaemon::start_opts(opts);
        Self {
            _root: root,
            _pm: pm,
            daemon,
        }
    }

    fn copy_source(into: &std::path::Path, app: &str) {
        for name in [
            "app.md",
            "workflows/blog-post.md",
            "rubrics/blog.md",
            "templates/brief.md",
            "templates/post.md",
        ] {
            let destination = into.join(name);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::copy(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("apps/blog-post")
                    .join(name),
                &destination,
            )
            .unwrap();
        }
        if app != "blog-post" {
            let manifest = into.join("app.md");
            let text = std::fs::read_to_string(&manifest).unwrap();
            std::fs::write(
                manifest,
                text.replace("app: blog-post", &format!("app: {app}")),
            )
            .unwrap();
        }
    }

    fn install(&self) -> Value {
        self.daemon
            .operator_rpc(
                "app_workspace_install",
                json!({"source": self._root.path().join("source")}),
            )
            .unwrap()
    }

    fn context(&self, install: &str, label: &str, request: &str) -> Value {
        self.daemon
            .operator_rpc(
                "app_context_create",
                json!({"install_id": install, "label": label, "input_defaults": {}, "request_id": request}),
            )
            .unwrap()["context"]
            .clone()
    }

    fn save(&self, install: &str, context: &str, campaign: &str, rev: Option<u64>) -> Value {
        let mut params = json!({"install_id": install, "context_id": context, "campaign_id": campaign, "subject": "Spring launch", "preheader": "News", "blocks": blocks()});
        if let Some(expected) = rev {
            params["expected_revision"] = json!(expected);
        }
        self.daemon
            .operator_rpc("app_content_save", params)
            .unwrap()
    }

    /// Open a scoped chat turn: the operator's `thread_send` carries
    /// the installation/context binding the daemon verifies and
    /// stamps; the test then claims the message running under a turn
    /// token current for the planted pane's generation, the way
    /// delivery marks a submitted message running.
    fn chat_turn(&self, agent: &str, install: &str, context_id: &str, message: &str) -> String {
        self.daemon
            .operator_rpc(
                "thread_send",
                json!({"alias": agent, "text": "help draft the launch email for launch-1", "message": message,
                       "app": {"install_id": install, "context_id": context_id}}),
            )
            .unwrap();
        let token = format!("pty-planted-{}", uuid::Uuid::new_v4().simple());
        let conn = rusqlite::Connection::open(self.daemon.state.join("cadence.sqlite3")).unwrap();
        let changed = conn
            .execute(
                "UPDATE messages SET state='running',turn_id=? WHERE id=?",
                rusqlite::params![token, message],
            )
            .unwrap();
        assert_eq!(changed, 1, "chat turn message missing: {message}");
        token
    }

    /// The operator mints a one-time proposal request against a chat
    /// message: host-stamped campaign + content source revision.
    fn mint(
        &self,
        install: &str,
        context: &str,
        campaign: &str,
        message: &str,
        request: &str,
    ) -> cadence_agent::Result<Value> {
        self.daemon.operator_rpc(
            "app_content_proposal_request",
            json!({"install_id": install, "context_id": context, "campaign_id": campaign,
                   "message": message, "request_id": request}),
        )
    }
}

#[test]
fn cad813_request_mint_binds_campaign_and_source() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-813-r1");
    let context_id = context["id"].as_str().unwrap();
    w.save(install, context_id, "launch-1", None);

    let lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-minter", "claude", None, lane.pid());
    w.chat_turn("crm-minter", install, context_id, "chat-813-req-1");

    // The mint stamps the campaign the operator named and the live
    // source revision — both host-derived, never agent text.
    let minted = w
        .mint(install, context_id, "launch-1", "chat-813-req-1", "req-1")
        .unwrap();
    let request = &minted["request"];
    assert_eq!(request["campaign_id"], "launch-1");
    assert_eq!(request["source_revision"], 1);
    assert_eq!(request["message_id"], "chat-813-req-1");
    assert_eq!(request["state"], "open");

    // Identical re-mint is idempotent; a reused request id on a
    // different scope refuses.
    assert_eq!(
        w.mint(install, context_id, "launch-1", "chat-813-req-1", "req-1")
            .unwrap()["request"],
        *request
    );
    assert!(
        w.mint(install, context_id, "launch-2", "chat-813-req-1", "req-1")
            .is_err(),
        "request id reused on another campaign"
    );

    // Unknown message, unknown context and malformed campaign refuse
    // with nothing stored.
    assert!(
        w.mint(install, context_id, "launch-1", "chat-no-such", "req-x")
            .is_err(),
        "request minted for an unknown message"
    );
    assert!(
        w.mint(
            install,
            "ctx-no-such-context",
            "launch-1",
            "chat-813-req-1",
            "req-x"
        )
        .is_err(),
        "request minted for an unknown context"
    );
    assert!(
        w.mint(install, context_id, "../escape", "chat-813-req-1", "req-x")
            .is_err(),
        "request minted for a malformed campaign"
    );
}

#[test]
fn cad813_assistant_turn_redeems_request_once() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-813-1");
    let context_id = context["id"].as_str().unwrap();
    let created = w.save(install, context_id, "launch-1", None);

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-writer", "claude", None, lane.pid());
    let token = w.chat_turn("crm-writer", install, context_id, "chat-813-1");
    w.mint(install, context_id, "launch-1", "chat-813-1", "req-1")
        .unwrap();

    // The assigned turn redeems the request through its own
    // connection: the caller is derived, never named.
    let proposed: Value = lane.rpc(
        &w.daemon.state,
        "app_content_assistant_propose",
        json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1",
               "proposal_id": "prop-a1", "subject": "Spring launch, assistant draft",
               "preheader": "News", "blocks": blocks(),
               "message": "chat-813-1", "token": token, "request_id": "req-1"}),
    );
    assert_eq!(proposed["ok"], true, "{proposed}");
    let proposal = &proposed["result"]["proposal"];
    assert_eq!(proposal["state"], "pending");
    assert_eq!(proposal["actor"], "assistant");
    assert_eq!(proposal["origin"], "assistant-receipt");
    assert_eq!(proposal["campaign_id"], "launch-1");
    assert_eq!(proposal["source_revision"], 1);
    let receipt = &proposal["assistant_receipt"];
    assert_eq!(receipt["message_id"], "chat-813-1");
    assert_eq!(receipt["agent"], "crm-writer");
    assert_eq!(receipt["request_id"], "req-1");
    assert_eq!(receipt["install_id"], install);
    assert_eq!(receipt["context_id"], context_id);
    assert_eq!(receipt["campaign_id"], "launch-1");
    assert_eq!(receipt["source_revision"], 1);

    // The draft is unchanged and the request is spent.
    assert_eq!(
        w.daemon
            .operator_rpc(
                "app_content_show",
                json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1"}),
            )
            .unwrap()["content"],
        created["content"]
    );

    // Operator Apply creates a new attributed revision and
    // invalidates approval; Discard leaves the draft unchanged.
    w.daemon
        .operator_rpc(
            "app_content_approve",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "expected_revision": 1}),
        )
        .unwrap();
    let applied = w
        .daemon
        .operator_rpc(
            "app_content_proposal_apply",
            json!({"install_id": install, "context_id": context_id, "proposal_id": "prop-a1", "expected_revision": 1}),
        )
        .unwrap();
    assert_eq!(applied["content"]["revision"], 2);
    assert_eq!(applied["content"]["approval"]["valid"], false);

    let token2 = w.chat_turn("crm-writer", install, context_id, "chat-813-2");
    w.mint(install, context_id, "launch-1", "chat-813-2", "req-2")
        .unwrap();
    let proposed2: Value = lane.rpc(
        &w.daemon.state,
        "app_content_assistant_propose",
        json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1",
               "proposal_id": "prop-a2", "subject": "Another assistant draft",
               "blocks": blocks(), "message": "chat-813-2", "token": token2, "request_id": "req-2"}),
    );
    assert_eq!(proposed2["ok"], true, "{proposed2}");
    let before = w
        .daemon
        .operator_rpc(
            "app_content_show",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1"}),
        )
        .unwrap();
    w.daemon
        .operator_rpc(
            "app_content_proposal_discard",
            json!({"install_id": install, "context_id": context_id, "proposal_id": "prop-a2"}),
        )
        .unwrap();
    assert_eq!(
        w.daemon
            .operator_rpc(
                "app_content_show",
                json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1"}),
            )
            .unwrap(),
        before
    );
}

#[test]
fn cad813_agent_cannot_mint_apply_discard_approve_or_send() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-813-2");
    let context_id = context["id"].as_str().unwrap();
    w.save(install, context_id, "launch-1", None);

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-agent", "claude", None, lane.pid());
    let token = w.chat_turn("crm-agent", install, context_id, "chat-813-agent");
    w.mint(
        install,
        context_id,
        "launch-1",
        "chat-813-agent",
        "req-agent",
    )
    .unwrap();
    let proposed: Value = lane.rpc(
        &w.daemon.state,
        "app_content_assistant_propose",
        json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1",
               "proposal_id": "prop-agent", "subject": "Agent draft", "blocks": blocks(),
               "message": "chat-813-agent", "token": token, "request_id": "req-agent"}),
    );
    assert_eq!(proposed["ok"], true, "{proposed}");

    // The same agent connection reaches none of the operator verbs:
    // request minting, Apply, Discard, approve, test-prepare and
    // send-prepare all refuse on caller authority, as does the
    // operator-direct propose path.
    for (method, params) in [
        (
            "app_content_proposal_request",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1",
                   "message": "chat-813-agent", "request_id": "req-evil"}),
        ),
        (
            "app_content_proposal_apply",
            json!({"install_id": install, "context_id": context_id, "proposal_id": "prop-agent"}),
        ),
        (
            "app_content_proposal_discard",
            json!({"install_id": install, "context_id": context_id, "proposal_id": "prop-agent"}),
        ),
        (
            "app_content_approve",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "expected_revision": 1}),
        ),
        (
            "app_content_test_prepare",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "to_email": "op@example.com"}),
        ),
        (
            "app_content_send_prepare",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "binding_id": "bind-1"}),
        ),
        (
            "app_content_propose",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "proposal_id": "prop-direct", "subject": "S", "blocks": blocks()}),
        ),
        (
            "app_content_save",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "subject": "Hijack", "blocks": blocks(), "expected_revision": 1}),
        ),
    ] {
        let frame: Value = lane.rpc(&w.daemon.state, method, params);
        assert_eq!(frame["ok"], false, "agent reached {method}");
        assert!(
            frame.to_string().contains("operator"),
            "agent refusal missed caller authority for {method}: {frame}"
        );
    }
    // And the operator connection cannot mint assistant attribution:
    // the assistant path refuses the operator outright.
    assert!(
        w.daemon
            .operator_rpc(
                "app_content_assistant_propose",
                json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1",
                       "proposal_id": "prop-op", "subject": "S", "blocks": blocks(),
                       "message": "chat-813-agent", "token": token, "request_id": "req-agent"}),
            )
            .is_err(),
        "operator minted assistant attribution"
    );
}

#[test]
fn cad813_same_context_wrong_campaign_refuses() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-813-3");
    let context_id = context["id"].as_str().unwrap();
    w.save(install, context_id, "launch-1", None);
    w.save(install, context_id, "launch-2", None);

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-camp", "claude", None, lane.pid());
    // The operator's chat turn is about launch-1; the host stamps
    // exactly that campaign on the request.
    let token = w.chat_turn("crm-camp", install, context_id, "chat-813-camp");
    w.mint(install, context_id, "launch-1", "chat-813-camp", "req-camp")
        .unwrap();

    // The agent names a different campaign in the SAME context: the
    // stamped campaign disagrees, so the proposal refuses — campaign
    // authority comes from the host-stamped request, never agent text.
    let wrong: Value = lane.rpc(
        &w.daemon.state,
        "app_content_assistant_propose",
        json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-2",
               "proposal_id": "prop-wrong", "subject": "Attached elsewhere", "blocks": blocks(),
               "message": "chat-813-camp", "token": token, "request_id": "req-camp"}),
    );
    assert_eq!(
        wrong["ok"], false,
        "same-context wrong campaign admitted: {wrong}"
    );

    // The stamped campaign itself still redeems on a fresh call.
    let right: Value = lane.rpc(
        &w.daemon.state,
        "app_content_assistant_propose",
        json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1",
               "proposal_id": "prop-right", "subject": "Attached where stamped", "blocks": blocks(),
               "message": "chat-813-camp", "token": token, "request_id": "req-camp"}),
    );
    assert_eq!(right["ok"], true, "{right}");
    assert_eq!(right["result"]["proposal"]["campaign_id"], "launch-1");

    // launch-2's draft never moved and holds no proposal.
    assert_eq!(
        w.daemon
            .operator_rpc(
                "app_content_show",
                json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-2"}),
            )
            .unwrap()["content"]["revision"],
        1
    );
    assert!(
        w.daemon
            .operator_rpc(
                "app_content_proposal_show",
                json!({"install_id": install, "context_id": context_id, "proposal_id": "prop-wrong"}),
            )
            .is_err(),
        "refused wrong-campaign proposal left a row"
    );
}

#[test]
fn cad813_omitted_and_stale_source_refuse() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-813-4");
    let context_id = context["id"].as_str().unwrap();
    w.save(install, context_id, "launch-1", None);

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-stale", "claude", None, lane.pid());
    let token = w.chat_turn("crm-stale", install, context_id, "chat-813-stale");
    // The request stamps source revision 1.
    w.mint(
        install,
        context_id,
        "launch-1",
        "chat-813-stale",
        "req-stale",
    )
    .unwrap();

    let base = || {
        json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1",
               "proposal_id": "prop-stale", "subject": "Late output", "blocks": blocks(),
               "message": "chat-813-stale", "token": token, "request_id": "req-stale"})
    };
    // No request field, an unknown request, or a caller-supplied
    // source revision (not a transport field at all) all refuse:
    // there is no omitting the stamped source.
    for params in [
        {
            let mut p = base();
            p.as_object_mut().unwrap().remove("request_id");
            p
        },
        {
            let mut p = base();
            p["request_id"] = json!("req-no-such-request");
            p
        },
        {
            let mut p = base();
            p["source_revision"] = json!(1);
            p
        },
    ] {
        let frame: Value = lane.rpc(&w.daemon.state, "app_content_assistant_propose", params);
        assert_eq!(
            frame["ok"], false,
            "omitted/unknown source admitted: {frame}"
        );
    }

    // The draft moves on (operator save, rev 1 -> 2); the stamped
    // source is now stale, so late output refuses instead of
    // attaching to the newer draft.
    w.save(install, context_id, "launch-1", Some(1));
    let late: Value = lane.rpc(&w.daemon.state, "app_content_assistant_propose", base());
    assert_eq!(late["ok"], false, "stale source admitted: {late}");

    // A request minted against the new revision redeems cleanly.
    let token2 = w.chat_turn("crm-stale", install, context_id, "chat-813-stale-2");
    w.mint(
        install,
        context_id,
        "launch-1",
        "chat-813-stale-2",
        "req-stale-2",
    )
    .unwrap();
    let fresh: Value = lane.rpc(
        &w.daemon.state,
        "app_content_assistant_propose",
        json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1",
               "proposal_id": "prop-fresh", "subject": "Current output", "blocks": blocks(),
               "message": "chat-813-stale-2", "token": token2, "request_id": "req-stale-2"}),
    );
    assert_eq!(fresh["ok"], true, "{fresh}");
    assert_eq!(fresh["result"]["proposal"]["source_revision"], 2);

    // The refused attempts left no proposal rows behind.
    assert!(
        w.daemon
            .operator_rpc(
                "app_content_proposal_show",
                json!({"install_id": install, "context_id": context_id, "proposal_id": "prop-stale"}),
            )
            .is_err(),
        "refused stale proposal left a row"
    );
}

#[test]
fn cad813_fresh_id_replay_refuses_but_identical_replays() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-813-5");
    let context_id = context["id"].as_str().unwrap();
    w.save(install, context_id, "launch-1", None);

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-replay", "claude", None, lane.pid());
    let token = w.chat_turn("crm-replay", install, context_id, "chat-813-replay");
    w.mint(
        install,
        context_id,
        "launch-1",
        "chat-813-replay",
        "req-replay",
    )
    .unwrap();
    let base = || {
        json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1",
               "proposal_id": "prop-first", "subject": "Assistant draft", "blocks": blocks(),
               "message": "chat-813-replay", "token": token, "request_id": "req-replay"})
    };
    let ok: Value = lane.rpc(&w.daemon.state, "app_content_assistant_propose", base());
    assert_eq!(ok["ok"], true, "{ok}");

    // The same message/request behind a FRESH proposal id refuses —
    // one chat message claims one assistant proposal, whichever id it
    // names first. Identical bytes do not excuse a second id.
    for proposal_id in ["prop-second", "prop-third"] {
        let mut params = base();
        params["proposal_id"] = json!(proposal_id);
        let clash: Value = lane.rpc(&w.daemon.state, "app_content_assistant_propose", params);
        assert_eq!(
            clash["ok"], false,
            "fresh-ID replay admitted as {proposal_id}: {clash}"
        );
    }
    // A fresh id with different bytes refuses the same way.
    let mut different = base();
    different["proposal_id"] = json!("prop-other");
    different["subject"] = json!("Different copy");
    let clash: Value = lane.rpc(&w.daemon.state, "app_content_assistant_propose", different);
    assert_eq!(clash["ok"], false, "fresh-ID replay admitted: {clash}");

    // The identical request (same id, bytes, provenance) replays
    // idempotently; the spent request never mints a second proposal.
    let again: Value = lane.rpc(&w.daemon.state, "app_content_assistant_propose", base());
    assert_eq!(again["ok"], true, "identical replay refused: {again}");
    assert_eq!(
        again["result"]["proposal"], ok["result"]["proposal"],
        "identical replay diverged"
    );
    let listed = w
        .daemon
        .operator_rpc(
            "app_content_proposal_list",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1"}),
        )
        .unwrap();
    assert_eq!(listed["proposals"].as_array().unwrap().len(), 1);
}

#[test]
fn cad813_concurrent_fresh_ids_claim_atomically() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-813-6");
    let context_id = context["id"].as_str().unwrap();
    w.save(install, context_id, "launch-1", None);

    let lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-race", "claude", None, lane.pid());
    // One chat turn, one stamped request: every racer redeems the
    // same message/request under a distinct fresh proposal id.
    // Exactly one claim wins; the rest refuse as already claimed.
    let token = w.chat_turn("crm-race", install, context_id, "chat-813-race");
    w.mint(install, context_id, "launch-1", "chat-813-race", "req-race")
        .unwrap();
    let install_o = install.to_string();
    let context_o = context_id.to_string();
    let outcomes = std::thread::scope(|scope| {
        (0..6)
            .map(|i| {
                let state = w.daemon.state.clone();
                let (install_c, context_c, token_c) =
                    (install_o.clone(), context_o.clone(), token.clone());
                scope.spawn(move || {
                    let params = json!({"install_id": install_c, "context_id": context_c,
                        "campaign_id": "launch-1", "proposal_id": format!("prop-race-{i}"),
                        "subject": "Assistant draft", "blocks": blocks(),
                        "message": "chat-813-race", "token": token_c, "request_id": "req-race"});
                    cadence_agent::test_seam::scoped(
                        cadence_agent::test_seam::Asserted::Agent("crm-race".into()),
                        || {
                            cadence_agent::client::rpc(
                                &state,
                                "app_content_assistant_propose",
                                params,
                            )
                            .is_ok()
                        },
                    )
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        outcomes.iter().filter(|ok| **ok).count(),
        1,
        "concurrent fresh-ID claims admitted {outcomes:?}"
    );
    let listed = w
        .daemon
        .operator_rpc(
            "app_content_proposal_list",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1"}),
        )
        .unwrap();
    assert_eq!(listed["proposals"].as_array().unwrap().len(), 1);
    assert_eq!(listed["proposals"][0]["actor"], "assistant");
}

#[test]
fn cad813_forged_cross_scope_and_detached_refuse() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context_a = w.context(install, "Client A", "ctx-813-7a");
    let context_a_id = context_a["id"].as_str().unwrap();
    let context_b = w.context(install, "Client B", "ctx-813-7b");
    let context_b_id = context_b["id"].as_str().unwrap();
    w.save(install, context_a_id, "launch-1", None);
    w.save(install, context_b_id, "launch-1", None);

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-forge", "claude", None, lane.pid());
    let token = w.chat_turn("crm-forge", install, context_a_id, "chat-813-forge");
    w.mint(
        install,
        context_a_id,
        "launch-1",
        "chat-813-forge",
        "req-forge",
    )
    .unwrap();
    let base = || {
        json!({"install_id": install, "context_id": context_a_id, "campaign_id": "launch-1",
               "proposal_id": "prop-forge", "subject": "Assistant draft", "blocks": blocks(),
               "message": "chat-813-forge", "token": token, "request_id": "req-forge"})
    };
    // Forged identity / receipt / routing fields never confer provenance.
    for params in [
        {
            let mut p = base();
            p["by"] = json!("operator");
            p
        },
        {
            let mut p = base();
            p["actor"] = json!("assistant");
            p
        },
        {
            let mut p = base();
            p["assistant_receipt"] = json!({"turn_id": "t-1"});
            p
        },
        {
            let mut p = base();
            p["turn_id"] = json!("t-1");
            p
        },
        {
            let mut p = base();
            p["nonce"] = json!("n-1");
            p
        },
        {
            let mut p = base();
            p["source_revision"] = json!(1);
            p
        },
        {
            let mut p = base();
            p["workspace"] = json!("default");
            p
        },
        {
            let mut p = base();
            p["project"] = json!("client");
            p
        },
    ] {
        let frame: Value = lane.rpc(&w.daemon.state, "app_content_assistant_propose", params);
        assert_eq!(
            frame["ok"], false,
            "forged field reached assistant propose: {frame}"
        );
    }
    // A token for another turn, a message bound to another context, a
    // request minted for another context, and cross-install scope all
    // refuse.
    let other_token = w.chat_turn("crm-forge", install, context_b_id, "chat-813-other");
    w.mint(
        install,
        context_b_id,
        "launch-1",
        "chat-813-other",
        "req-other",
    )
    .unwrap();
    for params in [
        {
            let mut p = base();
            p["token"] = json!(other_token);
            p
        },
        {
            let mut p = base();
            p["message"] = json!("chat-813-other");
            p["token"] = json!(other_token);
            p
        },
        {
            let mut p = base();
            p["context_id"] = json!(context_b_id);
            p
        },
        {
            let mut p = base();
            p["request_id"] = json!("req-other");
            p
        },
        {
            let mut p = base();
            p["install_id"] = json!("install-no-such-install");
            p
        },
    ] {
        let frame: Value = lane.rpc(&w.daemon.state, "app_content_assistant_propose", params);
        assert_eq!(frame["ok"], false, "cross-scope probe admitted: {frame}");
    }
    // A detached child of the agent — outside the endpoint session —
    // is refused too.
    let request = lane.dir.path().join("detached-813.json");
    std::fs::write(
        &request,
        cadence_agent::proto::request("app_content_assistant_propose", base()).to_string(),
    )
    .unwrap();
    let (rc, output) = lane.run(&format!("setsid python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");print(s.makefile().readline())' {} {}", cadence_agent::client::socket_path(&w.daemon.state).display(), request.display()));
    assert_eq!(rc, 0);
    let frame: Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(
        frame["ok"], false,
        "detached child reached assistant propose: {frame}"
    );

    // Valid control redeems exactly once; cross-context Apply of the
    // stored row refuses and neither draft moves.
    let ok: Value = lane.rpc(&w.daemon.state, "app_content_assistant_propose", base());
    assert_eq!(ok["ok"], true, "{ok}");
    assert!(
        w.daemon
            .operator_rpc(
                "app_content_proposal_apply",
                json!({"install_id": install, "context_id": context_b_id, "proposal_id": "prop-forge"}),
            )
            .is_err(),
        "cross-context proposal applied"
    );
    for (ctx, rev) in [(context_a_id, 1), (context_b_id, 1)] {
        assert_eq!(
            w.daemon
                .operator_rpc(
                    "app_content_show",
                    json!({"install_id": install, "context_id": ctx, "campaign_id": "launch-1"}),
                )
                .unwrap()["content"]["revision"],
            rev
        );
    }
}

#[test]
fn cad813_assistant_handler_is_source_pinned_to_request_receipt() {
    // Guard-removal tripwire: the assistant surface routes through one
    // handler that derives the agent caller, binds the endpoint
    // session, re-proves the chat turn and its App binding, and
    // redeems a host-stamped request receipt — campaign and source
    // come from the stamp, never agent text. If the gate is removed
    // or the method is folded into the operator handler, this fails
    // without running any daemon.
    let source = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/daemon/app_content_rpc.rs"),
    )
    .unwrap();
    let handler = source
        .split_once("fn rpc_app_content_assistant_propose")
        .expect("assistant propose handler moved")
        .1;
    for pin in [
        "connection_caller",
        "slot_identity",
        "pi_bash_tool_session",
        "message_app",
        "turn_token_current",
        "app_context_proof",
        "AssistantClaim",
        "content_request_id",
        "the stamp names exactly",
        "detached child is outside the assigned agent endpoint session",
    ] {
        assert!(
            handler.contains(pin),
            "assistant propose handler lost its {pin} pin"
        );
    }
    // The atomic claim lives in the store: staleness, single-use
    // and the UNIQUE backstop are pinned here so a guard removal in
    // either file fails without a daemon.
    let store = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/store/app_content.rs"),
    )
    .unwrap();
    let claim = store
        .split_once("fn app_content_assistant_propose")
        .expect("assistant claim moved")
        .1;
    for pin in [
        "source revision is stale",
        "does not match its stamped campaign",
        "already claimed",
        "is_claim_conflict",
    ] {
        assert!(claim.contains(pin), "assistant claim lost its {pin} pin");
    }
    assert!(
        !handler.contains("operator_connection"),
        "assistant propose handler grew an operator bypass"
    );
    assert!(
        source.contains("\"app_content_assistant_propose\""),
        "assistant propose method missing from the allowlist"
    );
    assert!(
        source.contains("\"app_content_proposal_request\""),
        "proposal request mint missing from the operator allowlist"
    );
    let rules = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/daemon/caller_rule.rs"),
    )
    .unwrap();
    assert!(
        rules.contains("\"app_content_assistant_propose\""),
        "assistant propose missing from the caller-rule table"
    );
    assert!(
        rules.contains("\"app_content_proposal_request\""),
        "proposal request mint missing from the caller-rule table"
    );
}
