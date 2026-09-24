//! The one filter grammar every `ls`/`list` command shares (CAD-437).
//!
//! ```text
//! - A value flag repeats and comma-joins — `--status doing --status
//!   review` and `--status doing,review` both match ANY of the values.
//! - Different flags AND — a row must satisfy every flag given.
//! - An unknown value is an error naming the valid set.
//! - `--json` prints rows as JSON; `--limit N` caps the count;
//!   `--sort KEY` orders rows (`-KEY` for descending); `--fields a,b`
//!   keeps only those keys in each --json row.
//! ```
//!
//! The one documented exception is `issue ls --tag`: its values must all
//! be present on the issue (all-of), matching the board API's `tag=a,b`.
//!
//! This module owns the helpers shared by every handler; the grammar
//! text is printed in each list command's help via [`GRAMMAR`].

use serde_json::Value;

use crate::error::{Error, Result};

/// The paragraph printed in every list command's long help.
pub const GRAMMAR: &str = "\
Filter grammar — the same on every `ls`/`list` command:
  · a value flag repeats and comma-joins (`--status doing --status
    review` = `--status doing,review`) and matches ANY of its values;
  · different flags AND — a row must satisfy every flag given;
  · an unknown value is an error naming the valid set;
  · `--json` prints the rows as JSON; `--limit N` caps the count;
    `--sort KEY` orders rows (`-KEY` for descending); `--fields a,b`
    keeps only those keys in each --json row.
  `issue ls --tag` is the one exception: every tag must be present.";

/// `set` empty or `value` is one of its members — the any-of test every
/// filter field uses. `None` never matches a non-empty set.
pub fn any_of(set: &[String], value: Option<&str>) -> bool {
    set.is_empty() || set.iter().any(|v| Some(v.as_str()) == value)
}

/// The "unknown value names the valid set" error.
pub fn unknown(flag: &str, value: &str, valid: &[&str]) -> Error {
    Error::rejected(format!(
        "Unknown --{flag} '{value}' — one of {}",
        valid.join(" ")
    ))
}

/// The union vocabulary check for a repeatable value flag: every value
/// must be in `valid`.
pub fn check_set(flag: &str, values: &[String], valid: &[&str]) -> Result<()> {
    for v in values {
        if !valid.contains(&v.as_str()) {
            return Err(unknown(flag, v, valid));
        }
    }
    Ok(())
}

/// Parse a `--since`/`--until` bound: `30m`/`24h`/`7d` lookback,
/// `YYYY-MM-DDTHH:MM:SSZ` or a bare `YYYY-MM-DD` date, or a raw epoch.
/// The same grammar `summary::parse_since` accepts.
pub fn parse_time(flag: &str, s: &str, now: i64) -> Result<i64> {
    if let Ok(secs) = s.parse::<i64>() {
        return Ok(secs);
    }
    if let Some(t) = crate::issue::time::parse_iso(s)
        .or_else(|| crate::issue::time::parse_iso(&format!("{s}T00:00:00Z")))
    {
        return Ok(t);
    }
    let unit = match s.chars().last() {
        Some('m') => 60,
        Some('h') => 3600,
        Some('d') => 86_400,
        _ => 0,
    };
    if unit > 0 {
        if let Ok(n) = s[..s.len() - 1].parse::<i64>() {
            if n >= 0 {
                return Ok(now - n * unit);
            }
        }
    }
    Err(Error::rejected(format!(
        "--{flag} '{s}' is not a time — try 24h, 7d, YYYY-MM-DD or an epoch"
    )))
}

/// Sort JSON row objects by `spec` (`key` ascending, `-key`
/// descending). `keys` pairs a user-facing name with the row path it
/// reads — `"id"` or a dotted path like `"work.stage.id"`. Numbers
/// compare numerically, strings with `board::natural_key` (X-2 before
/// X-10); missing or null values sort last ascending / first
/// descending, and `tie` (the row's identity key — `id`, `alias`, …)
/// breaks ties so the order is always total.
pub fn sort_rows(rows: &mut [Value], spec: &str, keys: &[(&str, &str)], tie: &str) -> Result<()> {
    let (key, desc) = match spec.strip_prefix('-') {
        Some(k) => (k, true),
        None => (spec, false),
    };
    let Some((_, path)) = keys.iter().find(|(name, _)| *name == key) else {
        let names: Vec<&str> = keys.iter().map(|(n, _)| *n).collect();
        return Err(unknown("sort", spec, &names));
    };
    rows.sort_by(|a, b| {
        let ord = cmp_key(at_path(a, path), at_path(b, path))
            .then_with(|| cmp_key(a.get(tie), b.get(tie)));
        if desc {
            ord.reverse()
        } else {
            ord
        }
    });
    Ok(())
}

/// `row["a"]["b"]["c"]` for a `a.b.c` path; `None` when any step is
/// missing.
fn at_path<'a>(row: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = row;
    for part in path.split('.') {
        cur = cur.get(part)?;
    }
    Some(cur)
}

fn cmp_key(a: Option<&Value>, b: Option<&Value>) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a, b) {
        (Some(Value::Number(x)), Some(Value::Number(y))) => {
            x.as_f64().partial_cmp(&y.as_f64()).unwrap_or(Equal)
        }
        (Some(Value::String(x)), Some(Value::String(y))) => {
            crate::issue::board::natural_key(x).cmp(&crate::issue::board::natural_key(y))
        }
        (Some(Value::Bool(x)), Some(Value::Bool(y))) => x.cmp(y),
        (Some(_), None) | (Some(_), Some(Value::Null)) => Less,
        (None, Some(_)) | (Some(Value::Null), Some(_)) => Greater,
        _ => Equal,
    }
}

/// Keep only the named keys in each row object (`--fields`). A name
/// absent from every row is an error listing the keys the rows carry.
/// An empty row set cannot be validated and passes through.
pub fn apply_fields(rows: &mut [Value], fields: &[String]) -> Result<()> {
    if fields.is_empty() {
        return Ok(());
    }
    let mut known: Vec<String> = Vec::new();
    for row in rows.iter() {
        if let Some(obj) = row.as_object() {
            for k in obj.keys() {
                if !known.contains(k) {
                    known.push(k.clone());
                }
            }
        }
    }
    // An empty page has no rows to read keys from — nothing to prove a
    // name unknown, so the projection is a no-op rather than an error.
    if known.is_empty() {
        return Ok(());
    }
    for f in fields {
        if !known.contains(f) {
            return Err(Error::rejected(format!(
                "Unknown --fields '{f}' — row keys: {}",
                known.join(" ")
            )));
        }
    }
    for row in rows.iter_mut() {
        if let Some(obj) = row.as_object_mut() {
            obj.retain(|k, _| fields.iter().any(|f| f == k));
        }
    }
    Ok(())
}

/// `--limit`: keep the first `n` rows.
pub fn apply_limit<T>(rows: &mut Vec<T>, limit: Option<usize>) {
    if let Some(n) = limit {
        rows.truncate(n);
    }
}

/// `--fields` selects JSON row keys — reject it on table output rather
/// than silently doing nothing.
pub fn fields_need_json(fields: &[String], json: bool) -> Result<()> {
    if !fields.is_empty() && !json {
        return Err(Error::rejected(
            "--fields applies to --json output — add --json",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn any_of_empty_matches_everything() {
        assert!(any_of(&[], None));
        assert!(any_of(&[], Some("x")));
        assert!(any_of(&strs(&["a", "b"]), Some("b")));
        assert!(!any_of(&strs(&["a"]), Some("b")));
        assert!(!any_of(&strs(&["a"]), None));
    }

    #[test]
    fn check_set_rejects_unknown_with_valid_list() {
        check_set("state", &strs(&["open", "done"]), &["open", "done"]).unwrap();
        let e = check_set("state", &strs(&["bogus"]), &["open", "done"]).unwrap_err();
        assert!(e.to_string().contains("bogus"));
        assert!(e.to_string().contains("open done"));
    }

    #[test]
    fn parse_time_forms() {
        let now = 1_800_000_000i64;
        assert_eq!(
            parse_time("since", "1700000000", now).unwrap(),
            1_700_000_000
        );
        assert_eq!(
            parse_time("since", "2026-09-17T17:24:00Z", now).unwrap(),
            crate::issue::time::parse_iso("2026-09-17T17:24:00Z").unwrap()
        );
        assert_eq!(
            parse_time("since", "2026-09-17", now).unwrap(),
            crate::issue::time::parse_iso("2026-09-17T00:00:00Z").unwrap()
        );
        assert_eq!(parse_time("since", "30m", now).unwrap(), now - 1800);
        assert_eq!(parse_time("since", "24h", now).unwrap(), now - 86400);
        assert_eq!(parse_time("since", "7d", now).unwrap(), now - 604800);
        assert!(parse_time("since", "yesterday", now).is_err());
        assert!(parse_time("since", "2026-13-40", now).is_err());
        assert!(parse_time("since", "-5d", now).is_err());
    }

    #[test]
    fn sort_rows_asc_desc_and_ties() {
        let mut rows = vec![
            json!({"id": "X-10", "priority": "P2"}),
            json!({"id": "X-2", "priority": "P1"}),
            json!({"id": "X-1", "priority": "P1"}),
        ];
        let keys = [("id", "id"), ("priority", "priority")];
        sort_rows(&mut rows, "priority", &keys, "id").unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| r["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["X-1", "X-2", "X-10"]
        );
        sort_rows(&mut rows, "-id", &keys, "id").unwrap();
        // Natural key: X-10 after X-2, descending reverses everything.
        assert_eq!(
            rows.iter()
                .map(|r| r["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["X-10", "X-2", "X-1"]
        );
        let e = sort_rows(&mut rows, "nope", &keys, "id").unwrap_err();
        assert!(e.to_string().contains("--sort"));
    }

    #[test]
    fn sort_rows_missing_last_and_dotted_paths() {
        let mut rows = vec![
            json!({"id": "b", "work": {"stage": "verify"}}),
            json!({"id": "a"}),
            json!({"id": "c", "work": {"stage": "build"}}),
        ];
        sort_rows(&mut rows, "stage", &[("stage", "work.stage")], "id").unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| r["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["c", "b", "a"] // build < verify < missing
        );
    }

    #[test]
    fn sort_rows_numbers() {
        let mut rows = vec![
            json!({"id": "a", "n": 3}),
            json!({"id": "b", "n": 1}),
            json!({"id": "c", "n": 2}),
        ];
        sort_rows(&mut rows, "n", &[("n", "n")], "id").unwrap();
        assert_eq!(rows[0]["id"], "b");
        assert_eq!(rows[2]["id"], "a");
    }

    #[test]
    fn apply_fields_keeps_named_keys() {
        let mut rows = vec![
            json!({"id": "a", "state": "open", "title": "t"}),
            json!({"id": "b", "state": "done"}),
        ];
        apply_fields(&mut rows, &strs(&["id", "state"])).unwrap();
        assert_eq!(rows[0], json!({"id": "a", "state": "open"}));
        assert_eq!(rows[1], json!({"id": "b", "state": "done"}));
        let e = apply_fields(&mut rows, &strs(&["nope"])).unwrap_err();
        assert!(e.to_string().contains("--fields"));
        // An empty set cannot be validated — passes through.
        let mut empty: Vec<Value> = vec![];
        apply_fields(&mut empty, &strs(&["anything"])).unwrap();
    }

    #[test]
    fn fields_need_json_rejects_table_mode() {
        fields_need_json(&strs(&["id"]), true).unwrap();
        fields_need_json(&[], false).unwrap();
        assert!(fields_need_json(&strs(&["id"]), false).is_err());
    }

    #[test]
    fn apply_limit_truncates() {
        let mut v = vec![1, 2, 3];
        apply_limit(&mut v, Some(2));
        assert_eq!(v, vec![1, 2]);
        apply_limit(&mut v, None);
        assert_eq!(v, vec![1, 2]);
    }
}
