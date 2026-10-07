//! Data-only app assistant action descriptors (CAD-1184).
//!
//! A package may advertise only action IDs in the host registry. The
//! descriptor is included in the installation consent digest; it never
//! chooses a command, URL, or daemon method.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};

pub const FILE: &str = "app-assistant.json";
pub const CONTRACT: &str = "app-assistant/v1";
const MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub contract: String,
    #[serde(default)]
    pub app: Option<String>,
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    pub description: String,
    pub input_schema: Value,
    pub effect: String,
    pub confirmation: String,
    #[serde(default)]
    pub availability: Option<String>,
}

/// Parse and validate descriptor data against the registry's immutable
/// action metadata. Package declarations may omit registered actions but
/// cannot weaken their effect or confirmation policy.
pub fn validate(text: &str) -> Result<Descriptor> {
    if text.len() > MAX_BYTES {
        return Err(Error::rejected(
            "app assistant descriptor exceeds its size bound",
        ));
    }
    let descriptor: Descriptor = serde_json::from_str(text).map_err(|_| {
        Error::rejected("app assistant descriptor is not valid app-assistant/v1 JSON")
    })?;
    if descriptor.contract != CONTRACT
        || descriptor.actions.is_empty()
        || descriptor.actions.len() > 32
    {
        return Err(Error::rejected(
            "app assistant descriptor has an unsupported contract or action count",
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for action in &descriptor.actions {
        let Some(registry) = crate::app_assistant::registered_action(&action.id) else {
            return Err(Error::rejected(format!(
                "unknown app assistant action '{}'; host registry owns action IDs",
                action.id
            )));
        };
        if !seen.insert(&action.id)
            || action.description.trim().is_empty()
            || action.description.len() > 240
            || action.description.chars().any(char::is_control)
            || action.effect != registry.effect
            || action.confirmation != registry.confirmation
            || !crate::app_assistant::schema_accepts(&action.id, &action.input_schema)
        {
            return Err(Error::rejected(format!(
                "app assistant action '{}' differs from the registered contract",
                action.id
            )));
        }
    }
    Ok(descriptor)
}

pub fn size_and_json(text: &str) -> Result<Value> {
    let descriptor = validate(text)?;
    serde_json::to_value(descriptor).map_err(Into::into)
}
