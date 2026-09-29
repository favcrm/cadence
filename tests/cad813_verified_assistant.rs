//! CAD-813 verified assistant email proposals over the real daemon socket.
//!
//! ADVERSARIAL-FIRST (RED): a real scoped assistant turn (operator
//! `thread_send` with a server-verified App binding, claimed running
//! under its turn token by the assigned pane agent) can propose email
//! copy that the host binds to installation, context, campaign, source
//! revision and turn/message identity with truthful assistant
//! attribution; the browser can never mint that provenance. An agent
//! caller, a detached child, forged `by`/`actor`/`turn_id`/`nonce`/
//! `assistant_receipt`/routing fields, replay with different bytes, a
//! stale source revision and cross-install/context/campaign probes are
//! all refused without changing any proposal or draft. Agent turns may
//! propose only: Apply/Discard/approve/test-send/send stay
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
                json!({"alias": agent, "text": "help draft the launch email", "message": message,
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

    fn db_message(&self, id: &str) -> (String, Option<String>) {
        let conn = rusqlite::Connection::open(self.daemon.state.join("cadence.sqlite3")).unwrap();
        conn.query_row("SELECT state,turn_id FROM messages WHERE id=?", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
    }
}

#[test]
fn cad813_assistant_turn_proposes_verified_proposal() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-813-1");
    let context_id = context["id"].as_str().unwrap();
    let created = w.save(install, context_id, "launch-1", None);
    assert_eq!(created["content"]["revision"], 1);

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-writer", "claude", None, lane.pid());
    let token = w.chat_turn("crm-writer", install, context_id, "chat-813-1");
    assert_eq!(w.db_message("chat-813-1").0, "running");

    // The assigned turn proposes through its own connection: the
    // caller is derived, never named.
    let proposed: Value = lane.rpc(
        &w.daemon.state,
        "app_content_assistant_propose",
        json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1",
               "proposal_id": "prop-a1", "subject": "Spring launch, assistant draft",
               "preheader": "News", "blocks": blocks(),
               "message": "chat-813-1", "token": token}),
    );
    assert_eq!(proposed["ok"], true, "{proposed}");
    let proposal = &proposed["result"]["proposal"];
    assert_eq!(proposal["state"], "pending");
    assert_eq!(proposal["actor"], "assistant");
    assert_eq!(proposal["origin"], "assistant-receipt");
    assert_eq!(proposal["campaign_id"], "launch-1");
    assert_eq!(proposal["source_revision"], 1);
    assert_eq!(proposal["install_id"], install);
    assert_eq!(proposal["context_id"], context_id);
    let receipt = &proposal["assistant_receipt"];
    assert_eq!(receipt["message_id"], "chat-813-1");
    assert_eq!(receipt["agent"], "crm-writer");
    assert_eq!(receipt["install_id"], install);
    assert_eq!(receipt["context_id"], context_id);
    assert_eq!(receipt["campaign_id"], "launch-1");
    assert_eq!(receipt["source_revision"], 1);

    // The draft is unchanged and the proposal lists with verified
    // attribution through the operator read path (the CAD-784 seam).
    assert_eq!(
        w.daemon
            .operator_rpc(
                "app_content_show",
                json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1"}),
            )
            .unwrap()["content"],
        created["content"]
    );
    let shown = w
        .daemon
        .operator_rpc(
            "app_content_proposal_show",
            json!({"install_id": install, "context_id": context_id, "proposal_id": "prop-a1"}),
        )
        .unwrap();
    assert_eq!(shown["proposal"]["actor"], "assistant");
    assert_eq!(
        shown["proposal"]["assistant_receipt"]["message_id"],
        "chat-813-1"
    );
    let listed = w
        .daemon
        .operator_rpc(
            "app_content_proposal_list",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1"}),
        )
        .unwrap();
    assert_eq!(listed["proposals"].as_array().unwrap().len(), 1);

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
    let proposed2: Value = lane.rpc(
        &w.daemon.state,
        "app_content_assistant_propose",
        json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1",
               "proposal_id": "prop-a2", "subject": "Another assistant draft",
               "blocks": blocks(), "message": "chat-813-2", "token": token2}),
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
fn cad813_agent_cannot_apply_discard_approve_or_send() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-813-2");
    let context_id = context["id"].as_str().unwrap();
    w.save(install, context_id, "launch-1", None);

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-agent", "claude", None, lane.pid());
    let token = w.chat_turn("crm-agent", install, context_id, "chat-813-agent");
    let proposed: Value = lane.rpc(
        &w.daemon.state,
        "app_content_assistant_propose",
        json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1",
               "proposal_id": "prop-agent", "subject": "Agent draft", "blocks": blocks(),
               "message": "chat-813-agent", "token": token}),
    );
    assert_eq!(proposed["ok"], true, "{proposed}");

    // The same agent connection reaches none of the operator verbs:
    // Apply, Discard, approve, test-prepare and send-prepare all
    // refuse on caller authority, as does the operator-direct
    // propose path.
    for (method, params) in [
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
                       "message": "chat-813-agent", "token": token}),
            )
            .is_err(),
        "operator minted assistant attribution"
    );
}

#[test]
fn cad813_forged_replayed_stale_cross_scope_and_detached_refuse() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context_a = w.context(install, "Client A", "ctx-813-3a");
    let context_a_id = context_a["id"].as_str().unwrap();
    let context_b = w.context(install, "Client B", "ctx-813-3b");
    let context_b_id = context_b["id"].as_str().unwrap();
    w.save(install, context_a_id, "launch-1", None);
    w.save(install, context_b_id, "launch-1", None);

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-forge", "claude", None, lane.pid());
    let token = w.chat_turn("crm-forge", install, context_a_id, "chat-813-forge");
    let base = || {
        json!({"install_id": install, "context_id": context_a_id, "campaign_id": "launch-1",
               "proposal_id": "prop-forge", "subject": "Assistant draft", "blocks": blocks(),
               "message": "chat-813-forge", "token": token})
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
    // A token for another turn and a message bound to another
    // context both refuse, as does a stale source revision. (A new
    // campaign in the same verified context is legitimate scope —
    // the receipt binds the campaign it names — so cross-campaign
    // is proved at Apply below, not here.)
    let other_token = w.chat_turn("crm-forge", install, context_b_id, "chat-813-other");
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
            p["source_revision"] = json!(7);
            p
        },
    ] {
        let frame: Value = lane.rpc(&w.daemon.state, "app_content_assistant_propose", params);
        assert_eq!(
            frame["ok"], false,
            "cross-scope/stale probe admitted: {frame}"
        );
    }
    // A detached child of the agent — no provable identity — is refused too.
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

    // Valid control proposes exactly once.
    let ok: Value = lane.rpc(&w.daemon.state, "app_content_assistant_propose", base());
    assert_eq!(ok["ok"], true, "{ok}");
    // Replay behind identical bytes AND identical provenance is
    // idempotent; any divergence refuses as already-used.
    let again: Value = lane.rpc(&w.daemon.state, "app_content_assistant_propose", base());
    assert_eq!(again["ok"], true, "identical replay refused: {again}");
    let mut different = base();
    different["subject"] = json!("Different copy");
    let clash: Value = lane.rpc(&w.daemon.state, "app_content_assistant_propose", different);
    assert_eq!(
        clash["ok"], false,
        "replay with different bytes admitted: {clash}"
    );
    // The control proposal is untouched and the draft never moved.
    let shown = w
        .daemon
        .operator_rpc(
            "app_content_proposal_show",
            json!({"install_id": install, "context_id": context_a_id, "proposal_id": "prop-forge"}),
        )
        .unwrap();
    assert_eq!(shown["proposal"]["actor"], "assistant");
    // Cross-context/cross-campaign Apply is bound to the stored
    // row: the sibling context knows no such proposal, and context
    // B's own draft never moves.
    assert!(
        w.daemon
            .operator_rpc(
                "app_content_proposal_apply",
                json!({"install_id": install, "context_id": context_b_id, "proposal_id": "prop-forge"}),
            )
            .is_err(),
        "cross-context proposal applied"
    );
    assert_eq!(
        w.daemon
            .operator_rpc(
                "app_content_show",
                json!({"install_id": install, "context_id": context_a_id, "campaign_id": "launch-1"}),
            )
            .unwrap()["content"]["revision"],
        1
    );
}

#[test]
fn cad813_concurrent_assistant_proposes_serialize() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-813-4");
    let context_id = context["id"].as_str().unwrap();
    w.save(install, context_id, "launch-1", None);

    // Concurrent identical proposes from the agent connection: every
    // call either inserts or replays the identical row — the stored
    // proposal is one row with verified attribution, never two.
    let lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-race", "claude", None, lane.pid());
    // Open the six turns serially on the operator path.
    let mut turns = Vec::new();
    for i in 0..6 {
        turns.push(w.chat_turn(
            "crm-race",
            install,
            context_id,
            &format!("chat-813-race-{i}"),
        ));
    }
    let install_o = install.to_string();
    let context_o = context_id.to_string();
    let outcomes = std::thread::scope(|scope| {
        (0..6)
            .map(|i| {
                let state = w.daemon.state.clone();
                let (token, msg) = (turns[i].clone(), format!("chat-813-race-{i}"));
                let (install_c, context_c) = (install_o.clone(), context_o.clone());
                scope.spawn(move || {
                    // Same proposal_id, different subjects: exactly one wins.
                    let params = json!({"install_id": install_c, "context_id": context_c,
                        "campaign_id": "launch-1", "proposal_id": "prop-race",
                        "subject": format!("Racer {i}"), "blocks": blocks(),
                        "message": msg, "token": token});
                    // Route through the agent pane's connection shape is
                    // covered above; here the daemon RPC proves the
                    // store serializes the proposal_id claim.
                    let outcome = cadence_agent::test_seam::scoped(
                        cadence_agent::test_seam::Asserted::Agent("crm-race".into()),
                        || {
                            cadence_agent::client::rpc(
                                &state,
                                "app_content_assistant_propose",
                                params,
                            )
                        },
                    );
                    outcome.is_ok()
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
        "concurrent assistant CAS admitted {outcomes:?}"
    );
}

#[test]
fn cad813_assistant_handler_is_source_pinned_to_turn_proof() {
    // Guard-removal tripwire: the assistant surface routes through one
    // handler whose first acts derive the agent caller, re-prove the
    // chat turn and never consult browser-supplied provenance. If the
    // gate is removed or the method is folded into the operator
    // handler, this fails without running any daemon.
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
        "detached child is outside the assigned agent endpoint session",
    ] {
        assert!(
            handler.contains(pin),
            "assistant propose handler lost its {pin} pin"
        );
    }
    assert!(
        !handler.contains("operator_connection"),
        "assistant propose handler grew an operator bypass"
    );
    assert!(
        source.contains("\"app_content_assistant_propose\""),
        "assistant propose method missing from the allowlist"
    );
    let rules = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/daemon/caller_rule.rs"),
    )
    .unwrap();
    assert!(
        rules.contains("\"app_content_assistant_propose\""),
        "assistant propose missing from the caller-rule table"
    );
}
