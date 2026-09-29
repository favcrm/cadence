//! CAD-786: the approved, bounded campaign send.
//!
//! `crm_send_prepare` commits every send input the operator approves —
//! content revision+digest, audience freeze+digest, sender
//! link+credential revision+digest, unsubscribe origin — into one
//! `send_digest` and a `prepared` row. `crm_send_approve` requires that
//! exact digest, re-runs every check, then in one App transaction CASes
//! `prepared → sending` and materializes the per-recipient `queued`
//! deliveries plus their sha256'd unsubscribe tokens; the core
//! `crm_sends` row is its PII-free crash witness. One in-process worker
//! per send walks the queue at `crm_send_interval`, reclaiming nothing
//! that is not `queued`, re-verifying every check before each
//! submission, holding the custody lock across dial+submit.
//!
//! Trust boundaries: writes are operator actions behind strict
//! per-method field allowlists — any field outside the list refuses.
//! The unsubscribe redeem is deliberately NOT operator-gated — the
//! 256-bit token is the credential — and reveals nothing but a
//! constant `{"unsubscribed": true}`.

use super::*;
use crate::store::app_records::RecordStore;
use crate::store::app_sends::{
    delivery_idempotency_key, masked_email, mint_unsubscribe_token, send_digest, send_view,
    unsubscribe_token_hash, SendDraft, Sendable,
};
use serde_json::{json, Value};
use std::sync::Arc;

/// Deferred/not-submitted rows retry bounded times, then `failed`.
const DELIVERY_ATTEMPT_MAX: i64 = 3;

/// The outcome of one claimed row, handed back across the scope
/// proof so the caller can write the row's next state.
enum Step {
    /// Consent/suppression refused — the row is `suppressed`.
    Suppressed(String),
    /// A submission ran; the row follows its classified outcome.
    Done(crate::platform::smtp::SmtpOutcome),
}

impl Shared {
    /// The send-level verbs: `crm_send_prepare`, `crm_send_approve`,
    /// `crm_send_show`, `crm_send_list`, `crm_send_resolve`. Every
    /// field outside each verb's allowlist refuses — identity,
    /// routing and secret material can never smuggle in.
    pub(super) fn rpc_crm_send(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection(method, params, peer_pid)?;
        let allowed: &[&str] = match method {
            "crm_send_prepare" => &[
                "install_id",
                "context_id",
                "campaign_id",
                "audience_freeze_id",
                "request_id",
            ],
            "crm_send_approve" => &["install_id", "context_id", "send_id", "send_digest"],
            "crm_send_show" => &["install_id", "context_id", "send_id"],
            "crm_send_list" => &["install_id", "context_id", "campaign_id"],
            "crm_send_resolve" => &[
                "install_id",
                "context_id",
                "send_id",
                "customer_id",
                "resolution",
            ],
            _ => return Err(Error::rejected("unknown CRM send method")),
        };
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("request body must be an object"))?;
        for (key, value) in fields {
            if !allowed.contains(&key.as_str()) {
                return Err(Error::rejected(format!("{key} is not a {method} field")));
            }
            if !value.is_string() {
                return Err(Error::rejected(format!("{key} must be a string")));
            }
        }
        let install = required_str(params, "install_id")?;
        let context = required_str(params, "context_id")?;
        match method {
            "crm_send_prepare" => {
                let campaign = required_str(params, "campaign_id")?;
                let freeze = required_str(params, "audience_freeze_id")?;
                let request = send_request_id(params)?;
                crate::proto::identifier(campaign, "campaign ID")?;
                crate::proto::identifier(freeze, "audience freeze ID")?;
                self.crm_send_prepare(install, context, campaign, freeze, &request)
            }
            "crm_send_approve" => {
                let send_id = required_str(params, "send_id")?;
                let digest = required_str(params, "send_digest")?;
                crate::proto::identifier(send_id, "send ID")?;
                self.crm_send_approve(install, context, send_id, digest)
            }
            "crm_send_show" => {
                let send_id = required_str(params, "send_id")?;
                crate::proto::identifier(send_id, "send ID")?;
                self.crm_send_show(install, context, send_id)
            }
            "crm_send_list" => {
                let campaign = params.get("campaign_id").and_then(Value::as_str);
                if let Some(c) = campaign {
                    crate::proto::identifier(c, "campaign ID")?;
                }
                self.crm_send_list(install, context, campaign)
            }
            "crm_send_resolve" => {
                let send_id = required_str(params, "send_id")?;
                let customer = required_str(params, "customer_id")?;
                let resolution = required_str(params, "resolution")?;
                crate::proto::identifier(send_id, "send ID")?;
                crate::proto::identifier(customer, "customer ID")?;
                if !matches!(resolution, "accepted" | "failed") {
                    return Err(Error::rejected("resolution must be 'accepted' or 'failed'"));
                }
                self.crm_send_resolve(install, context, send_id, customer, resolution)
            }
            _ => unreachable!(),
        }
    }

    /// `crm_unsubscribe_redeem` — deliberately NOT operator-gated:
    /// the recipient's 256-bit token IS the credential, and the
    /// answer reveals nothing either way.
    pub(super) fn rpc_crm_unsubscribe_redeem(&self, params: &Value) -> Result<Value> {
        let shaped = match params.as_object() {
            Some(fields)
                if fields.len() == 1
                    && fields
                        .get("token")
                        .and_then(Value::as_str)
                        .is_some_and(|t| {
                            t.len() == 43
                                && t.chars()
                                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                        }) =>
            {
                fields["token"].as_str().unwrap_or_default().to_string()
            }
            _ => String::new(),
        };
        if !shaped.is_empty() {
            let hash = unsubscribe_token_hash(&shaped);
            if let Ok(Some((install, context))) = self.store.crm_unsubscribe_index_lookup(&hash) {
                if let Ok(records) = RecordStore::open(&self.state_dir, &install) {
                    if let Ok(Some((_, customer_id, send_id))) =
                        records.app_unsubscribe_token_lookup(&hash)
                    {
                        // The customer's address comes from its own
                        // delivery row — the token row stays id-only.
                        let email = records
                            .app_campaign_delivery(&context, &send_id, &customer_id)
                            .ok()
                            .flatten()
                            .map(|d| d.email);
                        let _ = records.app_suppression_add(
                            &context,
                            None,
                            Some(&customer_id),
                            "unsubscribed",
                        );
                        if let Some(email) = email {
                            let _ = records.app_suppression_add(
                                &context,
                                Some(&email),
                                None,
                                "unsubscribed",
                            );
                        }
                    }
                }
            }
        }
        Ok(json!({"unsubscribed": true}))
    }

    /// `unsubscribe_origin` must be a base the recipient's browser can
    /// open: `https://` anywhere, or loopback `http://` for the
    /// isolated test rigs. Anything else refuses rather than minting
    /// dead links.
    fn crm_send_origin(&self) -> Result<String> {
        let origin = self
            .unsubscribe_origin
            .clone()
            .ok_or_else(|| Error::rejected("unsubscribe origin is not configured"))?;
        if !unsubscribe_origin_valid(&origin) {
            return Err(Error::rejected(
                "unsubscribe origin must be https, or http on a loopback host",
            ));
        }
        Ok(origin.trim_end_matches('/').to_string())
    }

    /// The facts `send_digest` commits and `approve` re-verifies:
    /// content at its approved revision, the live audience freeze,
    /// the live sender link and the authority behind it. Drift since
    /// the approve click refuses — never sends stale inputs.
    fn crm_send_facts(
        self: &Arc<Self>,
        install: &str,
        context: &str,
        records: &RecordStore,
        campaign: &str,
        freeze: &str,
    ) -> Result<SendFacts> {
        let origin = self.crm_send_origin()?;
        let (content_revision, content_digest) = records.app_content_approved(context, campaign)?;
        let audience = records.app_audience_show(context, freeze)?;
        if audience["valid"] != json!(true) {
            return Err(Error::rejected(
                "audience freeze is stale; refreeze before preparing a send",
            ));
        }
        let frozen = records.app_audience_frozen(context, freeze)?;
        let _custody = self
            .platform_custody_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let link = self.store.crm_smtp_link(install, context)?.ok_or_else(|| {
            Error::rejected("no SMTP sender is bound to this installation and context")
        })?;
        if link.state != "live" {
            return Err(Error::rejected(
                "SMTP sender binding is revoked; bind it again instead",
            ));
        }
        self.crm_smtp_authority(&link.connection_id, link.auth_revision)?;
        Ok(SendFacts {
            origin,
            content_revision,
            content_digest,
            frozen_member_ids: frozen.member_ids,
            frozen_digest: frozen.digest,
            max_recipients: frozen.max_recipients,
            link,
        })
    }

    fn crm_send_prepare(
        self: &Arc<Self>,
        install: &str,
        context: &str,
        campaign: &str,
        freeze: &str,
        request_id: &str,
    ) -> Result<Value> {
        self.crm_smtp_scope(install, context, || {
            let records = RecordStore::open(&self.state_dir, install)?;
            let facts = self.crm_send_facts(install, context, &records, campaign, freeze)?;
            if (facts.frozen_member_ids.len() as i64) > facts.max_recipients {
                return Err(Error::rejected(
                    "frozen audience exceeds its max_recipients ceiling",
                ));
            }
            if !records.app_campaign_test_send_accepted(
                context,
                campaign,
                &facts.content_digest,
                &facts.link.digest,
            )? {
                return Err(Error::rejected(
                    "an SMTP-accepted test send of this exact content and binding is required first",
                ));
            }
            let mut draft = SendDraft {
                send_id: format!("send-{request_id}"),
                campaign_id: campaign.to_string(),
                request_id: request_id.to_string(),
                content_revision: facts.content_revision,
                content_digest: facts.content_digest,
                audience_freeze_id: freeze.to_string(),
                audience_digest: facts.frozen_digest,
                connection_id: facts.link.connection_id.clone(),
                auth_revision: facts.link.auth_revision,
                link_revision: facts.link.link_revision,
                link_digest: facts.link.digest.clone(),
                max_recipients: facts.max_recipients,
                send_digest: String::new(),
            };
            draft.send_digest = send_digest(install, context, &draft);
            let stored = records.app_campaign_send_prepare(context, &draft)?;
            // Counts plus a bounded masked sample: `suppressed_now`
            // shows the live suppression shadow on the frozen list
            // before the operator approves.
            let mut suppressed_now = 0_i64;
            let mut sample = Vec::new();
            for customer_id in &facts.frozen_member_ids {
                match records.app_customer_sendable(context, customer_id)? {
                    Sendable::Ok { email, .. } => {
                        if sample.len() < 5 {
                            sample.push(json!({
                                "customer_id": customer_id,
                                "email": masked_email(&email),
                            }));
                        }
                    }
                    Sendable::Refused(_) => suppressed_now += 1,
                }
            }
            Ok(json!({
                "send": {
                    "send_id": stored.send_id,
                    "state": stored.state,
                    "send_digest": stored.send_digest,
                    "campaign_id": stored.campaign_id,
                    "content_revision": stored.content_revision,
                    "content_digest": stored.content_digest,
                    "audience_freeze_id": stored.audience_freeze_id,
                    "audience_digest": stored.audience_digest,
                    "connection_id": stored.connection_id,
                    "auth_revision": stored.auth_revision,
                    "link_revision": stored.link_revision,
                    "link_digest": stored.link_digest,
                    "max_recipients": stored.max_recipients,
                    "unsubscribe_origin": facts.origin,
                },
                "counts": {
                    "included": facts.frozen_member_ids.len(),
                    "excluded": 0,
                    "suppressed_now": suppressed_now,
                    "final": facts.frozen_member_ids.len(),
                    "max_recipients": facts.max_recipients,
                },
                "sample": sample,
                "send_digest": stored.send_digest,
            }))
        })
    }

    fn crm_send_approve(
        self: &Arc<Self>,
        install: &str,
        context: &str,
        send_id: &str,
        given_digest: &str,
    ) -> Result<Value> {
        self.crm_smtp_scope(install, context, || {
            let records = RecordStore::open(&self.state_dir, install)?;
            let send = records
                .app_campaign_send(context, send_id)?
                .ok_or_else(|| {
                    Error::rejected(
                        "campaign send is unavailable for this installation and context",
                    )
                })?;
            if send.send_digest != given_digest {
                return Err(Error::rejected(
                    "send digest does not match the prepared send",
                ));
            }
            if send.state != "prepared" {
                return Err(Error::rejected(
                    "campaign send is not awaiting approval — already decided",
                ));
            }
            // Re-check every prepare condition before any delivery
            // row exists: an edited approval, a stale freeze, a
            // rotated credential or lost test-send proof refuses the
            // approval, never inherits its inputs.
            let facts = self.crm_send_facts(
                install,
                context,
                &records,
                &send.campaign_id,
                &send.audience_freeze_id,
            )?;
            if facts.content_revision != send.content_revision
                || facts.content_digest != send.content_digest
            {
                return Err(Error::rejected(
                    "approved content changed since prepare; prepare again",
                ));
            }
            if facts.frozen_digest != send.audience_digest {
                return Err(Error::rejected(
                    "audience freeze changed since prepare; prepare again",
                ));
            }
            if facts.link.link_revision != send.link_revision
                || facts.link.digest != send.link_digest
                || facts.link.connection_id != send.connection_id
                || facts.link.auth_revision != send.auth_revision
            {
                return Err(Error::rejected(
                    "SMTP sender binding changed since prepare; prepare again",
                ));
            }
            if !records.app_campaign_test_send_accepted(
                context,
                &send.campaign_id,
                &send.content_digest,
                &send.link_digest,
            )? {
                return Err(Error::rejected(
                    "accepted test-send evidence no longer matches the prepared material",
                ));
            }
            // Durable intent first: the core row is the crash
            // witness. A lost claim races a concurrent winner only in
            // the CAS below — and only the winner's row stays live.
            self.store.crm_send_open(install, context, send_id)?;
            // One queued row per frozen member, each with its own
            // unsubscribe token — minted here, hashed everywhere
            // else.
            let mut deliveries = Vec::with_capacity(facts.frozen_member_ids.len());
            for customer_id in &facts.frozen_member_ids {
                let email = match records.app_customer_sendable(context, customer_id)? {
                    Sendable::Ok { email, .. } => email,
                    // An already-unfit member still gets a row — its
                    // submission marks it `suppressed` — so the ledger
                    // always mirrors the freeze.
                    Sendable::Refused(_) => String::new(),
                };
                deliveries.push((
                    customer_id.clone(),
                    email,
                    delivery_idempotency_key(install, context, send_id, customer_id),
                    mint_unsubscribe_token(),
                ));
            }
            if let Err(error) = records.app_campaign_send_approve(context, send_id, &deliveries) {
                // Only close the intent when nobody claimed the send;
                // a concurrent winner's witness must survive.
                if records
                    .app_campaign_send(context, send_id)?
                    .is_some_and(|row| row.state == "prepared")
                {
                    let _ = self
                        .store
                        .crm_send_transition(install, context, send_id, "closed");
                }
                return Err(error);
            }
            // Core learns only token hashes → file locations.
            for hash in records.app_unsubscribe_hashes(context, send_id)? {
                self.store
                    .crm_unsubscribe_index_add(&hash, install, context)?;
            }
            self.spawn_crm_send_worker(install, context, send_id);
            let send = records
                .app_campaign_send(context, send_id)?
                .ok_or_else(|| Error::internal("approved send vanished"))?;
            let deliveries = records.app_campaign_deliveries(context, send_id)?;
            let counts = records.app_campaign_delivery_counts(context, send_id)?;
            Ok(send_view(install, context, &send, &deliveries, counts))
        })
    }

    fn crm_send_show(
        self: &Arc<Self>,
        install: &str,
        context: &str,
        send_id: &str,
    ) -> Result<Value> {
        self.crm_smtp_scope(install, context, || {
            let records = RecordStore::open(&self.state_dir, install)?;
            let send = records
                .app_campaign_send(context, send_id)?
                .ok_or_else(|| {
                    Error::rejected(
                        "campaign send is unavailable for this installation and context",
                    )
                })?;
            let deliveries = records.app_campaign_deliveries(context, send_id)?;
            let counts = records.app_campaign_delivery_counts(context, send_id)?;
            Ok(send_view(install, context, &send, &deliveries, counts))
        })
    }

    fn crm_send_list(
        self: &Arc<Self>,
        install: &str,
        context: &str,
        campaign: Option<&str>,
    ) -> Result<Value> {
        self.crm_smtp_scope(install, context, || {
            let records = RecordStore::open(&self.state_dir, install)?;
            let sends = records.app_campaign_sends(context, campaign)?;
            let mut out = Vec::with_capacity(sends.len());
            for send in &sends {
                let counts = records.app_campaign_delivery_counts(context, &send.send_id)?;
                out.push(json!({
                    "send_id": send.send_id,
                    "campaign_id": send.campaign_id,
                    "state": send.state,
                    "send_digest": send.send_digest,
                    "created": send.created,
                    "approved_at": send.approved_at,
                    "counts": counts,
                    "delivery_claim": "smtp-acceptance-only",
                }));
            }
            Ok(json!({"sends": out}))
        })
    }

    fn crm_send_resolve(
        self: &Arc<Self>,
        install: &str,
        context: &str,
        send_id: &str,
        customer_id: &str,
        resolution: &str,
    ) -> Result<Value> {
        self.crm_smtp_scope(install, context, || {
            let records = RecordStore::open(&self.state_dir, install)?;
            if records.app_campaign_send(context, send_id)?.is_none() {
                return Err(Error::rejected(
                    "campaign send is unavailable for this installation and context",
                ));
            }
            records.app_campaign_delivery_resolve(
                context,
                send_id,
                customer_id,
                resolution,
                "operator",
            )?;
            let delivery = records
                .app_campaign_delivery(context, send_id, customer_id)?
                .ok_or_else(|| Error::internal("resolved delivery vanished"))?;
            Ok(json!({"delivery": {
                "send_id": delivery.send_id,
                "customer_id": delivery.customer_id,
                "email": masked_email(&delivery.email),
                "state": delivery.state,
                "resolved_by": delivery.resolved_by,
                "delivery_claim": "smtp-acceptance-only",
            }}))
        })
    }

    /// One worker per live send: the registry refuses a second
    /// runner for the same `(install, context, send)`, so a restart
    /// or a double approve can never double-submit.
    pub(super) fn spawn_crm_send_worker(
        self: &Arc<Self>,
        install: &str,
        context: &str,
        send_id: &str,
    ) {
        let key = format!("{install}/{context}/{send_id}");
        {
            let mut workers = self
                .crm_send_workers
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !workers.insert(key.clone()) {
                return;
            }
        }
        let shared = Arc::clone(self);
        let install = install.to_string();
        let context = context.to_string();
        let send_id = send_id.to_string();
        std::thread::spawn(move || {
            if let Err(error) = shared.crm_send_run(&install, &context, &send_id) {
                eprintln!("crm send {send_id} worker failed: {error}");
            }
            shared
                .crm_send_workers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key);
        });
    }

    /// Boot reconciliation: every send whose durable intent is still
    /// `sending` gets its `submitting` leftovers marked `uncertain`
    /// (they may have been delivered before the crash — never
    /// resend) and a worker respawned for the still-`queued` rest.
    pub(super) fn reconcile_crm_sends(self: &Arc<Self>) {
        let Ok(sends) = self.store.crm_sends_sending() else {
            return;
        };
        for (install, context, send_id) in sends {
            match RecordStore::open(&self.state_dir, &install) {
                Ok(records) => {
                    let _ = records.app_campaign_mark_submitting_uncertain(&context, &send_id);
                    self.spawn_crm_send_worker(&install, &context, &send_id);
                }
                Err(error) => {
                    // The record file is gone — close the intent
                    // rather than leave a phantom running send.
                    eprintln!(
                        "crm send {send_id} reconciliation: record file unavailable: {error}"
                    );
                    let _ = self
                        .store
                        .crm_send_transition(&install, &context, &send_id, "closed");
                }
            }
        }
    }

    /// Close the send mid-flight: the claimed row, every queued row,
    /// the send itself and the core intent all land `closed` with
    /// one reason. Terminal rows are untouched.
    fn crm_send_close(
        &self,
        records: &RecordStore,
        install: &str,
        context: &str,
        send_id: &str,
        reason: &str,
    ) {
        if let Ok(deliveries) = records.app_campaign_deliveries(context, send_id) {
            for delivery in deliveries {
                if matches!(delivery.state.as_str(), "queued" | "submitting") {
                    let _ = records.app_campaign_delivery_finish(
                        context,
                        send_id,
                        &delivery.customer_id,
                        "closed",
                        None,
                        None,
                        Some(reason),
                        None,
                    );
                }
            }
        }
        let _ = records.app_campaign_send_close(context, send_id, "closed", Some(reason));
        let _ = self
            .store
            .crm_send_transition(install, context, send_id, "closed");
    }

    /// The worker's drain loop: claim → re-verify → submit → record,
    /// one interval apart, deterministic `customer_id` order.
    fn crm_send_run(self: &Arc<Self>, install: &str, context: &str, send_id: &str) -> Result<()> {
        let records = RecordStore::open(&self.state_dir, install)?;
        let send = records
            .app_campaign_send(context, send_id)?
            .ok_or_else(|| Error::rejected("campaign send is unavailable"))?;
        if send.state != "sending" {
            return Ok(());
        }
        loop {
            let deliveries = records.app_campaign_deliveries(context, send_id)?;
            let Some(next) = deliveries.iter().find(|d| d.state == "queued") else {
                break;
            };
            let customer_id = next.customer_id.clone();
            if !records.app_campaign_delivery_claim(context, send_id, &customer_id)? {
                continue;
            }
            // Every check again, right before submission: context
            // proof, live link at the pinned revision/digest,
            // authority, content still approved at the pinned
            // revision, customer still sendable — all inside the
            // custody lock the submission itself holds.
            let step = match self.crm_smtp_scope(install, context, || {
                self.crm_send_row_step(&records, install, context, &send, &customer_id)
            }) {
                Ok(step) => step,
                Err(error) => {
                    // Authority-level failure — the authority beneath
                    // the send is gone: this row, the queued rest and
                    // the send itself close with one reason.
                    self.crm_send_close(&records, install, context, send_id, &error.to_string());
                    return Ok(());
                }
            };
            match step {
                Step::Suppressed(why) => {
                    records.app_campaign_delivery_finish(
                        context,
                        send_id,
                        &customer_id,
                        "suppressed",
                        None,
                        None,
                        Some(&why),
                        None,
                    )?;
                }
                Step::Done(outcome) => match outcome {
                    crate::platform::smtp::SmtpOutcome::Accepted { code, message } => {
                        records.app_campaign_delivery_finish(
                            context,
                            send_id,
                            &customer_id,
                            "accepted",
                            Some(i64::from(code)),
                            Some(&message),
                            None,
                            None,
                        )?;
                    }
                    crate::platform::smtp::SmtpOutcome::Rejected { code, message } => {
                        records.app_campaign_delivery_finish(
                            context,
                            send_id,
                            &customer_id,
                            "failed",
                            Some(i64::from(code)),
                            Some(&message),
                            None,
                            None,
                        )?;
                    }
                    crate::platform::smtp::SmtpOutcome::Uncertain { message } => {
                        records.app_campaign_delivery_finish(
                            context,
                            send_id,
                            &customer_id,
                            "uncertain",
                            None,
                            Some(&message),
                            None,
                            None,
                        )?;
                    }
                    crate::platform::smtp::SmtpOutcome::Deferred { code, message } => {
                        self.crm_send_delivery_retry(
                            &records,
                            context,
                            send_id,
                            &customer_id,
                            Some(code),
                            &message,
                        )?;
                    }
                    crate::platform::smtp::SmtpOutcome::NotSubmitted { message } => {
                        self.crm_send_delivery_retry(
                            &records,
                            context,
                            send_id,
                            &customer_id,
                            None,
                            &message,
                        )?;
                    }
                },
            }
            std::thread::sleep(self.crm_send_interval);
        }
        // Nothing queued and no other worker can claim — the send is
        // done. A concurrent close already landed its state; only a
        // live 'sending' row moves to completed.
        if let Some(current) = records.app_campaign_send(context, send_id)? {
            if current.state == "sending" {
                records.app_campaign_send_close(context, send_id, "completed", None)?;
                self.store
                    .crm_send_transition(install, context, send_id, "completed")?;
            }
        }
        Ok(())
    }

    /// One claim: the per-submission re-verification plus the
    /// transport, all inside the custody lock. Errors bubble up to
    /// the caller as authority-level failures (the send closes);
    /// `Step` carries only per-row results.
    fn crm_send_row_step(
        &self,
        records: &RecordStore,
        install: &str,
        context: &str,
        send: &crate::store::app_sends::CampaignSend,
        customer_id: &str,
    ) -> Result<Step> {
        let _custody = self
            .platform_custody_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let link = self
            .store
            .crm_smtp_link(install, context)?
            .ok_or_else(|| Error::rejected("SMTP sender binding is gone"))?;
        if link.state != "live"
            || link.link_revision != send.link_revision
            || link.digest != send.link_digest
        {
            return Err(Error::rejected("SMTP sender binding changed"));
        }
        let (_, envelope, projection, _) =
            self.crm_smtp_authority(&link.connection_id, link.auth_revision)?;
        let (content_revision, content_digest) =
            records.app_content_approved(context, &send.campaign_id)?;
        if content_revision != send.content_revision || content_digest != send.content_digest {
            return Err(Error::rejected("approved content changed mid-send"));
        }
        let (email, first_name) = match records.app_customer_sendable(context, customer_id)? {
            Sendable::Ok { email, first_name } => (email, first_name),
            Sendable::Refused(why) => return Ok(Step::Suppressed(why.to_string())),
        };
        let delivery = records
            .app_campaign_delivery(context, &send.send_id, customer_id)?
            .ok_or_else(|| Error::internal("claimed delivery vanished"))?;
        let unsubscribe_url = format!(
            "{}/unsubscribe/{}",
            self.crm_send_origin()?,
            delivery.unsubscribe_token
        );
        let rendered = records.app_content_send_bytes(
            context,
            &send.campaign_id,
            &projection.sender_name,
            &projection.sender,
            first_name.as_deref(),
            &unsubscribe_url,
        )?;
        let message = crate::platform::smtp::SmtpMessage {
            to: email,
            subject: rendered["subject"].as_str().unwrap_or_default().to_string(),
            html: rendered["html"].as_str().unwrap_or_default().to_string(),
            text: rendered["text"].as_str().unwrap_or_default().to_string(),
            unsubscribe_url,
            idempotency_key: Some(delivery.idempotency_key.clone()),
        };
        let outcome = crate::platform::smtp::send_outcome(
            &envelope,
            &message,
            &content_digest,
            self.smtp_test_ca.as_deref(),
        )?;
        Ok(Step::Done(outcome))
    }

    /// A deferred or never-submitted row retries bounded times —
    /// back to `queued` with the evidence attached — then `failed`.
    /// `uncertain` never reaches here: retry can never resend a
    /// maybe-delivered message.
    fn crm_send_delivery_retry(
        &self,
        records: &RecordStore,
        context: &str,
        send_id: &str,
        customer_id: &str,
        code: Option<u16>,
        message: &str,
    ) -> Result<()> {
        let attempts = records
            .app_campaign_delivery(context, send_id, customer_id)?
            .map(|d| d.attempts)
            .unwrap_or(DELIVERY_ATTEMPT_MAX);
        if attempts >= DELIVERY_ATTEMPT_MAX {
            records.app_campaign_delivery_finish(
                context,
                send_id,
                customer_id,
                "failed",
                code.map(i64::from),
                Some(message),
                Some("retry bound exhausted"),
                None,
            )?;
        } else {
            records.app_campaign_delivery_finish(
                context,
                send_id,
                customer_id,
                "queued",
                code.map(i64::from),
                Some(message),
                Some("retry scheduled"),
                None,
            )?;
        }
        Ok(())
    }
}

struct SendFacts {
    origin: String,
    content_revision: i64,
    content_digest: String,
    frozen_member_ids: Vec<String>,
    frozen_digest: String,
    max_recipients: i64,
    link: crate::store::crm_smtp::SmtpLink,
}

/// `https://` anywhere, or loopback `http://` for the isolated test
/// rigs. No other scheme or host class mints unsubscribe links.
fn unsubscribe_origin_valid(origin: &str) -> bool {
    if let Some(rest) = origin.strip_prefix("https://") {
        return !rest.trim_end_matches('/').is_empty()
            && rest
                .chars()
                .all(|c| !c.is_whitespace() && c != '<' && c != '>' && c != '"');
    }
    if let Some(rest) = origin.strip_prefix("http://") {
        let host = rest
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default()
            .split(':')
            .next()
            .unwrap_or_default();
        return matches!(host, "localhost" | "127.0.0.1" | "[::1]");
    }
    false
}

/// `request_id`, with the same bounded identifier grammar the rest
/// of the request-id verbs enforce.
fn send_request_id(params: &Value) -> Result<String> {
    let id = required_str(params, "request_id")?;
    crate::proto::identifier(id, "request ID")
}
