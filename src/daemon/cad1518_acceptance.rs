//! Independent CAD-1518 refusal acceptance check, written by the reviewer
//! from the ticket's acceptance (not by the implementer; AGENTS.md "Gates
//! and security work"). The implementer must not edit or weaken this file.
//!
//! Rule under test: a registered host action, or a scoped chat verb mapped
//! to one, runs for an installation only if that installation's consented
//! bundle declares it. Nothing is decided by app name.
//!
//! Everything goes through the real `Shared::dispatch` on a live scoped
//! chat turn (connection-derived agent, running message under a current
//! turn token, verified App stamp, the installation's own conversation).
//! The bad installation is a non-CRM package (`crm-lite`) whose consented
//! `app-assistant.json` omits `segments.save` and whose `app-chat.json`
//! declares no `csv-import`. Each refusal is paired with a positive control
//! in the same fixture so the check cannot pass vacuously:
//! - the same bad installation's declared `segments.list` verb succeeds on
//!   the same kind of turn, so only the declaration refuses the save;
//! - the CRM fixture, which declares `segments.save`, saves on the same RPC.
#![cfg(feature = "test-seam")]

use super::*;
use crate::store::app_records::RecordStore;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const CRM_SOURCE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/apps/crm");
const MASTER_GENERATION: &str = "0123456789abcdef0123456789abcdef";

struct Fx {
    dir: tempfile::TempDir,
    shared: Arc<Shared>,
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn refused(result: Result<Value>, needle: &str, what: &str) {
    match result {
        Ok(value) => panic!("{what} was accepted: {value}"),
        Err(error) => {
            let text = error.to_string();
            assert!(
                text.contains(needle),
                "{what} refused for the wrong reason (want '{needle}'): {text}"
            );
        }
    }
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("c1518").tempdir().unwrap();
        let pm = dir.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let cwd = dir.path().to_str().unwrap();
        shared
            .store
            .register_agent(&NewAgent {
                alias: "master",
                provider: "pi",
                endpoint_kind: "managed",
                role: "worker",
                cwd,
                sandbox: "read-only",
                instructions: None,
                params: Some(r#"{"model":"fixture/model"}"#),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        crate::test_seam::seed_pi_fixture_policy(&shared.store, "fixture/model").unwrap();
        shared
            .store
            .set_identity_with_quota(
                "master",
                &crate::adapter::Identity {
                    thread_id: "cad1518-thread".into(),
                    session_id: "cad1518-session".into(),
                    model: None,
                    effort: None,
                    pid: std::process::id(),
                    endpoint: None,
                    generation: Some(MASTER_GENERATION.into()),
                    attach: None,
                },
                None,
            )
            .unwrap();
        Self { dir, shared }
    }

    fn call(&self, who: Asserted, method: &str, params: Value) -> Result<Value> {
        scoped(who, || {
            self.shared.dispatch(method, &params, std::process::id())
        })
    }

    fn operator(&self, method: &str, params: Value) -> Value {
        self.call(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }

    fn agent(&self, method: &str, params: Value) -> Result<Value> {
        self.call(Asserted::Agent("master".into()), method, params)
    }

    /// A non-CRM package built from the CRM fixture: renamed app, its
    /// assistant descriptor without `segments.save`, and no chat
    /// attachments (so no `csv-import`).
    fn lite_package(&self) -> PathBuf {
        let app = self.dir.path().join("crm-lite");
        copy_dir(Path::new(CRM_SOURCE), &app);
        let manifest = std::fs::read_to_string(app.join("app.md")).unwrap();
        let renamed = manifest.replacen("app: crm\n", "app: crm-lite\n", 1);
        assert_ne!(manifest, renamed, "manifest rename applied");
        std::fs::write(
            app.join("app.md"),
            renamed.replacen("title: CRM\n", "title: CRM Lite\n", 1),
        )
        .unwrap();
        let mut assistant: Value =
            serde_json::from_str(&std::fs::read_to_string(app.join("app-assistant.json")).unwrap())
                .unwrap();
        assistant["app"] = json!("crm-lite");
        let actions = assistant["actions"].as_array_mut().unwrap();
        let before = actions.len();
        actions.retain(|a| a["id"] != "segments.save");
        assert_eq!(actions.len() + 1, before, "segments.save removed");
        assert!(
            actions.iter().any(|a| a["id"] == "segments.list"),
            "the lite package still declares segments.list"
        );
        std::fs::write(
            app.join("app-assistant.json"),
            serde_json::to_string_pretty(&assistant).unwrap(),
        )
        .unwrap();
        let mut chat: Value =
            serde_json::from_str(&std::fs::read_to_string(app.join("app-chat.json")).unwrap())
                .unwrap();
        chat["app"] = json!("crm-lite");
        chat["attachments"] = json!([]);
        chat.as_object_mut().unwrap().remove("directives");
        std::fs::write(
            app.join("app-chat.json"),
            serde_json::to_string_pretty(&chat).unwrap(),
        )
        .unwrap();
        app
    }

    /// Install and consent `source`; returns the install and one context.
    fn install(&self, source: &Path, request: &str) -> (String, String) {
        let installed = self.operator(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        );
        let install = installed["install_id"].as_str().unwrap().to_string();
        let approved = self.call(
            Asserted::Operator,
            "app_local_install_approve",
            json!({"install_id": install, "digest": installed["digest"]}),
        );
        if let Err(error) = approved {
            // An app whose digest is already approved answers so.
            assert!(
                !error.to_string().contains("operator"),
                "approve refused: {error}"
            );
        }
        let config = crate::store::app_contexts::ContextConfig::new(
            "CAD-1518 acceptance",
            std::collections::BTreeMap::new(),
        )
        .unwrap();
        let context = self
            .shared
            .store
            .app_context_create(&install, &config, request)
            .unwrap()["context"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        (install, context)
    }

    /// A live scoped chat turn of `master` in `install`'s conversation.
    fn turn(&self, install: &str, context: &str, message: &str) -> String {
        self.operator(
            "conversation_create",
            json!({"alias":"master", "install_id":install, "context_id":context, "general":true}),
        );
        self.operator(
            "thread_send",
            json!({"alias":"master", "text":"Save a VIP segment", "message":message,
                "app":{"install_id":install,"context_id":context}}),
        );
        let token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(MASTER_GENERATION);
        rusqlite::Connection::open(crate::rollout::db_file(self.dir.path()))
            .unwrap()
            .execute(
                "UPDATE messages SET state='submitting', started=1.0 WHERE id=?",
                [message],
            )
            .unwrap();
        self.shared.store.mark_running(message, &token).unwrap();
        token
    }

    fn segment_save(
        &self,
        install: &str,
        context: &str,
        message: &str,
        token: &str,
        segment: &str,
    ) -> Result<Value> {
        self.agent(
            "app_segment_assistant_save",
            json!({"install_id":install,"context_id":context,"segment_id":segment,
                "name":"VIP","predicates":[{"field":"tag","op":"eq","value":"vip"}],
                "message":message,"token":token}),
        )
    }

    fn segments(&self, install: &str, context: &str) -> Vec<Value> {
        RecordStore::open(self.dir.path(), install)
            .unwrap()
            .app_segment_list(context)
            .unwrap()["segments"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
}

#[test]
fn cad1518_undeclared_scoped_verbs_and_actions_are_refused_at_the_real_rpc() {
    let fx = Fx::new();
    let lite_source = fx.lite_package();
    let (lite, lite_ctx) = fx.install(&lite_source, "c1518-lite");
    let (crm, crm_ctx) = fx.install(Path::new(CRM_SOURCE), "c1518-crm");

    // Positive control on the BAD installation: its declared
    // `segments.list` verb runs on a live scoped turn, so the turn token,
    // stamp, conversation, context and consent proofs all pass for it.
    let token = fx.turn(&lite, &lite_ctx, "m-lite-list");
    let listed = fx
        .agent(
            "app_segment_assistant_list",
            json!({"install_id":lite,"context_id":lite_ctx,"message":"m-lite-list","token":token}),
        )
        .unwrap_or_else(|e| panic!("declared segments.list must run: {e}"));
    assert!(listed.is_object(), "{listed}");

    // 1. Bad case: the non-CRM installation never declared `segments.save`;
    //    its own live turn calling `segment-assistant-save` is refused and
    //    nothing is written.
    let token = fx.turn(&lite, &lite_ctx, "m-lite-save");
    refused(
        fx.segment_save(&lite, &lite_ctx, "m-lite-save", &token, "seg-vip"),
        "is not declared by this installation",
        "undeclared segment-assistant-save",
    );
    assert!(
        RecordStore::open(fx.dir.path(), &lite)
            .unwrap()
            .app_segment_show(&lite_ctx, "seg-vip")
            .is_err(),
        "a refused save must not store the segment"
    );
    assert!(
        fx.segments(&lite, &lite_ctx).is_empty(),
        "a refused save wrote a segment"
    );

    // 2. The same undeclared action through the generic invoke path is
    //    refused as well, and writes nothing.
    let token = fx.turn(&lite, &lite_ctx, "m-lite-invoke");
    refused(
        fx.agent(
            "app_assistant_invoke",
            json!({"install_id":lite,"context_id":lite_ctx,"message":"m-lite-invoke",
                "token":token,"action_id":"segments.save","operation_id":"op-lite-save",
                "input":{"segment_id":"seg-vip","name":"VIP",
                    "predicates":[{"field":"tag","op":"eq","value":"vip"}]}}),
        ),
        "not declared by this consented installation",
        "undeclared segments.save invoke",
    );
    assert!(fx.segments(&lite, &lite_ctx).is_empty());

    // 3. The CSV verbs need the `csv-import` attachment, which the lite
    //    package does not declare.
    let token = fx.turn(&lite, &lite_ctx, "m-lite-csv");
    refused(
        fx.agent(
            "app_record_csv_assistant_preview",
            json!({"install_id":lite,"context_id":lite_ctx,"message":"m-lite-csv",
                "token":token,"csv_text":"email,name\na@example.com,A\n"}),
        ),
        "is not declared by this installation",
        "undeclared csv-assistant-preview",
    );

    // Positive control on the same RPC: the CRM fixture declares
    // `segments.save`, and its live turn saves the segment.
    let token = fx.turn(&crm, &crm_ctx, "m-crm-save");
    fx.segment_save(&crm, &crm_ctx, "m-crm-save", &token, "seg-vip")
        .unwrap_or_else(|e| panic!("declared segments.save must run: {e}"));
    assert_eq!(fx.segments(&crm, &crm_ctx).len(), 1);
    assert!(fx.segments(&lite, &lite_ctx).is_empty());
}
