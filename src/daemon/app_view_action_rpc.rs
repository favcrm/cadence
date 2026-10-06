//! Operator-only, receipt-bound CRM customer actions (CAD-867).
//!
//! Every write re-proves the bundle, app-views/v1 descriptor,
//! app-bindings/v1 binding and app-actions/v2 declaration from one
//! verified installation snapshot. The PM lock and app-release lock span
//! both that proof and the direct RecordStore mutation; a package upgrade
//! cannot change the action bytes between authorization and write. The
//! handler never nests app-record RPCs and never accepts caller identity,
//! source, consent, or a create record id.

use super::*;
use crate::issue::app_catalog::workspace;
use crate::issue::{app, app_action, app_action_v2, app_binding, app_view};
use crate::store::app_records::{ConsentState, CustomerConsent, CustomerProfile, RecordStore};
use serde_json::{json, Map, Value};

const REQUEST_KEYS: &[&str] = &[
    "install_id",
    "context_id",
    "view_id",
    "action_id",
    "digest",
    "view_descriptor_digest",
    "view_binding_digest",
    "record_id",
    "expected_revision",
    "input",
];

#[derive(Default)]
struct CustomerPatch {
    display_name: Option<String>,
    email: Option<Option<String>>,
    phone: Option<Option<String>>,
    source: Option<Option<String>>,
    tags: Option<Vec<String>>,
}

fn digest_shape(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn positive_revision(value: &Value) -> Result<i64> {
    value
        .as_i64()
        .filter(|revision| *revision > 0)
        .ok_or_else(|| Error::rejected("expected record revision must be a positive integer"))
}

fn action_field<'a>(
    action: &'a app_action_v2::Action,
    id: &str,
) -> Result<&'a app_action_v2::Field> {
    action
        .input
        .fields
        .iter()
        .find(|field| field.id == id)
        .ok_or_else(|| Error::rejected("installed customer action input shape is unsupported"))
}

fn text_limit(field: &app_action_v2::Field) -> Result<usize> {
    let host = app_action_v2::host_text_limit(&field.id)
        .ok_or_else(|| Error::rejected("installed customer action text field is unsupported"))?;
    Ok(field
        .max_length
        .map_or(host, |declared| host.min(declared as usize)))
}

fn optional_text(
    object: &Map<String, Value>,
    field: &app_action_v2::Field,
    update: bool,
) -> Result<Option<Option<String>>> {
    let Some(value) = object.get(&field.id) else {
        return Ok(if update { None } else { Some(None) });
    };
    let value = match value {
        Value::Null if field.nullable && !field.required => None,
        Value::String(text) if field.nullable && !field.required && text.trim().is_empty() => None,
        Value::String(text) => {
            if text.chars().count() > text_limit(field)? {
                return Err(Error::rejected(
                    "customer action field exceeds its supported bound",
                ));
            }
            Some(text.clone())
        }
        _ => {
            return Err(Error::rejected(
                "customer action text field has an invalid value",
            ))
        }
    };
    Ok(Some(value))
}

fn required_text(object: &Map<String, Value>, field: &app_action_v2::Field) -> Result<String> {
    let value = object
        .get(&field.id)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::rejected("customer action is missing required display_name"))?;
    if value.chars().count() > text_limit(field)? {
        return Err(Error::rejected(
            "customer action field exceeds its supported bound",
        ));
    }
    Ok(value.to_string())
}

fn tags_value(
    object: &Map<String, Value>,
    field: &app_action_v2::Field,
    update: bool,
) -> Result<Option<Vec<String>>> {
    let Some(value) = object.get(&field.id) else {
        return Ok(if update { None } else { Some(Vec::new()) });
    };
    let values = value
        .as_array()
        .ok_or_else(|| Error::rejected("customer tags must be a list"))?;
    let package_max = field
        .max_items
        .map_or(app_action_v2::HOST_TAG_ITEMS, |max| {
            app_action_v2::HOST_TAG_ITEMS.min(max as usize)
        });
    let package_length = field
        .max_length
        .map_or(app_action_v2::HOST_TAG_LENGTH, |max| {
            app_action_v2::HOST_TAG_LENGTH.min(max as usize)
        });
    if values.len() > package_max {
        return Err(Error::rejected(
            "customer tags exceed their supported bound",
        ));
    }
    let mut tags = Vec::with_capacity(values.len());
    for value in values {
        let tag = value
            .as_str()
            .ok_or_else(|| Error::rejected("customer tags must be text"))?;
        if tag.chars().count() > package_length {
            return Err(Error::rejected(
                "customer tags exceed their supported bound",
            ));
        }
        tags.push(tag.to_string());
    }
    Ok(Some(tags))
}

fn parse_patch(
    action: &app_action_v2::Action,
    input: &Value,
    update: bool,
) -> Result<CustomerPatch> {
    let object = input
        .as_object()
        .ok_or_else(|| Error::rejected("customer action input must be an object"))?;
    if object
        .keys()
        .any(|key| !action.input.fields.iter().any(|field| field.id == *key))
    {
        return Err(Error::rejected(
            "customer action input has unsupported fields",
        ));
    }
    let display_name = required_text(object, action_field(action, "display_name")?)?;
    let email = optional_text(object, action_field(action, "email")?, update)?;
    let phone = optional_text(object, action_field(action, "phone")?, update)?;
    let source = optional_text(object, action_field(action, "source")?, update)?;
    let tags = tags_value(object, action_field(action, "tags")?, update)?;
    Ok(CustomerPatch {
        display_name: Some(display_name),
        email,
        phone,
        source,
        tags,
    })
}

fn create_profile(patch: CustomerPatch) -> CustomerProfile {
    CustomerProfile {
        schema: 1,
        display_name: patch.display_name.unwrap_or_default(),
        email: patch.email.unwrap_or(None),
        phone: patch.phone.unwrap_or(None),
        tags: patch.tags.unwrap_or_default(),
        source: patch.source.unwrap_or(None),
        consent: CustomerConsent {
            email: ConsentState::Unknown,
            sms: None,
        },
    }
}

fn merge_update(
    current: &Value,
    patch: CustomerPatch,
    install: &str,
    context: &str,
    record: &str,
    expected: i64,
) -> Result<CustomerProfile> {
    if current["id"].as_str() != Some(record)
        || current["install_id"].as_str() != Some(install)
        || current["context_id"].as_str() != Some(context)
        || current["kind"].as_str() != Some("customer")
        || current["revision"]
            .as_i64()
            .is_none_or(|revision| revision < 1)
    {
        return Err(Error::rejected(
            "customer record receipt does not match the requested scope",
        ));
    }
    if current["revision"].as_i64() != Some(expected) {
        return Err(Error::rejected("record revision is stale"));
    }
    let mut profile = CustomerProfile::parse(&current["profile"])
        .map_err(|_| Error::rejected("record integrity refused"))?;
    profile.display_name = patch
        .display_name
        .ok_or_else(|| Error::rejected("customer action is missing required display_name"))?;
    if let Some(email) = patch.email {
        profile.email = email;
    }
    if let Some(phone) = patch.phone {
        profile.phone = phone;
    }
    if let Some(source) = patch.source {
        profile.source = source;
    }
    if let Some(tags) = patch.tags {
        profile.tags = tags;
    }
    // `consent` is not part of CustomerPatch. The validated current
    // CustomerProfile is the sole consent/provenance source for update.
    Ok(profile)
}

fn snapshot_descriptor(
    files: &std::collections::BTreeMap<String, String>,
) -> Result<app_view::Descriptor> {
    app_view::parse_descriptor(
        files
            .get(app_view::REL_PATH)
            .ok_or_else(|| Error::rejected("installed descriptor is absent from this bundle"))?,
    )
    .map_err(|error| Error::rejected(format!("installed descriptor: {error}")))
}

fn snapshot_binding(
    files: &std::collections::BTreeMap<String, String>,
) -> Result<app_binding::Binding> {
    app_binding::parse_binding(
        files
            .get(app_binding::REL_PATH)
            .ok_or_else(|| Error::rejected("installed binding is absent from this bundle"))?,
    )
    .map_err(|error| Error::rejected(format!("installed binding: {error}")))
}

fn snapshot_actions(
    files: &std::collections::BTreeMap<String, String>,
) -> Result<app_action_v2::Descriptor> {
    app_action_v2::parse_str(
        files.get(app_action_v2::REL_PATH).ok_or_else(|| {
            Error::rejected("installed action descriptor is absent from this bundle")
        })?,
    )
    .map_err(|error| Error::rejected(format!("installed action descriptor: {error}")))
}

impl Shared {
    /// Authoritative live customer mutation. Caller identity is proven
    /// before even the request object is trusted. The runtime snapshot
    /// callback retains PM->release->store lock order across all checks
    /// and the write/CAS.
    pub(super) fn rpc_app_view_action(
        self: &Arc<Self>,
        _method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("app view action", params, peer_pid)?;
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("app view action payload must be an object"))?;
        if fields
            .keys()
            .any(|key| !REQUEST_KEYS.contains(&key.as_str()))
        {
            return Err(Error::rejected(
                "app view action payload has unsupported fields",
            ));
        }
        let install = required_str(params, "install_id")?;
        crate::proto::identifier(install, "installation ID")?;
        let context = required_str(params, "context_id")?;
        crate::proto::identifier(context, "context ID")?;
        let view = required_str(params, "view_id")?;
        crate::proto::identifier(view, "form view ID")?;
        let action_id = required_str(params, "action_id")?;
        let digest = required_str(params, "digest")?;
        let descriptor_digest = required_str(params, "view_descriptor_digest")?;
        let binding_digest = required_str(params, "view_binding_digest")?;
        for pin in [digest, descriptor_digest, binding_digest] {
            if !digest_shape(pin) {
                return Err(Error::rejected("app view action digest pin is invalid"));
            }
        }
        let input = params
            .get("input")
            .ok_or_else(|| Error::rejected("app view action input is required"))?;
        let update = match action_id {
            "customer.create" => false,
            "customer.update" => true,
            _ => return Err(Error::rejected("unsupported installed customer action")),
        };
        let record_id = if update {
            let record = required_str(params, "record_id")?;
            crate::proto::identifier(record, "record ID")?;
            Some(record)
        } else {
            if fields.contains_key("record_id") || fields.contains_key("expected_revision") {
                return Err(Error::rejected(
                    "customer create does not accept a record id or revision",
                ));
            }
            None
        };
        let expected_revision = if update {
            Some(positive_revision(
                fields
                    .get("expected_revision")
                    .ok_or_else(|| Error::rejected("customer update requires expected_revision"))?,
            )?)
        } else {
            None
        };

        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, install, |row, files| {
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if row["digest"].as_str() != Some(digest) {
                return Err(Error::rejected("installation digest is stale"));
            }
            if row["view_descriptor_digest"].as_str() != Some(descriptor_digest) {
                return Err(Error::rejected("view descriptor digest is stale"));
            }
            if row["view_binding_digest"].as_str() != Some(binding_digest) {
                return Err(Error::rejected("view binding digest is stale"));
            }
            let manifest =
                app::parse_manifest(files.get("app.md").ok_or_else(|| {
                    Error::rejected("installed manifest is absent from this bundle")
                })?)
                .map_err(|error| Error::rejected(format!("installed manifest: {error}")))?;
            if manifest.action_contract.as_deref() != Some(app_action_v2::CONTRACT)
                || manifest.view_contract.as_deref() != Some(app_view::CONTRACT)
                || manifest.binding_contract.as_deref() != Some(app_binding::CONTRACT)
            {
                return Err(Error::rejected(
                    "installed bundle does not declare the pinned action/view/binding pair",
                ));
            }
            let views = snapshot_descriptor(files)?;
            let binding = snapshot_binding(files)?;
            let actions = snapshot_actions(files)?;
            app_action_v2::validate_against(&actions, &manifest, Some(&views), Some(&binding))
                .map_err(|error| {
                    Error::rejected(format!("installed action/view/binding pair: {error}"))
                })?;
            let action = app_action_v2::action(&actions, action_id)
                .ok_or_else(|| Error::rejected("action is not in the installed descriptor"))?;
            if action.form_view != view
                || (action.operation == app_action::Operation::RecordUpdate) != update
            {
                return Err(Error::rejected(
                    "action does not match the installed form route",
                ));
            }
            if update != record_id.is_some() || update != expected_revision.is_some() {
                return Err(Error::rejected(
                    "customer action route selectors are incomplete",
                ));
            }

            // Prove an active context owned by this install before the
            // installation-specific record file is opened.
            self.store.app_context_proof(install, context)?;
            let records = RecordStore::open(&self.state_dir, install)?;
            let (result, record, create) = if update {
                let record = record_id.expect("update record validated");
                let expected = expected_revision.expect("update revision validated");
                let current = records.app_record_show(context, record)?;
                let patch = parse_patch(action, input, true)?;
                let profile = merge_update(
                    &current["record"],
                    patch,
                    install,
                    context,
                    record,
                    expected,
                )?;
                let result =
                    records.app_record_update(context, record, expected, &profile, None)?;
                (result, record.to_string(), false)
            } else {
                let patch = parse_patch(action, input, false)?;
                let profile = create_profile(patch);
                let record = format!("cust-{}", uuid::Uuid::new_v4().simple());
                crate::proto::identifier(&record, "record ID")?;
                let result = records.app_record_create_with(context, &record, &profile, None)?;
                (result, record, true)
            };
            let revision = result["record"]["revision"]
                .as_i64()
                .ok_or_else(|| Error::internal("customer action returned no record revision"))?;
            let record_digest = result["record"]["digest"]
                .as_str()
                .ok_or_else(|| Error::internal("customer action returned no record digest"))?;
            self.store
                .note_app_record(install, context, &record, revision, record_digest, create);
            self.wake();
            Ok(json!({
                "record": result["record"].clone(),
                "digest": digest,
                "view_descriptor_digest": descriptor_digest,
                "view_binding_digest": binding_digest,
            }))
        })
    }
}
