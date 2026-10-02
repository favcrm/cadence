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
    fn preview(&self, install: &str, context: &str, csv: &str) -> Value {
        self.daemon
            .operator_rpc(
                "app_record_csv_preview",
                json!({"install_id": install, "context_id": context, "csv_text": csv}),
            )
            .unwrap()
    }

    /// The operator's explicit host-side confirm of the exact previewed
    /// plan — mints the one-use `confirm_token` the assistant import
    /// redeems AND stores the confirmed `csv_text`+`decisions` as the
    /// durable plan the agent resolves by request id + nonce (the bytes
    /// never ride the text-only chat). `decisions_digest` binds the
    /// confirmed decision set (empty array = the default preview plan).
    fn confirm(
        &self,
        install: &str,
        context: &str,
        csv: &str,
        decisions: &Value,
        preview_token: &str,
        request: &str,
    ) -> String {
        self.daemon
            .operator_rpc(
                "app_record_csv_confirm",
                json!({"install_id": install, "context_id": context, "preview_token": preview_token,
                       "request_id": request, "csv_text": csv, "decisions": decisions,
                       "decisions_digest": cadence_agent::store::app_records::csv_decisions_digest(decisions).unwrap()}),
            )
            .unwrap()["confirm_token"]
            .as_str()
            .unwrap()
            .to_string()
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

    // WITHOUT the operator's confirm the import MUST refuse: a scoped
    // chat message alone is not confirmation — the agent cannot mint or
    // hash-forge the host-side confirm receipt.
    let unconfirmed: Value = lane.rpc(
        &w.daemon.state,
        "app_record_csv_assistant_import",
        json!({"install_id": install, "context_id": context_id,
               "request_id": "req-1014-1", "confirm_token": "confirm-forged",
               "message": "chat-1014-1", "token": token}),
    );
    assert_eq!(
        unconfirmed["ok"], false,
        "assistant import ran without a host-minted confirm: {unconfirmed}"
    );

    // The operator confirms the exact previewed plan (host side); the
    // agent redeems the minted nonce.
    // The scoped-chat CSV preview read admits the agent on the live
    // turn — the agent plans the import read-only, no claim consumed.
    let preview: Value = lane.rpc(
        &w.daemon.state,
        "app_record_csv_assistant_preview",
        json!({"install_id": install, "context_id": context_id, "csv_text": CSV,
               "message": "chat-1014-1", "token": token}),
    );
    assert_eq!(
        preview["ok"], true,
        "scoped CSV preview refused for the agent: {preview}"
    );
    assert_eq!(
        preview["result"]["preview_token"].as_str(),
        Some(token_preview.as_str()),
        "agent preview token differs from operator's: {preview}"
    );

    // The operator's confirm mints the durable plan (bytes + decisions
    // stored host-side); the agent redeems it by request id + nonce only
    // — the CSV bytes never ride the chat or the redeem call.
    let confirm_token = w.confirm(
        install,
        context_id,
        CSV,
        &json!([]),
        &token_preview,
        "req-1014-1",
    );
    let redeem: Value = lane.rpc(
        &w.daemon.state,
        "app_record_csv_assistant_import",
        json!({"install_id": install, "context_id": context_id,
               "request_id": "req-1014-1", "confirm_token": confirm_token,
               "message": "chat-1014-1", "token": token}),
    );
    // GREEN post-guard: the stamped scoped chat message redeems one
    // byte-bound, operator-confirmed import under the verified scope.
    assert_eq!(redeem["ok"], true, "{redeem}");
    let applied = redeem["result"]["summary"]["applied"]
        .as_i64()
        .unwrap_or(-1);
    assert_eq!(applied, 1, "expected one imported record: {redeem}");
    // Replay with the identical request id is idempotent — the claim
    // row matches action+request, and the spent confirm is re-proved
    // (redeem_confirm tolerates the already-'used' state for a replay
    // of the SAME request id is NOT re-redeemed: the import's own
    // receipt replay returns before confirm is consulted — see the
    // claim/dedupe ordering). We assert the replay reads back.
    let replay: Value = lane.rpc(
        &w.daemon.state,
        "app_record_csv_assistant_import",
        json!({"install_id": install, "context_id": context_id,
               "request_id": "req-1014-1", "confirm_token": confirm_token,
               "message": "chat-1014-1", "token": token}),
    );
    assert_eq!(replay["ok"], true, "identical replay refused: {replay}");
    // A second request id under the same message is a second intent —
    // refused even though the message+nonce are reused.
    let second: Value = lane.rpc(
        &w.daemon.state,
        "app_record_csv_assistant_import",
        json!({"install_id": install, "context_id": context_id,
               "request_id": "req-1014-2", "confirm_token": confirm_token,
               "message": "chat-1014-1", "token": token}),
    );
    assert_eq!(
        second["ok"], false,
        "one message minted two imports: {second}"
    );
}

/// Durable pending intent: the claim burns the message BEFORE the CSV
/// write commits, so a failed import still holds the intent — a retry
/// with the SAME request id + bytes replays/completes, and a retry with
/// a CHANGED payload under that spent claim refuses. Proves the
/// claim-then-write ordering never lets one message mint two intents
/// nor lose the confirmed plan to a mid-import failure.
#[test]
fn cad1014_scoped_chat_csv_failed_row_keeps_intent() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1014-5");
    let context_id = context["id"].as_str().unwrap();
    // Row 1 valid, row 2 malformed (missing display_name) so the import
    // lands a partial failure — the claim is still spent.
    let bad_csv = "record_id,display_name,email\ncust-ok,Amina,amina@example.invalid\ncust-bad,,x@example.invalid\n";
    let token_preview = w.preview(install, context_id, bad_csv)["preview_token"]
        .as_str()
        .unwrap()
        .to_string();

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-chat", "claude", None, lane.pid());
    let token = w.chat_turn("crm-chat", install, context_id, "chat-1014-5");
    let confirm = w.confirm(
        install,
        context_id,
        bad_csv,
        &json!([]),
        &token_preview,
        "req-1014-5",
    );

    let first: Value = lane.rpc(
        &w.daemon.state,
        "app_record_csv_assistant_import",
        json!({"install_id": install, "context_id": context_id,
               "request_id": "req-1014-5", "confirm_token": confirm,
               "message": "chat-1014-5", "token": token}),
    );
    assert_eq!(first["ok"], true, "partial import refused: {first}");
    // The bad row drops as skipped/"row error" — a partial import, not
    // an all-or-nothing failure; the claim+confirm are still spent.
    assert!(
        first["result"]["summary"]["skipped"].as_i64().unwrap_or(0) >= 1
            || first["result"]["summary"]["failed"].as_i64().unwrap_or(0) >= 1,
        "expected a dropped/failed row: {first}"
    );
    // A retry under the SAME request id + bytes replays the stored
    // receipt without needing a new confirm (claim binds the payload).
    let retry: Value = lane.rpc(
        &w.daemon.state,
        "app_record_csv_assistant_import",
        json!({"install_id": install, "context_id": context_id,
               "request_id": "req-1014-5", "confirm_token": confirm,
               "message": "chat-1014-5", "token": token}),
    );
    assert_eq!(retry["ok"], true, "same-request replay refused: {retry}");
    // A DIFFERENT confirmed plan (other request id + its own nonce)
    // under the SAME spent message is a second intent — refused even
    // though the message matches. The claim binds the resolved plan.
    let other_csv = "record_id,display_name,email\ncust-2,Other,o@example.invalid\n";
    let other_token = w.preview(install, context_id, other_csv)["preview_token"]
        .as_str()
        .unwrap()
        .to_string();
    let other_confirm = w.confirm(
        install,
        context_id,
        other_csv,
        &json!([]),
        &other_token,
        "req-1014-9",
    );
    let changed: Value = lane.rpc(
        &w.daemon.state,
        "app_record_csv_assistant_import",
        json!({"install_id": install, "context_id": context_id,
               "request_id": "req-1014-9", "confirm_token": other_confirm,
               "message": "chat-1014-5", "token": token}),
    );
    assert_eq!(
        changed["ok"], false,
        "a second plan under a spent message admitted: {changed}"
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
    // The scoped-chat READ admits the agent on the same live turn —
    // segment revision/membership, no claim consumed.
    let read: Value = lane.rpc(
        &w.daemon.state,
        "app_segment_assistant_list",
        json!({"install_id": install, "context_id": context_id,
               "message": "chat-1014-2", "token": token}),
    );
    assert_eq!(read["ok"], true, "scoped segment list refused: {read}");
    // A second segment on the SAME message is refused (one claim), AND
    // a same-id segment with a CHANGED payload is a different intent —
    // the claim binds the normalized payload digest, so it refuses too.
    for probe in [
        json!({"install_id": install, "context_id": context_id,
               "segment_id": "other", "name": "Another",
               "predicates": [{"field": "tag", "op": "eq", "value": "x"}],
               "message": "chat-1014-2", "token": token}),
        // same segment id, edited payload — a second intent, not a replay
        json!({"install_id": install, "context_id": context_id,
               "segment_id": "vip", "name": "Renamed VIP",
               "predicates": [{"field": "tag", "op": "eq", "value": "vip"}],
               "message": "chat-1014-2", "token": token}),
    ] {
        let frame: Value = lane.rpc(&w.daemon.state, "app_segment_assistant_save", probe);
        assert_eq!(
            frame["ok"], false,
            "one message minted a second save: {frame}"
        );
    }
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
    // The operator confirms the exact ctx-A plan; the probes below must
    // each refuse for their OWN reason, not merely for a missing confirm.
    let confirm_token = w.confirm(
        install,
        &ctx_a,
        CSV,
        &json!([]),
        &token_preview,
        "req-1014-3",
    );

    // The durable plan resolves bytes host-side: the redeem names only
    // request_id + confirm_token; csv_text/preview_token/decisions are
    // never on the wire.
    let base = || {
        json!({"install_id": install, "context_id": ctx_a,
               "request_id": "req-1014-3", "confirm_token": confirm_token,
               "message": "chat-1014-3a", "token": token})
    };
    // An agent can never mint its own confirm receipt (operator-only).
    let agent_mint: Value = lane.rpc(
        &w.daemon.state,
        "app_record_csv_confirm",
        json!({"install_id": install, "context_id": ctx_a,
               "preview_token": token_preview, "request_id": "req-forged-mint",
               "csv_text": CSV, "decisions": [],
               "decisions_digest": cadence_agent::store::app_records::csv_decisions_digest(&json!([])).unwrap()}),
    );
    assert_eq!(
        agent_mint["ok"], false,
        "agent minted its own CSV confirm: {agent_mint}"
    );
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
            // A forged nonce field is a transport field now; the field
            // name itself is allowed but a wrong nonce refuses.
            p["confirm_token"] = json!("confirm-forged");
            p
        },
        {
            let mut p = base();
            // csv_text is no longer an assistant field — carrying it is
            // an unsupported-fields refusal (bytes never ride the wire).
            p["csv_text"] = json!(CSV);
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
                       "request_id": "req-op", "confirm_token": confirm_token,
                       "message": "chat-1014-3a", "token": token}),
            )
            .is_err(),
        "operator reached the assistant redeem"
    );
}

/// Email creation from scoped chat — the COMPOSER-FREE initial-draft
/// path root requires: no operator mint, no request id. The verified
/// turn IS the request; `app_content_assistant_draft` derives the
/// campaign source host-side (0 on a fresh campaign, live revision on
/// an existing one), produces an inert `pending`/`assistant-receipt`
/// proposal, and enforces one turn = one draft. No send, no approval —
/// Apply stays the operator's.
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

    // Fresh campaign, NO manual mint — the agent drafts directly and
    // the host derives source_revision 0 (no doc exists).
    let proposed: Value = lane.rpc(
        &w.daemon.state,
        "app_content_assistant_draft",
        json!({"install_id": install, "context_id": context_id, "campaign_id": "welcome-1",
               "proposal_id": "prop-1014-e1", "subject": "Welcome, {{first_name|friend}}",
               "preheader": "A note", "blocks": blocks(),
               "message": "chat-1014-4", "token": token}),
    );
    assert_eq!(proposed["ok"], true, "{proposed}");
    let proposal = &proposed["result"]["proposal"];
    assert_eq!(proposal["state"], "pending");
    assert_eq!(proposal["actor"], "assistant");
    assert_eq!(proposal["origin"], "assistant-receipt");
    assert_eq!(proposal["source_revision"], 0);
    // One turn = one draft: a second draft under the SAME message
    // refuses — the unique per-message claim holds.
    let second: Value = lane.rpc(
        &w.daemon.state,
        "app_content_assistant_draft",
        json!({"install_id": install, "context_id": context_id, "campaign_id": "welcome-2",
               "proposal_id": "prop-1014-e2", "subject": "Other",
               "preheader": "A note", "blocks": blocks(),
               "message": "chat-1014-4", "token": token}),
    );
    assert_eq!(second["ok"], false, "one turn minted two drafts: {second}");
}

/// The composer-free draft also covers a REPLACEMENT revision: a scoped
/// turn on a campaign that already has a draft produces a proposal at
/// the host-derived live source_revision — never agent text. The saved
/// content doc is untouched; the proposal stays inert pending.
#[test]
fn cad1014_scoped_chat_email_draft_binds_live_revision() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1014-6");
    let context_id = context["id"].as_str().unwrap();
    // An operator-saved draft at revision 1 already exists.
    w.daemon
        .operator_rpc(
            "app_content_save",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1",
                   "subject": "Spring launch", "preheader": "News", "blocks": blocks()}),
        )
        .unwrap();

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-chat", "claude", None, lane.pid());
    let token = w.chat_turn("crm-chat", install, context_id, "chat-1014-6");
    let proposed: Value = lane.rpc(
        &w.daemon.state,
        "app_content_assistant_draft",
        json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1",
               "proposal_id": "prop-1014-e3", "subject": "Revised launch",
               "preheader": "A note", "blocks": blocks(),
               "message": "chat-1014-6", "token": token}),
    );
    assert_eq!(proposed["ok"], true, "{proposed}");
    // Host-derived live revision 1 — never agent text; doc unchanged.
    assert_eq!(proposed["result"]["proposal"]["source_revision"], 1);
    assert_eq!(proposed["result"]["proposal"]["state"], "pending");
}

/// The required segment-preview acceptance: a scoped turn reads a
/// bounded membership preview (counts + a small sample) over a SAVED
/// segment — never the full member list, never a freeze or send. And
/// the agent's inert pending draft is discoverable in the campaign's
/// proposal list BEFORE the operator applies it.
#[test]
fn cad1014_scoped_chat_segment_preview_and_draft_discoverable() {
    let w = Crm::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1014-7");
    let context_id = context["id"].as_str().unwrap();
    // Two customers so the preview has membership to count/sample —
    // the customer profile grammar is `{schema, display_name, email,
    // consent:{email}}`; a `consent.email:"granted"` counts as opted-in.
    for id in ["cust-a", "cust-b"] {
        w.daemon
            .operator_rpc(
                "app_record_create",
                json!({"install_id": install, "context_id": context_id, "record_id": id,
                       "profile": {"schema":1, "email": format!("{id}@ex.com"), "display_name": id,
                                   "tags": [], "consent": {"email":"granted"}}}),
            )
            .unwrap();
    }
    // A saved segment (operator-saved, predicated on consent_email).
    w.daemon
        .operator_rpc(
            "app_segment_save",
            json!({"install_id": install, "context_id": context_id, "segment_id": "engaged",
                   "name": "Engaged", "predicates": [{"field":"consent_email","op":"eq","value":"granted"}]}),
        )
        .unwrap();

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "crm-chat", "claude", None, lane.pid());
    let token = w.chat_turn("crm-chat", install, context_id, "chat-1014-7");

    // Bounded membership preview over the saved segment.
    let preview: Value = lane.rpc(
        &w.daemon.state,
        "app_segment_assistant_preview",
        json!({"install_id": install, "context_id": context_id, "segment_id": "engaged",
               "message": "chat-1014-7", "token": token}),
    );
    assert_eq!(preview["ok"], true, "{preview}");
    let body = &preview["result"];
    assert_eq!(body["base_count"].as_i64(), Some(2), "{body}");
    assert_eq!(body["final_count"].as_i64(), Some(2), "{body}");
    // A bounded sample, never the full member list.
    assert!(body["sample"].is_array());
    // No send/freeze — a read only; the segment revision is unchanged.

    // An inert draft is discoverable in the campaign's proposal list.
    let token2 = w.chat_turn("crm-chat", install, context_id, "chat-1014-8");
    let drafted: Value = lane.rpc(
        &w.daemon.state,
        "app_content_assistant_draft",
        json!({"install_id": install, "context_id": context_id, "campaign_id": "welcome-9",
               "proposal_id": "prop-1014-e9", "subject": "Hi", "preheader": "P",
               "blocks": blocks(), "message": "chat-1014-8", "token": token2}),
    );
    assert_eq!(drafted["ok"], true, "{drafted}");
    // Discoverable: list by campaign surfaces the pending proposal.
    let listed: Value = lane.rpc(
        &w.daemon.state,
        "app_content_assistant_proposals",
        json!({"install_id": install, "context_id": context_id, "campaign_id": "welcome-9",
               "message": "chat-1014-8", "token": token2}),
    );
    assert_eq!(listed["ok"], true, "{listed}");
    let ids: Vec<&str> = listed["result"]["proposals"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|p| p["proposal_id"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        ids.contains(&"prop-1014-e9"),
        "draft not discoverable: {listed}"
    );
    // And shown directly by proposal id.
    let shown: Value = lane.rpc(
        &w.daemon.state,
        "app_content_assistant_proposal_show",
        json!({"install_id": install, "context_id": context_id, "proposal_id": "prop-1014-e9",
               "message": "chat-1014-8", "token": token2}),
    );
    assert_eq!(shown["ok"], true, "{shown}");
    assert_eq!(shown["result"]["proposal"]["state"], "pending");

    // The operator's BEFORE-APPLY preview renders the pending proposal
    // through the same safe render_html/text — inert, preview-only,
    // send_ready always false. No apply/save/approve/send ran.
    let rendered: Value = w
        .daemon
        .operator_rpc(
            "app_content_proposal_render",
            json!({"install_id": install, "context_id": context_id, "proposal_id": "prop-1014-e9"}),
        )
        .unwrap();
    let render = &rendered["render"];
    assert_eq!(render["proposal_id"], "prop-1014-e9");
    assert_eq!(render["state"], "pending");
    assert_eq!(render["send_ready"], false);
    assert_eq!(render["preview_only"], true);
    // The proposal body text made it into the render — the operator
    // reads exactly what they'd apply.
    assert!(
        render["html"].as_str().unwrap_or("").contains("Hello")
            || render["text"].as_str().unwrap_or("").contains("Hello"),
        "render dropped the proposal body: {render}"
    );
}
