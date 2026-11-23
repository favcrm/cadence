//! The one canonical material digest, extracted from the existing
//! `src/store/app_runs.rs` algorithm so native app material, Connection
//! dispatches and the protected ledger all share one canonicalizer.
//!
//! `material_digest(&value)` is BYTE-IDENTICAL to the pre-existing app
//! material digest: `sha256("cadence-app-material-v1\n" + compact JSON with
//! object keys recursively sorted and array order preserved)` rendered as
//! `sha256:<lowercase hex>`. No Unicode normalization is applied.
//!
//! Serialization walks borrowed values through a canonical `Serialize`
//! wrapper and streams straight into SHA-256; no `Value` tree is cloned and
//! bytes are only materialized when [`canonical_bytes`] is explicitly called.
use crate::connection_dispatch::primitives::Digest;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;
use std::fmt;
use std::io::Write;

pub const APP_MATERIAL_DOMAIN: &str = "cadence-app-material-v1";
pub const CONNECTION_DISPATCH_DOMAIN: &str = "cadence-connection-dispatch-v1";
pub const PROTECTED_LEDGER_DOMAIN: &str = "cadence-protected-ledger-v1";
pub const MAX_JSON_DEPTH: usize = 16;

/// Borrowed canonical JSON: object keys sorted, array order preserved. The
/// produced bytes equal the historical `Value`-rebuild implementation exactly.
pub struct Canonical<'a>(pub &'a Value);

impl Serialize for Canonical<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Object(fields) => {
                let mut keys: Vec<&String> = fields.keys().collect();
                keys.sort_unstable();
                let mut map = serializer.serialize_map(Some(keys.len()))?;
                for key in keys {
                    let value = fields.get(key).expect("a collected key is present");
                    map.serialize_entry(key, &Canonical(value))?;
                }
                map.end()
            }
            Value::Array(items) => {
                let mut sequence = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    sequence.serialize_element(&Canonical(item))?;
                }
                sequence.end()
            }
            other => other.serialize(serializer),
        }
    }
}

/// Debug/tooling helper: a fully owned canonical value. Prefer
/// [`material_digest`] on the hot path.
pub fn canonical_value(value: &Value) -> Value {
    match value {
        Value::Object(fields) => {
            let ordered: std::collections::BTreeMap<_, _> = fields
                .iter()
                .map(|(key, value)| (key.clone(), canonical_value(value)))
                .collect();
            Value::Object(ordered.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_value).collect()),
        other => other.clone(),
    }
}

struct HashWriter(Sha256);

impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Streaming digest over `<domain>\n<canonical compact JSON>`.
pub fn material_digest_for_domain(domain: &str, value: &Value) -> String {
    let mut writer = HashWriter(Sha256::new());
    writer
        .write_all(domain.as_bytes())
        .expect("hash writer never blocks");
    writer.write_all(b"\n").expect("hash writer never blocks");
    serde_json::to_writer(&mut writer, &Canonical(value))
        .expect("canonical JSON of a bounded value");
    format!("sha256:{:x}", writer.0.finalize())
}

/// The pre-existing app material digest. Drop-in for
/// `crate::store::app_runs::material_digest`.
pub fn material_digest(value: &Value) -> String {
    material_digest_for_domain(APP_MATERIAL_DOMAIN, value)
}

/// A typed digest value from the same material.
pub fn material_digest_value(domain: &str, value: &Value) -> Digest {
    Digest::try_from(material_digest_for_domain(domain, value))
        .expect("a material digest is always canonical")
}

/// Allocate the exact canonical bytes; only callers that need the bytes pay
/// for them.
pub fn canonical_bytes(domain: &str, value: &Value) -> Vec<u8> {
    let mut out = Vec::with_capacity(domain.len() + 1 + 64);
    out.extend_from_slice(domain.as_bytes());
    out.push(b'\n');
    serde_json::to_writer(&mut out, &Canonical(value)).expect("bounded canonical JSON");
    out
}

/// Exact-byte digest: `sha256:<lowercase hex>`.
pub fn exact_byte_digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    TooLarge,
    TooDeep,
    Ambiguous,
    Invalid(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::TooLarge => f.write_str("payload exceeds its byte limit"),
            ParseError::TooDeep => f.write_str("JSON nesting exceeds 16 levels"),
            ParseError::Ambiguous => {
                f.write_str("payload is not unambiguous UTF-8 JSON (duplicate or bad key)")
            }
            ParseError::Invalid(message) => write!(f, "payload is invalid: {message}"),
        }
    }
}
impl std::error::Error for ParseError {}

/// Maximum object/array nesting outside string literals.
pub fn json_depth(bytes: &[u8]) -> usize {
    let mut depth = 0usize;
    let mut max = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for byte in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                max = max.max(depth);
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

/// `serde_json::Value` with duplicate object keys refused. One textual parse,
/// no second YAML pass; depth is bounded by [`json_depth`] before parsing and
/// by serde_json's own recursion limit during it.
struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueVisitor)
    }
}

struct UniqueVisitor;

impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = UniqueValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an unambiguous JSON value")
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Bool(value)))
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(value.into())))
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(value.into())))
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        let number = serde_json::Number::from_f64(value)
            .ok_or_else(|| de::Error::custom("JSON number is not finite"))?;
        Ok(UniqueValue(Value::Number(number)))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value.to_string())))
    }
    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value)))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }
    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }
    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        UniqueValue::deserialize(deserializer)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = sequence.next_element::<UniqueValue>()? {
            items.push(item.0);
        }
        Ok(UniqueValue(Value::Array(items)))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut fields = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(de::Error::custom("duplicate object key"));
            }
            let value: UniqueValue = map.next_value()?;
            fields.insert(key, value.0);
        }
        Ok(UniqueValue(Value::Object(fields)))
    }
}

/// Strict single-document decode: byte bound, UTF-8, depth <= 16, duplicate
/// mapping keys, no trailing bytes, no coercion, and unknown fields refused by
/// the target type's derives.
pub fn parse_strict<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    max_bytes: usize,
) -> Result<T, ParseError> {
    if bytes.len() > max_bytes {
        return Err(ParseError::TooLarge);
    }
    if json_depth(bytes) > MAX_JSON_DEPTH {
        return Err(ParseError::TooDeep);
    }
    let value: UniqueValue = serde_json::from_slice(bytes).map_err(|error| {
        let message = error.to_string();
        if message.contains("duplicate object key")
            || matches!(
                error.classify(),
                serde_json::error::Category::Syntax | serde_json::error::Category::Eof
            )
        {
            ParseError::Ambiguous
        } else {
            ParseError::Invalid(message)
        }
    })?;
    serde_json::from_value(value.0).map_err(|error| ParseError::Invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn app_digest_keeps_the_existing_bytes() {
        // The pre-CAD-1085 algorithm inline: BTreeMap-sorted JSON under the
        // literal prefix, then sha256.
        let value = json!({"b": [3, 2, 1], "a": {"z": 1, "y": "é"}});
        let sorted = json!({"a": {"y": "é", "z": 1}, "b": [3, 2, 1]});
        let expected = format!(
            "sha256:{:x}",
            Sha256::digest(format!("cadence-app-material-v1\n{sorted}").as_bytes())
        );
        assert_eq!(material_digest(&value), expected);
        assert_eq!(canonical_value(&value), sorted);
        assert_eq!(
            canonical_bytes(APP_MATERIAL_DOMAIN, &value),
            format!("cadence-app-material-v1\n{sorted}").into_bytes()
        );
    }

    #[test]
    fn domains_and_byte_digests_are_separated() {
        let value = json!({"v": 1});
        assert_ne!(
            material_digest_for_domain(CONNECTION_DISPATCH_DOMAIN, &value),
            material_digest_for_domain(PROTECTED_LEDGER_DOMAIN, &value)
        );
        assert_eq!(exact_byte_digest(b"abc").len(), 71);
    }

    #[test]
    fn strict_parse_refuses_depth_duplicates_and_trailing() {
        let deep = "[".repeat(17) + &"]".repeat(17);
        assert_eq!(json_depth(deep.as_bytes()), 17);
        assert!(matches!(
            parse_strict::<Value>(deep.as_bytes(), 1 << 20),
            Err(ParseError::TooDeep)
        ));
        assert!(matches!(
            parse_strict::<Value>(br#"{"a":1,"a":2}"#, 1024),
            Err(ParseError::Ambiguous)
        ));
        assert!(matches!(
            parse_strict::<Value>(br#"{"a":1} {}"#, 1024),
            Err(ParseError::Ambiguous) | Err(ParseError::Invalid(_))
        ));
        assert!(matches!(
            parse_strict::<Value>(b"{\"a\":1}", 4),
            Err(ParseError::TooLarge)
        ));
    }
}
