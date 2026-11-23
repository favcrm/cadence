//! Strict JSON for every private owner frame, obligation and RPC argument.
//!
//! The design requires: reject duplicate/unknown keys, coercion, malformed
//! UTF-8, trailing JSON and depth > 16. `serde_json` silently collapses
//! duplicate keys, so a small byte scanner runs first and refuses a duplicate
//! object key, an escaped object key, non-UTF-8 and an over-deep document.
//! Everything else parses through `serde_json`; field validation is explicit.

use crate::error::{Error, Result};

/// Parse one strict JSON document.
pub(crate) fn parse(bytes: &[u8]) -> Result<serde_json::Value> {
    if bytes.is_empty() {
        return Err(Error::rejected("empty JSON document"));
    }
    scan(bytes)?;
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| Error::rejected("invalid JSON"))?;
    Ok(value)
}

/// A JSON object with exactly `names` — deny unknown, duplicate and missing.
pub(crate) fn object<'a>(
    v: &'a serde_json::Value,
    names: &[&str],
) -> Result<&'a serde_json::Map<String, serde_json::Value>> {
    let m = v
        .as_object()
        .ok_or_else(|| Error::rejected("expected a JSON object"))?;
    if m.len() != names.len()
        || !names.iter().all(|n| m.contains_key(*n))
        || !m.keys().all(|k| names.contains(&k.as_str()))
    {
        return Err(Error::rejected(
            "object fields are not the exact expected set",
        ));
    }
    Ok(m)
}

/// A JSON object with the required `names` and at most the `optional` ones.
pub(crate) fn object_with<'a>(
    v: &'a serde_json::Value,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a serde_json::Map<String, serde_json::Value>> {
    let m = v
        .as_object()
        .ok_or_else(|| Error::rejected("expected a JSON object"))?;
    if !required.iter().all(|n| m.contains_key(*n))
        || !m
            .keys()
            .all(|k| required.contains(&k.as_str()) || optional.contains(&k.as_str()))
    {
        return Err(Error::rejected("object fields are not the expected set"));
    }
    Ok(m)
}

pub(crate) fn string<'a>(
    m: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<&'a str> {
    m.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::rejected(format!("'{key}' is missing or not a string")))
}

pub(crate) fn boolean(m: &serde_json::Map<String, serde_json::Value>, key: &str) -> Result<bool> {
    m.get(key)
        .and_then(|v| v.as_bool())
        .ok_or_else(|| Error::rejected(format!("'{key}' is missing or not a boolean")))
}

/// A JS-safe non-negative integer (`<= 2^53 - 1`); floats/strings refuse.
pub(crate) fn safe_u64(m: &serde_json::Map<String, serde_json::Value>, key: &str) -> Result<u64> {
    match m.get(key).and_then(|v| v.as_u64()) {
        Some(n) if n <= super::MAX_SAFE_INT => Ok(n),
        _ => Err(Error::rejected(format!(
            "'{key}' is missing or not a safe non-negative integer"
        ))),
    }
}

/// A positive JS-safe integer (revisions/generations).
pub(crate) fn positive_u64(
    m: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<u64> {
    let n = safe_u64(m, key)?;
    if n == 0 {
        return Err(Error::rejected(format!("'{key}' must be positive")));
    }
    Ok(n)
}

pub(crate) fn array<'a>(
    m: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<&'a Vec<serde_json::Value>> {
    m.get(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::rejected(format!("'{key}' is missing or not an array")))
}

/// Validate a `sha256:` + 64 lowercase hex digest.
pub(crate) fn digest(s: &str) -> Result<()> {
    let hex = s
        .strip_prefix("sha256:")
        .ok_or_else(|| Error::rejected("digest must be sha256:<64 lowercase hex>"))?;
    if !is_lower_hex(hex, 64) {
        return Err(Error::rejected("digest is not 64 lowercase hex"));
    }
    Ok(())
}

pub(crate) fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn hex_bytes(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::rejected("not even-length hexadecimal"));
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let nibble = |b: u8| -> u8 {
        match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => 0,
        }
    };
    let mut i = 0;
    while i + 1 < bytes.len() {
        out.push((nibble(bytes[i]) << 4) | nibble(bytes[i + 1]));
        i += 2;
    }
    Ok(out)
}

pub(crate) fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    for (i, &c) in b.iter().enumerate() {
        if matches!(i, 8 | 13 | 18 | 23) {
            if c != b'-' {
                return false;
            }
        } else if !c.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

fn reject(msg: &str) -> Error {
    Error::rejected(msg)
}

#[derive(Default)]
struct Frame {
    is_object: bool,
    expect_key: bool,
    keys: std::collections::BTreeSet<String>,
}

/// Byte-level checks `serde_json` cannot express: UTF-8, depth, duplicate or
/// escaped object keys. Escaped keys are refused outright — every key in this
/// protocol is plain ASCII.
fn scan(bytes: &[u8]) -> Result<()> {
    if std::str::from_utf8(bytes).is_err() {
        return Err(reject("JSON is not valid UTF-8"));
    }
    let mut stack: Vec<Frame> = Vec::new();
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                depth += 1;
                if depth > 16 {
                    return Err(reject("JSON nesting exceeds 16"));
                }
                stack.push(Frame {
                    is_object: true,
                    expect_key: true,
                    keys: Default::default(),
                });
                i += 1;
            }
            b'[' => {
                depth += 1;
                if depth > 16 {
                    return Err(reject("JSON nesting exceeds 16"));
                }
                stack.push(Frame {
                    is_object: false,
                    expect_key: false,
                    keys: Default::default(),
                });
                i += 1;
            }
            b'}' | b']' => {
                stack.pop();
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'"' => {
                let (literal, next) = read_string(bytes, i)?;
                if let Some(top) = stack.last_mut() {
                    if top.is_object && top.expect_key {
                        if literal.contains('\\') {
                            return Err(reject("escaped JSON object key is refused"));
                        }
                        if !top.keys.insert(literal) {
                            return Err(reject("duplicate JSON object key"));
                        }
                        top.expect_key = false;
                    }
                }
                i = next;
            }
            b':' => {
                if let Some(top) = stack.last_mut() {
                    if top.is_object {
                        top.expect_key = false;
                    }
                }
                i += 1;
            }
            b',' => {
                if let Some(top) = stack.last_mut() {
                    if top.is_object {
                        top.expect_key = true;
                    }
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    Ok(())
}

/// Scan one JSON string starting at `start` (which must be `"`). Returns the
/// raw literal without the quotes and the index just past the closing quote.
fn read_string(bytes: &[u8], start: usize) -> Result<(String, usize)> {
    let mut i = start + 1;
    let mut escaped = false;
    let mut out: Vec<u8> = Vec::new();
    while i < bytes.len() {
        let b = bytes[i];
        if escaped {
            out.push(b'\\');
            out.push(b);
            escaped = false;
            i += 1;
            continue;
        }
        match b {
            b'\\' => {
                escaped = true;
                i += 1;
            }
            b'"' => {
                // `from_utf8` already ran; a literal is a UTF-8 substring.
                let literal = String::from_utf8(out).map_err(|_| reject("bad string"))?;
                return Ok((literal, i + 1));
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    Err(reject("unterminated JSON string"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_duplicate_keys_serde_would_collapse() {
        assert!(parse(br#"{"a":1,"a":2}"#).is_err());
        assert!(parse(br#"{"a":{"b":1,"b":2}}"#).is_err());
    }

    #[test]
    fn accepts_same_key_in_sibling_objects() {
        assert!(parse(br#"{"a":{"k":1},"b":{"k":2}}"#).is_ok());
    }

    #[test]
    fn rejects_depth_and_trailing() {
        let deep = "[".repeat(17) + &"]".repeat(17);
        assert!(parse(deep.as_bytes()).is_err());
        assert!(parse(br#"{"a":1} {}"#).is_err());
    }

    #[test]
    fn rejects_escaped_and_duplicate_string_values_are_not_keys() {
        assert!(parse(br#"{"a":"x","b":"y"}"#).is_ok());
        assert!(parse(br#"{"a":"x","a":"y"}"#).is_err());
        assert!(parse(br#"{"\u0061":1}"#).is_err());
    }

    #[test]
    fn digest_and_uuid_validators() {
        let good = format!("sha256:{}", "a".repeat(64));
        assert!(digest(&good).is_ok());
        assert!(digest(&format!("sha256:{}", "A".repeat(64))).is_err());
        assert!(is_uuid("123e4567-e89b-42d3-a456-426614174000"));
        assert!(!is_uuid("123e4567e89b42d3a4564266141740000"));
    }

    #[test]
    fn hex_bytes_round_trip() {
        assert_eq!(hex_bytes("00ff10").unwrap(), vec![0x00, 0xff, 0x10]);
    }

    #[test]
    fn object_exactness() {
        let v = json!({"a":1,"b":2});
        assert!(object(&v, &["a", "b"]).is_ok());
        assert!(object(&v, &["a"]).is_err());
        assert!(object(&json!({"a":1,"c":2}), &["a", "b"]).is_err());
    }
}
