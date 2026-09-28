//! Strict operator management of local app content contexts.
use super::*;
use crate::issue::{app_catalog::workspace, workflow};
use crate::store::app_contexts::ContextConfig;
use std::collections::BTreeMap;

/// Each default must be explicitly content-safe everywhere it is declared.
/// Different workflows can declare different content keys; a run uses only its
/// selected workflow's defaults.
pub(super) fn validate_defaults(
    files: &BTreeMap<String, String>,
    defaults: &BTreeMap<String, String>,
) -> Result<()> {
    let mut declarations = BTreeMap::new();
    for (path, text) in files {
        if !path.starts_with("workflows/") {
            continue;
        }
        let template = workflow::parse_template(text)
            .map_err(|_| Error::rejected("installed workflow content declarations are invalid"))?;
        for (key, spec) in &template.inputs {
            let safe = declarations.entry(key.clone()).or_insert(true);
            *safe &= spec.context_default;
        }
        let selected = defaults
            .iter()
            .filter(|(key, _)| template.inputs.contains_key(*key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        workflow::check_context_defaults(text, &selected)
            .map_err(|_| Error::rejected("context defaults are not declared safe content"))?;
    }
    if defaults
        .keys()
        .any(|key| declarations.get(key) != Some(&true))
    {
        return Err(Error::rejected(
            "context defaults are not declared safe content",
        ));
    }
    Ok(())
}

impl Shared {
    pub(super) fn rpc_app_context(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("app context management", params, peer_pid)?;
        let allowed: &[&str] = match method {
            "app_context_create" => &["install_id", "label", "input_defaults", "request_id"],
            "app_context_list" => &["install_id"],
            "app_context_show" => &["install_id", "context_id"],
            "app_context_update" => &[
                "install_id",
                "context_id",
                "expected_revision",
                "label",
                "input_defaults",
            ],
            "app_context_archive" => &["install_id", "context_id", "expected_revision"],
            _ => return Err(Error::rejected("unknown app context method")),
        };
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("app context payload must be an object"))?;
        if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(Error::rejected(
                "app context payload has unsupported fields",
            ));
        }
        let install = required_str(params, "install_id")?;
        crate::proto::identifier(install, "installation ID")?;
        match method {
            "app_context_list" => self.store.app_context_list(install),
            "app_context_show" => self
                .store
                .app_context_show(install, required_str(params, "context_id")?),
            "app_context_archive" => {
                // Archive remains available for historical orphaned installs.
                // Take release then only SQL, never acquire PM/custody later.
                let _release = self
                    .app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let result = self.store.app_context_archive(
                    install,
                    required_str(params, "context_id")?,
                    context_revision(params)?,
                )?;
                self.wake();
                Ok(result)
            }
            "app_context_create" | "app_context_update" => {
                let defaults: BTreeMap<String, String> = serde_json::from_value(
                    params
                        .get("input_defaults")
                        .cloned()
                        .ok_or_else(|| Error::rejected("context defaults are required"))?,
                )
                .map_err(|_| Error::rejected("context defaults must be a string map"))?;
                let config = ContextConfig::new(required_str(params, "label")?, defaults)?;
                let pm = self.pm_at(&self.pm_dir()?)?;
                let result = workspace::with_runtime_snapshot(&pm, install, |_, files| {
                    let _release = self
                        .app_release_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    validate_defaults(files, &config.input_defaults)?;
                    if method == "app_context_create" {
                        self.store.app_context_create(
                            install,
                            &config,
                            required_str(params, "request_id")?,
                        )
                    } else {
                        self.store.app_context_update(
                            install,
                            required_str(params, "context_id")?,
                            context_revision(params)?,
                            &config,
                        )
                    }
                })?;
                self.wake();
                Ok(result)
            }
            _ => Err(Error::rejected("unknown app context method")),
        }
    }
}
fn context_revision(params: &Value) -> Result<i64> {
    params
        .get("expected_revision")
        .and_then(Value::as_i64)
        .filter(|revision| *revision > 0)
        .ok_or_else(|| Error::rejected("expected context revision must be a positive integer"))
}
