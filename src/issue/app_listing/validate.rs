//! YAML field and price-key validation helpers for `issue::app_listing`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use crate::error::{Error, Result};

/// `app.md` keys a price would hide behind (decision 8): refused
/// anywhere in the frontmatter mapping, at any depth.
const PRICE_KEYS: &[&str] = &["cost", "price", "pricing"];

fn check_plain_text(
    value: &serde_yaml::Value,
    what: &str,
    max: usize,
    allow_newlines: bool,
) -> Result<String> {
    let Some(text) = value.as_str() else {
        return Err(Error::rejected(format!("listing {what} is a plain string")));
    };
    let text = text.trim();
    if text.is_empty()
        || text.encode_utf16().count() > max
        || text.chars().any(|c| {
            (c.is_control() && (!allow_newlines || c != '\n'))
                || matches!(c, '\u{2028}' | '\u{2029}')
        })
        || text.contains('<')
        || text.contains('>')
        || text.contains("](")
        || text.starts_with('#')
        || text.starts_with("http://")
        || text.starts_with("https://")
    {
        return Err(Error::rejected(format!(
            "listing {what} is ≤{max} chars of plain text — no markup, links or controls"
        )));
    }
    Ok(text.to_string())
}

pub(super) fn plain_text(value: &serde_yaml::Value, what: &str, max: usize) -> Result<String> {
    check_plain_text(value, what, max, false)
}

pub(super) fn plain_multiline_text(
    value: &serde_yaml::Value,
    what: &str,
    max: usize,
) -> Result<String> {
    check_plain_text(value, what, max, true)
}

/// `assets/<leaf>.svg` — flat, tag-shaped stem, real file under the
/// bundle's `assets/` (paths outside are refused).
pub(super) fn asset_path(value: &serde_yaml::Value, what: &str, max_kb: u64) -> Result<String> {
    let raw = value
        .as_str()
        .ok_or_else(|| Error::rejected(format!("listing {what} is an assets/ path")))?;
    if raw.len() > 128 {
        return Err(Error::rejected(format!("listing {what} path is too long")));
    }
    let Some(leaf) = raw.strip_prefix("assets/") else {
        return Err(Error::rejected(format!(
            "listing {what} '{raw}' — assets stay under assets/"
        )));
    };
    let Some(stem) = leaf.strip_suffix(".svg") else {
        return Err(Error::rejected(format!(
            "listing {what} '{raw}' — assets are flat *.svg files"
        )));
    };
    if stem.is_empty()
        || stem.contains('/')
        || !stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(Error::rejected(format!(
            "listing {what} '{raw}' — the file name is [A-Za-z0-9_-]+.svg"
        )));
    }
    let _ = max_kb;
    Ok(raw.to_string())
}

pub(super) fn known_keys(map: &serde_yaml::Mapping, allowed: &[&str], what: &str) -> Result<()> {
    for key in map.keys() {
        let Some(k) = key.as_str() else {
            return Err(Error::rejected(format!(
                "listing {what} has a non-string key"
            )));
        };
        if !allowed.contains(&k) {
            return Err(Error::rejected(format!(
                "listing {what} key '{k}' is unknown — v1 knows {}",
                allowed.join(", ")
            )));
        }
    }
    Ok(())
}

pub(super) fn yget<'a>(map: &'a serde_yaml::Mapping, key: &str) -> Option<&'a serde_yaml::Value> {
    map.get(serde_yaml::Value::String(key.to_string()))
}

pub(super) fn seq<'a>(
    map: &'a serde_yaml::Mapping,
    key: &str,
    what: &str,
    max: usize,
) -> Result<Vec<&'a serde_yaml::Value>> {
    match yget(map, key) {
        None | Some(serde_yaml::Value::Null) => Ok(vec![]),
        Some(serde_yaml::Value::Sequence(items)) => {
            if items.len() > max {
                return Err(Error::rejected(format!(
                    "listing {what} carries more than {max} entries"
                )));
            }
            Ok(items.iter().collect())
        }
        Some(_) => Err(Error::rejected(format!("listing {what} is a list"))),
    }
}

/// Refuse `cost`/`price`/`pricing` at any depth of a YAML value
/// (decision 8) — including `listing` and every other frontmatter key.
pub(crate) fn refuse_price_keys(value: &serde_yaml::Value) -> Result<()> {
    match value {
        serde_yaml::Value::Mapping(map) => {
            for (k, v) in map {
                if let Some(key) = k.as_str() {
                    if PRICE_KEYS.contains(&key) {
                        return Err(Error::rejected(format!(
                            "app.md key '{key}': prices are not shown (CAD-1129 decision 8) — \
                             spend safety lives in the host's frozen quote, the charge \
                             ceiling and the global per-call bound"
                        )));
                    }
                }
                refuse_price_keys(v)?;
            }
            Ok(())
        }
        serde_yaml::Value::Sequence(items) => {
            for item in items {
                refuse_price_keys(item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
