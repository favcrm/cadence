//! Independent CAD-1365 producer-association acceptance guard.
//!
//! Scope (design-plans/designer-generation-provenance-v1.md, "Atomic
//! producer acceptance boundary"): a successfully fenced, claimed native
//! `media.generated.image` capability result must atomically create ONE
//! draft `app_media_assets` catalog row plus one private
//! `app_media_asset_origins` link to the immutable receipt, inside the
//! existing `Store::app_capability_record` write transaction. Replay of
//! the same eligible record leaves ONE asset. Malformed claimed original
//! provenance refuses atomically — no receipt, no asset, no origin row.
//!
//! This exercises the real recording transaction and the real run
//! approval/dispatch/claim APIs with a CONTROLLED trusted-adapter
//! outcome shaped exactly like `call_image`'s `media.generated.image`
//! result. It is NOT paid-provider execution, native JWT/HTTP proof, or
//! the full Designer journey — `cad1365_original_artifact_acceptance`
//! remains the separate proof that provider bytes/descriptors are
//! verified before derivative resize.
//!
//! Honest baseline on the current head: no producer linkage exists —
//! the positive test fails at the catalog asset count, and the
//! bad-case test fails because the malformed record is still accepted.

use super::app_runs::runtime_fixture;
use super::*;
use crate::store::app_capabilities::{AppCapabilityClaim, AppCapabilityRecord};
use crate::store::app_runs::{LocalRunProvenance, LocalRunRequest, LocalWorkflow};

/// A real minimal PNG: the bounded derivative a trusted adapter would
/// have already fitted into the 2 MiB custody bound.
const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

const JOB_ID: &str = "med_job1";
/// The provider original's verified sha256 (64 lowercase hex) — the
/// digest of provider bytes this fixture never needs to hold; only its
/// descriptor rides the receipt.
const ORIGINAL_DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
/// \> 2 MiB: the descriptor may exceed the custody bound — it describes
/// the retained provider original, not a bigger custody BLOB.
const ORIGINAL_BYTES: u64 = 3 * 1024 * 1024 + 17;

/// The exact claim sequence `start_local_step` performs on a dispatched
/// run (it is private to its sibling module): real app-proven claim,
/// real turn mint, real mark_running — never a faked step/message row.
fn claimed_turn(s: &Store, dispatched: &Value) -> (Message, String) {
    let message = dispatched["steps"][0]["message_id"].as_str().unwrap();
    let alias = dispatched["snapshot"]["assignments"]["s1"]["alias"]
        .as_str()
        .expect("dispatched step has a frozen registered assignee");
    let Take::Message(taken) = s
        .take_queued_app_proven(alias, Some((message, "sha256:bundle")))
        .unwrap()
    else {
        panic!("app kickoff must be claimed")
    };
    let token = crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("g1");
    s.mark_running(&taken.id, &token).unwrap();
    let writer = s.message(&taken.id).unwrap().unwrap();
    let turn = writer.turn_id.clone().unwrap();
    (writer, turn)
}

/// Decide + dispatch + claim the first step, then reserve and record
/// `result` under the real pre-call claim — identical to the cad632
/// path, with the media slot. Returns the receipt plus the still-live
/// `(message, turn)` so an identical replay can reuse the active turn.
fn record_media(
    s: &Store,
    run: &Value,
    call: &str,
    result: &Value,
    asset: Option<(&str, &[u8])>,
) -> Result<(Value, Message, String)> {
    let id = run["id"].as_str().unwrap();
    s.app_run_decide(
        id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    let dispatched = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    let (writer, turn) = claimed_turn(s, &dispatched);
    let input_digest = crate::store::app_runs::material_digest(&json!({"prompt":"badge"}));
    s.app_capability_claim(AppCapabilityClaim {
        run: id,
        step: "s1",
        message: &writer.id,
        turn: &turn,
        slot: "generate",
        request: "gen-req",
        binding_digest: "binding-media",
        input_digest: &input_digest,
        call_id: call,
    })
    .unwrap();
    let receipt = s.app_capability_record(AppCapabilityRecord {
        id: call,
        run: id,
        step: "s1",
        message: &writer.id,
        turn: &turn,
        slot: "generate",
        request: "gen-req",
        binding_digest: "binding-media",
        input_digest: &input_digest,
        result,
        asset,
    })?;
    Ok((receipt, writer, turn))
}

/// The controlled trusted-adapter `media.generated.image` outcome —
/// the closed shape `call_image` returns: derivative digest/MIME match
/// the retained bytes; `original_artifact` is closed schema 1 with
/// `ref = job_id + "." + digest`, canonical `sha256:` digest, positive
/// byte count and a real image MIME.
fn image_result() -> Value {
    let derivative = format!("sha256:{:x}", Sha256::digest(PNG_1X1));
    json!({
        "schema": 1,
        "kind": "media.generated.image",
        "provider": "agenticos",
        "job_id": JOB_ID,
        "asset_sha256": derivative,
        "asset_media_type": "image/png",
        "original_artifact": {
            "schema": 1,
            "ref": format!("{JOB_ID}.{ORIGINAL_DIGEST}"),
            "sha256": format!("sha256:{ORIGINAL_DIGEST}"),
            "bytes": ORIGINAL_BYTES,
            "media_type": "image/jpeg",
        },
    })
}

fn media_assets_count(s: &Store) -> i64 {
    s.conn()
        .query_row("SELECT count(*) FROM app_media_assets", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn cad1365_media_producer_records_one_catalog_asset_per_receipt_and_replays() {
    let (_dir, s, run) = runtime_fixture();
    // Named-creator run on the SAME fixture team and workflow: the real
    // internal attributed creation path freezes the verified creator
    // inside the snapshot before the digest (creator-capture boundary
    // is already accepted separately; here it is fixture input).
    let mut workflow: LocalWorkflow =
        serde_json::from_value(run["snapshot"]["workflow"].clone()).unwrap();
    // Distinct registered workers keep the named turn live for replay
    // while the legacy run independently claims its writer.
    workflow.steps[0].assignee = "reviewer".to_string();
    let inputs = std::collections::BTreeMap::new();
    let creator = json!({"kind": "user", "id": "cad1365-sub"});
    let named = s
        .app_run_create_attributed(
            LocalRunRequest {
                install_id: "install-1",
                bundle_digest: "sha256:bundle",
                workflow: &workflow,
                inputs: &inputs,
                request_id: "request-creator-1",
                owner_pm: Some("lead"),
                project_link: None,
            },
            None,
            None,
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
            LocalRunProvenance {
                selected_source: None,
                input_origins: &inputs,
                team_roles: &std::collections::BTreeMap::new(),
                human: None,
            },
            Some(&creator),
        )
        .unwrap();
    let named_id = named["id"].as_str().unwrap().to_string();

    // Positive: one eligible claimed image outcome records its receipt
    // and creates exactly one canonical draft metadata asset.
    let (receipt, writer, turn) = record_media(
        &s,
        &named,
        "gen-call-1",
        &image_result(),
        Some(("image/png", PNG_1X1)),
    )
    .expect("eligible media.generated.image receipt must record");
    assert_eq!(receipt["result"]["kind"], "media.generated.image");
    assert_eq!(
        receipt["result"]["original_artifact"]["bytes"], ORIGINAL_BYTES,
        "original descriptor >2MiB stays receipt metadata"
    );
    assert_eq!(
        media_assets_count(&s),
        1,
        "one eligible receipt must create exactly one catalog asset"
    );

    let (page, _next) = s.app_media_catalog(20, None).unwrap();
    assert_eq!(page.len(), 1);
    let asset = &page[0];
    assert_eq!(asset.install_id, "install-1");
    assert_eq!(asset.title, "Generated image");
    assert_eq!(asset.kind, "image");
    assert_eq!(asset.lifecycle, "draft");
    assert_eq!(asset.revision, 1);
    assert!(asset.created.is_finite() && asset.updated.is_finite());
    assert!(
        asset.id.starts_with("med-")
            && asset.id.len() == 36
            && asset.id[4..]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "catalog id must be native med-<32 lowercase hex>: {}",
        asset.id
    );
    // Creator is read only from the frozen snapshot — the named user.
    assert_eq!(
        asset.projection()["creator"],
        creator,
        "creator must come from the frozen snapshot"
    );
    // The metadata DTO exposes no original ref/bytes/locators.
    let projection = asset.projection();
    for leaked in ["ref", "original", "bytes", "receipt_id", "sha256", "job_id"] {
        assert!(
            projection.get(leaked).is_none(),
            "metadata DTO leaked {leaked}: {projection}"
        );
    }

    // Private origin association: exactly one row linking asset↔receipt.
    // Checked AFTER the positive asset exists so a missing table on the
    // baseline reports the real gap, not a phantom SQL error.
    let origin_table: i64 = s
        .conn()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='app_media_asset_origins'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        origin_table, 1,
        "private app_media_asset_origins table must exist"
    );
    let linked: Option<String> = s
        .conn()
        .query_row(
            "SELECT receipt_id FROM app_media_asset_origins WHERE asset_id=?1",
            params![asset.id],
            |r| r.get(0),
        )
        .optional()
        .unwrap();
    assert_eq!(
        linked.as_deref(),
        Some(receipt["id"].as_str().unwrap()),
        "origin row must link the asset to its immutable receipt"
    );

    // Replay of the same eligible record on the same still-active turn:
    // ONE asset, same id, no dup.
    let replay = s
        .app_capability_record(AppCapabilityRecord {
            id: "gen-call-1",
            run: &named_id,
            step: "s1",
            message: &writer.id,
            turn: &turn,
            slot: "generate",
            request: "gen-req",
            binding_digest: "binding-media",
            input_digest: receipt["input_digest"].as_str().unwrap(),
            result: &image_result(),
            asset: Some(("image/png", PNG_1X1)),
        })
        .expect("identical replay must return the existing receipt");
    assert_eq!(
        replay["id"], receipt["id"],
        "replay returns the immutable receipt"
    );
    assert_eq!(
        media_assets_count(&s),
        1,
        "replay must not duplicate the asset"
    );
    let (page, _next) = s.app_media_catalog(20, None).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].id, asset.id);

    // Legacy run (no frozen creator): an eligible outcome still links,
    // and the catalog projection honestly reports unknown attribution.
    let (legacy, _w, _t) = record_media(
        &s,
        &run,
        "gen-call-legacy",
        &image_result(),
        Some(("image/png", PNG_1X1)),
    )
    .unwrap();
    assert!(legacy["id"].as_str().is_some());
    let (page, _next) = s.app_media_catalog(20, None).unwrap();
    assert_eq!(
        page.len(),
        2,
        "named and legacy receipts must create two assets"
    );
    let legacy_asset = page
        .iter()
        .find(|a| a.id != asset.id)
        .expect("legacy run produced its own asset");
    assert_eq!(
        legacy_asset.projection()["creator"],
        json!({"kind": "unknown"}),
        "legacy unattributed run must project unknown creator"
    );
}

#[test]
fn cad1365_media_producer_refuses_malformed_original_atomically() {
    let (_dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    s.app_run_decide(
        id,
        run["snapshot_digest"].as_str(),
        false,
        Some("sha256:bundle"),
    )
    .unwrap();
    let dispatched = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    let (writer, turn) = claimed_turn(&s, &dispatched);
    let input_digest = crate::store::app_runs::material_digest(&json!({"prompt":"badge"}));
    s.app_capability_claim(AppCapabilityClaim {
        run: id,
        step: "s1",
        message: &writer.id,
        turn: &turn,
        slot: "generate",
        request: "gen-req",
        binding_digest: "binding-media",
        input_digest: &input_digest,
        call_id: "gen-call-bad",
    })
    .unwrap();

    // Malformed claimed original provenance: the descriptor's `ref`
    // digest does not agree with its `sha256` field (and the digest it
    // embeds is not the job's). A VALID bounded derivative rides along —
    // the refusal must not be rescued by good bytes.
    let mut bad = image_result();
    bad["original_artifact"]["sha256"] = json!(format!("sha256:{}", "f".repeat(64)));
    let recorded = s.app_capability_record(AppCapabilityRecord {
        id: "gen-call-bad",
        run: id,
        step: "s1",
        message: &writer.id,
        turn: &turn,
        slot: "generate",
        request: "gen-req",
        binding_digest: "binding-media",
        input_digest: &input_digest,
        result: &bad,
        asset: Some(("image/png", PNG_1X1)),
    });
    assert!(
        recorded.is_err(),
        "malformed original artifact must refuse, got {recorded:?}"
    );

    // Atomic: no receipt, no catalog asset, no origin row.
    let receipts: i64 = s
        .conn()
        .query_row(
            "SELECT count(*) FROM app_capability_results WHERE run_id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(receipts, 0, "refusal must not record a receipt");
    assert_eq!(
        media_assets_count(&s),
        0,
        "refusal must not catalog an asset"
    );
    let origin_table: i64 = s
        .conn()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='app_media_asset_origins'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    if origin_table == 1 {
        let origins: i64 = s
            .conn()
            .query_row("SELECT count(*) FROM app_media_asset_origins", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(origins, 0, "refusal must not leave an origin row");
    }

    // A legacy non-image result with no original provenance must not
    // invent a catalog original — it records its receipt as before.
    let (_dir2, s2, run2) = runtime_fixture();
    let text_result = json!({"posts":[{"id":"p1","caption":"ok"}]});
    let (receipt, _w, _t) = record_media(&s2, &run2, "gen-call-text", &text_result, None).unwrap();
    assert_eq!(receipt["result"], text_result);
    assert_eq!(
        media_assets_count(&s2),
        0,
        "a non-image result must not invent a catalog asset"
    );
}
