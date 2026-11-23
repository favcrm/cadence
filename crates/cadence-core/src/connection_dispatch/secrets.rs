//! Known-secret projection.
//!
//! This preserves the existing native screen semantics exactly
//! (`src/platform/mod.rs::carries_credential`/`refuse_json_leak` and
//! `src/platform/smtp.rs::carries_secret`): a value refuses when the text
//! carries the whole secret (or credential metadata such as the account) or
//! any 8-character window of it. Decoded structured values are walked
//! directly — object keys, strings and scalar text — so serialization
//! escaping is never the only thing protecting a value.
//!
//! There is no speculative encoding or global-DLP expansion here: no hex,
//! percent, base64 or Unicode-escape projection is invented. A hit refuses;
//! nothing is redacted or echoed, and the secret never appears in an error.
use crate::connection_dispatch::dto::{MAX_MODEL_TEXT_BYTES, MAX_RAW_CREDENTIAL_BYTES};
use crate::connection_dispatch::primitives::SECRET_FRAGMENT_BYTES;
use serde_json::Value;
use std::fmt;
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionError {
    /// A known credential value or fragment would appear in the output.
    SecretFragment,
    /// The candidate output is too large to screen safely.
    TooLarge,
    /// The supplied secret material is absent or unbounded.
    Malformed,
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ProjectionError::SecretFragment => "output contains a known credential fragment",
            ProjectionError::TooLarge => "output exceeds its screening bound",
            ProjectionError::Malformed => "credential material is absent or unbounded",
        })
    }
}
impl std::error::Error for ProjectionError {}

fn carries_credential(value: &str, text: &str) -> bool {
    if text.contains(value) {
        return true;
    }
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < SECRET_FRAGMENT_BYTES {
        return false;
    }
    chars
        .windows(SECRET_FRAGMENT_BYTES)
        .any(|window| text.contains(&window.iter().collect::<String>()))
}

/// A credential's known values (the secret and screened account metadata).
/// Zeroized on drop; never `Debug`/`Serialize`/`Clone`; it exposes only
/// screening.
pub struct KnownSecretV1 {
    values: Vec<Zeroizing<String>>,
}

impl KnownSecretV1 {
    pub fn new(secret: &[u8], metadata: &[String]) -> Result<Self, ProjectionError> {
        if secret.is_empty() || secret.len() > MAX_RAW_CREDENTIAL_BYTES {
            return Err(ProjectionError::Malformed);
        }
        let mut values = vec![Zeroizing::new(String::from_utf8_lossy(secret).into_owned())];
        for item in metadata {
            if item.is_empty() || item.len() > MAX_RAW_CREDENTIAL_BYTES {
                return Err(ProjectionError::Malformed);
            }
            values.push(Zeroizing::new(item.clone()));
        }
        Ok(Self { values })
    }

    fn any_carried(&self, text: &str) -> bool {
        self.values
            .iter()
            .any(|value| carries_credential(value.as_str(), text))
    }

    /// Screen one string with the model-result byte bound.
    pub fn screen_text(&self, text: &str) -> Result<(), ProjectionError> {
        if text.len() > MAX_MODEL_TEXT_BYTES {
            return Err(ProjectionError::TooLarge);
        }
        if self.any_carried(text) {
            return Err(ProjectionError::SecretFragment);
        }
        Ok(())
    }

    /// Walk a decoded JSON value exactly as the native screen does.
    pub fn screen_json(&self, value: &Value) -> Result<(), ProjectionError> {
        if self.json_carries(value) {
            return Err(ProjectionError::SecretFragment);
        }
        Ok(())
    }

    fn json_carries(&self, value: &Value) -> bool {
        match value {
            Value::String(text) => self.any_carried(text),
            Value::Object(fields) => fields
                .iter()
                .any(|(key, item)| self.any_carried(key) || self.json_carries(item)),
            Value::Array(items) => items.iter().any(|item| self.json_carries(item)),
            Value::Number(number) => self.any_carried(&number.to_string()),
            Value::Bool(boolean) => self.any_carried(if *boolean { "true" } else { "false" }),
            Value::Null => self.any_carried("null"),
        }
    }

    /// Screen a provider/result text before it is serialized.
    pub fn screen_model_text(&self, text: &str) -> Result<(), ProjectionError> {
        self.screen_text(text)
    }
}
