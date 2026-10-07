//! CAD-1184 revision acceptance: the per-message action claim must cover
//! the GENERIC `segments.save` path exactly as it covers the retained
//! dedicated `app_segment_assistant_save` verb, and the two paths must
//! share one claim key (the scoped chat message). Authored from the
//! ticket's "do not bypass or weaken existing special-verb one-message
//! claims" requirement and the PM's REVISE disposition on the
//! independent spec/security review of head
//! e3518bb6cfd8fc1e30a2f104b06c328753e6b6e8. Written against the real
//! `Shared::dispatch`/`scoped_chat_assistant` guard under the test-seam;
//! no fake assistant API or fake claim store. Implementers must not edit
//! or weaken this file.
//!
//! This module is not compiled until the PM adds
//! `#[cfg(all(test, feature = "test-seam"))] mod cad1184_revision_acceptance;`
//! in `src/daemon.rs`; the baseline expectation is failure/refusal where
//! the check asserts a refusal that the generic path currently skips.
#![cfg(feature = "test-seam")]

use super::*;
use crate::store::app_records::RecordStore;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::sync::Arc;

const CRM_SOURCE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/workspace-apps/crm");
const MASTER_GENERATION: &str = "0123456789abcdef0123456789abcdef";

struct Fx {
    dir: tempfile::TempDir,
    shared: Arc<Shared>,
    install: String,
    context: String,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("c1184rev")
            .tempdir()
            .unwrap();
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
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        shared
            .store
            .set_identity_with_quota(
                "master",
                &crate::adapter::Identity {
                    thread_id: "cad1184rev-thread".into(),
                    session_id: "cad1184rev-session".into(),
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
        let installed = scoped(Asserted::Operator, || {
            shared.dispatch(
                "app_workspace_install",
                &json!({"source": CRM_SOURCE}),
                std::process::id(),
            )
        })
        .unwrap();
        let install = installed["install_id"].as_str().unwrap().to_owned();
        let config = crate::store::app_contexts::ContextConfig::new(
            "Revision acceptance",
            std::collections::BTreeMap::new(),
        )
        .unwrap();
        let context = shared
            .store
            .app_context_create(&install, &config, "cad1184-rev-context")
            .unwrap()["context"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        Self {
            dir,
            shared,
            install,
            context,
        }
    }

    fn create_turn(&self, message: &str) -> String {
        let params = json!({"alias":"master", "install_id":self.install,
            "context_id":self.context, "general":true});
        scoped(Asserted::Operator, || {
            self.shared
                .dispatch("conversation_create", &params, std::process::id())
        })
        .unwrap();
        let sent = json!({"alias":"master", "text":"Perform the requested CRM action",
            "message":message, "app":{"install_id":self.install,"context_id":self.context}});
        scoped(Asserted::Operator, || {
            self.shared
                .dispatch("thread_send", &sent, std::process::id())
        })
        .unwrap();
        let token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(MASTER_GENERATION);
        self.db()
            .execute(
                "UPDATE messages SET state='submitting', started=1.0 WHERE id=?",
                [message],
            )
            .unwrap();
        self.shared.store.mark_running(message, &token).unwrap();
        token
    }

    fn generic_segment_save(
        &self,
        message: &str,
        token: &str,
        operation: &str,
        input: Value,
    ) -> Result<Value> {
        scoped(Asserted::Agent("master".into()), || {
            self.shared.dispatch(
                "app_assistant_invoke",
                &json!({
                    "install_id":self.install,"context_id":self.context,"message":message,
                    "token":token,"action_id":"segments.save","operation_id":operation,
                    "input":input,
                }),
                std::process::id(),
            )
        })
    }

    /// The retained dedicated segment-assistant-save verb under its own
    /// frozen params — the same live-turn proof fields, no claim fields.
    fn dedicated_segment_save(
        &self,
        message: &str,
        token: &str,
        segment: &str,
        name: &str,
        predicates: Value,
        expected_revision: Option<i64>,
    ) -> Result<Value> {
        let mut params = json!({
            "install_id":self.install,"context_id":self.context,
            "segment_id":segment,"name":name,"predicates":predicates,
            "message":message,"token":token,
        });
        if let Some(revision) = expected_revision {
            params["expected_revision"] = json!(revision);
        }
        scoped(Asserted::Agent("master".into()), || {
            self.shared
                .dispatch("app_segment_assistant_save", &params, std::process::id())
        })
    }

    fn segment(&self, id: &str) -> Result<Value> {
        RecordStore::open(self.dir.path(), &self.install)?.app_segment_show(&self.context, id)
    }

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(crate::rollout::db_file(self.dir.path())).unwrap()
    }

    fn finish(&self, message: &str) {
        self.db()
            .execute(
                "UPDATE messages SET state='completed', started=2.0 WHERE id=?",
                [message],
            )
            .unwrap();
    }
}

/// True only when a generic invoke ended in an honest, known refusal —
/// an `Err` from dispatch, or a durable operation receipt in `denied` or
/// `failed`. `succeeded`, `pending_permission`, `running` and `unknown`
/// are NOT acceptable claim-gate outcomes: the first three mean the write
/// was accepted or queued, and `unknown` means the outcome cannot be
/// trusted, which a pre-execution claim refusal must never produce.
fn refused(result: &Result<Value>, case: &str) {
    match result {
        Err(_) => {}
        Ok(value) => {
            let status = value.pointer("/operation/status").and_then(Value::as_str);
            assert!(
                matches!(status, Some("denied" | "failed")),
                "{case}: expected denial, got {value}"
            );
        }
    }
}

/// One verified scoped chat message redeems exactly one bounded segment
/// write across BOTH entry points. The generic path must claim the
/// message before executing; the claim is spent whether or not the write
/// lands; and the dedicated verb and the generic verb draw on the same
/// claim, so mixing paths on one message still redeems once.
///
/// Arms exercised through the real dispatcher:
///   1. generic `segments.save` on a live turn succeeds (positive control);
///   2. exact same operation-id/input replay returns the same receipt
///      (idempotent, no duplicate write);
///   3. a second DISTINCT segment (new id + operation id) on the SAME
///      message is refused and persists nothing;
///   4. a same-message generic `segments.save` that would REVISE the
///      just-created segment (different name, expected_revision = its
///      live revision) is refused and leaves the stored segment untouched;
///   5. on a fresh message, dedicated-first then generic-second is refused;
///   6. on a fresh message, generic-first then dedicated-second is refused.
#[test]
fn generic_segment_save_shares_the_one_message_claim_with_the_dedicated_verb() {
    let fx = Fx::new();
    let predicates = json!([{"field":"tag","op":"eq","value":"vip"}]);

    // 1. Positive control: a live scoped turn saves one segment.
    let token = fx.create_turn("m-claim-generic-first");
    let first = fx
        .generic_segment_save(
            "m-claim-generic-first",
            &token,
            "op-seg-a",
            json!({"segment_id":"seg-alpha","name":"Alpha","predicates":predicates}),
        )
        .unwrap();
    assert_eq!(
        first["operation"]["status"], "succeeded",
        "generic segment save control must succeed: {first}"
    );
    let stored_alpha = fx.segment("seg-alpha").unwrap();
    assert_eq!(stored_alpha["segment"]["name"], "Alpha");
    assert_eq!(stored_alpha["segment"]["predicates"], predicates);

    // 2. Identical replay returns the stored receipt; nothing re-writes.
    let replay = fx
        .generic_segment_save(
            "m-claim-generic-first",
            &token,
            "op-seg-a",
            json!({"segment_id":"seg-alpha","name":"Alpha","predicates":predicates}),
        )
        .unwrap();
    assert_eq!(
        replay["operation"]["id"], "op-seg-a",
        "exact replay must return the same operation: {replay}"
    );

    // 3. A second DISTINCT segment payload on the same message is a
    //    second intent on a spent claim: refused, and nothing persists.
    let second = fx.generic_segment_save(
        "m-claim-generic-first",
        &token,
        "op-seg-b",
        json!({"segment_id":"seg-beta","name":"Beta","predicates":predicates}),
    );
    refused(&second, "second distinct segment on one message");
    assert!(
        fx.segment("seg-beta").is_err(),
        "refused second segment write persisted seg-beta"
    );

    // 4. Revising the saved segment on the same message (changed name,
    //    live expected_revision) is likewise a second intent: refused,
    //    and seg-alpha keeps its original name/revision.
    let alpha_revision = stored_alpha["segment"]["revision"]
        .as_i64()
        .expect("stored segment revision");
    let revise = fx.generic_segment_save(
        "m-claim-generic-first",
        &token,
        "op-seg-revise",
        json!({"segment_id":"seg-alpha","name":"Renamed","predicates":predicates,"expected_revision":alpha_revision}),
    );
    refused(&revise, "same-message segment revision");
    let after = fx.segment("seg-alpha").unwrap();
    assert_eq!(
        after["segment"]["name"], "Alpha",
        "refused revision renamed the segment: {after}"
    );
    assert_eq!(
        after["segment"]["revision"], stored_alpha["segment"]["revision"],
        "refused revision bumped the segment revision"
    );
    fx.finish("m-claim-generic-first");

    // 5. Cross-path shared claim: dedicated verb first on a FRESH turn,
    //    then a generic save on the same message must refuse.
    let token = fx.create_turn("m-claim-dedicated-first");
    fx.dedicated_segment_save(
        "m-claim-dedicated-first",
        &token,
        "seg-dedicated",
        "Dedicated",
        predicates.clone(),
        None,
    )
    .expect("dedicated segment save control must succeed");
    let generic_after_dedicated = fx.generic_segment_save(
        "m-claim-dedicated-first",
        &token,
        "op-seg-after-dedicated",
        json!({"segment_id":"seg-generic-late","name":"Late","predicates":predicates}),
    );
    refused(
        &generic_after_dedicated,
        "generic save after dedicated claim",
    );
    assert!(
        fx.segment("seg-generic-late").is_err(),
        "generic save slipped past the dedicated verb's claim"
    );
    fx.finish("m-claim-dedicated-first");

    // 6. Reverse order on a fresh message: generic first, then the
    //    dedicated verb must meet the spent claim and refuse.
    let token = fx.create_turn("m-claim-mixed");
    fx.generic_segment_save(
        "m-claim-mixed",
        &token,
        "op-seg-gamma",
        json!({"segment_id":"seg-gamma","name":"Gamma","predicates":predicates}),
    )
    .unwrap();
    let dedicated_after_generic = fx.dedicated_segment_save(
        "m-claim-mixed",
        &token,
        "seg-dedicated-late",
        "DedicatedLate",
        predicates.clone(),
        None,
    );
    assert!(
        dedicated_after_generic.is_err(),
        "dedicated verb redeemed a message the generic path already spent: {dedicated_after_generic:?}"
    );
    assert!(
        fx.segment("seg-dedicated-late").is_err(),
        "dedicated write persisted after a spent claim"
    );
    assert_eq!(
        fx.segment("seg-gamma").unwrap()["segment"]["name"],
        "Gamma",
        "the first legitimate write must stand"
    );
    fx.finish("m-claim-mixed");
}
