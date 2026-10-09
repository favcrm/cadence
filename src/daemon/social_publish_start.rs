//! CAD-1123 HP4: publish from the binding — one operator tap stages,
//! imports media, freezes and (for now) sends; reschedule moves a queued
//! intent atomically.
//!
//! Generic host code: it names no app. Every input that decides WHERE and
//! HOW a post goes comes from the daemon's own records, never the request:
//! the destination, toolkit, timezone and send grant from the run's frozen
//! publication binding (set by the operator with `app_binding_publish_set`),
//! the install and context from the run, the artifact from the reviewer's
//! approval, the connection from the resolver, the approval id minted here.
//! The request names only a run, a mode and (for schedule) a time. The send
//! itself is the CAD-1020/CAD-1041 path, unchanged: one claim by identity,
//! so a double tap is one provider call. No price is read, stored or shown.

use super::*;
use crate::platform::agenticos_external::publish as device;

/// One year: a schedule further out is a typo, not a plan.
const MAX_SCHEDULE_AHEAD_SECS: i64 = 366 * 24 * 3600;

/// The operator's one-time publish settings carried on the publication
/// binding's receipt (`config.publish`). All five fields are required and
/// nothing else is allowed, so a typo or a forged extra key never passes.
pub(super) struct PublishTarget {
    pub destination_id: String,
    pub destination_label: String,
    pub toolkit: String,
    pub timezone: String,
    pub grant_id: String,
}

impl PublishTarget {
    const FIELDS: [&'static str; 5] = [
        "destination_id",
        "destination_label",
        "toolkit",
        "timezone",
        "grant_id",
    ];

    /// Validate a candidate `publish` object (operator input or a stored
    /// receipt) and return the typed target.
    pub(super) fn parse(publish: &Value) -> Result<Self> {
        Self::parse_with(publish, true)
    }

    /// CAD-1290/1291: the hosted draft form carries no standing grant. The
    /// owner confirms each post on AgenticOS, so the settings are exactly the
    /// four destination fields and `grant_id` is absent (empty on the target).
    pub(super) fn parse_hosted(publish: &Value) -> Result<Self> {
        Self::parse_with(publish, false)
    }

    fn parse_with(publish: &Value, grant_required: bool) -> Result<Self> {
        let object = publish
            .as_object()
            .ok_or_else(|| Error::rejected("publish settings must be an object"))?;
        let expected = Self::FIELDS.len() - usize::from(!grant_required);
        if object.len() != expected
            || object.keys().any(|k| !Self::FIELDS.contains(&k.as_str()))
            || (!grant_required && object.contains_key("grant_id"))
        {
            return Err(Error::rejected(
                "publish settings need exactly destination_id, destination_label, toolkit, timezone and grant_id",
            ));
        }
        let text = |field: &str| {
            publish[field]
                .as_str()
                .ok_or_else(|| Error::rejected(format!("publish {field} must be a string")))
        };
        let (id, label, toolkit, tz) = (
            text("destination_id")?,
            text("destination_label")?,
            text("toolkit")?,
            text("timezone")?,
        );
        let grant = if grant_required {
            text("grant_id")?
        } else {
            ""
        };
        let bad = |what: &str| Err(Error::rejected(format!("publish {what} is invalid")));
        if !(1..=120).contains(&id.len())
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return bad("destination_id");
        }
        if label.trim().is_empty()
            || label.chars().count() > 80
            || label.chars().any(char::is_control)
        {
            return bad("destination_label");
        }
        if device::Toolkit::parse(toolkit).is_none() {
            return bad("toolkit");
        }
        if tz.is_empty()
            || tz.len() > 64
            || !tz
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+'))
        {
            return bad("timezone");
        }
        if grant_required && !device::valid_grant_id(grant) {
            return bad("grant_id");
        }
        Ok(Self {
            destination_id: id.into(),
            destination_label: label.into(),
            toolkit: toolkit.into(),
            timezone: tz.into(),
            grant_id: grant.into(),
        })
    }

    /// The older publish path (`social_publish_start`) freezes a standing
    /// grant, so a hosted-draft target (no grant) is refused there.
    pub(super) fn require_grant(&self) -> Result<()> {
        if self.grant_id.is_empty() {
            return Err(Error::rejected(
                "no_grant: this destination was chosen for hosted drafts and carries no send grant — publish it from the draft's approval card",
            ));
        }
        Ok(())
    }

    /// The target a re-proved binding receipt carries, or the plain reason
    /// the operator has not set one yet. Either shape: with a standing
    /// grant (CLI) or the hosted draft form without one.
    pub(super) fn from_binding_config(config: &Value) -> Result<Self> {
        match config.get("publish") {
            Some(publish) if publish.get("grant_id").is_none() => Self::parse_hosted(publish),
            Some(publish) => Self::parse(publish),
            None => Err(Error::rejected(
                "no_destination: the publication binding has no destination — the operator sets it once when binding",
            )),
        }
    }
}

/// A rebuilt binding receipt keeps the operator's publish settings: the
/// settings are not derived from the connection, so re-deriving the rest
/// must not drop (or silently re-count as drift) what the operator chose.
pub(super) fn carry_publish(mut fresh: Value, old: &Value) -> Value {
    if let Some(publish) = old.get("publish") {
        fresh["publish"] = publish.clone();
    }
    fresh
}

impl Shared {
    /// `app_binding_publish_set`: the operator records the destination once,
    /// on the binding. A new binding revision: runs frozen on the old receipt
    /// go stale and effects pinned to it close, exactly as for any rebind.
    pub(super) fn set_binding_publish(&self, install: &str, params: &Value) -> Result<Value> {
        let publish = json!({
            "destination_id": params.get("destination_id"),
            "destination_label": params.get("destination_label"),
            "toolkit": params.get("toolkit"),
            "timezone": params.get("timezone"),
            "grant_id": params.get("grant_id"),
        });
        PublishTarget::parse(&publish)?;
        let id = required_str(params, "binding_id")?;
        let revision = params
            .get("expected_revision")
            .and_then(Value::as_i64)
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                Error::rejected("expected binding revision must be a positive integer")
            })?;
        let shown = self.store.app_binding_show(install, id)?;
        let mut config = shown["binding"]["config"].clone();
        config["publish"] = publish;
        self.store
            .app_binding_update(install, id, revision, &config)
    }

    /// `social_publish_start`: stage, import media, freeze, then send now or
    /// leave queued for the driver. Idempotent on the host-minted
    /// `request_id`: a retry or double tap resumes the same intent.
    pub(super) fn start_social_publish(&self, params: &Value) -> Result<Value> {
        let request_id = Self::required_segment(params, "request_id")?;
        crate::proto::identifier(request_id, "publish request id")?;
        let run_id = required_str(params, "run_id")?;
        let mode = required_str(params, "mode")?;
        if !matches!(mode, "now" | "schedule") {
            return Err(Error::rejected("publish mode must be now or schedule"));
        }
        let run = self.store.app_run_show(run_id)?;
        let install = required_str(&run, "install_id")?;
        let context = run["context_id"].as_str();
        // A retried or double-tapped start resumes its own intent. Another
        // run under the same request id is a forged reuse, never a resume.
        if let Some(existing) = self
            .store
            .social_publish_find_request(install, request_id)?
        {
            if existing["intent"]["frozen"]["run_id"].as_str() != Some(run_id) {
                return Err(Error::rejected(
                    "publish request id already belongs to a different run",
                ));
            }
            return self.finish_start(existing, mode, install, context);
        }
        let now = self.operator_now();
        let due = match (mode, params.get("due_epoch")) {
            ("now", None) => now,
            ("now", Some(_)) => {
                return Err(Error::rejected("publish now takes no due_epoch"));
            }
            ("schedule", Some(value)) => {
                let due = value
                    .as_i64()
                    .ok_or_else(|| Error::rejected("due_epoch must be an integer"))?;
                if due <= now || due > now + MAX_SCHEDULE_AHEAD_SECS {
                    return Err(Error::rejected(
                        "due_epoch must be in the future and within a year",
                    ));
                }
                due
            }
            _ => return Err(Error::rejected("publish schedule needs due_epoch")),
        };
        let slot = required_str(&run["snapshot"]["publication"], "slot")?;
        let bundle = required_str(&run["snapshot"], "bundle_digest")?;
        let artifact_id = self.store.app_run_approved_artifact(run_id)?;
        // Re-proves the approved run, review, artifact and current binding;
        // the destination comes only from that re-proved binding.
        let material = self
            .store
            .app_publication_material(run_id, &artifact_id, bundle, slot)?;
        let target = PublishTarget::from_binding_config(&material["binding"]["config"])?;
        target.require_grant()?;
        let effect = self.stage_app_artifact(&json!({
            "run_id": run_id,
            "artifact_id": artifact_id,
            "slot": slot,
            "request_id": format!("{request_id}-fx"),
            "title": target.destination_label,
        }))?;
        let effect_id = required_str(&effect["effect"], "effect_id")?.to_owned();
        let scope = |extra: Value| {
            let mut body = json!({
                "request_id": request_id,
                "install_id": install,
                "context_id": context,
                "run_id": run_id,
                "artifact_id": artifact_id,
                "bundle_digest": bundle,
                "slot": slot,
                "toolkit": target.toolkit,
                "destination_id": target.destination_id,
            });
            for (key, value) in extra.as_object().into_iter().flatten() {
                body[key] = value.clone();
            }
            body
        };
        let mut media_key = Value::Null;
        if let Some(asset) = material.get("asset") {
            let imported = self.import_social_media(&scope(json!({})))?;
            let reviewed = asset["digest"].as_str().unwrap_or("");
            let got = imported["image_digest"].as_str().unwrap_or("");
            if got.strip_prefix("sha256:").unwrap_or(got)
                != reviewed.strip_prefix("sha256:").unwrap_or(reviewed)
            {
                return Err(Error::rejected(
                    "grant_binding_mismatch: the imported image is not the reviewed image — nothing was scheduled",
                ));
            }
            media_key = imported["media_key"].clone();
        }
        let approval = format!(
            "apv-{}",
            uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_OID,
                format!("apv:{install}:{request_id}").as_bytes()
            )
            .simple()
        );
        let mut frozen = scope(json!({
            "effect_id": effect_id,
            "media_key": media_key,
            "grant_id": target.grant_id,
            "approval_id": approval,
            "due_epoch": due,
            "timezone": target.timezone,
        }));
        if frozen["media_key"].is_null() {
            frozen.as_object_mut().map(|o| o.remove("media_key"));
        }
        if frozen["context_id"].is_null() {
            frozen.as_object_mut().map(|o| o.remove("context_id"));
        }
        let queued = self.schedule_social_publish(&frozen)?;
        self.finish_start(queued, mode, install, context)
    }

    /// Schedule leaves the queued intent to the driver. Now sends it through
    /// the one claim-by-identity path; if that refuses, the just-frozen
    /// intent is cancelled so a failed tap never leaves a surprise post due.
    fn finish_start(
        &self,
        intent: Value,
        mode: &str,
        install: &str,
        context: Option<&str>,
    ) -> Result<Value> {
        if mode != "now" || intent["intent"]["state"] != "queued" {
            return Ok(intent);
        }
        let id = required_str(&intent["intent"], "intent_id")?.to_owned();
        match self.send_now_social_publish(&json!({
            "intent_id": id, "install_id": install, "context_id": context,
        })) {
            Ok(sent) => Ok(sent),
            Err(error) => {
                let _ = self.store.social_publish_cancel(&id, install, context);
                Err(error)
            }
        }
    }

    /// `social_publish_reschedule`: the CAS lives in the store; this mints
    /// the new approval and bounds the new time.
    pub(super) fn reschedule_social_publish(&self, params: &Value) -> Result<Value> {
        let now = self.operator_now();
        let int = |field: &str| {
            params
                .get(field)
                .and_then(Value::as_i64)
                .ok_or_else(|| Error::rejected(format!("Missing or non-integer '{field}'")))
        };
        let due = int("due_epoch")?;
        if due <= now || due > now + MAX_SCHEDULE_AHEAD_SECS {
            return Err(Error::rejected(
                "due_epoch must be in the future and within a year",
            ));
        }
        let approval = format!("apv-{}", uuid::Uuid::new_v4().simple());
        self.store.social_publish_reschedule(
            Self::required_segment(params, "intent_id")?,
            Self::required_segment(params, "install_id")?,
            Self::strict_optional_segment(params, "context_id")?,
            int("expected_due_epoch")?,
            due,
            &approval,
        )
    }
}

// ACCEPTANCE-CHECK SLOT (CAD-1123 HP4). Reserved for the independent
// acceptance check, written by the reviewer or the ticket author, not the
// implementer, in a new `tests/cad1123_hp4_acceptance.rs` (feature
// `test-seam`). It must prove, against the real guards, that these are
// refused: a double send (two `social_publish_start now` taps, or start then
// `social_publish_send_now`) beyond one provider call; a non-operator (agent,
// detached child, board member) on `social_publish_start`,
// `social_publish_reschedule` and `app_binding_publish_set`, over RPC and
// HTTP; a reschedule race (stale `expected_due_epoch`, or a claim in
// between) that changes the schedule; and a forged destination (a
// `destination_id`, `toolkit`, `grant_id`, `timezone`, scope or
// `approval_id` in the request).

#[cfg(test)]
mod tests {
    use super::*;

    fn publish() -> Value {
        json!({"destination_id":"17841400008460056","destination_label":"@harbour",
            "toolkit":"instagram","timezone":"Asia/Hong_Kong","grant_id":"dpq_synthetic_grant_01"})
    }

    /// The target is exactly the five operator fields: a forged extra key
    /// (a price, a connection id), a missing field or a malformed value
    /// never parses, and a binding without settings names why.
    #[test]
    fn publish_settings_are_exactly_five_valid_fields() {
        assert!(PublishTarget::parse(&publish()).is_ok());
        let mut extra = publish();
        extra["price"] = json!("0.06");
        assert!(PublishTarget::parse(&extra).is_err());
        for (field, bad) in [
            ("destination_id", json!("a b")),
            ("destination_label", json!("")),
            ("toolkit", json!("twitter")),
            ("timezone", json!("Hong Kong")),
            ("grant_id", json!("grant")),
            ("grant_id", json!(7)),
        ] {
            let mut candidate = publish();
            candidate[field] = bad;
            assert!(PublishTarget::parse(&candidate).is_err(), "{field}");
        }
        let mut missing = publish();
        missing.as_object_mut().unwrap().remove("grant_id");
        assert!(PublishTarget::parse(&missing).is_err());
        let none = PublishTarget::from_binding_config(&json!({"schema":1}));
        assert!(none.err().unwrap().to_string().contains("no_destination"));
    }

    /// CAD-1290: the hosted draft form has no grant; the older publish path
    /// refuses it, and a grant-less form never admits a smuggled grant.
    #[test]
    fn the_hosted_form_has_no_grant_and_the_older_path_refuses_it() {
        let mut hosted = publish();
        hosted.as_object_mut().unwrap().remove("grant_id");
        assert!(PublishTarget::parse_hosted(&hosted).is_ok());
        assert!(
            PublishTarget::parse(&hosted).is_err(),
            "the strict form still needs a grant"
        );
        assert!(
            PublishTarget::parse_hosted(&publish()).is_err(),
            "a grant is refused in the hosted form"
        );
        let target = PublishTarget::from_binding_config(&json!({"publish": hosted})).unwrap();
        assert!(target
            .require_grant()
            .unwrap_err()
            .to_string()
            .contains("no_grant"));
        let strict = PublishTarget::from_binding_config(&json!({"publish": publish()})).unwrap();
        assert!(strict.require_grant().is_ok());
    }

    #[test]
    fn a_rebuilt_receipt_keeps_the_operators_publish_settings() {
        let old = json!({"connection_id":"c","publish":publish()});
        let fresh = carry_publish(json!({"connection_id":"c"}), &old);
        assert_eq!(fresh, old);
        let bare = carry_publish(json!({"connection_id":"c"}), &json!({"connection_id":"c"}));
        assert!(bare.get("publish").is_none());
    }
}
