//! CAD-877: the one JSON output policy for stdout.
//!
//! Pretty JSON when stdout is a terminal (a human reading), compact
//! single-line JSON otherwise (an agent or pipe: 30-50% of pretty
//! JSON tokens are whitespace). `CADENCE_JSON=pretty|compact`
//! overrides the detection. Any other value warns once on stderr and
//! is ignored. Field names and order are never touched. Stderr and
//! on-disk files do not go through this policy.

use serde::Serialize;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether JSON printed to stdout should be pretty, given the
/// override value and whether stdout is a terminal.
pub fn pretty_for(override_value: Option<&str>, stdout_is_tty: bool) -> bool {
    note_json_override(override_value);
    match override_value {
        Some("pretty") => true,
        Some("compact") => false,
        _ => stdout_is_tty,
    }
}

/// Whether stdout JSON is pretty right now.
pub fn stdout_pretty() -> bool {
    pretty_for(
        std::env::var("CADENCE_JSON").ok().as_deref(),
        std::io::stdout().is_terminal(),
    )
}

/// Render `value` for stdout under the policy. Same signature shape
/// as `serde_json::to_string_pretty`, so call sites swap one name.
pub fn json_text<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<String> {
    if stdout_pretty() {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
}

/// Whether human-oriented table output is appropriate on stdout:
/// a terminal, unless `CADENCE_JSON` forces a JSON form.
/// `CADENCE_JSON=table` forces the table for a pipe (tests, `less`).
pub fn stdout_table() -> bool {
    table_for(
        std::env::var("CADENCE_JSON").ok().as_deref(),
        std::io::stdout().is_terminal(),
    )
}

pub fn table_for(override_value: Option<&str>, stdout_is_tty: bool) -> bool {
    note_json_override(override_value);
    match override_value {
        Some("pretty") | Some("compact") => false,
        Some("table") => true,
        _ => stdout_is_tty,
    }
}

/// `pretty`, `compact`, and `table` are the only recognized overrides.
pub fn recognized_json_override(value: &str) -> bool {
    matches!(value, "pretty" | "compact" | "table")
}

/// One stderr line per process for a typo such as `Pretty` or `tabel`.
fn note_json_override(value: Option<&str>) {
    let Some(value) = value else {
        return;
    };
    if recognized_json_override(value) {
        return;
    }
    static WARNED: AtomicBool = AtomicBool::new(false);
    if WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    eprintln!(
        "cadence: CADENCE_JSON={value:?} is not pretty, compact, or table — using the default"
    );
}

/// Explicit `--json` wins. Otherwise a non-table stdout is JSON.
pub fn json_selected(explicit_json: bool) -> bool {
    explicit_json || !stdout_table()
}

/// Body for one observation. Watch is always one compact line so a
/// pipe can split records, even when `CADENCE_JSON=pretty`.
pub fn observation_body<T: Serialize + ?Sized>(
    value: &T,
    watch: bool,
) -> serde_json::Result<String> {
    if watch {
        serde_json::to_string(value)
    } else {
        json_text(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_table() {
        assert!(pretty_for(None, true));
        assert!(!pretty_for(None, false));
        assert!(pretty_for(Some("pretty"), false));
        assert!(table_for(None, true));
        assert!(!table_for(None, false));
        assert!(!table_for(Some("compact"), true));
        assert!(table_for(Some("table"), false));
        assert!(!pretty_for(Some("compact"), true));
        assert!(pretty_for(Some("bogus"), true));
        assert!(!pretty_for(Some("bogus"), false));
        assert!(recognized_json_override("pretty"));
        assert!(recognized_json_override("compact"));
        assert!(recognized_json_override("table"));
        assert!(!recognized_json_override("Pretty"));
        assert!(!recognized_json_override("tabel"));
        assert!(!recognized_json_override(""));
    }

    #[test]
    fn watch_observation_is_one_compact_line() {
        #[derive(Serialize)]
        struct Row {
            a: u8,
        }
        let body = observation_body(&Row { a: 1 }, true).unwrap();
        assert_eq!(body, "{\"a\":1}");
        assert!(!body.contains('\n'));
        assert!(!body.contains('\u{1b}'));
    }
}
