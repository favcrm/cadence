//! Daemon-wide model defaults and the registration-time resolver.
//!
//! Defaults are a host policy for future registrations. They never
//! rewrite an existing agent, and they never change runtime `pm` /
//! `worker` authority. Team role is a separate lookup key.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adapter::registry;
use crate::error::{Error, Result};

mod json_scan;
mod resolution;

pub use json_scan::parse_settings_document;
use json_scan::unique_string_map;
pub use resolution::{
    explicit_override_selection, legacy_selection, pi_policy_default_selection,
    platform_default_selection, resolve, snapshot_json, SOURCE_PLATFORM_DEFAULT,
};

pub const MAX_MODEL_BYTES: usize = 200;
pub const MAX_CONFIG_BYTES: usize = 16 * 1024;
pub const MAX_HTTP_BODY_BYTES: usize = 20 * 1024;
pub const MAX_STORED_PARAMS: usize = 4_000;
/// Pre-serde duplicate-key scan limit. A body under 20 KiB can still
/// nest thousands of arrays; serde's recursion guard never runs if this
/// walk overflows the stack first.
pub(super) const MAX_JSON_DEPTH: usize = 128;

/// Canonical team-role keys. `ops` is a launch-input alias of `devops`
/// and is not itself a stored key.
pub const TEAM_ROLES: &[(&str, &str)] = &[
    ("pm", "PM"),
    ("research", "Research"),
    ("architect", "Architect"),
    ("dev", "Developer"),
    ("qa", "QA"),
    ("devops", "DevOps"),
    ("worker", "Worker"),
];

pub const SOURCE_EXPLICIT: &str = "explicit";
pub const SOURCE_EXPLICIT_PROVIDER_DEFAULT: &str = "explicit_provider_default";
pub const SOURCE_ROLE_DEFAULT: &str = "role_default";
pub const SOURCE_PROVIDER_BASELINE: &str = "provider_baseline";
pub const SOURCE_PROVIDER_DEFAULT: &str = "provider_default";
pub const SOURCE_LEGACY_CONFIGURED: &str = "legacy_configured";
pub const SOURCE_LEGACY_PROVIDER_DEFAULT: &str = "legacy_provider_default";
/// pm.yaml's `[pi].models.default` filled the launch model — the
/// operator's policy chose it, not the register/start caller.
pub const SOURCE_PI_POLICY_DEFAULT: &str = "pi_policy_default";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelPolicy {
    Inherit,
    ProviderDefault,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelSelector {
    Model { model: String },
    ProviderDefault,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDefaults {
    pub default: ModelSelector,
    #[serde(deserialize_with = "unique_string_map")]
    pub roles: BTreeMap<String, ModelSelector>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDefaults {
    pub schema: u32,
    #[serde(deserialize_with = "unique_string_map")]
    pub providers: BTreeMap<String, ProviderDefaults>,
    /// Agents-settings overlay. Absent or null on documents written by
    /// `model_defaults_set`; that writer keeps the stored object.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub agent_settings: Value,
}

impl ModelDefaults {
    pub fn empty() -> Self {
        Self {
            schema: 1,
            providers: BTreeMap::new(),
            agent_settings: Value::Null,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingsWrite {
    pub expected_revision: i64,
    pub config: ModelDefaults,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    pub actor: String,
    pub transport: &'static str,
}

pub struct ResolveRequest<'a> {
    pub provider: &'a str,
    pub endpoint_kind: &'a str,
    pub runtime_role: &'a str,
    pub team_role: Option<&'a str>,
    pub model_policy: Option<&'a str>,
    pub params: Option<&'a str>,
    pub config: &'a ModelDefaults,
    pub revision: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRegistration {
    pub params: Option<String>,
    pub team_role: Option<String>,
    pub model_selection: Option<Value>,
}

/// One agent's observed model strings, used only to label suggestions.
pub struct ObservedModel<'a> {
    pub provider: &'a str,
    pub configured: Option<&'a str>,
    pub reported: Option<&'a str>,
}

// Mirrored by `canonical_model_id` in remote_cli.rs; see its "Seam note (CAD-2096)".
pub fn validate_model_id(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error::invalid(
            "invalid_model",
            "model id must be a non-empty string",
        ));
    }
    if trimmed.len() > MAX_MODEL_BYTES {
        return Err(Error::invalid(
            "invalid_model",
            format!("model id must be at most {MAX_MODEL_BYTES} bytes"),
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(Error::invalid(
            "invalid_model",
            "model id must not contain control characters",
        ));
    }
    Ok(trimmed.to_string())
}

pub fn canonical_team_role(role: &str) -> bool {
    TEAM_ROLES.iter().any(|(id, _)| *id == role)
}

/// Launch input only. `ops` becomes `devops`; unknown roles are rejected
/// rather than stored as a new authority.
pub fn normalize_team_role(raw: &str) -> Result<String> {
    let key = raw.trim().to_ascii_lowercase();
    if key.is_empty() {
        return Err(Error::invalid(
            "invalid_team_role",
            "team role must be a non-empty string",
        ));
    }
    let key = if key == "ops" { "devops" } else { key.as_str() };
    if canonical_team_role(key) {
        Ok(key.to_string())
    } else {
        Err(Error::invalid(
            "invalid_team_role",
            format!(
                "unknown team role '{raw}' — expected one of: {}",
                TEAM_ROLES
                    .iter()
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ))
    }
}

pub fn parse_model_policy(raw: Option<&str>) -> Result<ModelPolicy> {
    match raw.map(str::trim) {
        None => Ok(ModelPolicy::Inherit),
        Some("inherit") => Ok(ModelPolicy::Inherit),
        Some("provider_default") => Ok(ModelPolicy::ProviderDefault),
        Some(other) => Err(Error::invalid(
            "invalid_model_policy",
            format!("model_policy must be 'inherit' or 'provider_default', not '{other}'"),
        )),
    }
}

fn normalize_selector(selector: ModelSelector) -> Result<ModelSelector> {
    match selector {
        ModelSelector::ProviderDefault => Ok(ModelSelector::ProviderDefault),
        ModelSelector::Model { model } => Ok(ModelSelector::Model {
            model: validate_model_id(&model)?,
        }),
    }
}

pub fn validate_config(config: &mut ModelDefaults) -> Result<()> {
    if config.schema != 1 {
        return Err(Error::invalid(
            "invalid_config",
            "model defaults schema must be 1",
        ));
    }
    let eligible = registry::model_provider_ids();
    for (provider, entry) in &mut config.providers {
        if !eligible.iter().any(|id| *id == provider) {
            return Err(Error::invalid(
                "invalid_config",
                format!("provider '{provider}' does not accept a model default"),
            ));
        }
        entry.default = normalize_selector(entry.default.clone())?;
        for (role, selector) in &mut entry.roles {
            if !canonical_team_role(role) {
                return Err(Error::invalid(
                    "invalid_config",
                    format!("unknown team role '{role}' in model defaults"),
                ));
            }
            *selector = normalize_selector(selector.clone())?;
        }
    }
    Ok(())
}

/// `Some` is the daemon's verified caller, never a request field
/// (CAD-337): its transport is the proven operator connection.
pub fn normalize_attribution(raw: Option<&str>) -> Result<Attribution> {
    match raw {
        None => Ok(Attribution {
            actor: "local".to_string(),
            transport: "local",
        }),
        Some(value) => {
            let actor = value.trim();
            if actor.is_empty() || actor.len() > 160 || actor.chars().any(char::is_control) {
                return Err(Error::invalid(
                    "invalid_request",
                    "attribution must be 1-160 characters without control characters",
                ));
            }
            Ok(Attribution {
                actor: actor.to_string(),
                transport: "operator-connection",
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(raw: &str) -> ModelDefaults {
        let mut parsed: ModelDefaults = serde_json::from_str(raw).unwrap();
        validate_config(&mut parsed).unwrap();
        parsed
    }

    fn resolved(
        provider: &str,
        kind: &str,
        role: &str,
        team: Option<&str>,
        policy: Option<&str>,
        params: Option<&str>,
        defaults: &ModelDefaults,
    ) -> ResolvedRegistration {
        resolve(ResolveRequest {
            provider,
            endpoint_kind: kind,
            runtime_role: role,
            team_role: team,
            model_policy: policy,
            params,
            config: defaults,
            revision: 4,
        })
        .unwrap()
    }

    fn source(resolved: &ResolvedRegistration) -> String {
        resolved.model_selection.as_ref().unwrap()["source"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn precedence_follows_explicit_role_baseline_then_native() {
        let defaults = config(
            r#"{"schema":1,"providers":{
                "claude":{
                    "default":{"mode":"model","model":"baseline"},
                    "roles":{
                        "qa":{"mode":"model","model":"qa-model"},
                        "dev":{"mode":"provider_default"},
                        "worker":{"mode":"model","model":"worker-model"}
                    }
                },
                "codex":{"default":{"mode":"provider_default"},"roles":{}}
            }}"#,
        );
        let explicit = resolved(
            "claude",
            "managed",
            "worker",
            Some("qa"),
            None,
            Some(r#"{"model":" explicit "}"#),
            &defaults,
        );
        assert_eq!(source(&explicit), "explicit");
        assert_eq!(
            explicit.model_selection.as_ref().unwrap()["revision"],
            json!(null)
        );
        assert_eq!(explicit.params.as_deref(), Some(r#"{"model":"explicit"}"#));

        let role = resolved("claude", "pty", "worker", Some("qa"), None, None, &defaults);
        assert_eq!(source(&role), "role_default");
        assert_eq!(role.model_selection.as_ref().unwrap()["model"], "qa-model");
        assert_eq!(role.model_selection.as_ref().unwrap()["revision"], 4);

        let native_role = resolved(
            "claude",
            "managed",
            "worker",
            Some("dev"),
            None,
            None,
            &defaults,
        );
        assert_eq!(source(&native_role), "role_default");
        assert_eq!(
            native_role.model_selection.as_ref().unwrap()["model"],
            json!(null)
        );
        assert!(native_role.params.is_none());

        let baseline = resolved(
            "claude",
            "managed",
            "worker",
            Some("architect"),
            None,
            None,
            &defaults,
        );
        assert_eq!(source(&baseline), "provider_baseline");
        assert_eq!(
            baseline.model_selection.as_ref().unwrap()["model"],
            "baseline"
        );

        let worker = resolved("claude", "managed", "worker", None, None, None, &defaults);
        assert_eq!(
            worker.model_selection.as_ref().unwrap()["lookup_role"],
            "worker"
        );
        assert_eq!(
            worker.model_selection.as_ref().unwrap()["model"],
            "worker-model"
        );

        let qa_ignores_worker = resolved(
            "claude",
            "managed",
            "worker",
            Some("research"),
            None,
            None,
            &defaults,
        );
        assert_eq!(
            qa_ignores_worker.model_selection.as_ref().unwrap()["model"],
            "baseline"
        );

        let ops = resolved(
            "claude",
            "managed",
            "worker",
            Some(" OPS "),
            None,
            None,
            &defaults,
        );
        assert_eq!(ops.team_role.as_deref(), Some("devops"));
        assert_eq!(
            ops.model_selection.as_ref().unwrap()["lookup_role"],
            "devops"
        );

        let bypass = resolved(
            "claude",
            "managed",
            "worker",
            Some("qa"),
            Some("provider_default"),
            None,
            &defaults,
        );
        assert_eq!(source(&bypass), "explicit_provider_default");
        assert_eq!(
            bypass.model_selection.as_ref().unwrap()["revision"],
            json!(null)
        );
        assert!(bypass.params.is_none());

        let codex = resolved(
            "codex",
            "managed-ws",
            "worker",
            Some("qa"),
            None,
            None,
            &defaults,
        );
        assert_eq!(source(&codex), "provider_default");
        assert!(codex.params.is_none());

        let empty = ModelDefaults::empty();
        let omitted = resolved("cursor", "pty", "pm", None, None, None, &empty);
        assert_eq!(source(&omitted), "provider_default");
        assert_eq!(omitted.model_selection.as_ref().unwrap()["revision"], 4);
        assert_eq!(
            omitted.model_selection.as_ref().unwrap()["lookup_role"],
            "pm"
        );
    }

    #[test]
    fn unsupported_endpoints_do_not_receive_a_model() {
        let defaults = config(
            r#"{"schema":1,"providers":{"claude":{"default":{"mode":"model","model":"baseline"},"roles":{}}}}"#,
        );
        let devin = resolved(
            "devin",
            "pty",
            "worker",
            Some("qa"),
            None,
            Some(r#"{"permission_mode":"auto"}"#),
            &defaults,
        );
        assert!(devin.model_selection.is_none());
        assert_eq!(devin.team_role.as_deref(), Some("qa"));
        assert_eq!(
            devin.params.as_deref(),
            Some(r#"{"permission_mode":"auto"}"#)
        );
        let err = resolve(ResolveRequest {
            provider: "inbox",
            endpoint_kind: "inbox",
            runtime_role: "worker",
            team_role: None,
            model_policy: Some("provider_default"),
            params: None,
            config: &defaults,
            revision: 1,
        })
        .unwrap_err();
        assert_eq!(err.code(), Some("unsupported_model_setting"));
    }

    #[test]
    fn invalid_documents_and_conflicts_reject() {
        let err = parse_settings_document(r#"{"expected_revision":0,"config":{"schema":1,"providers":{"nope":{"default":{"mode":"provider_default"},"roles":{}}}}}"#).unwrap_err();
        assert_eq!(err.code(), Some("invalid_config"));
        let err = parse_settings_document(
            r#"{"expected_revision":0,"expected_revision":1,"config":{"schema":1,"providers":{}}}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate key"));
        let err = parse_settings_document(
            r#"{"expected_revision":0,"config":{"schema":1,"providers":{"claude":{"default":{"mode":"model","model":"a"},"roles":{"qa":{"mode":"model","model":"b"},"qa":{"mode":"provider_default"}}}}}}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate key"));
        let err = parse_settings_document(
            r#"{"expected_revision":0,"config":{"schema":1,"providers":{},"extra":true}}"#,
        )
        .unwrap_err();
        assert_eq!(err.code(), Some("invalid_config"));
        let err = parse_settings_document(
            r#"{"expected_revision":1.5,"config":{"schema":1,"providers":{}}}"#,
        )
        .unwrap_err();
        assert_eq!(err.code(), Some("invalid_request"));
        let err = parse_settings_document(
            r#"{"expected_revision":-1,"config":{"schema":1,"providers":{}}}"#,
        )
        .unwrap_err();
        assert_eq!(err.code(), Some("invalid_request"));
        let mut bad = config(
            r#"{"schema":1,"providers":{"claude":{"default":{"mode":"model","model":"ok"},"roles":{}}}}"#,
        );
        bad.providers.get_mut("claude").unwrap().default = ModelSelector::Model {
            model: " \n ".to_string(),
        };
        assert_eq!(
            validate_config(&mut bad).unwrap_err().code(),
            Some("invalid_model")
        );
        let long = "m".repeat(201);
        assert!(validate_model_id(&long).is_err());
        assert!(validate_model_id("bad\u{0007}id").is_err());
        let defaults = ModelDefaults::empty();
        let err = resolve(ResolveRequest {
            provider: "claude",
            endpoint_kind: "managed",
            runtime_role: "worker",
            team_role: None,
            model_policy: Some("provider_default"),
            params: Some(r#"{"model":"explicit"}"#),
            config: &defaults,
            revision: 0,
        })
        .unwrap_err();
        assert_eq!(err.code(), Some("conflicting_model_policy"));
        let err = resolve(ResolveRequest {
            provider: "claude",
            endpoint_kind: "managed",
            runtime_role: "worker",
            team_role: None,
            model_policy: None,
            params: Some(r#"{"model":"   "}"#),
            config: &defaults,
            revision: 0,
        })
        .unwrap_err();
        assert_eq!(err.code(), Some("invalid_model"));
        let err = resolve(ResolveRequest {
            provider: "claude",
            endpoint_kind: "managed",
            runtime_role: "worker",
            team_role: None,
            model_policy: None,
            params: Some(r#"{"model":1}"#),
            config: &defaults,
            revision: 0,
        })
        .unwrap_err();
        assert_eq!(err.code(), Some("invalid_model"));
        assert!(normalize_team_role("nope").is_err());
        let huge = format!(
            r#"{{"expected_revision":0,"config":{{"schema":1,"providers":{{"claude":{{"default":{{"mode":"model","model":"{}"}},"roles":{{}}}}}}}}}}"#,
            "x".repeat(MAX_CONFIG_BYTES)
        );
        assert_eq!(
            parse_settings_document(&huge).unwrap_err().code(),
            Some("invalid_config")
        );
    }

    #[test]
    fn deep_json_nesting_is_rejected() {
        for body in [
            nested_settings("array", 9_000),
            nested_settings("object", 2_000),
        ] {
            assert!(
                body.len() <= MAX_HTTP_BODY_BYTES,
                "fixture must stay under the body cap, got {}",
                body.len()
            );
            let err = parse_settings_document(&body).unwrap_err();
            assert_eq!(err.code(), Some("invalid_config"));
            assert!(err.to_string().contains("nesting exceeds"), "{err}");
        }
        let still = parse_settings_document(
            r#"{"expected_revision":0,"config":{"schema":1,"providers":{}}}"#,
        )
        .unwrap();
        assert_eq!(still.expected_revision, 0);
        assert!(still.config.providers.is_empty());
    }

    fn nested_settings(kind: &str, depth: usize) -> String {
        let mut body = String::from(r#"{"expected_revision":0,"config":"#);
        match kind {
            "array" => {
                body.push_str(&"[".repeat(depth));
                body.push('0');
                body.push_str(&"]".repeat(depth));
            }
            "object" => {
                for _ in 0..depth {
                    body.push_str(r#"{"a":"#);
                }
                body.push('0');
                body.push_str(&"}".repeat(depth));
            }
            other => panic!("unknown nest {other}"),
        }
        body.push('}');
        body
    }

    #[test]
    fn identical_selector_round_trips_and_trims() {
        let write = parse_settings_document(
            r#"{"expected_revision":0,"config":{"schema":1,"providers":{"claude":{"default":{"mode":"model","model":"  opus  "},"roles":{"qa":{"mode":"provider_default"}}}}}}"#,
        )
        .unwrap();
        assert_eq!(write.expected_revision, 0);
        match &write.config.providers["claude"].default {
            ModelSelector::Model { model } => assert_eq!(model, "opus"),
            other => panic!("{other:?}"),
        }
    }
}
