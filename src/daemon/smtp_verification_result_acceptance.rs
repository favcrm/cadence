// Independently authored CAD-1085 acceptance for the production verification mapper.
use super::verification_result;
use crate::error::Error;
use cadence_core::connection_dispatch::dto::{
    DispatchStateV1, ErrorCodeV1, ReceiptErrorV1, ReceiptResultV1, SettlementV1,
    SmtpAcceptanceClaimV1, SmtpAcceptedKindV1, SmtpAcceptedV1,
};

fn verified_settlement() -> SettlementV1 {
    serde_json::from_value(serde_json::json!({
        "v": 1,
        "fence": false,
        "receipt": {
            "v": 1,
            "dispatch_id": "11111111-1111-4111-8111-111111111111",
            "request_id": "22222222-2222-4222-8222-222222222222",
            "payload_digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
            "commitment_digest": "sha256:2222222222222222222222222222222222222222222222222222222222222222",
            "state": "done",
            "result": {"kind": "smtp.verified@1", "verified": true, "claim": "authentication-only"},
            "error": null,
            "cost": {"currency": "USD", "reserved_micros": "0", "actual_micros": "0"},
            "usage": null
        }
    })).unwrap()
}

#[test]
fn only_a_valid_unfenced_smtp_verified_settlement_reports_verification_success() {
    let verified = verified_settlement();
    verified
        .receipt
        .validate()
        .expect("fixture is a valid authentication-only receipt");
    let expected = serde_json::to_value(&verified.receipt).unwrap();
    let reply =
        verification_result(verified.clone()).expect("positive AUTH settlement must succeed");
    assert_eq!(reply["receipt"], expected);
    assert_eq!(reply["fence"], false);

    let mut refused = verified.clone();
    refused.receipt.state = DispatchStateV1::Refused;
    refused.receipt.result = None;
    refused.receipt.error = Some(ReceiptErrorV1::fixed(ErrorCodeV1::ProviderRefused));
    refused
        .receipt
        .validate()
        .expect("AUTH refusal is a valid terminal receipt");
    let error =
        verification_result(refused.clone()).expect_err("AUTH refusal must never become verified");
    assert_eq!(error.code(), Some("provider_refused"));

    let mut unknown = verified.clone();
    unknown.receipt.state = DispatchStateV1::Unknown;
    unknown.receipt.result = None;
    unknown.receipt.error = None;
    unknown.receipt.cost.actual_micros = None;
    unknown
        .receipt
        .validate()
        .expect("interrupted AUTH has a valid unknown receipt");
    assert!(matches!(
        verification_result(unknown.clone()),
        Err(Error::OutcomeUnknown(_))
    ));

    let mut committed = unknown;
    committed.receipt.state = DispatchStateV1::Committed;
    committed
        .receipt
        .validate()
        .expect("an unresolved commitment is not AUTH success");
    assert!(matches!(
        verification_result(committed),
        Err(Error::OutcomeUnknown(_))
    ));

    let mut wrong_result = verified.clone();
    wrong_result.receipt.result = Some(ReceiptResultV1::SmtpAccepted(SmtpAcceptedV1 {
        kind: SmtpAcceptedKindV1::SmtpAccepted,
        accepted: true,
        claim: SmtpAcceptanceClaimV1::ServerAcceptanceOnly,
    }));
    wrong_result
        .receipt
        .validate()
        .expect("send acceptance is valid but is not verification");
    assert!(matches!(
        verification_result(wrong_result),
        Err(Error::OutcomeUnknown(_))
    ));

    let mut fenced = verified.clone();
    fenced.fence = true;
    assert!(matches!(
        verification_result(fenced),
        Err(Error::OutcomeUnknown(_))
    ));
    refused.fence = true;
    assert!(matches!(
        verification_result(refused),
        Err(Error::OutcomeUnknown(_))
    ));

    let mut malformed = verified.clone();
    malformed.receipt.v = 2;
    assert!(malformed.receipt.validate().is_err());
    assert!(matches!(
        verification_result(malformed),
        Err(Error::OutcomeUnknown(_))
    ));

    let mut not_verified = verified;
    if let Some(ReceiptResultV1::SmtpVerified(result)) = not_verified.receipt.result.as_mut() {
        result.verified = false;
    } else {
        panic!("fixture must carry the typed verification result");
    }
    assert!(not_verified.receipt.validate().is_err());
    assert!(matches!(
        verification_result(not_verified),
        Err(Error::OutcomeUnknown(_))
    ));
}
