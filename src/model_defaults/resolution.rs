//! Registration-time model resolution and snapshot formatting for `model_defaults`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use crate::adapter::registry;
use crate::error::{Error, Result};

use super::{
    normalize_team_role, parse_model_policy, validate_model_id, ModelDefaults, ModelPolicy,
    ModelSelector, ObservedModel, ResolveRequest, ResolvedRegistration, MAX_STORED_PARAMS,
    SOURCE_EXPLICIT, SOURCE_EXPLICIT_PROVIDER_DEFAULT, SOURCE_LEGACY_CONFIGURED,
    SOURCE_LEGACY_PROVIDER_DEFAULT, SOURCE_PI_POLICY_DEFAULT, SOURCE_PROVIDER_BASELINE,
    SOURCE_PROVIDER_DEFAULT, SOURCE_ROLE_DEFAULT, TEAM_ROLES,
};

fn selection(
    source: &str,
    lookup_role: &str,
    revision: Option<i64>,
    model: Option<String>,
) -> Value {
    json!({
        "source": source,
        "lookup_role": lookup_role,
        "revision": revision,
        "model": model,
    })
}

/// Existing rows keep their params. Supported endpoints with no stored
/// provenance are labeled legacy so the board does not invent a source.
pub fn legacy_selection(
    supports: bool,
    team_role: Option<&str>,
    runtime_role: &str,
    configured: Option<&str>,
) -> Value {
    if !supports {
        return Value::Null;
    }
    let source = if configured.is_some() {
        SOURCE_LEGACY_CONFIGURED
    } else {
        SOURCE_LEGACY_PROVIDER_DEFAULT
    };
    selection(
        source,
        team_role.unwrap_or(runtime_role),
        None,
        configured.map(str::to_string),
    )
}

fn parse_params(raw: Option<&str>) -> Result<Map<String, Value>> {
    let Some(raw) = raw else {
        return Ok(Map::new());
    };
    let parsed: Value =
        serde_json::from_str(raw).map_err(|_| Error::rejected("params must be a JSON object"))?;
    match parsed {
        Value::Object(map) => Ok(map),
        _ => Err(Error::rejected("params must be a JSON object")),
    }
}

fn finalize_params(original: Option<&str>, merged: &Map<String, Value>) -> Result<Option<String>> {
    if merged.is_empty() {
        return Ok(None);
    }
    if let Some(original) = original {
        if let Ok(Value::Object(previous)) = serde_json::from_str::<Value>(original) {
            if &previous == merged {
                if original.len() > MAX_STORED_PARAMS {
                    return Err(Error::invalid(
                        "params_too_large",
                        "params must be a JSON object of at most 4000 characters",
                    ));
                }
                return Ok(Some(original.to_string()));
            }
        }
    }
    let text = Value::Object(merged.clone()).to_string();
    if text.len() > MAX_STORED_PARAMS {
        return Err(Error::invalid(
            "params_too_large",
            "params must be a JSON object of at most 4000 characters",
        ));
    }
    Ok(Some(text))
}

fn explicit_model(params: &Map<String, Value>) -> Result<Option<String>> {
    match params.get("model") {
        None => Ok(None),
        Some(Value::String(model)) => Ok(Some(validate_model_id(model)?)),
        Some(_) => Err(Error::invalid(
            "invalid_model",
            "model must be a non-empty string",
        )),
    }
}

fn apply_selector(
    params: &mut Map<String, Value>,
    selector: &ModelSelector,
    source: &str,
    lookup_role: &str,
    revision: i64,
) -> Value {
    match selector {
        ModelSelector::Model { model } => {
            params.insert("model".to_string(), json!(model));
            selection(source, lookup_role, Some(revision), Some(model.clone()))
        }
        ModelSelector::ProviderDefault => {
            params.remove("model");
            selection(source, lookup_role, Some(revision), None)
        }
    }
}

fn inherit(
    params: &mut Map<String, Value>,
    config: &ModelDefaults,
    provider: &str,
    lookup_role: &str,
    revision: i64,
) -> Value {
    let Some(entry) = config.providers.get(provider) else {
        params.remove("model");
        return selection(SOURCE_PROVIDER_DEFAULT, lookup_role, Some(revision), None);
    };
    if let Some(selector) = entry.roles.get(lookup_role) {
        return apply_selector(params, selector, SOURCE_ROLE_DEFAULT, lookup_role, revision);
    }
    let source = match entry.default {
        ModelSelector::Model { .. } => SOURCE_PROVIDER_BASELINE,
        ModelSelector::ProviderDefault => SOURCE_PROVIDER_DEFAULT,
    };
    apply_selector(params, &entry.default, source, lookup_role, revision)
}

/// Resolve one fresh registration against a defaults snapshot already
/// read inside the caller's write transaction.
pub fn resolve(request: ResolveRequest<'_>) -> Result<ResolvedRegistration> {
    let team_role = match request.team_role {
        Some(raw) => Some(normalize_team_role(raw)?),
        None => None,
    };
    let policy = parse_model_policy(request.model_policy)?;
    let mut params = parse_params(request.params)?;
    let explicit = explicit_model(&params)?;
    if explicit.is_some() && policy == ModelPolicy::ProviderDefault {
        return Err(Error::invalid(
            "conflicting_model_policy",
            "an explicit model cannot be combined with model_policy provider_default",
        ));
    }
    if !registry::supports_model(request.provider, request.endpoint_kind) {
        if policy == ModelPolicy::ProviderDefault {
            return Err(Error::invalid(
                "unsupported_model_setting",
                format!(
                    "provider '{}' endpoint '{}' does not accept a model",
                    request.provider, request.endpoint_kind
                ),
            ));
        }
        return Ok(ResolvedRegistration {
            params: finalize_params(request.params, &params)?,
            team_role,
            model_selection: None,
        });
    }
    let lookup = team_role
        .clone()
        .unwrap_or_else(|| request.runtime_role.to_string());
    let model_selection = if let Some(model) = explicit {
        params.insert("model".to_string(), json!(model.clone()));
        selection(SOURCE_EXPLICIT, &lookup, None, Some(model))
    } else if policy == ModelPolicy::ProviderDefault {
        params.remove("model");
        selection(SOURCE_EXPLICIT_PROVIDER_DEFAULT, &lookup, None, None)
    } else {
        inherit(
            &mut params,
            request.config,
            request.provider,
            &lookup,
            request.revision,
        )
    };
    Ok(ResolvedRegistration {
        params: finalize_params(request.params, &params)?,
        team_role,
        model_selection: Some(model_selection),
    })
}

/// Provenance when pm.yaml's `[pi].models.default` fills the launch
/// model (CAD-559): `register_agent` re-derives `explicit` from the
/// merged params, so the caller stamps this over it. `revision` is
/// null — the operator's pm.yaml is not a revisioned document.
/// CAD-1176: provenance when the platform default filled the slot because
/// the policy resolved nothing for the role.
pub const SOURCE_PLATFORM_DEFAULT: &str = "platform_default";

/// Provenance for that fallback (`revision` null: it is not a revisioned
/// document).
pub fn platform_default_selection(lookup_role: &str, model: &str) -> Value {
    selection(
        SOURCE_PLATFORM_DEFAULT,
        lookup_role,
        None,
        Some(model.to_string()),
    )
}

pub fn pi_policy_default_selection(lookup_role: &str, model: &str) -> Value {
    selection(
        SOURCE_PI_POLICY_DEFAULT,
        lookup_role,
        None,
        Some(model.to_string()),
    )
}

/// Provenance for an explicit next-launch model change. Clearing the
/// model records provider-native behavior for that agent only.
pub fn explicit_override_selection(lookup_role: &str, model: Option<&str>) -> Result<Value> {
    match model {
        None => Ok(selection(
            SOURCE_EXPLICIT_PROVIDER_DEFAULT,
            lookup_role,
            None,
            None,
        )),
        Some(model) => {
            let model = validate_model_id(model)?;
            Ok(selection(SOURCE_EXPLICIT, lookup_role, None, Some(model)))
        }
    }
}

pub fn snapshot_json(revision: i64, config: &ModelDefaults, observed: &[ObservedModel]) -> Value {
    let providers: Vec<Value> = registry::model_provider_matrix()
        .iter()
        .map(|row| {
            let mut suggestions = BTreeSet::new();
            if row.eligible {
                for item in observed.iter().filter(|item| item.provider == row.id) {
                    for candidate in [item.configured, item.reported].into_iter().flatten() {
                        if let Ok(model) = validate_model_id(candidate) {
                            suggestions.insert(model);
                        }
                    }
                }
            }
            json!({
                "id": row.id,
                "label": provider_label(row.id),
                "eligible": row.eligible,
                "kinds": row.kinds,
                "suggestions": suggestions.into_iter().collect::<Vec<_>>(),
                "suggestions_note": "Previously observed on agents of this provider. Not a provider catalog.",
                "limitation": row.limitation,
            })
        })
        .collect();
    json!({
        "revision": revision,
        "config": config,
        "providers": providers,
        "roles": TEAM_ROLES.iter().map(|(id, label)| json!({"id": id, "label": label})).collect::<Vec<_>>(),
    })
}

fn provider_label(id: &str) -> &str {
    match id {
        "codex" => "Codex",
        "claude" => "Claude",
        "cursor" => "Cursor",
        "devin" => "Devin",
        other => other,
    }
}
