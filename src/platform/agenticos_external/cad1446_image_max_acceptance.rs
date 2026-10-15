//! CAD-1446 independent acceptance: the real adapter's wire request, not a
//! replacement submit implementation. All amounts and credentials are synthetic.
//! AOS-248 separately exercises the real server admission/pricing/ledger guard.
//! Production implementers must not edit or weaken this acceptance driver.

use super::tests::{image_proof, json_response, media_door};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn cad1446_image_max_acceptance() {
    // AOS media/unified-quote.ts::frozenPriceMinor and Core's native quote
    // both use scale-6 USD. This deliberately non-round figure must travel
    // unchanged: no cents conversion, provider-credit conversion or rounding.
    const APPROVED: u64 = 43_219;
    let price_reads = AtomicUsize::new(0);
    let (base, seen, worker) = media_door(move |request| {
        if request.method == "GET" && request.url == MEDIA_PRICE_PATH {
            let first = price_reads.fetch_add(1, Ordering::SeqCst) == 0;
            return json_response(
                200,
                json!({"ok":true,"data":{"kind":"image","model":IMAGE_MODEL,
                    "price":{"slug":IMAGE_TOOL,"chargeMinor":if first { APPROVED } else { APPROVED * 2 },
                    "currency":"USD","version":if first { "synthetic-v1" } else { "synthetic-v2" }}}}),
            );
        }
        if request.method == "POST" && request.url == MEDIA_SUBMIT_PATH {
            return if request.body["max_charge_minor"] == APPROVED - 1 {
                json_response(400, json!({"ok":false,"error":{"code":"price_limit"}}))
            } else {
                // An uncertain outcome must never cause an automatic second
                // POST. This is only transport behavior, not server billing proof.
                json_response(409, json!({"ok":false,"error":{"code":"uncertain"}}))
            };
        }
        json_response(
            400,
            json!({"ok":false,"error":{"code":"unexpected_request"}}),
        )
    });
    // Keep the reviewed @4 door; no deployment metadata or pin changes.
    let adapter =
        AgenticosExternalAdapter::with_deployment_pin(&base, Some(TEXT_MANIFEST_PIN)).unwrap();
    let binding = image_proof(Value::Null)["binding"].clone();
    let approved = adapter
        .quote_app_capability(b"synthetic-cad1446-token", &binding)
        .unwrap();
    assert!(approved.valid());
    assert_eq!(approved.currency, "USD");
    assert_eq!(approved.unit_price_micros, APPROVED);
    assert_eq!(approved.units, 1);
    assert_eq!(approved.total_price_micros, APPROVED);
    let mut proof = image_proof(serde_json::to_value(&approved).unwrap());
    proof["frozen_prompt"] = json!("A synthetic harbour illustration");

    // A later price read does not authorize the larger amount. The old,
    // frozen approval, not current pricing or caller input, owns each POST.
    let later = adapter
        .quote_app_capability(b"synthetic-cad1446-token", &binding)
        .unwrap();
    assert_eq!(later.total_price_micros, APPROVED * 2);
    assert_ne!(later.price_revision, approved.price_revision);
    for _ in 0..2 {
        let result = adapter.execute_app_capability_outcome(
            b"synthetic-cad1446-token",
            &proof,
            &json!({"max_charge_minor":APPROVED * 9,"quote":{"total_price_micros":APPROVED * 9}}),
            "cad1446-image-same-key",
        );
        assert!(matches!(result, Err(AppCapabilityError::Uncertain(_))));
    }
    let mut lower = proof.clone();
    lower["quote"]["unit_price_micros"] = json!(APPROVED - 1);
    lower["quote"]["total_price_micros"] = json!(APPROVED - 1);
    let result = adapter.execute_app_capability_outcome(
        b"synthetic-cad1446-token",
        &lower,
        &json!({}),
        "cad1446-image-same-key",
    );
    assert!(matches!(result, Err(AppCapabilityError::Refused(_))));

    // Missing/malformed native approval must refuse locally, before ANY
    // transport, rather than silently omit the AOS-required maximum.
    let mut invalid = vec![Value::Null, json!({})];
    for value in [
        Value::Null,
        json!(-1),
        json!(1.25),
        json!("43219"),
        json!(0),
        json!(9_007_199_254_740_992_u64),
    ] {
        let mut quote = proof["quote"].clone();
        quote["total_price_micros"] = value;
        invalid.push(quote);
    }
    for (field, value) in [
        ("schema", json!(2)),
        ("currency", json!("HKD")),
        ("units", json!(2)),
        ("unit_price_micros", json!(APPROVED + 1)),
        ("price_revision", json!("")),
    ] {
        let mut quote = proof["quote"].clone();
        quote[field] = value;
        invalid.push(quote);
    }
    let mut missing_maximum = proof["quote"].clone();
    missing_maximum
        .as_object_mut()
        .unwrap()
        .remove("total_price_micros");
    invalid.push(missing_maximum);
    for quote in invalid {
        let mut malformed = proof.clone();
        malformed["quote"] = quote;
        let result = adapter.execute_app_capability_outcome(
            b"synthetic-cad1446-token",
            &malformed,
            &json!({}),
            "cad1446-image-malformed",
        );
        assert!(matches!(result, Err(AppCapabilityError::Refused(_))));
    }

    // Preserve the existing fixture's full quiet window and adapter lifetime:
    // late or automatic retries must be observed, not hidden by teardown.
    worker.join().unwrap();
    let requests = seen.lock().unwrap();
    assert_eq!(
        requests.len(),
        5,
        "two quotes and three explicit submits only"
    );
    assert_eq!(requests.iter().filter(|r| r.method == "GET").count(), 2);
    let posts: Vec<_> = requests.iter().filter(|r| r.method == "POST").collect();
    assert_eq!(posts.len(), 3);
    assert_eq!(posts[0].body, posts[1].body);
    for (index, request) in posts.iter().enumerate() {
        assert_eq!(request.url, MEDIA_SUBMIT_PATH);
        assert_eq!(
            request.auth.as_deref(),
            Some("Bearer synthetic-cad1446-token")
        );
        assert_eq!(request.idem.as_deref(), Some("cad1446-image-same-key"));
        assert_eq!(request.body["model"], IMAGE_MODEL);
        assert_eq!(request.body["prompt"], "A synthetic harbour illustration");
        assert_eq!(
            request.body["max_charge_minor"],
            if index == 2 { APPROVED - 1 } else { APPROVED }
        );
    }
}
