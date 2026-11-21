//! Billing offer contract decoding and [`PlanFact`] rendering for first-run handover.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use super::{OfferRead, PlanFact, PlanState};

/// The published offer, as the server sends it. Only the fields the
/// handover prints are read; an absent or mistyped one makes the whole
/// offer unavailable rather than a default plan, price or expiry.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OfferBody {
    as_of: String,
    plan: Option<PlanBody>,
    trial: TrialBody,
    subscription: SubscriptionBody,
    checkout: CheckoutBody,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanBody {
    label: String,
    amount: String,
    currency: String,
    period: String,
    included_credit: String,
    grants_enabled: bool,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrialBody {
    state: String,
    eligibility: String,
    deadline_at: Option<String>,
    credit: Option<String>,
    token_cap: Option<u64>,
    terms: Option<TrialTerms>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrialTerms {
    policy_id: String,
    duration_hours: u64,
    credit: String,
    token_cap: u64,
}

#[derive(serde::Deserialize)]
struct SubscriptionBody {
    state: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckoutBody {
    state: String,
    url: Option<String>,
    expires_at: Option<String>,
    available: bool,
    blocked: Option<String>,
}

/// The plan fact for one read. Trial and paid subscription stay separate
/// states: the cardless trial is a promotional grant with its own deadline
/// and cap, and a paid plan exists only after an explicit owner Checkout.
pub fn plan_fact(read: &OfferRead) -> PlanFact {
    let offer = match read {
        OfferRead::Offer(offer) => offer,
        OfferRead::Unavailable { code, detail } => {
            return PlanFact::unavailable(code, format!("Billing unavailable: {detail}"))
        }
    };
    let body: OfferBody = match serde_json::from_value((*offer).clone()) {
        Ok(body) => body,
        Err(_) => {
            return PlanFact::unavailable(
                "invalid",
                "Billing unavailable: the offer did not match the published contract; no plan, price, trial or expiry is assumed",
            )
        }
    };
    let published_plan = published_plan_line(&body);
    // Read all three server facts before choosing a summary. A Checkout or a
    // paid subscription must never hide the trial (and vice versa): each is
    // reported in its own field below, and the combined line names all three.
    let trial = match trial_state(&body.trial, &body.as_of) {
        Ok(trial) => trial,
        Err(fact) => return fact,
    };
    let subscription = match subscription_state(&body.subscription, &body.as_of) {
        Ok(subscription) => subscription,
        Err(fact) => return fact,
    };
    let checkout = match checkout_state(&body.checkout, &body.as_of) {
        Ok(checkout) => checkout,
        Err(fact) => return fact,
    };
    // The summary keeps the previous precedence for the one plan fact; the
    // three component states above are reported independently regardless.
    let state = match subscription.state {
        "subscription_active" => "skipped",
        "subscription_past_due" => "past_due",
        "subscription_unknown" => "subscription_unknown",
        _ => match checkout.state {
            "checkout_pending" | "checkout_open" | "checkout_completed" | "checkout_settled"
            | "checkout_unknown" => checkout.state,
            _ => trial.state,
        },
    };
    // `available`/`blocked` describe a *new* Checkout only. A fenced
    // operation is reported by its own state even when it reports
    // `available: false`, so availability is consulted only when the server
    // says no operation is outstanding.
    let paid = if matches!(
        subscription.state,
        "subscription_none" | "subscription_inactive"
    ) && matches!(
        checkout.state,
        "checkout_none" | "checkout_expired" | "checkout_settled"
    ) {
        match paid_line(&body) {
            Ok(paid) => paid,
            Err(fact) => return fact,
        }
    } else {
        ""
    };
    let detail = [
        trial.detail.as_str(),
        subscription.detail.as_str(),
        checkout.detail.as_str(),
        published_plan.as_str(),
        paid,
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" ");
    PlanFact {
        state,
        detail,
        checkout_url: if checkout.state == "checkout_open" {
            body.checkout.url.clone()
        } else {
            None
        },
        code: None,
        // Boxed: these three facts make `PlanFact` the error value of the
        // state helpers below, and clippy refuses a 200-byte `Err`.
        trial: Some(Box::new(trial)),
        subscription: Some(Box::new(subscription)),
        checkout: Some(Box::new(checkout)),
    }
}

/// The published plan line. Only the server's own plan fields are printed;
/// an absent plan is reported as unread, never as a default price.
fn published_plan_line(body: &OfferBody) -> String {
    match &body.plan {
        Some(plan) => format!(
            "Plan {}: {} {}/{} with {} included credit (grants {}), as published at {}.",
            plan.label,
            money(&plan.amount),
            plan.currency,
            plan.period,
            money(&plan.included_credit),
            if plan.grants_enabled {
                "enabled"
            } else {
                "disabled"
            },
            body.as_of
        ),
        None => format!(
            "The published plan could not be read at {} — no price is assumed.",
            body.as_of
        ),
    }
}

/// The server's stored paid-subscription truth, reported on its own. `unknown`
/// is never described as inactive.
fn subscription_state(
    subscription: &SubscriptionBody,
    as_of: &str,
) -> std::result::Result<PlanState, PlanFact> {
    match subscription.state.as_str() {
        "active" => Ok(PlanState::new(
            "subscription_active",
            format!("Subscription active at {as_of}. No new plan needed."),
        )),
        "past_due" => Ok(PlanState::new(
            "subscription_past_due",
            format!("Subscription past due at {as_of} — it is not an active subscription."),
        )),
        "inactive" => Ok(PlanState::new(
            "subscription_inactive",
            format!("Subscription inactive at {as_of} — no paid plan is active."),
        )),
        "none" => Ok(PlanState::new(
            "subscription_none",
            format!("No paid subscription at {as_of}."),
        )),
        "unknown" => Ok(PlanState::new(
            "subscription_unknown",
            format!(
                "Subscription state unknown at {as_of} — it is not an active subscription, \
                 and no new Checkout starts until it can be read."
            ),
        )),
        other => Err(PlanFact::unavailable(
            "invalid",
            format!(
                "Billing unavailable: the offer reported an unknown subscription state `{other}`; it is not an active subscription and nothing is assumed"
            ),
        )),
    }
}

/// The durable Checkout operation, reported on its own. Only a state that
/// fences nothing (`none`, `expired`, `settled`) leaves a new Checkout
/// possible; `available`/`blocked` are consulted separately for those.
fn checkout_state(
    checkout: &CheckoutBody,
    as_of: &str,
) -> std::result::Result<PlanState, PlanFact> {
    match checkout.state.as_str() {
        "open" => {
            let until = checkout.expires_at.as_deref().unwrap_or("its expiry");
            let where_ = checkout
                .url
                .as_deref()
                .map(|url| format!(": {url}"))
                .unwrap_or_default();
            Ok(PlanState::new(
                "checkout_open",
                format!(
                    "A Checkout is already open until {until}{where_}. No second Checkout is \
                     opened here and no card is taken."
                ),
            ))
        }
        "pending" => Ok(PlanState::new(
            "checkout_pending",
            "A Checkout is pending — Stripe has not acknowledged a session yet. No second \
             Checkout may start; re-read the offer before acting.",
        )),
        "completed" => Ok(PlanState::new(
            "checkout_completed",
            format!(
                "Stripe completed a Checkout at {as_of}; the subscription is not reconciled \
                 yet. No second Checkout may start."
            ),
        )),
        "settled" => Ok(PlanState::new(
            "checkout_settled",
            format!(
                "A Checkout completed and was reconciled at {as_of}; no Checkout is fenced."
            ),
        )),
        "unknown" => Ok(PlanState::new(
            "checkout_unknown",
            format!(
                "The Checkout outcome is unknown at {as_of} — no new Checkout may start until \
                 the server reconciles it."
            ),
        )),
        "none" => Ok(PlanState::new(
            "checkout_none",
            format!("No Checkout is outstanding at {as_of}."),
        )),
        "expired" => Ok(PlanState::new(
            "checkout_expired",
            format!("The last Checkout expired before {as_of}; none is outstanding."),
        )),
        other => Err(PlanFact::unavailable(
            "invalid",
            format!(
                "Billing unavailable: the offer reported an unknown Checkout state `{other}`; no Checkout is started and none is assumed"
            ),
        )),
    }
}

/// What the offer says about starting a *new* Checkout. Only a state that
/// fences nothing (`none`, `expired`, `settled`) consults this: for
/// `pending`/`open`/`completed`/`unknown` the state itself is the answer,
/// and `blocked` is expected to name that same operation.
fn paid_line(body: &OfferBody) -> std::result::Result<&'static str, PlanFact> {
    if body.checkout.available {
        return Ok(
            "Starting a paid plan is a separate, explicit action in the workspace dashboard; this terminal never opens a Checkout.",
        );
    }
    match body.checkout.blocked.as_deref() {
        Some("plan_unavailable") => Ok("A paid plan cannot start right now (plan_unavailable)."),
        other => Err(PlanFact::unavailable(
            "invalid",
            format!(
                "Billing unavailable: the offer says no Checkout can start ({}) while reporting subscription `{}` and Checkout `{}`; no plan state is assumed",
                other.unwrap_or("no reason"),
                body.subscription.state,
                body.checkout.state
            ),
        )),
    }
}

/// The trial's own state, server-authored and reported independently of the
/// subscription and Checkout. `not_started` is not an error and not
/// ineligibility — eligibility says which; terms exist only when the server
/// publishes them. `eligible` is a decision this read can make, not a
/// promise: only the registration door grants the trial.
fn trial_state(trial: &TrialBody, as_of: &str) -> std::result::Result<PlanState, PlanFact> {
    match trial.state.as_str() {
        "active" => {
            let until = trial.deadline_at.as_deref().unwrap_or("its deadline");
            let mut bounds = Vec::new();
            if let Some(credit) = trial.credit.as_deref() {
                bounds.push(format!("{} credit", money(credit)));
            }
            if let Some(cap) = trial.token_cap {
                bounds.push(format!("{cap} tokens"));
            }
            let limits = if bounds.is_empty() {
                " (credit and token cap not published)".to_string()
            } else {
                format!(" ({})", bounds.join(" and "))
            };
            Ok(PlanState::new(
                "trial_active",
                format!(
                    "Cardless trial active until {until}{limits}; it never converts to a paid \
                     plan by itself."
                ),
            ))
        }
        "not_started" => match trial.eligibility.as_str() {
            "eligible" => match &trial.terms {
                Some(terms) => Ok(PlanState::new(
                    "trial_eligible",
                    format!(
                        "The cardless registration trial is eligible under policy {} ({} hours, {} credit, {} tokens); only the registration door grants it and it can still fail at admission. It never opens a Checkout or charges.",
                        terms.policy_id,
                        terms.duration_hours,
                        money(&terms.credit),
                        terms.token_cap
                    ),
                )),
                None => Ok(PlanState::new(
                    "trial_unknown",
                    "The cardless trial is eligible for this workspace, but the server \
                     published no trial terms — no duration, cap or expiry is assumed.",
                )),
            },
            "used" => Ok(PlanState::new(
                "trial_used",
                "The cardless trial was already used for this workspace, its owner or its \
                 email domain.",
            )),
            "unavailable" => Ok(PlanState::new(
                "trial_unknown",
                "This deployment cannot grant a trial (unavailable) — do not assume one applies.",
            )),
            "unproven" => Ok(PlanState::new(
                "trial_unknown",
                format!(
                    "Trial eligibility is unproven at {as_of}: this read found no prior use, \
                     but only the registration door can start the trial, so nothing is claimed."
                ),
            )),
            "unknown" => Ok(PlanState::new(
                "trial_unknown",
                format!(
                    "Trial eligibility is unknown at {as_of} — do not assume the trial applies."
                ),
            )),
            other => Err(PlanFact::unavailable(
                "invalid",
                format!(
                    "Billing unavailable: the offer reported an unknown trial eligibility `{other}`; no trial, duration or expiry is assumed"
                ),
            )),
        },
        "expired" => Ok(PlanState::new(
            "trial_expired",
            "The cardless trial expired.",
        )),
        "failed" => Ok(PlanState::new(
            "trial_failed",
            "The cardless trial did not start (failed).",
        )),
        "unavailable" => Ok(PlanState::new(
            "trial_unknown",
            "The trial record could not be read (unavailable) — nothing is assumed.",
        )),
        other => Err(PlanFact::unavailable(
            "invalid",
            format!(
                "Billing unavailable: the offer reported an unknown trial state `{other}`; no trial, duration or expiry is assumed"
            ),
        )),
    }
}

/// `20.000000` → `20.00`; anything else is printed exactly as the server
/// sent it. Display only — no rounding, no invented precision, and no
/// byte-index slicing of a string that is not already ASCII digits.
fn money(scaled: &str) -> String {
    let Some((whole, fraction)) = scaled.split_once('.') else {
        return scaled.to_string();
    };
    let bytes = fraction.as_bytes();
    if whole.bytes().all(|b| b.is_ascii_digit())
        && bytes.len() == 6
        && bytes.iter().all(u8::is_ascii_digit)
        && bytes[2..].iter().all(|b| *b == b'0')
    {
        return format!("{whole}.{}", &fraction[..2]);
    }
    scaled.to_string()
}
