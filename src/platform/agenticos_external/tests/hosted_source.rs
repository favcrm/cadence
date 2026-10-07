//! CAD-1060: `social.read` over the hosted lease door. The bound account is
//! the credentialless builtin `hosted`; the Worker derives the company from
//! the container, so no request carries a bearer, company or account.
use super::*;

fn proof() -> Value {
    let mut proof = authority();
    proof["binding"]["config"]["account"] = json!("hosted");
    proof["binding"]["config"]["connection_kind"] = json!("builtin");
    proof["quote"] = json!({"schema":1,"currency":"USD","unit_price_micros":2000,"units":1,"total_price_micros":2000,"price_revision":"sha256:price"});
    proof
}

fn view() -> Value {
    json!({"ok":true,"data":{"slug":POSTS_TOOL,"effect":"read","chargePrecondition":"max_charge_minor@1","price":{"currency":"USD","scale":6,"amount":"0.002000"},"unitPrice":null}})
}

fn post() -> Value {
    json!({"id":"post-1","code":"AbCd123","created_at":"2026-09-27T00:00:00Z","user":{"username":"juicysuite_crm","is_private":false},"caption":{"text":"Hosted caption"}})
}

fn receipt(post: Value, amount: &str) -> Value {
    json!({"ok":true,"data":{"slug":POSTS_TOOL,"repeated":false,"price":{"currency":"USD","scale":6,"amount":amount},"result":{"success":true,"status":"ok","items":[post]}}})
}

fn refusal(code: &str) -> Value {
    json!({"ok":false,"error":{"code":code}})
}

fn hosted_at(base: &str) -> AgenticosExternalAdapter {
    let mut host = hosted_adapter();
    // Private unit fixture only: admission still comes from validated
    // metadata. No production origin override or feature seam.
    host.base = base.to_owned();
    host
}

#[test]
fn cad1060_hosted_source_quotes_and_calls_the_lease_door_without_bearer_or_company() {
    let (base, seen, worker) = media_door(|request| match request.method.as_str() {
        "GET" => {
            assert_eq!(request.url, format!("/v1/runtime/tools/{POSTS_TOOL}"));
            json_response(200, view())
        }
        _ => json_response(200, receipt(post(), "0.002000")),
    });
    let host = hosted_at(&base);
    let mut proof = proof();
    let quote = host.quote_app_capability(b"", &proof["binding"]).unwrap();
    assert_eq!(quote.total_price_micros, 2000);
    proof["quote"] = json!(quote);
    let output = host
        .execute_app_capability(b"", &proof, &json!({}), "app-call-1060")
        .unwrap();
    assert!(output.asset.is_none());
    assert_eq!(output.result["posts"][0]["caption"], "Hosted caption");
    assert_eq!(output.result["charge"]["amount"], "0.002000");
    worker.join().unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "one quote read and one call");
    assert!(seen.iter().all(|request| request.auth.is_none()));
    assert_eq!(seen[1].url, CALL_PATH);
    assert_eq!(seen[1].idem.as_deref(), Some("app-call-1060"));
    // Company and account come from the container binding, never the body.
    assert_eq!(
        seen[1].body,
        json!({"slug":POSTS_TOOL,"query":{"handle":"juicysuite_crm"},"max_charge_minor":2000})
    );
}

#[test]
fn cad1060_hosted_source_refuses_forged_custody_binding_and_context_before_traffic() {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    let host = hosted_at(&base);
    let refused = |adapter: &AgenticosExternalAdapter, forged: &Value, credential: &[u8]| {
        let quote = adapter.quote_app_capability(credential, &forged["binding"]);
        let call = adapter.execute_app_capability(credential, forged, &json!({}), "app-call-1060");
        assert!(quote.is_err() && call.is_err(), "{forged}");
    };
    // A token in custody never travels to the hosted door.
    refused(&host, &proof(), b"fake-bearer");
    refused(&host, &proof(), b"\xff");
    // An external door never inherits lease authority for a hosted binding.
    refused(&image_adapter(&base), &proof(), b"");
    for (field, value) in [
        ("account", json!("company1")),
        ("account", json!("ws_11111111-1111-4111-8111-111111111111")),
        ("account", Value::Null),
        ("connection_kind", json!("enrolled")),
        ("connection_kind", Value::Null),
        ("provider", json!("agenticos")),
        ("connection_id", Value::Null),
    ] {
        let mut forged = proof();
        forged["binding"]["config"][field] = value;
        refused(&host, &forged, b"");
    }
    for field in [
        "capability",
        "version",
        "action",
        "resource_kind",
        "tool",
        "effect",
    ] {
        let mut forged = proof();
        forged["binding"]["config"]["mapping"][field] = Value::Null;
        refused(&host, &forged, b"");
    }
    // A binding frozen for another install, or a worker-chosen company,
    // account, profile, tool, URL or transport, never reaches the door.
    let mut foreign = proof();
    foreign["install_id"] = json!("install-other");
    // Hosted mode makes the ceiling optional on the wire, so a run without
    // its frozen one-unit quote must never send an uncapped call.
    let mut uncapped = proof();
    uncapped["quote"] = Value::Null;
    let mut widened = proof();
    widened["quote"]["total_price_micros"] = json!(3000);
    let mut inputs = vec![
        (foreign, json!({})),
        (uncapped, json!({})),
        (widened, json!({})),
    ];
    for input in [
        json!({"company":"other"}),
        json!({"account":"hosted"}),
        json!({"handle":"other_profile"}),
        json!({"tool":IMAGE_TOOL}),
        json!({"url":"http://127.0.0.1/"}),
        json!({"transport":"hosted-media-lease@1"}),
    ] {
        inputs.push((proof(), input));
    }
    for (forged, input) in inputs {
        assert!(host
            .execute_app_capability(b"", &forged, &input, "app-call-1060")
            .is_err());
    }
    assert!(server
        .recv_timeout(Duration::from_millis(20))
        .unwrap()
        .is_none());
}

#[test]
fn cad1060_hosted_source_quote_fails_closed_without_the_charge_ceiling_contract() {
    let mut legacy = view();
    legacy["data"]["chargePrecondition"] = Value::Null;
    for (status, payload, needle) in [
        (200u16, legacy, "charge ceiling"),
        (404, refusal("not_allowlisted"), "not_allowlisted"),
        (409, refusal("disabled"), "disabled"),
        (409, refusal("unpriced"), "unpriced"),
        (409, refusal("lease_lost"), "lease_lost"),
    ] {
        let (base, seen, worker) = media_door(move |_| json_response(status, payload.clone()));
        let error = hosted_at(&base)
            .quote_app_capability(b"", &proof()["binding"])
            .unwrap_err();
        assert!(error.contains(needle), "{error} lacks {needle}");
        worker.join().unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
}

#[test]
fn cad1060_hosted_source_unavailable_gateway_and_bad_receipts_return_nothing() {
    let mut private = post();
    private["user"]["is_private"] = json!(true);
    let mut unnamed = post();
    unnamed["id"] = Value::Null;
    let mut drift = receipt(post(), "0.002000");
    drift["data"]["slug"] = json!(IMAGE_TOOL);
    let oversized = json!({"ok":true,"data":{"pad":"x".repeat(RESPONSE_CAP as usize)}});
    for (status, payload, needle) in [
        (
            503u16,
            refusal("billing_unavailable"),
            "billing_unavailable",
        ),
        (409, refusal("price_changed"), "price_changed"),
        (409, refusal("disabled"), "disabled"),
        (409, refusal("unpriced"), "unpriced"),
        (404, refusal("not_allowlisted"), "not_allowlisted"),
        (200, oversized, "supported bound"),
        (200, receipt(unnamed, "0.002000"), "identity"),
        (200, receipt(private, "0.002000"), "private"),
        (200, receipt(post(), "0.002001"), "approved charge"),
        (200, drift, "identity changed"),
    ] {
        let (base, seen, worker) = media_door(move |_| json_response(status, payload.clone()));
        let error = hosted_at(&base)
            .execute_app_capability(b"", &proof(), &json!({}), "app-call-1060")
            .err()
            .unwrap();
        assert!(error.contains(needle), "{error} lacks {needle}");
        worker.join().unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "one POST and no retry for {needle}");
        assert!(seen[0].auth.is_none());
        // The hosted door does not require a ceiling; every call sends it.
        assert_eq!(seen[0].body["max_charge_minor"], 2000, "{needle}");
    }
    let dead = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let host = hosted_at(&format!("http://{}", dead.server_addr().to_ip().unwrap()));
    drop(dead);
    let error = host
        .execute_app_capability(b"", &proof(), &json!({}), "app-call-1060")
        .err()
        .unwrap();
    assert!(error.contains("outcome is uncertain"), "{error}");
    assert!(host.quote_app_capability(b"", &proof()["binding"]).is_err());
}

#[test]
fn cad1060_hosted_source_replay_returns_the_recorded_receipt_from_one_post() {
    let mut replayed = receipt(post(), "0.002000");
    replayed["data"]["repeated"] = json!(true);
    let (base, seen, worker) = media_door(move |_| json_response(200, replayed.clone()));
    let output = hosted_at(&base)
        .execute_app_capability(b"", &proof(), &json!({}), "app-call-1060")
        .unwrap();
    assert_eq!(output.result["repeated"], true);
    assert_eq!(output.result["posts"][0]["id"], "post-1");
    worker.join().unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].idem.as_deref(), Some("app-call-1060"));
    assert_eq!(seen[0].body["max_charge_minor"], 2000);
}

#[test]
fn cad1060_hosted_unit_priced_view_freezes_the_unit_price_as_the_ceiling() {
    // A unit-priced hosted row reports price 0 and the real cost in unitPrice.
    let mut unit = view();
    unit["data"]["price"]["amount"] = json!("0.000000");
    unit["data"]["unitPrice"] = json!({"currency":"USD","scale":6,"amount":"0.002000"});
    let (base, seen, worker) = media_door(move |request| match request.method.as_str() {
        "GET" => json_response(200, unit.clone()),
        _ => json_response(200, receipt(post(), "0.002000")),
    });
    let host = hosted_at(&base);
    let mut proof = proof();
    let quote = host.quote_app_capability(b"", &proof["binding"]).unwrap();
    assert_eq!(quote.total_price_micros, 2000);
    proof["quote"] = json!(quote);
    host.execute_app_capability(b"", &proof, &json!({}), "app-call-1060")
        .unwrap();
    worker.join().unwrap();
    assert_eq!(seen.lock().unwrap()[1].body["max_charge_minor"], 2000);
}
