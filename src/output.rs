//! CAD-877: the one JSON output policy for stdout.
//!
//! Pretty JSON when stdout is a terminal (a human reading), compact
//! single-line JSON otherwise (an agent or pipe: 30-50% of pretty
//! JSON tokens are whitespace). `CADENCE_JSON=pretty|compact`
//! overrides the detection; any other value is ignored. Field names
//! and order are never touched. Stderr and on-disk files do not go
//! through this policy.

use serde::Serialize;
use std::io::IsTerminal;

/// Whether JSON printed to stdout should be pretty, given the
/// override value and whether stdout is a terminal.
pub fn pretty_for(override_value: Option<&str>, stdout_is_tty: bool) -> bool {
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
    match std::env::var("CADENCE_JSON").ok().as_deref() {
        Some("pretty") | Some("compact") => false,
        Some("table") => true,
        _ => std::io::stdout().is_terminal(),
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
        assert!(!pretty_for(Some("compact"), true));
        assert!(pretty_for(Some("bogus"), true));
        assert!(!pretty_for(Some("bogus"), false));
    }
}
