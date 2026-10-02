//! CAD-771 slice-2 adversarial tests: durable scheduled-intent lifecycle.
//!
//! Vectors: forged fields, concurrent claimants, restart reopen, stale and
//! unknown identities. Every refusal leaves state unchanged; exactly one
//! claimant wins dispatch; a restart loses nothing and duplicates nothing.

use super::*;
use crate::store::social_publish::NewSocialPublish;

fn digest(byte: u8) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest([byte])
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn cap_digest() -> &'static str {
    static CELL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CELL.get_or_init(|| digest(1))
}

fn img_digest() -> &'static str {
    static CELL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CELL.get_or_init(|| digest(9))
}

fn intent(request: &str) -> NewSocialPublish<'_> {
    NewSocialPublish {
        request_id: request,
        install_id: "install-harbour",
        context_id: None,
        run_id: "cad_run_01",
        effect_id: "cad_fx_01",
        artifact_id: None,
        bundle_digest: None,
        slot: None,
        connection_id: "con_harbour_ig",
        aos_connection_id: Some("connA_harbour_ig"),
        destination_id: "17841400008460056",
        toolkit: "instagram",
        caption_digest: cap_digest(),
        image_digest: Some(img_digest()),
        media_key: None,
        grant_id: "dpq_synthetic_grant_01",
        // CAD-1027: an approval authorizes exactly one intent, so each
        // fixture request carries its own approval identity.
        approval_id: request,
        due_epoch: 1_750_000_000,
        timezone: "Asia/Hong_Kong",
    }
}

#[test]
fn cad771_schedule_freezes_exact_intent_and_replays_same_request() {
    let (_dir, s) = store();
    let first = s.social_publish_schedule(&intent("req-1")).unwrap();
    assert_eq!(first["intent"]["state"], "queued");
    assert_eq!(
        first["intent"]["frozen"]["destination_id"],
        "17841400008460056"
    );
    let digest = first["intent"]["frozen_digest"]
        .as_str()
        .unwrap()
        .to_owned();
    // Same request + same frozen content replays the same intent.
    let replay = s.social_publish_schedule(&intent("req-1")).unwrap();
    assert_eq!(replay["intent"]["intent_id"], first["intent"]["intent_id"]);
    assert_eq!(replay["intent"]["frozen_digest"], digest);
    // Same request + changed content fails instead of forking the key.
    let mut changed = intent("req-1");
    changed.destination_id = "999999999999999";
    assert!(s.social_publish_schedule(&changed).is_err());
    assert_eq!(
        s.social_publish_show(first["intent"]["intent_id"].as_str().unwrap())
            .unwrap()["intent"]["state"],
        "queued"
    );
}

#[test]
fn cad771_schedule_refuses_forged_and_mismatched_shapes() {
    let (_dir, s) = store();
    // Forged destination: empty.
    let mut bad = intent("req-bad-dest");
    bad.destination_id = "";
    assert!(s.social_publish_schedule(&bad).is_err());
    // Forged digests: non-hex / wrong length.
    let mut bad = intent("req-bad-digest");
    bad.caption_digest = "not-a-digest";
    assert!(s.social_publish_schedule(&bad).is_err());
    // Instagram without an image digest cannot be scheduled.
    let mut bad = intent("req-no-image");
    bad.image_digest = None;
    assert!(s.social_publish_schedule(&bad).is_err());
    // Unknown toolkit.
    let mut bad = intent("req-bad-toolkit");
    bad.toolkit = "tiktok";
    assert!(s.social_publish_schedule(&bad).is_err());
    // Empty approval is not a human decision.
    let mut bad = intent("req-no-approval");
    bad.approval_id = "";
    assert!(s.social_publish_schedule(&bad).is_err());
    // Bad timezone and non-positive due time.
    let mut bad = intent("req-bad-tz");
    bad.timezone = "";
    assert!(s.social_publish_schedule(&bad).is_err());
    let mut bad = intent("req-bad-due");
    bad.due_epoch = 0;
    assert!(s.social_publish_schedule(&bad).is_err());
    // Nothing was stored.
    assert_eq!(
        s.social_publish_list(Some("install-harbour"), None)
            .unwrap()["intents"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

/// CAD-1027 adversarial: an approval authorizes exactly one intent. The
/// same-request retry is idempotent; the same approval under any other
/// request (a replay, a double submit with a fresh request id, a
/// re-schedule after cancel, another install) refuses and stores nothing.
#[test]
fn cad1027_approval_authorizes_exactly_one_intent() {
    let (_dir, s) = store();
    let mut first = intent("req-apv-1");
    first.approval_id = "apv-once";
    let made = s.social_publish_schedule(&first).unwrap();
    let id = made["intent"]["intent_id"].as_str().unwrap().to_owned();
    let retry = s.social_publish_schedule(&first).unwrap();
    assert_eq!(retry["intent"]["intent_id"], id.as_str());
    let refuse = |request: &str, install: &str| {
        let mut replay = intent(request);
        replay.approval_id = "apv-once";
        replay.install_id = install;
        let err = s.social_publish_schedule(&replay).unwrap_err().to_string();
        assert!(err.contains("approval_replay"), "{request}: {err}");
    };
    refuse("req-apv-2", "install-harbour");
    refuse("req-apv-3", "install-other");
    s.social_publish_cancel(&id, "install-harbour", None)
        .unwrap();
    refuse("req-apv-4", "install-harbour");
    let rows: i64 = s
        .conn()
        .query_row("SELECT count(*) FROM social_publish_intents", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1, "a replayed approval stored a second intent");
}

#[test]
fn cad771_cancel_only_before_dispatch() {
    let (_dir, s) = store();
    let staged = s.social_publish_schedule(&intent("req-cancel")).unwrap();
    let id = staged["intent"]["intent_id"].as_str().unwrap().to_owned();
    let cancelled = s
        .social_publish_cancel(&id, "install-harbour", None)
        .unwrap();
    assert_eq!(cancelled["intent"]["state"], "cancelled");
    // CAD-1027: cancel is scoped — another install or a context the intent
    // does not carry refuses and leaves it queued.
    let mut later = intent("req-cancel-scope");
    later.due_epoch = 1_900_000_000;
    let staged = s.social_publish_schedule(&later).unwrap();
    let scoped = staged["intent"]["intent_id"].as_str().unwrap();
    assert!(s
        .social_publish_cancel(scoped, "install-other", None)
        .is_err());
    assert!(s
        .social_publish_cancel(scoped, "install-harbour", Some("ctx-a"))
        .is_err());
    assert_eq!(
        s.social_publish_show(scoped).unwrap()["intent"]["state"],
        "queued"
    );
    // A cancelled intent cannot be cancelled again or claimed.
    assert!(s
        .social_publish_cancel(&id, "install-harbour", None)
        .is_err());
    assert!(s
        .social_publish_claim_due(1_800_000_000, |_, _, _| Ok(true))
        .unwrap()
        .is_none());
    // Unknown intent ids are refused, never created.
    assert!(s
        .social_publish_cancel("spub-nope", "install-harbour", None)
        .is_err());
    assert!(s.social_publish_show("spub-nope").is_err());
}

#[test]
fn cad771_claim_due_picks_only_due_queued_and_holds_on_stale_authority() {
    let (_dir, s) = store();
    s.social_publish_schedule(&intent("req-early")).unwrap();
    let mut late = intent("req-late");
    late.due_epoch = 1_900_000_000;
    s.social_publish_schedule(&late).unwrap();
    // Not yet due: nothing claimable.
    assert!(s
        .social_publish_claim_due(1_700_000_000, |_, _, _| Ok(true))
        .unwrap()
        .is_none());
    // Stale authority at dispatch holds for a new human decision: the row
    // stays queued, nothing is claimed.
    assert!(s
        .social_publish_claim_due(1_800_000_000, |_, _, _| Ok(false))
        .unwrap()
        .is_none());
    // Current authority claims the early intent only.
    let claimed = s
        .social_publish_claim_due(1_800_000_000, |_, _, _| Ok(true))
        .unwrap()
        .unwrap();
    assert_eq!(claimed["intent"]["state"], "processing");
    assert_eq!(
        claimed["intent"]["frozen"]["destination_id"],
        "17841400008460056"
    );
    // A second claim finds nothing due-and-queued (late is future-dated).
    assert!(s
        .social_publish_claim_due(1_800_000_000, |_, _, _| Ok(true))
        .unwrap()
        .is_none());
}

#[test]
fn cad771_concurrent_claimants_have_exactly_one_winner() {
    use std::sync::{Arc, Barrier};
    let (_dir, s) = store();
    s.social_publish_schedule(&intent("req-race")).unwrap();
    let s = Arc::new(s);
    let barrier = Arc::new(Barrier::new(8));
    let wins = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let (s, barrier, wins) = (Arc::clone(&s), Arc::clone(&barrier), Arc::clone(&wins));
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            if s.social_publish_claim_due(1_800_000_000, |_, _, _| Ok(true))
                .unwrap()
                .is_some()
            {
                wins.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(wins.load(std::sync::atomic::Ordering::SeqCst), 1);
}

fn note_matching_evidence(s: &Store, id: &str) {
    s.social_publish_note_evidence(
        id,
        &json!({"state": "posted",
            "permalink": "https://www.instagram.com/p/ABC/",
            "provider_ids": ["provider-post-1"],
            "provider_payload": "{\"id\":\"provider-post-1\"}",
            "destination_id": "17841400008460056",
            "caption_digest": digest(1),
            "image_digest": img_digest()}),
    )
    .unwrap();
}

#[test]
fn cad771_report_needs_verified_receipt_and_never_bare_success() {
    let (_dir, s) = store();
    let staged = s.social_publish_schedule(&intent("req-report")).unwrap();
    let id = staged["intent"]["intent_id"].as_str().unwrap().to_owned();
    // Reporting before claim is refused: nothing is processing.
    assert!(s
        .social_publish_report(
            &id,
            "posted",
            &json!({"permalink": "https://www.instagram.com/p/ABC/",
                "destination_id": "17841400008460056",
                "caption_digest": digest(1),
            "image_digest": img_digest(),
                "provider_ids": ["provider-post-1"],
                "provider_payload": "{\"id\":\"provider-post-1\"}"})
        )
        .is_err());
    s.social_publish_claim_due(1_800_000_000, |_, _, _| Ok(true))
        .unwrap()
        .unwrap();
    // A bare success string is insufficient proof.
    assert!(s
        .social_publish_report(&id, "posted", &json!("posted"))
        .is_err());
    assert!(s
        .social_publish_report(
            &id,
            "posted",
            &json!({"permalink": "https://www.instagram.com/p/ABC/"})
        )
        .is_err());
    // Unknown decisions are refused.
    assert!(s
        .social_publish_report(&id, "maybe", &json!({"reason": "x"}))
        .is_err());
    // The verified receipt closes the intent as posted — but only with
    // daemon-observed evidence on file.
    assert!(s
        .social_publish_report(
            &id,
            "posted",
            &json!({"permalink": "https://www.instagram.com/p/ABC/",
                "destination_id": "17841400008460056",
                "caption_digest": digest(1),
            "image_digest": img_digest(),
                "provider_ids": ["provider-post-1"],
                "provider_payload": "{\"id\":\"provider-post-1\"}"}),
        )
        .unwrap_err()
        .to_string()
        .contains("no trusted upstream evidence"));
    assert_eq!(
        s.social_publish_show(&id).unwrap()["intent"]["state"],
        "processing"
    );
    note_matching_evidence(&s, &id);
    let posted = s
        .social_publish_report(
            &id,
            "posted",
            &json!({"permalink": "https://www.instagram.com/p/ABC/",
                "destination_id": "17841400008460056",
                "caption_digest": digest(1),
            "image_digest": img_digest(),
                "provider_ids": ["provider-post-1"],
                "provider_payload": "{\"id\":\"provider-post-1\"}"}),
        )
        .unwrap();
    assert_eq!(posted["intent"]["state"], "posted");
    assert_eq!(
        posted["intent"]["receipt"]["permalink"],
        "https://www.instagram.com/p/ABC/"
    );
    // Terminal: no second report, no re-claim.
    assert!(s
        .social_publish_report(
            &id,
            "posted",
            &json!({"permalink": "https://www.instagram.com/p/ABC/",
                "destination_id": "17841400008460056",
                "caption_digest": digest(1),
            "image_digest": img_digest(),
                "provider_ids": ["provider-post-1"],
                "provider_payload": "{\"id\":\"provider-post-1\"}"})
        )
        .is_err());
}

#[test]
fn cad771_refused_and_held_are_terminal_for_dispatch() {
    let (_dir, s) = store();
    for (request, decision, receipt) in [
        ("req-ref", "refused", json!({"error": "grant_revoked"})),
        (
            "req-held",
            "held",
            json!({"reason": "binding rotated; needs a new human decision"}),
        ),
    ] {
        let staged = s.social_publish_schedule(&intent(request)).unwrap();
        let id = staged["intent"]["intent_id"].as_str().unwrap().to_owned();
        s.social_publish_claim_due(1_800_000_000, |_, _, _| Ok(true))
            .unwrap()
            .unwrap();
        let done = s.social_publish_report(&id, decision, &receipt).unwrap();
        assert_eq!(done["intent"]["state"], decision);
        // Neither can be claimed again; held never silently republishes.
        assert!(s
            .social_publish_claim_due(1_800_000_000, |_, _, _| Ok(true))
            .unwrap()
            .is_none());
    }
}

#[test]
fn cad771_restart_loses_nothing_and_duplicates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.sqlite3");
    let id = {
        let s = Store::open(&path).unwrap();
        let staged = s.social_publish_schedule(&intent("req-restart")).unwrap();
        staged["intent"]["intent_id"].as_str().unwrap().to_owned()
    };
    // Reopen: the queued intent survives and is claimable exactly once.
    let s = Store::open(&path).unwrap();
    assert_eq!(
        s.social_publish_show(&id).unwrap()["intent"]["state"],
        "queued"
    );
    s.social_publish_claim_due(1_800_000_000, |_, _, _| Ok(true))
        .unwrap()
        .unwrap();
    drop(s);
    // Reopen mid-processing: still processing, never auto-duplicated.
    let s = Store::open(&path).unwrap();
    assert_eq!(
        s.social_publish_show(&id).unwrap()["intent"]["state"],
        "processing"
    );
    assert!(s
        .social_publish_claim_due(1_800_000_000, |_, _, _| Ok(true))
        .unwrap()
        .is_none());
    // Reconcile by explicit report, then the receipt is durable too.
    note_matching_evidence(&s, &id);
    s.social_publish_report(
        &id,
        "posted",
        &json!({"permalink": "https://www.instagram.com/p/ABC/",
            "destination_id": "17841400008460056",
            "caption_digest": digest(1),
            "image_digest": img_digest(),
            "provider_ids": ["provider-post-1"],
            "provider_payload": "{\"id\":\"provider-post-1\"}"}),
    )
    .unwrap();
    drop(s);
    let s = Store::open(&path).unwrap();
    let shown = s.social_publish_show(&id).unwrap();
    assert_eq!(shown["intent"]["state"], "posted");
    assert!(shown["intent"]["receipt"]["permalink"].is_string());
}

#[test]
fn cad771_freeze_without_approved_material_is_refused() {
    use crate::store::social_publish::FreezeFromArtifact;
    let (_dir, s) = store();
    // No run exists: nothing can be frozen from it.
    assert!(s
        .social_publish_freeze_from_artifact(&FreezeFromArtifact {
            request_id: "req-freeze-unknown",
            install_id: "install-harbour",
            context_id: None,
            run_id: "run-unknown",
            artifact_id: "artifact-unknown",
            bundle_digest: "digest-unknown",
            slot: "publication",
            effect_id: "cad_fx_freeze_01",
            destination_id: "17841400008460056",
            toolkit: "instagram",
            aos_connection_id: "connA_harbour_ig",
            media_key: None,
            grant_id: "dpq_synthetic_grant_01",
            approval_id: "cad_approval_freeze_01",
            due_epoch: 1_750_000_000,
            timezone: "Asia/Hong_Kong",
        })
        .is_err());
    assert_eq!(
        s.social_publish_list(Some("install-harbour"), None)
            .unwrap()["intents"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn cad771_partial_artifact_triple_is_refused() {
    let (_dir, s) = store();
    let mut partial = intent("req-partial-triple");
    partial.artifact_id = Some("artifact-a");
    assert!(s.social_publish_schedule(&partial).is_err());
    assert_eq!(
        s.social_publish_list(Some("install-harbour"), None)
            .unwrap()["intents"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn cad771_material_reproof_covers_modes_and_unknown_intents() {
    let (_dir, s) = store();
    // Explicit-mode intents carry no artifact triple: re-proof is vacuous.
    let staged = s
        .social_publish_schedule(&intent("req-explicit-reproof"))
        .unwrap();
    let id = staged["intent"]["intent_id"].as_str().unwrap().to_owned();
    assert!(s.social_publish_material_current(&id).unwrap());
    assert!(s.social_publish_show(&id).unwrap()["intent"]["frozen"]["artifact_id"].is_null());
    // Unknown intents are refused, never current.
    assert!(s.social_publish_material_current("spub-nope").is_err());
}

#[test]
fn cad771_posted_receipt_must_match_frozen_intent() {
    // Operator JSON alone never posts: a forged or mismatched receipt is
    // refused and the intent stays processing (uncertain) for reconcile.
    let (_dir, s) = store();
    let staged = s
        .social_publish_schedule(&intent("req-receipt-bind"))
        .unwrap();
    let id = staged["intent"]["intent_id"].as_str().unwrap().to_owned();
    let frozen = &staged["intent"]["frozen"];
    s.social_publish_claim_due(1_800_000_000, |_, _, _| Ok(true))
        .unwrap()
        .unwrap();
    let good = json!({"permalink": "https://www.instagram.com/p/ABC/",
        "destination_id": frozen["destination_id"],
        "caption_digest": frozen["caption_digest"],
        "image_digest": frozen["image_digest"],
        "provider_ids": ["provider-post-1"],
        "provider_payload": "{\"id\":\"provider-post-1\"}"});
    // Forged destination, caption, and image digests each refuse.
    for (field, value) in [
        ("destination_id", json!("999999999999999")),
        ("caption_digest", json!(digest(7))),
        ("image_digest", json!(digest(8))),
    ] {
        let mut forged = good.clone();
        forged[field] = value;
        let err = s
            .social_publish_report(&id, "posted", &forged)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("does not match the frozen intent"),
            "{field}: {err}"
        );
        assert_eq!(
            s.social_publish_show(&id).unwrap()["intent"]["state"],
            "processing"
        );
    }
    // Evidence on file, then: a receipt whose evidence the daemon never
    // observed refuses, even with matching binding fields.
    note_matching_evidence(&s, &id);
    let mut fabricated = good.clone();
    fabricated["provider_payload"] = json!("{\"id\":\"forged\"}");
    fabricated["permalink"] = json!("https://www.instagram.com/p/FORGED/");
    assert!(s
        .social_publish_report(&id, "posted", &fabricated)
        .unwrap_err()
        .to_string()
        .contains("does not match trusted upstream evidence"));
    assert_eq!(
        s.social_publish_show(&id).unwrap()["intent"]["state"],
        "processing"
    );
    // Copied payload with forged permalink/IDs still fails: full
    // receipt-to-outcome equality, not payload-only.
    let mut copied = good.clone();
    copied["permalink"] = json!("https://www.instagram.com/p/FORGED/");
    copied["provider_ids"] = json!(["provider-post-9"]);
    assert!(s
        .social_publish_report(&id, "posted", &copied)
        .unwrap_err()
        .to_string()
        .contains("does not match trusted upstream evidence"));
    assert_eq!(
        s.social_publish_show(&id).unwrap()["intent"]["state"],
        "processing"
    );
    // The matching receipt posts.
    let posted = s.social_publish_report(&id, "posted", &good).unwrap();
    assert_eq!(posted["intent"]["state"], "posted");
}

#[test]
fn cad771_note_evidence_with_foreign_binding_fails_closed() {
    // A status reply for the same key with a different destination or
    // content must never persist: the intent stays processing with no
    // upstream evidence recorded.
    let (_dir, s) = store();
    let staged = s
        .social_publish_schedule(&intent("req-foreign-note"))
        .unwrap();
    let id = staged["intent"]["intent_id"].as_str().unwrap().to_owned();
    s.social_publish_claim_due(1_800_000_000, |_, _, _| Ok(true))
        .unwrap()
        .unwrap();
    for (field, value) in [
        ("destination_id", json!("999999999999999")),
        ("caption_digest", json!(digest(7))),
        ("image_digest", json!(digest(8))),
    ] {
        let mut foreign = json!({"state": "posted",
            "permalink": "https://www.instagram.com/p/ABC/",
            "provider_ids": ["provider-post-1"],
            "provider_payload": "{\"id\":\"provider-post-1\"}",
            "destination_id": "17841400008460056",
            "caption_digest": digest(1),
            "image_digest": img_digest()});
        foreign[field] = value;
        let err = s
            .social_publish_note_evidence(&id, &foreign)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("does not match the frozen intent"),
            "{field}: {err}"
        );
    }
    let shown = s.social_publish_show(&id).unwrap()["intent"].clone();
    assert_eq!(shown["state"], "processing");
    assert!(shown["upstream"].is_null());
}

#[test]
fn cad771_report_rejects_planted_foreign_upstream() {
    // Defense in depth, proven by surgery: even if foreign evidence were
    // somehow persisted, a posted report still requires upstream binding
    // equality with frozen.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.sqlite3");
    let id = {
        let s = Store::open(&path).unwrap();
        let staged = s.social_publish_schedule(&intent("req-planted")).unwrap();
        let id = staged["intent"]["intent_id"].as_str().unwrap().to_owned();
        s.social_publish_claim_due(1_800_000_000, |_, _, _| Ok(true))
            .unwrap()
            .unwrap();
        note_matching_evidence(&s, &id);
        id
    };
    let foreign = json!({"state": "posted",
        "permalink": "https://www.instagram.com/p/ABC/",
        "provider_ids": ["provider-post-1"],
        "provider_payload": "{\"id\":\"provider-post-1\"}",
        "destination_id": "999999999999999",
        "caption_digest": digest(1),
        "image_digest": img_digest()})
    .to_string();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE social_publish_intents SET upstream=? WHERE intent_id=?",
            rusqlite::params![foreign, id],
        )
        .unwrap();
    let s = Store::open(&path).unwrap();
    let err = s
        .social_publish_report(
            &id,
            "posted",
            &json!({"permalink": "https://www.instagram.com/p/ABC/",
                "destination_id": "17841400008460056",
                "caption_digest": digest(1),
                "image_digest": img_digest(),
                "provider_ids": ["provider-post-1"],
                "provider_payload": "{\"id\":\"provider-post-1\"}"}),
        )
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("does not match trusted upstream evidence"),
        "{err}"
    );
    assert_eq!(
        s.social_publish_show(&id).unwrap()["intent"]["state"],
        "processing"
    );
}

/// CAD-1041: the send-now claim is identity-pinned — `claim_id` takes
/// the named row or nothing, and the same single `queued→processing`
/// CAS that guards `claim_due` guards it: two Store handles on one
/// file, plus a raw second connection replaying the claim UPDATE, can
/// never produce two claims of one intent. Proven by mutation: drop
/// the `AND state='queued'` predicate and the raw replay re-takes the
/// row.
#[test]
fn cad1041_claim_id_identity_and_two_handles_one_claim() {
    use rusqlite::params;
    let (dir, s1) = store();
    let path = dir.path().join("t.sqlite3");
    let id = s1.social_publish_schedule(&intent("req-claimid")).unwrap()["intent"]["intent_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Identity pin: claiming while naming a DIFFERENT id is a no-claim.
    let other = s1
        .social_publish_schedule(&intent("req-claimid-b"))
        .unwrap()["intent"]["intent_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(s1
        .social_publish_claim_id(&id, |_, candidate, _| Ok(candidate == other))
        .unwrap()
        .is_none());
    assert_eq!(
        s1.social_publish_show(&id).unwrap()["intent"]["state"],
        "queued"
    );
    // Named claim wins once.
    let claimed = s1
        .social_publish_claim_id(&id, |_, _, _| Ok(true))
        .unwrap()
        .unwrap();
    assert_eq!(claimed["intent"]["state"], "processing");
    // A second Store handle on the same file cannot re-claim it.
    let s2 = Store::open(&path).unwrap();
    assert!(s2
        .social_publish_claim_id(&id, |_, _, _| Ok(true))
        .unwrap()
        .is_none());
    // A raw second connection replaying the claim UPDATE shape finds
    // zero rows — the `AND state='queued'` predicate is the guard.
    let conn2 = rusqlite::Connection::open(&path).unwrap();
    let re_taken = conn2
        .execute(
            "UPDATE social_publish_intents SET state='processing',updated=? \
             WHERE intent_id=? AND state='queued'",
            params![1_800_000_001_i64, id],
        )
        .unwrap();
    assert_eq!(
        re_taken, 0,
        "the claim predicate must refuse a row already processing"
    );
}
