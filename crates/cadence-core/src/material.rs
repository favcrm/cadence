//! The exact bytes an app-material digest covers.
//!
//! Native app and Works digests are `sha256:<hex>` over
//! [`MATERIAL_PREFIX`] followed by the canonical JSON of the value: object
//! keys sorted at every depth, arrays in order, numbers unchanged. The
//! canonicalisation lives here so native storage and every host hash the
//! same bytes; the kernel stays hash-free (no crypto dependency), and each
//! host applies the same SHA-256 to this UTF-8 string.
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Domain separator prefixed to every material value.
pub const MATERIAL_PREFIX: &str = "cadence-app-material-v1\n";

fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(fields) => {
            let ordered: BTreeMap<_, _> = fields
                .iter()
                .map(|(key, value)| (key.clone(), canonical(value)))
                .collect();
            Value::Object(ordered.into_iter().collect::<Map<_, _>>())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

/// The material string a host hashes: `cadence-app-material-v1\n` and the
/// canonical JSON, byte for byte.
pub fn material(value: &Value) -> String {
    format!("{MATERIAL_PREFIX}{}", canonical(value))
}
