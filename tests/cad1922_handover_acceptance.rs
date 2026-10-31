//! CAD-1922: trial, subscription and checkout are distinct server facts.

use cadence_agent::setup::{plan_fact, OfferRead};
use serde_json::json;

#[test]
fn active_trial_remains_visible_with_an_open_checkout() {
    let offer = OfferRead::Offer(json!({
        "asOf": "2030-01-01T00:00:00Z",
        "plan": {
            "label": "Example", "amount": "10.000000", "currency": "USD",
            "period": "month", "includedCredit": "10.000000", "grantsEnabled": true
        },
        "trial": {
            "state": "active", "eligibility": "used", "deadlineAt": "2030-01-04T00:00:00Z",
            "credit": "7.000000", "tokenCap": 12345, "terms": null
        },
        "subscription": {"state": "none"},
        "checkout": {
            "state": "open", "url": "https://checkout.stripe.com/c/pay/cs_fixture",
            "expiresAt": "2030-01-02T00:00:00Z", "available": false, "blocked": "checkout_open"
        }
    }));
    let fact = plan_fact(&offer);
    assert_eq!(fact.trial.as_ref().unwrap().state, "trial_active");
    assert_eq!(
        fact.subscription.as_ref().unwrap().state,
        "subscription_none"
    );
    assert_eq!(fact.checkout.as_ref().unwrap().state, "checkout_open");
    assert!(fact.detail.contains("2030-01-04T00:00:00Z"));
    assert!(fact.detail.contains("12345"));
    assert_eq!(
        fact.checkout_url.as_deref(),
        Some("https://checkout.stripe.com/c/pay/cs_fixture")
    );
}
