//! CAD-771 slice 2: operator RPC for scheduled external posts.
//!
//! The board previews the frozen intent (`show`/`list` envelopes carry the
//! exact destination, digests, due time and state), the human approves by
//! scheduling with an approval identity, then chooses Post now
//! (`due_epoch` at now) or Schedule (a future `due_epoch` with timezone).
//! Cancellation is operator-only before dispatch. Dispatch claims recheck
//! operator authority field-for-field and re-prove approved material;
//! store already enforces the lifecycle (`pub(crate)` claim/report).

use super::app_bindings_rpc::strict_fields;
use super::*;

impl Shared {
    pub(super) fn rpc_social_publish(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        pid: u32,
    ) -> Result<Value> {
        self.operator_connection("social publish schedule", params, pid)?;
        strict_fields(
            params,
            match method {
                "social_publish_schedule" => &[
                    "request_id",
                    "install_id",
                    "context_id",
                    "run_id",
                    "effect_id",
                    "artifact_id",
                    "bundle_digest",
                    "slot",
                    "destination_id",
                    "toolkit",
                    "media_key",
                    "grant_id",
                    "approval_id",
                    "due_epoch",
                    "timezone",
                ],
                "social_publish_cancel" => &["intent_id"],
                "social_publish_show" => &["intent_id"],
                "social_publish_list" => &["install_id", "context_id"],
                "social_publish_claim_due" => &["now_epoch", "recheck"],
                "social_publish_report" => &["intent_id", "decision", "receipt"],
                _ => return Err(Error::rejected("unknown social publish method")),
            },
        )?;
        match method {
            "social_publish_schedule" => self.schedule_social_publish(params),
            "social_publish_cancel" => self
                .store
                .social_publish_cancel(required_str(params, "intent_id")?),
            "social_publish_show" => self
                .store
                .social_publish_show(required_str(params, "intent_id")?),
            "social_publish_list" => self.store.social_publish_list(
                optional_str(params, "install_id"),
                optional_str(params, "context_id"),
            ),
            "social_publish_claim_due" => self.claim_social_publish(params),
            "social_publish_report" => self.store.social_publish_report(
                required_str(params, "intent_id")?,
                required_str(params, "decision")?,
                params
                    .get("receipt")
                    .ok_or_else(|| Error::rejected("Missing 'receipt'"))?,
            ),
            _ => Err(Error::rejected("unknown social publish method")),
        }
    }

    /// Schedule freezes from the approved run's reviewed material only:
    /// every digest and the connection derive server-side (explicit
    /// caller-frozen digests are refused — no caller-string trust at
    /// either point). Post now is `due_epoch` at now; Schedule is a
    /// future `due_epoch` with an explicit timezone.
    fn schedule_social_publish(&self, params: &Value) -> Result<Value> {
        use crate::store::social_publish::FreezeFromArtifact;
        let due = params
            .get("due_epoch")
            .and_then(Value::as_i64)
            .ok_or_else(|| Error::rejected("Missing or non-integer 'due_epoch'"))?;
        let common = |field: &str| required_str(params, field);
        self.store
            .social_publish_freeze_from_artifact(&FreezeFromArtifact {
                request_id: common("request_id")?,
                install_id: common("install_id")?,
                context_id: optional_str(params, "context_id"),
                run_id: common("run_id")?,
                artifact_id: common("artifact_id")?,
                bundle_digest: common("bundle_digest")?,
                slot: common("slot")?,
                effect_id: common("effect_id")?,
                destination_id: common("destination_id")?,
                toolkit: common("toolkit")?,
                media_key: optional_str(params, "media_key"),
                grant_id: common("grant_id")?,
                approval_id: common("approval_id")?,
                due_epoch: due,
                timezone: common("timezone")?,
            })
    }

    /// Dispatch claim with recheck: the operator supplies current authority
    /// and the daemon compares it field-for-field against frozen before
    /// claiming. Artifact-frozen intents additionally re-prove the approved
    /// material is unchanged (stale binding or changed review holds even
    /// when the operator recheck matches). On any mismatch the intent is
    /// claimed and immediately held for a new human decision - never
    /// silently published. Backend grant liveness wires at the provider.
    fn claim_social_publish(&self, params: &Value) -> Result<Value> {
        let now = params
            .get("now_epoch")
            .and_then(Value::as_i64)
            .ok_or_else(|| Error::rejected("Missing or non-integer 'now_epoch'"))?;
        let recheck = params
            .get("recheck")
            .and_then(Value::as_object)
            .ok_or_else(|| Error::rejected("Missing or non-object 'recheck'"))?;
        let Some(due) = self.store.social_publish_peek_due(now)? else {
            return Ok(json!({"claimed": false}));
        };
        let frozen = &due["intent"]["frozen"];
        let matches = [
            "grant_id",
            "connection_id",
            "destination_id",
            "caption_digest",
        ]
        .iter()
        .all(|field| recheck.get(*field) == frozen.get(*field))
            && recheck.get("image_digest") == frozen.get("image_digest");
        if !matches {
            let Some(claimed) = self.store.social_publish_claim_due(now, |_, _| Ok(true))? else {
                return Ok(json!({"claimed": false}));
            };
            let id = claimed["intent"]["intent_id"].as_str().unwrap_or("");
            return self.store.social_publish_report(
                id,
                "held",
                &json!({"reason": "dispatch authority differs from frozen approval"}),
            );
        }
        let expected = frozen.clone();
        let claimed = self
            .store
            .social_publish_claim_due(now, move |_, frozen| Ok(frozen == &expected))?;
        let Some(claimed) = claimed else {
            return Ok(json!({"claimed": false}));
        };
        // Daemon-side re-proof: a stale operator recheck must not dispatch
        // against changed approved material. Mismatch holds the just-claimed
        // intent for a new human decision.
        let id = claimed["intent"]["intent_id"].as_str().unwrap_or("");
        if !self.store.social_publish_material_current(id)? {
            return self.store.social_publish_report(
                id,
                "held",
                &json!({"reason": "approved material changed since freeze"}),
            );
        }
        Ok(claimed)
    }
}
