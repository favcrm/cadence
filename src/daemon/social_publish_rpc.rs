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
        // WITNESS: skip the operator gate for the media import verb only.
        if method != "social_publish_media_import" {
            self.operator_connection("social publish schedule", params, pid)?;
        }
        strict_fields(
            params,
            match method {
                // CAD-979: the operator requests a media import against the
                // approved run's reviewed retained asset — full provenance
                // triple + scope, never caller bytes/path/URL.
                "social_publish_media_import" => &[
                    "request_id",
                    "install_id",
                    "context_id",
                    "run_id",
                    "artifact_id",
                    "bundle_digest",
                    "slot",
                ],
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
                "social_publish_reconcile" => &["intent_id"],
                "social_publish_report" => &["intent_id", "decision", "receipt"],
                _ => return Err(Error::rejected("unknown social publish method")),
            },
        )?;
        match method {
            "social_publish_media_import" => self.import_social_media(params),
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
            "social_publish_reconcile" => self.reconcile_social_publish(params),
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

    /// Segment-shaped required param: a nonempty, ≤128-byte ASCII id.
    fn required_segment<'a>(params: &'a Value, field: &str) -> Result<&'a str> {
        let value = required_str(params, field)?;
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(Error::rejected(format!("malformed parameter '{field}'")));
        }
        Ok(value)
    }

    /// Strict optional param: absent/null → None; a valid nonempty
    /// segment string → Some; any other JSON type or an empty/oversize
    /// string is a rejection, never a silent None.
    fn strict_optional_segment<'a>(params: &'a Value, field: &str) -> Result<Option<&'a str>> {
        match params.get(field) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => {
                if s.is_empty()
                    || s.len() > 128
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
                {
                    return Err(Error::rejected(format!("malformed parameter '{field}'")));
                }
                Ok(Some(s.as_str()))
            }
            Some(_) => Err(Error::rejected(format!("malformed parameter '{field}'"))),
        }
    }

    /// CAD-979: operator-only retained-media import. Proves the run's
    /// reviewed asset + this request's exact scope, reads the retained bytes
    /// by receipt custody, uploads them to the device media door and returns
    /// the validated `media_key`/`image_digest` for the operator to schedule.
    /// No grant minted, no send, no persisted row — freeze owns durability.
    fn import_social_media(&self, params: &Value) -> Result<Value> {
        // Required request_id: a nonempty, bounded, segment-shaped string,
        // validated BEFORE any custody read or provider call. import writes
        // no durable row, so this is an idempotency/receipt shape bound, not
        // a claimed durable uniqueness (schedule's `request` UNIQUE is).
        let _request_id = Self::required_segment(params, "request_id")?;
        let common = |field: &str| required_str(params, field);
        let request_install = common("install_id")?;
        // I2 scope pin: `context_id` is strict — absent/null → None; a valid
        // nonempty string → Some; a number/object/bool/empty/oversize string
        // refuses rather than silently mapping to None (optional_str would).
        let request_context = Self::strict_optional_segment(params, "context_id")?;
        let run_id = common("run_id")?;
        let artifact_id = common("artifact_id")?;
        let bundle_digest = common("bundle_digest")?;
        let slot = common("slot")?;
        // Reuse the exact provenance re-proof freeze uses: approved completed
        // run, frozen slot, current binding — keyed by run+artifact+bundle+slot.
        let material =
            self.store
                .app_publication_material(run_id, artifact_id, bundle_digest, slot)?;
        // I2 scope pin (E3): the request's install/context must equal the
        // run's OWN scope — `app_publication_material` re-proves only the
        // run's binding against the run's own scope, so a request naming a
        // different install/context would still resolve without this compare.
        // context_id is exact/null-preserving (no wildcard).
        if material["run"]["install_id"].as_str() != Some(request_install)
            || material["run"]["context_id"].as_str() != request_context
        {
            return Err(Error::rejected(
                "grant_binding_mismatch: media import request names a different install or context",
            ));
        }
        // The reviewed retained asset for this run.
        let asset = &material["asset"];
        let receipt_id = asset["receipt_id"]
            .as_str()
            .ok_or_else(|| Error::rejected("run has no reviewed retained asset"))?;
        let media_type = asset["media_type"]
            .as_str()
            .ok_or_else(|| Error::rejected("run has no reviewed retained asset"))?;
        let connection_id = material["binding"]["config"]["connection_id"]
            .as_str()
            .ok_or_else(|| Error::rejected("reviewed binding names no connection"))?
            .to_owned();
        // Read the retained bytes by receipt custody — never a caller
        // path/URL — and re-verify the digest matches the reviewed asset.
        let db_path = self.state_dir.join("cadence.sqlite3");
        let (_receipt, bytes) =
            crate::store::app_capabilities::read_asset_material(&db_path, receipt_id)?;
        // The importer re-verifies jpeg/png signature + the 2 MiB bound and
        // recomputes the digest; the door's receipt must echo all of it.
        let importer = self.social_media_importer.clone().ok_or_else(|| {
            Error::rejected("capability_unavailable: no media importer configured")
        })?;
        let receipt = importer
            .import(&connection_id, media_type, &bytes)
            .map_err(|refusal| Error::rejected(refusal.to_string()))?;
        Ok(json!({
            "ok": true,
            "media_key": receipt.media_key,
            "image_digest": receipt.digest,
            "connection_id": receipt.connection_id,
            "mime": receipt.mime,
            "size_bytes": receipt.size_bytes,
        }))
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
        let id = claimed["intent"]["intent_id"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        if !self.store.social_publish_material_current(&id)? {
            return self.store.social_publish_report(
                &id,
                "held",
                &json!({"reason": "approved material changed since freeze"}),
            );
        }
        // Daemon-observed dispatch: when a sender is registered, the exact
        // frozen binding executes here and its evidence is persisted before
        // any report. Without a sender the intent stays processing until
        // the send adapter lands — posted reports require evidence.
        let Some(sender) = self.social_publish_sender.clone() else {
            return Ok(claimed);
        };
        let frozen = &claimed["intent"]["frozen"];
        let Some(binding) = sender_binding(frozen, &claimed["intent"]["request"]) else {
            return self.store.social_publish_report(
                &id,
                "held",
                &json!({"reason": "frozen binding does not parse for dispatch"}),
            );
        };
        match sender.execute(&binding) {
            Ok(outcome)
                if matches!(
                    outcome.state,
                    crate::platform::agenticos_external::publish::PublishState::Posted
                        | crate::platform::agenticos_external::publish::PublishState::Processing
                ) =>
            {
                self.store
                    .social_publish_note_evidence(&id, &outcome.evidence_json())
            }
            Ok(outcome) => self.store.social_publish_report(
                &id,
                "refused",
                &json!({"error": format!("dispatch ended {}", outcome.state.as_str())}),
            ),
            Err(refusal) => self.store.social_publish_report(
                &id,
                "refused",
                &json!({"error": refusal.to_string()}),
            ),
        }
    }

    /// Reconcile one processing intent against the provider door: refresh
    /// daemon-observed evidence without a second provider call, so a lost
    /// response recovers to posted instead of retrying blind.
    fn reconcile_social_publish(&self, params: &Value) -> Result<Value> {
        let id = required_str(params, "intent_id")?;
        let shown = self.store.social_publish_show(id)?;
        if shown["intent"]["state"] != "processing" {
            return Err(Error::rejected(
                "only a processing intent can be reconciled",
            ));
        }
        let Some(sender) = self.social_publish_sender.clone() else {
            return Err(Error::rejected("no dispatch sender registered"));
        };
        let key = shown["intent"]["request"]
            .as_str()
            .ok_or_else(|| Error::rejected("intent has no stable key"))?;
        let outcome = sender
            .status(key)
            .map_err(|refusal| Error::rejected(refusal.to_string()))?;
        self.store
            .social_publish_note_evidence(id, &outcome.evidence_json())
    }
}

/// The exact frozen binding as the dispatch sender speaks it. `None`
/// when frozen fails its own contract shapes — held, never dispatched.
fn sender_binding(
    frozen: &Value,
    request: &Value,
) -> Option<crate::platform::agenticos_external::publish::SendBinding> {
    use crate::platform::agenticos_external::publish::{SendBinding, Toolkit};
    let binding = SendBinding {
        key: request.as_str()?.to_owned(),
        connection_id: frozen["connection_id"].as_str()?.to_owned(),
        destination_id: frozen["destination_id"].as_str()?.to_owned(),
        toolkit: Toolkit::parse(frozen["toolkit"].as_str()?)?,
        caption_digest: frozen["caption_digest"].as_str()?.to_owned(),
        image_digest: frozen["image_digest"].as_str().map(str::to_owned),
        cadence_run_id: frozen["run_id"].as_str()?.to_owned(),
        cadence_effect_id: frozen["effect_id"].as_str()?.to_owned(),
        grant_id: frozen["grant_id"].as_str()?.to_owned(),
    };
    binding.validate().ok()?;
    Some(binding)
}
