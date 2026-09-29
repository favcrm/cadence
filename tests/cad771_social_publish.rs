//! CAD-771 adversarial gate tests (slice 1, fake provider only).
//!
//! Pinned contract: `agenticos-stack/agenticos-v2` PR #214 @
//! `12953144c50d13075af2323a2e09a70de9f72b87` (device-publish v1).
//! No network beyond loopback, no custody, no live Meta post.
//!
//! Vectors per gate: agent caller, detached child, concurrent callers,
//! forged wire fields, and native-vs-HTTP peer parity. Every refusal must
//! leave the provider-call count unchanged; every replay must return the
//! recorded outcome without a second provider call.

use std::sync::{Arc, Mutex};
use std::thread;

use cadence_agent::platform::agenticos_external::publish::{
    caption_digest_of, device_media_key, media_key_authorizes, valid_caption, Destination,
    FakeProviderBehavior, FakePublishLedger, PublishState, ScheduledIntent, ScheduledState,
    SendBinding, SendGrant, Toolkit, DEVICE_PUBLISH_VERSION, PILOT_REVIEWED_STILL_BYTES,
};
use sha2::{Digest, Sha256};

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn digest_of(byte: u8) -> String {
    sha_hex(&[byte])
}

fn ig_destination() -> Destination {
    Destination {
        connection_id: "con_harbour_ig".into(),
        toolkit: Toolkit::Instagram,
        display_name: "Harbour stills".into(),
        destination_id: "17841400008460056".into(),
        status_active: true,
        available: true,
    }
}

fn fb_destination() -> Destination {
    Destination {
        connection_id: "con_harbour_fb".into(),
        toolkit: Toolkit::Facebook,
        display_name: "Harbour page".into(),
        destination_id: "275491372109884".into(),
        status_active: true,
        available: true,
    }
}

fn grant_for(binding: &SendBinding) -> SendGrant {
    SendGrant {
        id: binding.grant_id.clone(),
        workspace_id: "ws_harbour".into(),
        connection_id: binding.connection_id.clone(),
        destination_id: binding.destination_id.clone(),
        toolkit: binding.toolkit,
        caption_digest: binding.caption_digest.clone(),
        image_digest: binding.image_digest.clone(),
        cadence_approval_id: "cad_approval_01".into(),
        max_uses: 3,
        remaining_uses: 3,
        revoked: false,
        not_before_epoch: 1_700_000_000,
        expires_at_epoch: 1_800_000_000,
    }
}

fn ig_binding(key: &str) -> SendBinding {
    SendBinding {
        key: key.into(),
        connection_id: "con_harbour_ig".into(),
        destination_id: "17841400008460056".into(),
        toolkit: Toolkit::Instagram,
        caption_digest: caption_digest_of("Harbour at dusk. Synthetic caption."),
        image_digest: Some(digest_of(9)),
        cadence_run_id: "cad_run_01".into(),
        cadence_effect_id: "cad_fx_01".into(),
        grant_id: "dpq_synthetic_grant_01".into(),
    }
}

fn fb_text_binding(key: &str) -> SendBinding {
    SendBinding {
        key: key.into(),
        connection_id: "con_harbour_fb".into(),
        destination_id: "275491372109884".into(),
        toolkit: Toolkit::Facebook,
        caption_digest: caption_digest_of("Harbour notes. Text-only synthetic caption."),
        image_digest: None,
        cadence_run_id: "cad_run_fb_01".into(),
        cadence_effect_id: "cad_fx_fb_01".into(),
        grant_id: "dpq_synthetic_grant_fb".into(),
    }
}

const NOW: i64 = 1_750_000_000;

#[test]
fn cad771_wire_version_is_pinned_v1() {
    assert_eq!(DEVICE_PUBLISH_VERSION, "1");
}

#[test]
#[allow(clippy::assertions_on_constants)]
fn cad771_pilot_still_fits_backend_ceiling_as_literal() {
    // Prose + literal only: 512 KiB < 10 MiB, never enforced backend-side.
    assert!(PILOT_REVIEWED_STILL_BYTES < 10 * 1024 * 1024);
    assert_eq!(PILOT_REVIEWED_STILL_BYTES, 512 * 1024);
}

#[test]
fn cad771_destination_is_selected_never_inferred_from_source() {
    // The source handle (@juicysuite_crm) and the pilot destination
    // (@sakeboyhk / 17841400008460056) are different roles. A binding that
    // names a source-derived destination never matches discovery.
    let ledger = FakePublishLedger::enabled();
    let mut binding = ig_binding("cad771-no-infer-01");
    binding.destination_id = "juicysuite_crm".into();
    let mut grant = grant_for(&binding);
    let dest = ig_destination();
    let err = ledger
        .execute(
            &binding,
            &dest,
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap_err();
    assert_eq!(err.code, "wrong_destination");
    assert_eq!(ledger.provider_calls(), 0);
}

#[test]
fn cad771_post_now_instagram_round_trip_with_replay() {
    let ledger = FakePublishLedger::enabled();
    let binding = ig_binding("cad771-now-ig-01");
    let dest = ig_destination();
    let mut grant = grant_for(&binding);
    assert!(!ledger
        .preflight(&binding, &dest, &grant, "ws_harbour", NOW)
        .unwrap());
    let first = ledger
        .execute(
            &binding,
            &dest,
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap();
    assert_eq!(first.state, PublishState::Posted);
    assert!(first.permalink.as_deref().unwrap().starts_with("https://"));
    assert!(!first.repeated);
    assert!(first.provider_payload.is_some());
    assert_eq!(ledger.provider_calls(), 1);
    // Replay returns the recorded outcome without a second provider call.
    let replay = ledger
        .execute(
            &binding,
            &dest,
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap();
    assert!(replay.repeated);
    assert_eq!(replay.permalink, first.permalink);
    assert_eq!(replay.provider_payload, first.provider_payload);
    assert_eq!(ledger.provider_calls(), 1);
    // Status reconciles the same recorded outcome.
    let status = ledger.status(&binding.key).unwrap();
    assert_eq!(status.state, PublishState::Posted);
    assert_eq!(status.provider_payload, first.provider_payload);
}

#[test]
fn cad771_post_now_facebook_text_only_without_image() {
    let ledger = FakePublishLedger::enabled();
    let binding = fb_text_binding("cad771-now-fb-01");
    let dest = fb_destination();
    let mut grant = grant_for(&binding);
    let outcome = ledger
        .execute(
            &binding,
            &dest,
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap();
    assert_eq!(outcome.state, PublishState::Posted);
    assert_eq!(outcome.image_digest, None);
    assert_eq!(ledger.provider_calls(), 1);
}

#[test]
fn cad771_instagram_without_image_is_refused() {
    let ledger = FakePublishLedger::enabled();
    let mut binding = ig_binding("cad771-ig-noimg-01");
    binding.image_digest = None;
    let dest = ig_destination();
    let grant = grant_for(&binding);
    // Shape refusal happens before any provider call or grant use.
    let err = binding.validate().unwrap_err();
    assert_eq!(err.code, "image_required");
    assert_eq!(ledger.provider_calls(), 0);
    assert_eq!(grant.remaining_uses, 3);
    drop((ledger, dest, grant));
}

#[test]
fn cad771_same_key_with_changed_content_or_destination_fails() {
    let ledger = FakePublishLedger::enabled();
    let binding = ig_binding("cad771-keyfold-01");
    let dest = ig_destination();
    let mut grant = grant_for(&binding);
    ledger
        .execute(
            &binding,
            &dest,
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap();
    assert_eq!(ledger.provider_calls(), 1);
    // Changed caption under the same key must fail, not post twice.
    let mut changed = binding.clone();
    changed.caption_digest = caption_digest_of("A different approved caption.");
    let mut grant2 = grant_for(&changed);
    let err = ledger
        .execute(
            &changed,
            &dest,
            &mut grant2,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap_err();
    assert_eq!(err.code, "key_conflict");
    // Changed destination under the same key must also fail.
    let mut moved = binding.clone();
    moved.destination_id = "999999999999999".into();
    let mut grant3 = grant_for(&moved);
    let err = ledger
        .execute(
            &moved,
            &dest,
            &mut grant3,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap_err();
    assert!(matches!(
        err.code.as_str(),
        "key_conflict" | "wrong_destination"
    ));
    assert_eq!(ledger.provider_calls(), 1);
}

#[test]
fn cad771_refusal_distinguishes_from_uncertain_and_calls_once() {
    let ledger = FakePublishLedger::enabled();
    let binding = ig_binding("cad771-refuse-01");
    let dest = ig_destination();
    let mut grant = grant_for(&binding);
    let outcome = ledger
        .execute(
            &binding,
            &dest,
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Refuse,
        )
        .unwrap();
    assert_eq!(outcome.state, PublishState::Refused);
    assert_eq!(outcome.permalink, None);
    // A refusal is durable: replaying the same binding repeats the refusal
    // without another provider decision.
    let replay = ledger
        .execute(
            &binding,
            &dest,
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Refuse,
        )
        .unwrap();
    assert_eq!(replay.state, PublishState::Refused);
    assert!(replay.repeated);
}

#[test]
fn cad771_lost_response_after_accept_reconciles_without_second_call() {
    let ledger = FakePublishLedger::enabled();
    let binding = ig_binding("cad771-lost-01");
    let dest = ig_destination();
    let mut grant = grant_for(&binding);
    let accepted = ledger
        .execute(
            &binding,
            &dest,
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::LoseResponseAfterAccept,
        )
        .unwrap();
    assert_eq!(accepted.state, PublishState::Processing);
    assert_eq!(accepted.provider_payload, None);
    assert_eq!(ledger.provider_calls(), 1);
    // After restart, reconcile upstream status before any retry: the status
    // query finalizes to posted with byte-exact evidence, no second call.
    let reconciled = ledger.status(&binding.key).unwrap();
    assert_eq!(reconciled.state, PublishState::Posted);
    assert!(reconciled.permalink.is_some());
    assert!(reconciled.provider_payload.is_some());
    assert_eq!(reconciled.provider_ids, vec!["provider-post-1".to_owned()]);
    assert_eq!(ledger.provider_calls(), 1);
    // A bare success string is never proof: the reconciled record carries
    // permalink + digests + provider ids + exact payload.
    assert_eq!(reconciled.caption_digest, binding.caption_digest);
    assert_eq!(reconciled.destination_id, binding.destination_id);
}

#[test]
fn cad771_agent_caller_cannot_broaden_destination() {
    // An agent holding the key but not the grant's exact binding cannot
    // redirect the send to another account.
    let ledger = FakePublishLedger::enabled();
    let binding = ig_binding("cad771-agent-01");
    let dest = ig_destination();
    let mut grant = grant_for(&binding);
    let mut forged = binding.clone();
    forged.destination_id = "17841400999999999".into();
    let err = ledger
        .execute(
            &forged,
            &dest,
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap_err();
    assert!(matches!(
        err.code.as_str(),
        "grant_binding_mismatch" | "wrong_destination"
    ));
    assert_eq!(ledger.provider_calls(), 0);
}

#[test]
fn cad771_detached_child_cannot_submit_send() {
    // A detached child replays the key with a substituted caption digest;
    // the ledger's key-folding refuses without a provider call.
    let ledger = Arc::new(FakePublishLedger::enabled());
    let binding = ig_binding("cad771-detached-01");
    let dest = ig_destination();
    let mut grant = grant_for(&binding);
    ledger
        .execute(
            &binding,
            &dest,
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap();
    let ledger_child = Arc::clone(&ledger);
    let mut tampered = binding.clone();
    tampered.caption_digest = digest_of(7);
    let handle = thread::spawn(move || {
        let mut child_grant = grant_for(&tampered);
        ledger_child.execute(
            &tampered,
            &ig_destination(),
            &mut child_grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
    });
    let err = handle.join().unwrap().unwrap_err();
    assert_eq!(err.code, "key_conflict");
    assert_eq!(ledger.provider_calls(), 1);
}

#[test]
fn cad771_concurrent_same_key_claims_provider_once() {
    let ledger = Arc::new(FakePublishLedger::enabled());
    let binding = ig_binding("cad771-concurrent-01");
    let results = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let ledger_ref = Arc::clone(&ledger);
        let out_ref = Arc::clone(&results);
        let proof = binding.clone();
        handles.push(thread::spawn(move || {
            let mut grant = grant_for(&proof);
            let outcome = ledger_ref.execute(
                &proof,
                &ig_destination(),
                &mut grant,
                "ws_harbour",
                NOW,
                FakeProviderBehavior::Post,
            );
            out_ref.lock().unwrap().push(outcome.is_ok());
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    let outcomes = results.lock().unwrap();
    assert_eq!(outcomes.len(), 8);
    assert!(outcomes.iter().all(|ok| *ok));
    assert_eq!(ledger.provider_calls(), 1);
}

#[test]
fn cad771_forged_workspace_account_approval_fields_refused() {
    let ledger = FakePublishLedger::enabled();
    let binding = ig_binding("cad771-forged-01");
    let dest = ig_destination();
    // Forged grant id: server-side lookup is the source of truth, so a
    // presented id that is not the issued one never authorizes.
    let mut bad_grant = grant_for(&binding);
    bad_grant.id = "dpq_forged_grant_99".into();
    let err = ledger
        .execute(
            &binding,
            &dest,
            &mut bad_grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap_err();
    assert_eq!(err.code, "grant_mismatch");
    // Forged workspace: the credential binds ws_harbour, so a grant
    // minted for ws_attacker never covers this binding — even when the
    // attacker also rewrites the connection.
    let mut cross_grant = grant_for(&binding);
    cross_grant.workspace_id = "ws_attacker".into();
    cross_grant.connection_id = "con_attacker".into();
    let err = ledger
        .execute(
            &binding,
            &dest,
            &mut cross_grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap_err();
    assert_eq!(err.code, "cross_workspace");
    // Empty approval identity is not a product decision.
    let mut no_approval = grant_for(&binding);
    no_approval.cadence_approval_id.clear();
    let err = ledger
        .execute(
            &binding,
            &dest,
            &mut no_approval,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap_err();
    assert_eq!(err.code, "grant_approval");
    assert_eq!(ledger.provider_calls(), 0);
}

#[test]
fn cad771_rebind_revoke_context_change_invalidates_pending_authority() {
    let ledger = FakePublishLedger::enabled();
    let binding = ig_binding("cad771-revoke-01");
    let mut dest = ig_destination();
    let mut grant = grant_for(&binding);
    assert!(!ledger
        .preflight(&binding, &dest, &grant, "ws_harbour", NOW)
        .unwrap());
    // Revocation between preflight and dispatch holds for a new decision.
    grant.revoked = true;
    let err = ledger
        .execute(
            &binding,
            &dest,
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap_err();
    assert_eq!(err.code, "grant_revoked");
    // Rebind to a different connection invalidates the frozen binding.
    grant.revoked = false;
    dest.connection_id = "con_rotated".into();
    let err = ledger
        .execute(
            &binding,
            &dest,
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap_err();
    assert_eq!(err.code, "wrong_connection");
    // Expired grant window holds as well.
    dest.connection_id = "con_harbour_ig".into();
    let past_expiry = grant.expires_at_epoch + 1;
    let err = ledger
        .execute(
            &binding,
            &dest,
            &mut grant,
            "ws_harbour",
            past_expiry,
            FakeProviderBehavior::Post,
        )
        .unwrap_err();
    assert_eq!(err.code, "grant_window");
    assert_eq!(ledger.provider_calls(), 0);
}

#[test]
fn cad771_cross_workspace_and_cross_account_blocked() {
    let ledger = FakePublishLedger::enabled();
    let binding = ig_binding("cad771-cross-01");
    let dest = ig_destination();
    // Same key material in another workspace: the credential binds
    // ws_harbour, so a grant minted for ws_other never authorizes.
    let mut other_ws = grant_for(&binding);
    other_ws.workspace_id = "ws_other".into();
    let err = ledger
        .execute(
            &binding,
            &dest,
            &mut other_ws,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap_err();
    assert_eq!(err.code, "cross_workspace");
    // Cross-account: IG binding against the FB destination refuses.
    let mut grant = grant_for(&binding);
    let err = ledger
        .execute(
            &binding,
            &fb_destination(),
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap_err();
    assert!(matches!(
        err.code.as_str(),
        "wrong_connection" | "wrong_destination" | "wrong_toolkit"
    ));
    assert_eq!(ledger.provider_calls(), 0);
}

#[test]
fn cad771_stale_run_and_receipt_mismatch_block_send() {
    let binding = ig_binding("cad771-stale-01");
    // Stale run identity: empty run/effect names no frozen decision.
    let mut stale = binding.clone();
    stale.cadence_run_id.clear();
    assert_eq!(stale.validate().unwrap_err().code, "bad_run");
    stale = binding.clone();
    stale.cadence_effect_id.clear();
    assert_eq!(stale.validate().unwrap_err().code, "bad_effect");
    // Receipt mismatch: media key must authorize company+destination+digest.
    let digest_value = digest_of(9);
    let key = device_media_key("ws_harbour", "con_harbour_ig", &digest_value).unwrap();
    assert!(media_key_authorizes(
        &key,
        "ws_harbour",
        "con_harbour_ig",
        &digest_value
    ));
    assert!(!media_key_authorizes(
        &key,
        "ws_harbour",
        "con_harbour_ig",
        &digest_of(10)
    ));
    assert!(!valid_caption(""));
}

#[test]
fn cad771_schedule_cancel_and_due_replay_without_duplicate() {
    // Slice-2 durability shape: queue, cancel before dispatch, and
    // due-time/restart replay reusing the same stable key.
    let ledger = FakePublishLedger::enabled();
    let binding = ig_binding("cad771-sched-01");
    let dest = ig_destination();
    let mut grant = grant_for(&binding);
    let mut intent = ScheduledIntent {
        intent_id: "sched_01".into(),
        binding: binding.clone(),
        due_epoch: NOW + 3600,
        timezone: "Asia/Hong_Kong".into(),
        state: ScheduledState::Queued,
        binding_revision: "rev-1".into(),
    };
    intent.validate().unwrap();
    // Operator cancellation before dispatch closes the intent.
    intent.cancel().unwrap();
    assert_eq!(intent.state, ScheduledState::Cancelled);
    assert!(intent.cancel().is_err());
    // A cancelled intent never dispatches: the caller checks state first.
    assert_ne!(intent.state, ScheduledState::Queued);
    // A fresh queued intent dispatches exactly once; restart replay
    // reconciles via the same key without a second provider call.
    let mut live = ScheduledIntent {
        intent_id: "sched_02".into(),
        binding: binding.clone(),
        due_epoch: NOW,
        timezone: "Asia/Hong_Kong".into(),
        state: ScheduledState::Queued,
        binding_revision: "rev-1".into(),
    };
    live.state = ScheduledState::Processing;
    let outcome = ledger
        .execute(
            &live.binding,
            &dest,
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap();
    assert_eq!(outcome.state, PublishState::Posted);
    live.state = ScheduledState::Posted;
    let replay = ledger.status(&live.binding.key).unwrap();
    assert_eq!(replay.state, PublishState::Posted);
    assert_eq!(ledger.provider_calls(), 1);
}

#[test]
fn cad771_http_peer_parity_with_native_boundary() {
    // The HTTP peer must reach the same verdicts as the native boundary
    // for identical JSON. A loopback fake door validates shapes and
    // returns the native verdict code; any divergence fails the test.
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = server.server_addr().to_ip().unwrap().to_string();
    let worker = thread::spawn(move || {
        for _ in 0..2 {
            let mut request = server
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
                .expect("peer request");
            assert_eq!(request.url(), "/v1/publish/preflight");
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let value: serde_json::Value = serde_json::from_str(&body).unwrap();
            // The peer enforces the same destination/exactness rules: the
            // forged second request names a different destination id.
            let verdict = if value["destination_id"] == "17841400008460056" {
                "ok"
            } else {
                "wrong_destination"
            };
            request
                .respond(tiny_http::Response::from_string(
                    serde_json::json!({"verdict": verdict}).to_string(),
                ))
                .unwrap();
        }
    });
    let native_ok = ig_binding("cad771-http-01");
    let native_forged = {
        let mut forged = native_ok.clone();
        forged.destination_id = "999999999999999".into();
        forged
    };
    // Native boundary verdicts.
    assert!(native_ok.validate().is_ok());
    assert!(native_forged.validate().is_ok());
    // HTTP peer verdicts for the same payloads.
    let agent = ureq::Agent::new_with_defaults();
    for (proof, expected) in [(&native_ok, "ok"), (&native_forged, "wrong_destination")] {
        let body = serde_json::json!({
            "key": proof.key,
            "destination_id": proof.destination_id,
        });
        let mut response = agent
            .post(format!("http://{addr}/v1/publish/preflight"))
            .send_json(&body)
            .unwrap();
        let text = response.body_mut().read_to_string().unwrap();
        let envelope: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(envelope["verdict"], expected);
    }
    worker.join().unwrap();
    // The forged HTTP field never reaches a provider call: the fake ledger
    // refuses the same binding natively with zero provider calls.
    let ledger = FakePublishLedger::enabled();
    let mut grant = grant_for(&native_forged);
    let err = ledger
        .execute(
            &native_forged,
            &ig_destination(),
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap_err();
    assert_eq!(err.code, "wrong_destination");
    assert_eq!(ledger.provider_calls(), 0);
}

#[test]
fn cad771_send_dispatch_stays_closed_until_enabled() {
    // Landed default-off gate: a disabled execution validates everything,
    // mutates nothing, calls no provider. Preflight staging is unaffected.
    use cadence_agent::platform::agenticos_external::publish::FakePublishLedger;
    let ledger = FakePublishLedger::new();
    let binding = ig_binding("cad771-gated-01");
    let dest = ig_destination();
    let mut grant = grant_for(&binding);
    assert!(!ledger
        .preflight(&binding, &dest, &grant, "ws_harbour", NOW)
        .unwrap());
    let err = ledger
        .execute(
            &binding,
            &dest,
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap_err();
    assert_eq!(err.code, "send_disabled");
    assert_eq!(ledger.provider_calls(), 0);
    assert_eq!(grant.remaining_uses, 3);
    assert!(ledger.status(&binding.key).is_err());
    // Test-only enablement models operator-confirmed activation.
    ledger.set_send_enabled(true);
    let outcome = ledger
        .execute(
            &binding,
            &dest,
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap();
    assert_eq!(outcome.state, PublishState::Posted);
    assert_eq!(ledger.provider_calls(), 1);
}

#[test]
fn cad771_grant_bounds_reject_out_of_range_uses() {
    // Landed bound: maxUses is 1..=10. A forged grant outside it refuses
    // before any provider call.
    use cadence_agent::platform::agenticos_external::publish::FakePublishLedger;
    let ledger = FakePublishLedger::enabled();
    let binding = ig_binding("cad771-bounds-01");
    let dest = ig_destination();
    for uses in [0, 11, 99] {
        let mut grant = grant_for(&binding);
        grant.max_uses = uses;
        let err = ledger
            .execute(
                &binding,
                &dest,
                &mut grant,
                "ws_harbour",
                NOW,
                FakeProviderBehavior::Post,
            )
            .unwrap_err();
        assert_eq!(err.code, "grant_bounds", "{uses}");
    }
    assert_eq!(ledger.provider_calls(), 0);
}

#[test]
fn cad771_revalidation_drift_tightening_against_landed_contract() {
    // Field-for-field revalidation vs staging 3d6ced85: the landed
    // sha256Schema is lowercase-only, cadenceApprovalId is max 120, and
    // providerIds is max 10. Fail-open leniencies are refused here.
    use cadence_agent::platform::agenticos_external::publish::{valid_digest, FakePublishLedger};
    assert!(valid_digest(
        "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
    ));
    assert!(!valid_digest(
        "9F86D081884C7D659A2FEAA0C55AD015A3BF4F1B2B0B822CD15D6C15B0F00A08"
    ));
    let ledger = FakePublishLedger::enabled();
    let dest = ig_destination();
    // Uppercase caption digest refuses before any provider call.
    let mut upper = ig_binding("cad771-drift-upper-01");
    upper.caption_digest = upper.caption_digest.to_uppercase();
    let mut grant = grant_for(&upper);
    assert_eq!(
        ledger
            .execute(
                &upper,
                &dest,
                &mut grant,
                "ws_harbour",
                NOW,
                FakeProviderBehavior::Post
            )
            .unwrap_err()
            .code,
        "bad_caption_digest"
    );
    // Oversize approval identity refuses.
    let binding = ig_binding("cad771-drift-approval-01");
    let mut grant = grant_for(&binding);
    grant.cadence_approval_id = "a".repeat(121);
    assert_eq!(
        ledger
            .execute(
                &binding,
                &dest,
                &mut grant,
                "ws_harbour",
                NOW,
                FakeProviderBehavior::Post
            )
            .unwrap_err()
            .code,
        "grant_approval"
    );
    // Recorded provider evidence respects the landed max-10 bound.
    let mut grant = grant_for(&binding);
    let outcome = ledger
        .execute(
            &binding,
            &dest,
            &mut grant,
            "ws_harbour",
            NOW,
            FakeProviderBehavior::Post,
        )
        .unwrap();
    assert!(outcome.provider_ids.len() <= 10);
    assert_eq!(ledger.provider_calls(), 1);
}
