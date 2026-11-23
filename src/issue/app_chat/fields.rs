//! Primitive field, identifier, bounds, and i18n validators for `issue::app_chat`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::HashSet;

use serde_json::Value;

use super::{grammar, FILE, MAX_DEPTH, MAX_NODES};
use crate::error::{Error, Result};

pub(super) fn fail<T>(path: &str, message: impl std::fmt::Display) -> Result<T> {
    Err(Error::rejected(format!("{FILE}: {path}: {message}")))
}

fn is_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}' | '\u{2028}' | '\u{2029}')
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    s.len() <= 64
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

pub(super) fn is_match(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("cadence_") else {
        return false;
    };
    let mut chars = rest.chars();
    rest.len() <= 48
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

pub(super) fn is_screen(s: &str) -> bool {
    s.strip_prefix("screen:")
        .is_some_and(crate::issue::app_screen_pkg::valid_tag)
}

/// Bounds first, as the client does: depth, node count and forbidden keys
/// at every level, so the shape checks never run over a hostile shell.
pub(super) fn scan(value: &Value, path: &str, nodes: &mut usize, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return fail(path, "nested too deeply");
    }
    *nodes += 1;
    if *nodes > MAX_NODES {
        return fail(path, "too many nodes");
    }
    match value {
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                scan(item, &format!("{path}.{i}"), nodes, depth + 1)?;
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                if grammar().forbidden.contains(key) {
                    return fail(path, format!("forbidden descriptor key: {key}"));
                }
                scan(item, &format!("{path}.{key}"), nodes, depth + 1)?;
            }
        }
        Value::Number(n) if n.as_f64().is_none_or(|f| !f.is_finite()) => {
            return fail(path, "expected plain JSON data")
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn object<'a>(
    value: &'a Value,
    path: &str,
    allowed: &[&str],
) -> Result<&'a serde_json::Map<String, Value>> {
    let Some(map) = value.as_object() else {
        return fail(path, "expected an object");
    };
    if let Some(key) = map.keys().find(|k| !allowed.contains(&k.as_str())) {
        return fail(path, format!("unknown key: {key}"));
    }
    Ok(map)
}

pub(super) fn text(value: Option<&Value>, path: &str, max: usize) -> Result<String> {
    let Some(s) = value.and_then(Value::as_str) else {
        return fail(path, "expected a non-empty string");
    };
    if s.is_empty() {
        return fail(path, "expected a non-empty string");
    }
    if s.encode_utf16().count() > max {
        return fail(path, format!("string is longer than {max} characters"));
    }
    if s.chars().any(is_control) {
        return fail(path, "control characters are not allowed");
    }
    Ok(s.to_string())
}

pub(super) fn ident(value: Option<&Value>, path: &str) -> Result<String> {
    let s = text(value, path, 64)?;
    if !is_ident(&s) {
        return fail(path, format!("unsafe identifier: {s:?}"));
    }
    Ok(s)
}

pub(super) fn array<'a>(value: Option<&'a Value>, path: &str, max: usize) -> Result<&'a [Value]> {
    let Some(value) = value else { return Ok(&[]) };
    let Some(items) = value.as_array() else {
        return fail(path, "expected an array");
    };
    if items.len() > max {
        return fail(path, format!("more than {max} entries"));
    }
    Ok(items)
}

pub(super) fn prompts(value: Option<&Value>, path: &str) -> Result<()> {
    for (i, p) in array(value, path, 3)?.iter().enumerate() {
        text(Some(p), &format!("{path}.{i}"), 80)?;
    }
    Ok(())
}

pub(super) fn unique(
    seen: &mut HashSet<String>,
    key: String,
    path: &str,
    what: &str,
) -> Result<()> {
    if !seen.insert(key.clone()) {
        return fail(path, format!("duplicate {what}: {key}"));
    }
    Ok(())
}

fn i18n_locales(
    value: &Value,
    path: &str,
    allowed: &[&str],
    mut check: impl FnMut(&serde_json::Map<String, Value>, &str) -> Result<()>,
) -> Result<()> {
    let Some(map) = value.as_object() else {
        return fail(path, "expected an object");
    };
    if map.is_empty() || map.len() > cadence_core::app_screen_decl::MAX_I18N_LOCALES {
        return fail(
            path,
            format!(
                "expected 1 to {} locales",
                cadence_core::app_screen_decl::MAX_I18N_LOCALES
            ),
        );
    }
    let mut seen_locales = HashSet::new();
    for (locale, entry) in map {
        if !cadence_core::app_screen_decl::valid_locale_tag(locale) {
            return fail(path, format!("invalid locale tag: {locale:?}"));
        }
        if !seen_locales.insert(locale.to_ascii_lowercase()) {
            return fail(path, format!("duplicate locale tag: {locale:?}"));
        }
        let lp = format!("{path}.{locale}");
        let entry_map = object(entry, &lp, allowed)?;
        if entry_map.is_empty() {
            return fail(&lp, "expected at least one localized field");
        }
        check(entry_map, &lp)?;
    }
    Ok(())
}

fn i18n_text(value: Option<&Value>, path: &str, max: usize) -> Result<String> {
    let s = text(value, path, max)?;
    if s.trim().is_empty() {
        return fail(path, "expected a non-blank string");
    }
    if s.chars().any(|c| matches!(c, '\u{80}'..='\u{9f}')) {
        return fail(path, "control characters are not allowed");
    }
    Ok(s)
}

fn i18n_prompts(value: Option<&Value>, path: &str) -> Result<()> {
    let items = array(value, path, 3)?;
    if items.is_empty() {
        return fail(path, "expected at least 1 entry");
    }
    for (i, p) in items.iter().enumerate() {
        i18n_text(Some(p), &format!("{path}.{i}"), 80)?;
    }
    Ok(())
}

pub(super) fn label_i18n(parent: &serde_json::Map<String, Value>, path: &str) -> Result<()> {
    let Some(val) = parent.get("i18n") else {
        return Ok(());
    };
    i18n_locales(val, &format!("{path}.i18n"), &["label"], |entry, lp| {
        i18n_text(entry.get("label"), &format!("{lp}.label"), 40)?;
        Ok(())
    })
}

pub(super) fn context_i18n(parent: &serde_json::Map<String, Value>, path: &str) -> Result<()> {
    let Some(val) = parent.get("i18n") else {
        return Ok(());
    };
    i18n_locales(
        val,
        &format!("{path}.i18n"),
        &["label", "prompts"],
        |entry, lp| {
            if entry.contains_key("label") {
                i18n_text(entry.get("label"), &format!("{lp}.label"), 40)?;
            }
            if entry.contains_key("prompts") {
                i18n_prompts(entry.get("prompts"), &format!("{lp}.prompts"))?;
            }
            Ok(())
        },
    )
}

pub(super) fn card_i18n(parent: &serde_json::Map<String, Value>, path: &str) -> Result<()> {
    let Some(val) = parent.get("i18n") else {
        return Ok(());
    };
    i18n_locales(
        val,
        &format!("{path}.i18n"),
        &["title", "text"],
        |entry, lp| {
            if entry.contains_key("title") {
                i18n_text(entry.get("title"), &format!("{lp}.title"), 80)?;
            }
            if entry.contains_key("text") {
                i18n_text(entry.get("text"), &format!("{lp}.text"), 240)?;
            }
            Ok(())
        },
    )
}
