//! CAD-1014 adversarial-first (RED) tests for a narrow delegated CRM
//! capability: a connection-derived agent on its live scoped chat turn
//! may run a bounded, per-intent customer CSV import and segment save —
//! never an unscoped or operator-only mutation.
//!
//! The redeem gate is the CAD-813 pattern, generalized minimally:
//! caller derived from the connection (agent only — the operator and an
//! unproven/detached caller are refused), the message must address the
//! caller and be `running` under `turn_id == token` current for the
//! endpoint's scheme, `message_app` is re-proved for exactly the named
//! install+context, and one chat message redeems one action across all
//! request ids. Scope comes from the daemon-stamped App binding on the
//! operator's own chat message — never agent text. The existing
//! byte-bound `preview_token` and idempotent `request_id` are unchanged;
//! so are the record grammar, consent defaults and revision CAS.
//!
//! RED: against today's build every redeem call fails (unknown method),
//! which itself proves the current operator-only posture — no agent can
//! import or save a segment. The guard lands after the PM chooses
//! mint-vs-message-consent; these tests pin whichever lands.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::path::PathBuf;

const CSV: &str = "record_id,display_name,email\ncust-1,Amina Diallo,amina@example.invalid\n";

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

    /// Open a scoped chat turn on an agent: the operator's `thread_send`
    /// carries the verified App binding, then the test claims the message
    /// running under a turn token current for the planted pane — exactly
    /// the CAD-813 fixture.
    fn chat_turn(&self, agent: &str, install: &str, context_id: &str, message: &str) -> String {
        self.daemon
            .operator_rpc(
                "thread_send",
                json!({"alias": agent, "text": "import this customer list", "message": message,
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

    /// The operator mints a one-time email proposal request on a scoped
    /// chat message — the CAD-813 verb, reused for the no-draft case.
    fn mint(
        &self,
        install: &str,
        context: &str,
        campaign: &str,
        message: &str,
        request: &str,
    ) -> Value {
        self.daemon
            .operator_rpc(
                "app_content_proposal_request",
                json!({"install_id": install, "context_id": context, "campaign_id": campaign,
                       "message": message, "request_id": request}),
            )
            .unwrap()
    }

    fn preview(&self, install: &str, context: &str, csv: &str) -> Value {
        self.daemon
            .operator_rpc(
                "app_record_csv_preview",
                json!({"install_id": install, "context_id": context, "csv_text": csv}),
            )
            .unwrap()
    }
}

/// RED: a scoped chat turn's delegated CSV import — the operator's own
/// stamped message supplies install/context, the preview token binds the
/// exact bytes, `request_id` is the idempotency key. Today the verb does
/// not exist, so every call is refused (proving no agent path). Once the
/// guard lands these flip to the asserted behaviour.
#[test]
fn cad1014_scoped_chat_csv_import_redeems_scope_once() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1014-1");
    let context_id = context["id"].as_str().unwrap();
    let token_preview = w.preview(install, context_id, CSV)["preview_token"]
        .as_str()
        .unwrap()
        .to_string();

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-chat", "claude", None, lane.pid());
    let token = w.chat_turn("crm-chat", install, context_id, "chat-1014-1");

    // The redeem verb (proposed name): scope from the stamped message,
    // bytes bound by the existing preview token. Refused today.
    let redeem: Value = lane.rpc(
        &w.daemon.state,
        "app_record_csv_assistant_import",
        json!({"install_id": install, "context_id": context_id,
               "csv_text": CSV, "preview_token": token_preview, "request_id": "req-1014-1",
               "message": "chat-1014-1", "token": token}),
    );
    // GREEN post-guard: the stamped scoped chat message redeems one
    // byte-bound import under the verified scope.
    assert_eq!(redeem["ok"], true, "{redeem}");
    let applied = redeem["result"]["summary"]["applied"]
        .as_i64()
        .unwrap_or(-1);
    assert_eq!(applied, 1, "expected one imported record: {redeem}");
    // Replay with the identical request id is idempotent; a fresh id on
    // the same message is refused as already claimed.
    let replay: Value = lane.rpc(
        &w.daemon.state,
        "app_record_csv_assistant_import",
        json!({"install_id": install, "context_id": context_id,
               "csv_text": CSV, "preview_token": token_preview, "request_id": "req-1014-1",
               "message": "chat-1014-1", "token": token}),
    );
    assert_eq!(replay["ok"], true, "identical replay refused: {replay}");
    let second: Value = lane.rpc(
        &w.daemon.state,
        "app_record_csv_assistant_import",
        json!({"install_id": install, "context_id": context_id,
               "csv_text": CSV, "preview_token": token_preview, "request_id": "req-1014-2",
               "message": "chat-1014-1", "token": token}),
    );
    assert_eq!(
        second["ok"], false,
        "one message minted two imports: {second}"
    );
}

#[test]
fn cad1014_scoped_chat_segment_save_redeems_scope_once() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1014-2");
    let context_id = context["id"].as_str().unwrap();

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-chat", "claude", None, lane.pid());
    let token = w.chat_turn("crm-chat", install, context_id, "chat-1014-2");

    let redeem: Value = lane.rpc(
        &w.daemon.state,
        "app_segment_assistant_save",
        json!({"install_id": install, "context_id": context_id,
               "segment_id": "vip", "name": "VIP customers",
               "predicates": [{"field": "tag", "op": "eq", "value": "vip"}],
               "message": "chat-1014-2", "token": token}),
    );
    assert_eq!(redeem["ok"], true, "{redeem}");
    assert_eq!(
        redeem["result"]["segment"]["id"].as_str(),
        Some("vip"),
        "{redeem}"
    );
    // A second segment on the SAME message is refused (one claim).
    let second: Value = lane.rpc(
        &w.daemon.state,
        "app_segment_assistant_save",
        json!({"install_id": install, "context_id": context_id,
               "segment_id": "other", "name": "Another",
               "predicates": [{"field": "tag", "op": "eq", "value": "x"}],
               "message": "chat-1014-2", "token": token}),
    );
    assert_eq!(
        second["ok"], false,
        "one message minted two segment saves: {second}"
    );
}

/// Cross-scope, forged-field, wrong-turn and detached-child refusals —
/// the same adversarial grid CAD-813 pins for proposals.
#[test]
fn cad1014_scoped_chat_refusals_are_closed() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let ctx_a = w.context(install, "Client A", "ctx-1014-3a")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let ctx_b = w.context(install, "Client B", "ctx-1014-3b")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let token_preview = w.preview(install, &ctx_a, CSV)["preview_token"]
        .as_str()
        .unwrap()
        .to_string();

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-chat", "claude", None, lane.pid());
    let token = w.chat_turn("crm-chat", install, &ctx_a, "chat-1014-3a");
    // A second scoped message to context B exists; its binding must never
    // lend scope to a context-A call.
    let _other = w.chat_turn("crm-chat", install, &ctx_b, "chat-1014-3b");

    let base = || {
        json!({"install_id": install, "context_id": ctx_a,
               "csv_text": CSV, "preview_token": token_preview, "request_id": "req-1014-3",
               "message": "chat-1014-3a", "token": token})
    };
    // Forged identity/receipt/routing fields are not transport fields.
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
            p["workspace"] = json!("default");
            p
        },
        {
            let mut p = base();
            p["project"] = json!("client");
            p
        },
        // Cross-scope and wrong-turn probes.
        {
            let mut p = base();
            p["context_id"] = json!(ctx_b);
            p
        },
        {
            let mut p = base();
            p["install_id"] = json!("install-no-such");
            p
        },
        {
            let mut p = base();
            p["message"] = json!("chat-1014-3b");
            p
        },
        {
            let mut p = base();
            p["token"] = json!("stale-token");
            p
        },
        {
            let mut p = base();
            p["preview_token"] = json!("sha256:forged");
            p
        },
    ] {
        let frame: Value = lane.rpc(&w.daemon.state, "app_record_csv_assistant_import", params);
        assert_eq!(frame["ok"], false, "probe admitted: {frame}");
    }
    // Detached child (outside the endpoint session) is refused.
    let request = lane.dir.path().join("detached-1014.json");
    std::fs::write(
        &request,
        cadence_agent::proto::request("app_record_csv_assistant_import", base()).to_string(),
    )
    .unwrap();
    let (rc, output) = lane.run(&format!("setsid python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");print(s.makefile().readline())' {} {}", cadence_agent::client::socket_path(&w.daemon.state).display(), request.display()));
    assert_eq!(rc, 0);
    let frame: Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(frame["ok"], false, "detached child admitted: {frame}");
    // The operator itself cannot mint assistant attribution.
    assert!(
        w.daemon
            .operator_rpc(
                "app_record_csv_assistant_import",
                json!({"install_id": install, "context_id": ctx_a,
                       "csv_text": CSV, "preview_token": token_preview, "request_id": "req-op",
                       "message": "chat-1014-3a", "token": token}),
            )
            .is_err(),
        "operator reached the assistant redeem"
    );
}

/// Email creation from scoped chat with NO prior draft — the composer-
/// free initial-draft path. The operator's own stamped chat message is
/// the intent; the one-time proposal request mints on a fresh campaign
/// (`source_revision` resolves to 0 with no doc), and the agent redeems
/// it through `app_content_assistant_propose` into an inert `pending`
/// proposal carrying `assistant-receipt` provenance. No send, no
/// approval — Apply stays the operator's.
#[test]
fn cad1014_scoped_chat_email_first_draft_is_inert() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1014-4");
    let context_id = context["id"].as_str().unwrap();

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-chat", "claude", None, lane.pid());
    let token = w.chat_turn("crm-chat", install, context_id, "chat-1014-4");

    // Fresh campaign — no draft exists; the mint stamps source_revision 0.
    let minted = w.mint(
        install,
        context_id,
        "welcome-1",
        "chat-1014-4",
        "req-1014-e1",
    );
    assert_eq!(
        minted["request"]["source_revision"].as_i64(),
        Some(0),
        "{minted}"
    );

    let proposed: Value = lane.rpc(
        &w.daemon.state,
        "app_content_assistant_propose",
        json!({"install_id": install, "context_id": context_id, "campaign_id": "welcome-1",
               "proposal_id": "prop-1014-e1", "subject": "Welcome, {{first_name|friend}}",
               "preheader": "A note", "blocks": blocks(),
               "message": "chat-1014-4", "token": token, "request_id": "req-1014-e1"}),
    );
    assert_eq!(proposed["ok"], true, "{proposed}");
    let proposal = &proposed["result"]["proposal"];
    assert_eq!(proposal["state"], "pending");
    assert_eq!(proposal["actor"], "assistant");
    assert_eq!(proposal["origin"], "assistant-receipt");
    assert_eq!(proposal["source_revision"], 0);
    // The proposal is inert: applying/approving/sending stays the
    // operator's — the assistant verb can never reach them (asserted in
    // the CAD-813 suite).
}
