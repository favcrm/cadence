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
                    // v9: `(toolkit, destination_id)` drive the local→AOS
                    // `connectionId` resolution before upload.
                    "destination_id",
                    "toolkit",
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
                "social_publish_cancel" => &["intent_id", "install_id", "context_id"],
                "social_publish_show" => &["intent_id"],
                "social_publish_list" => &["install_id", "context_id"],
                "social_publish_claim_due" => &["now_epoch", "recheck"],
                "social_publish_send_now" => &["intent_id"],
                "social_publish_reconcile" => &["intent_id"],
                "social_publish_report" => &["intent_id", "decision", "receipt"],
                _ => return Err(Error::rejected("unknown social publish method")),
            },
        )?;
        match method {
            "social_publish_media_import" => self.import_social_media(params),
            "social_publish_schedule" => self.schedule_social_publish(params),
            // CAD-1027: cancel is scoped — the intent's own install and
            // exact context (strict: a non-string context refuses).
            "social_publish_cancel" => self.store.social_publish_cancel(
                required_str(params, "intent_id")?,
                Self::required_segment(params, "install_id")?,
                Self::strict_optional_segment(params, "context_id")?,
            ),
            "social_publish_show" => self
                .store
                .social_publish_show(required_str(params, "intent_id")?),
            "social_publish_list" => self.store.social_publish_list(
                optional_str(params, "install_id"),
                optional_str(params, "context_id"),
            ),
            "social_publish_claim_due" => self.claim_social_publish(params),
            "social_publish_send_now" => self.send_now_social_publish(params),
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
        // The local custody `conn-<uuid4>` (install/custody identity) and the
        // send intent's `(toolkit, destination_id)` select the remote AOS
        // `connectionId` — the upstream wire identity the key must bind.
        let _local_connection_id = material["binding"]["config"]["connection_id"]
            .as_str()
            .ok_or_else(|| Error::rejected("reviewed binding names no connection"))?;
        let toolkit = common("toolkit")?;
        let destination_id = common("destination_id")?;
        // v9: resolve local→AOS `connectionId` under the read credential. The
        // resolver never asserts workspace — the send credential's workspace is
        // enforced upstream when the door mints the key and when the grant is
        // authorized. 0 matches → `grant_binding_mismatch`; >1/full-window →
        // `capability_unavailable`; unreachable → `capability_unavailable`. A
        // caller/operator-supplied `connectionId` is never trusted.
        let resolver = self.social_media_resolver.clone().ok_or_else(|| {
            Error::rejected("capability_unavailable: no media resolver configured")
        })?;
        let resolved = resolver
            .resolve(toolkit, destination_id)
            .into_result()
            .map_err(|refusal| Error::rejected(refusal.to_string()))?;
        // Read the retained bytes by receipt custody — never a caller
        // path/URL — and re-verify the digest matches the reviewed asset.
        let db_path = self.state_dir.join("cadence.sqlite3");
        let (_receipt, bytes) =
            crate::store::app_capabilities::read_asset_material(&db_path, receipt_id)?;
        // The importer re-verifies jpeg/png signature + the 2 MiB bound and
        // recomputes the digest; the door's receipt must echo all of it. The
        // resolved remote AOS `connectionId` is sent — never the local
        // `conn-<uuid4>` — so the minted `dp1.<ws>.<aos_conn>.<digest>` is the
        // upstream wire binding (the send credential's workspace scopes it).
        let importer = self.social_media_importer.clone().ok_or_else(|| {
            Error::rejected("capability_unavailable: no media importer configured")
        })?;
        let receipt = importer
            .import(&resolved.aos_connection_id, media_type, &bytes)
            .map_err(|refusal| Error::rejected(refusal.to_string()))?;
        Ok(json!({
            "ok": true,
            "media_key": receipt.media_key,
            "image_digest": receipt.digest,
            // The wire identity — the remote AOS `connectionId`, not the local
            // `conn-<uuid4>` custody id.
            "connection_id": receipt.connection_id,
            "aos_connection_id": resolved.aos_connection_id,
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
        let toolkit = common("toolkit")?;
        let destination_id = common("destination_id")?;
        let media_key = optional_str(params, "media_key");
        // Backend refusal order is pinned by tests (run resolution precedes
        // approval/binding): surface `unknown app run` before any
        // `capability_unavailable` from a missing resolver. Re-prove the
        // reviewed material first — the same pure store call freeze makes.
        self.store.app_publication_material(
            common("run_id")?,
            common("artifact_id")?,
            common("bundle_digest")?,
            common("slot")?,
        )?;
        // v9: the wire identity on `frozen` is the remote AOS `connectionId`,
        // resolved under the read credential — never the local `conn-<uuid4>`
        // custody id and never a caller-supplied id. A supplied `media_key`
        // requires resolution (the key's `parts[2]` is the AOS id); a
        // text-only schedule without a key still resolves so the AOS wire id
        // is bound at freeze for the send path. Resolver absent/failed →
        // `capability_unavailable` (fail closed).
        let resolver = self.social_media_resolver.clone().ok_or_else(|| {
            Error::rejected("capability_unavailable: no media resolver configured")
        })?;
        let resolved = resolver
            .resolve(toolkit, destination_id)
            .into_result()
            .map_err(|refusal| Error::rejected(refusal.to_string()))?;
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
                destination_id,
                toolkit,
                aos_connection_id: &resolved.aos_connection_id,
                media_key,
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
        // v9: `aos_connection_id` is the wire identity compared at recheck —
        // the local `connection_id` stays custody, never sent on the wire.
        let matches = [
            "grant_id",
            "aos_connection_id",
            "destination_id",
            "caption_digest",
        ]
        .iter()
        .all(|field| recheck.get(*field) == frozen.get(*field))
            && recheck.get("image_digest") == frozen.get("image_digest");
        if !matches {
            // Claim the same row the peek read — the candidate id pins
            // the claim so a queue-head move can't hold a row the
            // operator never rechecked.
            let want = due["intent"]["intent_id"].as_str().unwrap_or("").to_owned();
            let Some(claimed) = self
                .store
                .social_publish_claim_due(now, move |_, candidate, _| Ok(candidate == want))?
            else {
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
        let want = due["intent"]["intent_id"].as_str().unwrap_or("").to_owned();
        let claimed = self
            .store
            .social_publish_claim_due(now, move |_, candidate, frozen| {
                Ok(candidate == want && frozen == &expected)
            })?;
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
        self.dispatch_claimed(&id, claimed)
    }

    /// CAD-1041: the one claim-execution path, shared by the operator
    /// `claim_due` RPC above and `send_now`. Callers must have already
    /// proven dispatch authority (operator recheck, or send-now's own
    /// preflight + `material_current`); this helper runs the exact
    /// frozen binding through the registered sender and persists the
    /// provider's evidence before any report. Without a sender the
    /// intent stays processing — posted reports require evidence.
    pub(crate) fn dispatch_claimed(&self, id: &str, claimed: Value) -> Result<Value> {
        let Some(sender) = self.social_publish_sender.clone() else {
            return Ok(claimed);
        };
        let frozen = &claimed["intent"]["frozen"];
        let Some(binding) = sender_binding(frozen, &claimed["intent"]["request"]) else {
            return self.store.social_publish_report(
                id,
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
                    .social_publish_note_evidence(id, &outcome.evidence_json())
            }
            Ok(outcome) => self.store.social_publish_report(
                id,
                "refused",
                &json!({"error": format!("dispatch ended {}", outcome.state.as_str())}),
            ),
            // `nothing_sent` means the POST provably never left (the
            // re-preflight inside execute_request stayed ambiguous) —
            // hold the row for a human decision rather than burning it
            // `refused` on a door blip.
            Err(refusal) if refusal.code == "nothing_sent" => self
                .store
                .social_publish_report(
                    id,
                    "held",
                    &json!({"reason": "staging stayed ambiguous; nothing was sent — retry under a fresh key"}),
                ),
            Err(refusal) => self.store.social_publish_report(
                id,
                "refused",
                &json!({"error": refusal.to_string()}),
            ),
        }
    }

    /// CAD-1041: the operator's explicit "send this queued intent now".
    /// One named row is claimed BY IDENTITY (the candidate-id pin — a
    /// peek/claim head move can never claim a row the operator did not
    /// click), prefight-staged before the claim so a door blip leaves it
    /// queued, dispatched exactly once through `dispatch_claimed`, then
    /// reconciled once via `status` (never a second provider send).
    /// Refuses a row more than `MAX_LATENESS` overdue — re-schedule it
    /// first. There is no background loop: the operator's click is the
    /// only trigger.
    fn send_now_social_publish(&self, params: &Value) -> Result<Value> {
        use crate::platform::agenticos_external::publish::Preflight;
        const MAX_LATENESS_SECS: i64 = 900;
        let id = Self::required_segment(params, "intent_id")?.to_owned();
        let shown = self.store.social_publish_show(&id)?;
        if shown["intent"]["state"] != "queued" {
            return Err(Error::rejected(
                "send-now needs a queued intent — this one already left queued",
            ));
        }
        // The lateness bound reads the due_epoch COLUMN (the same value
        // the claim SQL selects on), never the frozen doc — a forged
        // column can't hide an over-stale row behind a healthy frozen.
        let due = shown["intent"]["due_epoch"].as_i64().unwrap_or(i64::MAX);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        if now - due > MAX_LATENESS_SECS {
            return Err(Error::rejected(
                "intent is more than 15 minutes overdue — re-schedule it before send-now",
            ));
        }
        let Some(sender) = self.social_publish_sender.clone() else {
            return Err(Error::rejected("no dispatch sender registered"));
        };
        // Pre-claim staging on the row the operator named — the exact
        // frozen binding, never a re-typed request. A definitive door
        // refusal is claimed and reported terminal; ambiguity stays
        // queued for a retry.
        let binding = sender_binding(&shown["intent"]["frozen"], &shown["intent"]["request"])
            .ok_or_else(|| Error::rejected("frozen binding does not parse — held for a human"))?;
        match sender.preflight(&binding) {
            Preflight::Approved => {}
            Preflight::Uncertain(refusal) => {
                return Err(Error::rejected(format!(
                    "staging is uncertain ({refusal}); the intent stays queued — retry"
                )));
            }
            Preflight::Refused(refusal) => {
                let want = id.clone();
                if self
                    .store
                    .social_publish_claim_id(&id, move |_, candidate, _| Ok(candidate == want))?
                    .is_some()
                {
                    return self.store.social_publish_report(
                        &id,
                        "refused",
                        &json!({"error": refusal.to_string()}),
                    );
                }
                return Ok(json!({"sent": false, "intent_id": id}));
            }
        }
        // Claim BY IDENTITY: only this exact row transitions. A
        // concurrent send-now or cancel reads state='processing' and
        // gets `claimed: false`; the single CAS inside `claim_id` is
        // what makes a double-click one provider call, not two.
        let want = id.clone();
        let Some(claimed) = self
            .store
            .social_publish_claim_id(&id, move |_, candidate, _| Ok(candidate == want))?
        else {
            return Ok(json!({"sent": false, "intent_id": id, "reason": "no longer queued"}));
        };
        if !self.store.social_publish_material_current(&id)? {
            return self.store.social_publish_report(
                &id,
                "held",
                &json!({"reason": "approved material changed since freeze"}),
            );
        }
        // Exactly one provider send. `dispatch_claimed` notes posted
        // evidence upstream but the row still reads `processing` — the
        // operator path reports the outcome itself; send-now does the
        // same with the daemon-observed evidence (a posted or refused
        // upstream, verified against frozen inside `report`). When the
        // send left the row processing (lost response), one status
        // reconcile refreshes evidence first — never a second send.
        self.dispatch_claimed(&id, claimed)?;
        let after = self.store.social_publish_show(&id)?;
        if after["intent"]["state"] != "processing" {
            return Ok(after);
        }
        let evidence = after["intent"]["upstream"].clone();
        if evidence["state"] == "posted" || evidence["state"] == "refused" {
            let decision = evidence["state"].as_str().unwrap_or("").to_owned();
            let receipt = if decision == "posted" {
                evidence.clone()
            } else {
                json!({"error": "dispatch refused — see upstream evidence"})
            };
            return self.store.social_publish_report(&id, &decision, &receipt);
        }
        // Still processing and no settled upstream: one status read.
        let key = after["intent"]["request"].as_str().unwrap_or("").to_owned();
        let Ok(outcome) = sender.status(&key) else {
            return Ok(after);
        };
        let settled = self
            .store
            .social_publish_note_evidence(&id, &outcome.evidence_json())?;
        let evidence = settled["intent"]["upstream"].clone();
        if evidence["state"] == "posted" {
            return self.store.social_publish_report(&id, "posted", &evidence);
        }
        Ok(settled)
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
    // v9: `aos_connection_id` is the wire identity sent as `connectionId`/
    // `grant.connectionId` and compared by `check_material` (`parts[2]`).
    // A pre-v9 `frozen` without it is held, never sent under a local id.
    let binding = SendBinding {
        key: request.as_str()?.to_owned(),
        connection_id: frozen["aos_connection_id"].as_str()?.to_owned(),
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
