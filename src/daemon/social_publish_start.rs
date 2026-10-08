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
use crate::store::social_publish::{NewPreparedIntent, PublishAccount};

/// One year: legacy schedule paths retain their existing ceiling.
const MAX_SCHEDULE_AHEAD_SECS: i64 = 366 * 24 * 3600;
/// AOS owner grants expire no later than 30 days after mint. Prepared
/// owner-authorized schedules cannot extend the old legacy ceiling.
const MAX_OWNER_INTENT_WINDOW_SECS: i64 = 30 * 24 * 3600;

/// Legacy direct-send settings carried on the publication binding's receipt.
/// The optional AOS connection selector preserves account affinity for the
/// owner-authorized flow; the old grant-bearing path still requires its grant.
pub(super) struct PublishTarget {
    pub destination_id: String,
    pub destination_label: String,
    pub toolkit: String,
    pub timezone: String,
    pub grant_id: String,
}

impl PublishTarget {
    const FIELDS: [&'static str; 6] = [
        "destination_id",
        "destination_label",
        "toolkit",
        "timezone",
        "grant_id",
        "aos_connection_id",
    ];
    const REQUIRED_FIELDS: [&'static str; 5] = [
        "destination_id",
        "destination_label",
        "toolkit",
        "timezone",
        "grant_id",
    ];

    /// Validate a candidate `publish` object (operator input or a stored
    /// receipt) and return the typed target.
    pub(super) fn parse(publish: &Value) -> Result<Self> {
        let object = publish
            .as_object()
            .ok_or_else(|| Error::rejected("publish settings must be an object"))?;
        if object.keys().any(|k| !Self::FIELDS.contains(&k.as_str()))
            || Self::REQUIRED_FIELDS
                .iter()
                .any(|key| !object.contains_key(*key))
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
        let (id, label, toolkit, tz, grant) = (
            text("destination_id")?,
            text("destination_label")?,
            text("toolkit")?,
            text("timezone")?,
            text("grant_id")?,
        );
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
        if !device::valid_grant_id(grant) {
            return bad("grant_id");
        }
        if object
            .get("aos_connection_id")
            .is_some_and(|value| !value.as_str().is_some_and(device::valid_connection_id))
        {
            return bad("aos_connection_id");
        }
        Ok(Self {
            destination_id: id.into(),
            destination_label: label.into(),
            toolkit: toolkit.into(),
            timezone: tz.into(),
            grant_id: grant.into(),
        })
    }

    /// The target a re-proved binding receipt carries, or the plain reason
    /// the operator has not set one yet.
    fn from_binding_config(config: &Value) -> Result<Self> {
        match config.get("publish") {
            Some(publish) => Self::parse(publish),
            None => Err(Error::rejected(
                "no_destination: the publication binding has no destination — the operator sets it once when binding",
            )),
        }
    }
}

/// A rebuilt receipt carries publish settings only when the bound local
/// connection is unchanged. Rebinding must drop the account selector and
/// any legacy grant rather than transfer another connection's authority.
pub(super) fn carry_publish(mut fresh: Value, old: &Value) -> Value {
    let same_connection = fresh
        .get("connection_id")
        .and_then(Value::as_str)
        .zip(old.get("connection_id").and_then(Value::as_str))
        .is_some_and(|(fresh, old)| fresh == old);
    if same_connection {
        if let Some(publish) = old.get("publish") {
            fresh["publish"] = publish.clone();
        }
    }
    fresh
}

fn expose_owner_intent_descriptor(mut prepared: Value, material: &Value) -> Result<Value> {
    let prepared_id = required_str(&prepared["prepared"], "prepared_id")?;
    let frozen = &prepared["prepared"]["descriptor"];
    let owner_intent =
        super::social_owner_intent::owner_intent_descriptor(prepared_id, frozen, material)?;
    prepared["prepared"]["owner_intent"] = owner_intent;
    Ok(prepared)
}

impl Shared {
    /// `app_binding_publish_set`: the operator records the destination once,
    /// on the binding. A new binding revision: runs frozen on the old receipt
    /// go stale and effects pinned to it close, exactly as for any rebind.
    pub(super) fn set_binding_publish(&self, install: &str, params: &Value) -> Result<Value> {
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
        let account = json!({
            "destination_id": params.get("destination_id"),
            "destination_label": params.get("destination_label"),
            "toolkit": params.get("toolkit"),
            "timezone": params.get("timezone"),
        });
        let publish = if params.get("aos_connection_id").is_some() {
            let account = PublishAccount::parse(&account)?;
            let expected_aos = params
                .get("aos_connection_id")
                .and_then(Value::as_str)
                .filter(|id| device::valid_connection_id(id))
                .ok_or_else(|| Error::rejected("publish AOS connection selector is invalid"))?;
            let resolver = self.social_media_resolver.clone().ok_or_else(|| {
                Error::rejected("capability_unavailable: no media resolver configured")
            })?;
            let rows = match resolver.list() {
                crate::platform::agenticos_external::media_import::DestinationList::Complete(
                    rows,
                ) => rows,
                crate::platform::agenticos_external::media_import::DestinationList::Ambiguous
                | crate::platform::agenticos_external::media_import::DestinationList::Unavailable =>
                {
                    return Err(Error::rejected(
                        "capability_unavailable: the destinations read did not produce a complete list",
                    ));
                }
            };
            let matches: Vec<_> = rows
                .iter()
                .filter(|row| {
                    row.connection_id.as_str() == expected_aos
                        && row.destination_id == account.destination_id
                        && row.toolkit == account.toolkit
                        && row.status == "active"
                        && row.available
                        && row.publishable
                })
                .collect();
            let [row] = matches.as_slice() else {
                return Err(Error::rejected(
                    "capability_unavailable: the selected account is no longer uniquely available",
                ));
            };
            if row.display_name != account.destination_label {
                return Err(Error::rejected(
                    "stale: the selected account label changed; refresh accounts and try again",
                ));
            }
            let previous = config.get("publish").and_then(Value::as_object);
            let same_account = previous.is_some_and(|old| {
                old.get("aos_connection_id").and_then(Value::as_str) == Some(expected_aos)
                    && old.get("destination_id").and_then(Value::as_str)
                        == Some(account.destination_id.as_str())
                    && old.get("toolkit").and_then(Value::as_str) == Some(account.toolkit.as_str())
            });
            let grant = same_account
                .then(|| {
                    previous
                        .and_then(|old| old.get("grant_id"))
                        .and_then(Value::as_str)
                        .filter(|grant| device::valid_grant_id(grant))
                        .map(str::to_owned)
                })
                .flatten();
            json!({
                "destination_id": row.destination_id,
                "destination_label": row.display_name,
                "toolkit": row.toolkit,
                "timezone": account.timezone,
                "aos_connection_id": row.connection_id,
                "grant_id": grant,
            })
        } else {
            let publish = json!({
                "destination_id": params.get("destination_id"),
                "destination_label": params.get("destination_label"),
                "toolkit": params.get("toolkit"),
                "timezone": params.get("timezone"),
                "grant_id": params.get("grant_id"),
            });
            PublishTarget::parse(&publish)?;
            publish
        };
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

    /// `app_publish_intent_prepare` (CAD-1143): build the immutable,
    /// non-dispatchable PREPARED intent for an approved run — account-only,
    /// grant-free, in its own store. Re-proves the approved run, its
    /// independent review, the current publication binding and the imported
    /// media exactly like `start`, then records a stable effect/intent
    /// identity. It NEVER queues, claims, executes or sends, and never
    /// writes or inherits a grant. The operator gate ran before this call.
    /// Params are strict: host-minted `request_id`, `run_id`, `mode`
    /// (`now`|`schedule`), `due_epoch` (schedule only). Install/context/
    /// artifact/binding/connection/destination are DERIVED from the run —
    /// never from a caller body field (no artifact/grant/owner/material).
    pub(super) fn prepare_publish_intent(&self, params: &Value) -> Result<Value> {
        let request_id = Self::required_segment(params, "request_id")?;
        crate::proto::identifier(request_id, "publish prepare request id")?;
        let run_id = required_str(params, "run_id")?;
        let mode = required_str(params, "mode")?;
        if !matches!(mode, "now" | "schedule") {
            return Err(Error::rejected("prepare mode must be now or schedule"));
        }
        let run = self.store.app_run_show(run_id)?;
        let install = required_str(&run, "install_id")?;
        let context = run["context_id"].as_str();
        let now = self.operator_now();
        let existing_request = self
            .store
            .social_publish_prepared_find_request(install, request_id)?;
        let requested_due = match (mode, params.get("due_epoch")) {
            ("now", None) => None,
            ("now", Some(_)) => {
                return Err(Error::rejected("prepare now takes no due_epoch"));
            }
            ("schedule", Some(value)) => Some(
                value
                    .as_i64()
                    .ok_or_else(|| Error::rejected("due_epoch must be an integer"))?,
            ),
            _ => return Err(Error::rejected("prepare schedule needs due_epoch")),
        };
        let existing = match existing_request {
            Some(existing) => Some(existing),
            None => self.store.social_publish_prepared_find_scope(
                install,
                context,
                run_id,
                mode,
                requested_due,
            )?,
        };
        let (due, not_before, expires) = if let Some(existing) = existing.as_ref() {
            let prepared = &existing["prepared"];
            let descriptor = &prepared["descriptor"];
            let state = required_str(prepared, "state")?;
            if !matches!(state, "prepared" | "authorized")
                || descriptor["install_id"].as_str() != Some(install)
                || descriptor["context_id"] != run["context_id"]
                || descriptor["run_id"].as_str() != Some(run_id)
                || descriptor["mode"].as_str() != Some(mode)
            {
                return Err(Error::rejected(
                    "prepared publish request already belongs to a different run or scope",
                ));
            }
            let due = descriptor["due_epoch"]
                .as_i64()
                .ok_or_else(|| Error::rejected("prepared publish due time is corrupt"))?;
            let not_before = descriptor["not_before_epoch"]
                .as_i64()
                .ok_or_else(|| Error::rejected("prepared publish window is corrupt"))?;
            let expires = descriptor["expires_epoch"]
                .as_i64()
                .ok_or_else(|| Error::rejected("prepared publish expiry is corrupt"))?;
            if requested_due.is_some_and(|requested| requested != due)
                || requested_due.is_none() && mode == "schedule"
                || not_before != due
                || due <= 0
                || expires < due
            {
                return Err(Error::rejected(
                    "prepared publish request already has a different immutable window",
                ));
            }
            if state == "prepared" && expires <= now {
                return Err(Error::rejected("prepared owner intent has expired"));
            }
            (due, not_before, expires)
        } else {
            let due = match requested_due {
                None => now,
                Some(due) => {
                    if due <= now || due > now + MAX_OWNER_INTENT_WINDOW_SECS {
                        return Err(Error::rejected(
                            "owner-authorized due_epoch must be in the future and within 30 days",
                        ));
                    }
                    due
                }
            };
            let not_before = due;
            // AOS expiry is capped from mint time, not from a future scheduled
            // due time. `due <= expires` is enforced by the store as well.
            let expires = now + MAX_OWNER_INTENT_WINDOW_SECS;
            (due, not_before, expires)
        };
        let slot = required_str(&run["snapshot"]["publication"], "slot")?;
        let bundle = required_str(&run["snapshot"], "bundle_digest")?;
        let artifact_id = self.store.app_run_approved_artifact(run_id)?;
        // Re-prove the approved run, review, artifact and current binding.
        let material = self
            .store
            .app_publication_material(run_id, &artifact_id, bundle, slot)?;
        let binding_config = &material["binding"]["config"];
        // The account is the binding's configured publish ACCOUNT — four
        // fields, grant excluded. A configured five-field receipt keeps its
        // grant in place (untouched here); we only read the four account
        // fields and never carry the old grant forward as authority.
        let mut account = PublishAccount::parse(&json!({
            "destination_id": binding_config["publish"]["destination_id"],
            "destination_label": binding_config["publish"]["destination_label"],
            "toolkit": binding_config["publish"]["toolkit"],
            "timezone": binding_config["publish"]["timezone"],
        }))
        .map_err(|e| {
            Error::rejected(format!(
                "publication binding has no configured publish account: {e}"
            ))
        })?;
        // Derive the app slug from the exact current bundle; the owner
        // descriptor binds the app/install pair without trusting caller data.
        let pm = self.pm_at(&self.pm_dir()?)?;
        let app_id = crate::issue::app_catalog::workspace::with_completed_bundle_snapshot(
            &pm,
            install,
            bundle,
            |_, files| {
                let manifest = crate::issue::app::parse_manifest(
                    files
                        .get("app.md")
                        .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
                )?;
                Ok(manifest.app)
            },
        )?;
        let binding_id = required_str(&material["binding"], "id")?.to_owned();
        let binding_revision = material["binding"]["revision"]
            .as_i64()
            .filter(|revision| *revision > 0)
            .ok_or_else(|| Error::rejected("publication binding revision is invalid"))?;
        let binding_digest = required_str(&material["binding"], "digest")?.to_owned();
        let connection_id = binding_config["connection_id"]
            .as_str()
            .ok_or_else(|| Error::rejected("reviewed binding names no connection"))?
            .to_owned();
        // Resolve the remote AOS connection for BOTH image and text-only
        // prepares. This is discovery under the runtime READ credential, not
        // proof that the local binding and AOS connection share a workspace.
        let resolver = self.social_media_resolver.clone().ok_or_else(|| {
            Error::rejected("capability_unavailable: no media resolver configured")
        })?;
        let rows = match resolver.list() {
            crate::platform::agenticos_external::media_import::DestinationList::Complete(rows) => {
                rows
            }
            crate::platform::agenticos_external::media_import::DestinationList::Ambiguous
            | crate::platform::agenticos_external::media_import::DestinationList::Unavailable => {
                return Err(Error::rejected(
                    "capability_unavailable: the destinations read did not produce a complete list",
                ));
            }
        };
        let pinned_connection = match binding_config["publish"].get("aos_connection_id") {
            None => None,
            Some(Value::String(expected)) if device::valid_connection_id(expected) => {
                Some(expected.as_str())
            }
            Some(_) => {
                return Err(Error::rejected(
                    "capability_unavailable: the configured account connection is invalid; refresh accounts",
                ));
            }
        };
        let matches: Vec<_> = rows
            .iter()
            .filter(|row| {
                row.toolkit == account.toolkit
                    && row.destination_id == account.destination_id
                    && row.status == "active"
                    && row.available
                    && row.publishable
                    && pinned_connection
                        .is_none_or(|expected| row.connection_id.as_str() == expected)
            })
            .collect();
        let row = match matches.as_slice() {
            [] if pinned_connection.is_some() => {
                return Err(Error::rejected(
                    "capability_unavailable: the configured account connection changed; refresh accounts",
                ));
            }
            [] => {
                return Err(Error::rejected(
                    "grant_binding_mismatch: the configured account is not publishable",
                ));
            }
            [row] => *row,
            _ => {
                return Err(Error::rejected(
                    "capability_unavailable: the configured account is ambiguous",
                ));
            }
        };
        account = PublishAccount::parse(&json!({
            "destination_id": row.destination_id,
            "destination_label": row.display_name,
            "toolkit": row.toolkit,
            "timezone": account.timezone,
        }))?;
        let aos_connection_id = row.connection_id.clone();
        let caption_wire = required_str(&material["artifact"], "digest")?;
        let caption_digest = caption_wire
            .strip_prefix("sha256:")
            .ok_or_else(|| Error::rejected("reviewed caption digest is malformed"))?;
        if !device::valid_digest(caption_digest) {
            return Err(Error::rejected("reviewed caption digest is malformed"));
        }
        let image_digest = material
            .get("asset")
            .and_then(|asset| asset["digest"].as_str())
            .map(|digest| {
                let bare = digest.strip_prefix("sha256:").unwrap_or(digest);
                if device::valid_digest(bare) {
                    Ok(bare.to_owned())
                } else {
                    Err(Error::rejected("reviewed image digest is malformed"))
                }
            })
            .transpose()?;
        if account.toolkit == "instagram" && image_digest.is_none() {
            return Err(Error::rejected("instagram needs a reviewed image asset"));
        }
        // The effect id is stable across exact retries. Reuse the frozen id
        // rather than staging again; only a first prepare creates this local,
        // nondispatchable app-artifact record.
        let effect_id = if let Some(existing) = existing.as_ref() {
            required_str(&existing["prepared"]["descriptor"], "effect_id")?.to_owned()
        } else {
            let effect = self.stage_app_artifact(&json!({
                "run_id": run_id,
                "artifact_id": artifact_id,
                "slot": slot,
                "request_id": format!("{request_id}-fx"),
                "title": account.destination_label,
            }))?;
            required_str(&effect["effect"], "effect_id")?.to_owned()
        };
        // Stable, content-bound approval identity: one prepare gesture ->
        // one intent identity, derived from install+request_id.
        let approval = match existing.as_ref() {
            Some(existing) => {
                required_str(&existing["prepared"]["descriptor"], "approval_id")?.to_owned()
            }
            None => format!(
                "apv-{}",
                uuid::Uuid::new_v5(
                    &uuid::Uuid::NAMESPACE_OID,
                    format!("apv-prepare:{install}:{request_id}").as_bytes()
                )
                .simple()
            ),
        };
        let descriptor = json!({
            "app_id": app_id,
            "install_id": install,
            "context_id": context,
            "binding_id": binding_id,
            "binding_revision": binding_revision,
            "binding_digest": binding_digest,
            "run_id": run_id,
            "run_snapshot_digest": run["snapshot_digest"],
            "approved_digest": run["approved_digest"],
            "artifact_id": artifact_id,
            "artifact_digest": caption_wire,
            "review_receipt_digest": crate::store::app_runs::material_digest(&material["review_receipt"]),
            "bundle_digest": bundle,
            "slot": slot,
            "binding_connection_id": connection_id,
            "aos_connection_id": aos_connection_id,
            "destination_id": account.destination_id,
            "toolkit": account.toolkit,
            "caption_digest": caption_digest,
            "image_digest": image_digest,
            "effect_id": effect_id,
            "approval_id": approval,
            "mode": mode,
            "due_epoch": due,
            "not_before_epoch": not_before,
            "expires_epoch": expires,
        });
        // On an exact retry, reuse the already-imported media key; do not
        // upload again. The store recomputes the full descriptor digest and
        // rejects any changed run/material/binding/window/receipt.
        let mut media_key: Option<String> = None;
        if let Some(existing) = existing {
            let frozen = &existing["prepared"]["descriptor"];
            media_key = match image_digest.as_deref() {
                Some(digest) => {
                    let key = required_str(frozen, "media_key")?;
                    if !device::media_key_authorizes_connection(key, &aos_connection_id, digest) {
                        return Err(Error::rejected(
                            "prepared intent media receipt does not bind the current connection and image",
                        ));
                    }
                    Some(key.to_owned())
                }
                None if frozen.get("media_key").is_none_or(Value::is_null) => None,
                None => {
                    return Err(Error::rejected(
                        "prepared intent unexpectedly carries media for a text-only artifact",
                    ));
                }
            };
            let prepared = self
                .store
                .social_publish_prepare_intent(&NewPreparedIntent {
                    request_id,
                    install_id: install,
                    context_id: context,
                    run_id,
                    effect_id: &effect_id,
                    connection_id: &connection_id,
                    aos_connection_id: Some(&aos_connection_id),
                    account: &account,
                    caption_digest,
                    image_digest: image_digest.as_deref(),
                    media_key: media_key.as_deref(),
                    approval_id: &approval,
                    mode,
                    due_epoch: due,
                    not_before_epoch: not_before,
                    expires_epoch: expires,
                    descriptor: &descriptor,
                })?;
            return expose_owner_intent_descriptor(prepared, &material);
        }
        if image_digest.is_some() {
            let imported = self.import_social_media(&json!({
                "request_id": request_id,
                "install_id": install,
                "context_id": context,
                "run_id": run_id,
                "artifact_id": artifact_id,
                "bundle_digest": bundle,
                "slot": slot,
                "toolkit": account.toolkit,
                "destination_id": account.destination_id,
            }))?;
            if imported["aos_connection_id"].as_str() != Some(aos_connection_id.as_str()) {
                return Err(Error::rejected(
                    "grant_binding_mismatch: the media import resolved another AOS connection",
                ));
            }
            let got = imported["image_digest"]
                .as_str()
                .unwrap_or_default()
                .strip_prefix("sha256:")
                .unwrap_or(imported["image_digest"].as_str().unwrap_or_default());
            if Some(got) != image_digest.as_deref() {
                return Err(Error::rejected(
                    "grant_binding_mismatch: the imported image is not the reviewed image",
                ));
            }
            media_key = Some(required_str(&imported, "media_key")?.to_owned());
        }
        let prepared = self
            .store
            .social_publish_prepare_intent(&NewPreparedIntent {
                request_id,
                install_id: install,
                context_id: context,
                run_id,
                effect_id: &effect_id,
                connection_id: &connection_id,
                aos_connection_id: Some(&aos_connection_id),
                account: &account,
                caption_digest,
                image_digest: image_digest.as_deref(),
                media_key: media_key.as_deref(),
                approval_id: &approval,
                mode,
                due_epoch: due,
                not_before_epoch: not_before,
                expires_epoch: expires,
                descriptor: &descriptor,
            })?;
        expose_owner_intent_descriptor(prepared, &material)
    }

    /// `app_publish_intent_attach` (CAD-1143): trusted-operator path. The
    /// caller supplies only the scoped prepared-id selector; the daemon
    /// derives a selector-only queue check and accepts only the distinct
    /// signed `social.queue-validation.v1` receipt. The unsigned owner
    /// exchange result and `social.intent.read.v1` assertion are not attach
    /// authority. A verified receipt and queued-row handoff commit together.
    pub(super) fn attach_publish_intent(&self, params: &Value) -> Result<Value> {
        let prepared_id = Self::required_segment(params, "prepared_id")?;
        let install = Self::required_segment(params, "install_id")?;
        let context = Self::strict_optional_segment(params, "context_id")?;
        let shown =
            self.store
                .social_publish_prepared_show_scoped(prepared_id, install, context)?;
        match shown["prepared"]["state"].as_str() {
            // A committed attach is idempotent even if the upstream action
            // has since been replaced; its actual send path fences current
            // grant/action state immediately before dispatch.
            Some("authorized") => {
                let status = self.store.social_publish_prepared_status_scoped(
                    prepared_id,
                    install,
                    context,
                )?;
                match (
                    status["queued"]["state"].as_str(),
                    status["queued"]["claim_armed"].as_bool(),
                ) {
                    // A fully armed attachment is an idempotent read. Do not
                    // fetch another receipt, consume another JTI, or move its
                    // maturity floor.
                    (_, Some(true)) => self.store.social_publish_attached_show_scoped(
                        prepared_id,
                        install,
                        context,
                    ),
                    // A committed but not-yet-armed handoff must prove fresh
                    // AOS authority again before the same row can be armed.
                    (Some("queued"), Some(false)) => {
                        let grant = self.verify_authorized_queue_grant_for_recovery(
                            prepared_id,
                            install,
                            context,
                            &shown,
                        )?;
                        self.store.social_publish_attach_intent(
                            prepared_id,
                            install,
                            context,
                            &grant,
                            self.publish_queue_clock.as_ref(),
                        )
                    }
                    _ => Err(Error::rejected(
                        "authorized owner attachment is not safely recoverable",
                    )),
                }
            }
            Some("prepared") => {
                let grant =
                    self.verify_prepared_queue_grant(prepared_id, install, context, &shown)?;
                self.store.social_publish_attach_intent(
                    prepared_id,
                    install,
                    context,
                    &grant,
                    self.publish_queue_clock.as_ref(),
                )
            }
            _ => Err(Error::rejected(
                "only a prepared publish intent can accept an owner grant",
            )),
        }
    }

    /// Read-only owner-completion observation. It verifies the existing
    /// queue-validation receipt through the shared inspector, but never
    /// consumes its JTI or creates attach authority. Persisted local state is
    /// re-read after the remote observation so cancellation/attach wins races.
    pub(super) fn status_publish_intent(&self, params: &Value) -> Result<Value> {
        let prepared_id = Self::required_segment(params, "prepared_id")?;
        let install = Self::required_segment(params, "install_id")?;
        let context = Self::required_nullable_segment(params, "context_id")?;
        let mut status =
            self.store
                .social_publish_prepared_status_scoped(prepared_id, install, context)?;
        let owner_status = if status["state"] == "prepared" {
            let shown =
                self.store
                    .social_publish_prepared_show_scoped(prepared_id, install, context)?;
            let observed =
                self.prepared_owner_completion_status(prepared_id, install, context, &shown);
            let current =
                self.store
                    .social_publish_prepared_status_scoped(prepared_id, install, context)?;
            if current["state"] == "prepared" {
                observed
            } else {
                // A local attach/cancel won while the remote observation was
                // in flight. Return that persisted lifecycle, not a stale
                // owner-completion claim.
                let cancelled = current["state"] == "cancelled";
                status = current;
                if cancelled {
                    super::social_publish_queue::OwnerCompletionStatus::Refused
                } else {
                    super::social_publish_queue::OwnerCompletionStatus::Unknown
                }
            }
        } else if status["state"] == "cancelled"
            || status["state"] == "refused"
            || status["state"] == "superseded"
        {
            super::social_publish_queue::OwnerCompletionStatus::Refused
        } else {
            // Authorized queue lifecycle is reported separately. It is not
            // a fresh AOS receipt and must not be mislabeled `ready`.
            super::social_publish_queue::OwnerCompletionStatus::Unknown
        };
        status["owner_status"] = json!(owner_status.as_str());
        Ok(status)
    }

    /// Terminal, operator-only cancellation. The store atomically cancels
    /// the prepared row and, if already attached, its still-unclaimed queue
    /// row; it never asks AOS to delete or revoke anything remotely.
    pub(super) fn cancel_publish_intent(&self, params: &Value) -> Result<Value> {
        let prepared_id = Self::required_segment(params, "prepared_id")?;
        let install = Self::required_segment(params, "install_id")?;
        let context = Self::required_nullable_segment(params, "context_id")?;
        self.store
            .social_publish_prepared_cancel_scoped(prepared_id, install, context)
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

    #[test]
    fn a_rebuilt_receipt_keeps_the_operators_publish_settings() {
        let old = json!({"connection_id":"c","publish":publish()});
        let fresh = carry_publish(json!({"connection_id":"c"}), &old);
        assert_eq!(fresh, old);
        let bare = carry_publish(json!({"connection_id":"c"}), &json!({"connection_id":"c"}));
        assert!(bare.get("publish").is_none());
        let rebound = carry_publish(json!({"connection_id":"other"}), &old);
        assert!(rebound.get("publish").is_none());
    }
}
