//! Shared `@mention` parsing and outcome types for channels, topics, and bell intents (CAD-2711).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Default maximum consecutive agent-to-agent turns in a channel before a
/// human turn is required to resume (`waiting_for_person`).
pub const AGENT_CHAIN_LIMIT: i64 = 2;

/// Outcome recorded on a channel post for each resolved `@mention`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionOutcome {
    /// One of: `"notified"`, `"queued"`, `"silent"`, `"blocked_acl"`, `"blocked_dm"`, `"blocked_chain"`.
    pub kind: String,
    pub handle: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Extracted `@mention` handles and silent mode for a posted message body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMentions {
    pub silent: bool,
    pub handles: Vec<String>,
}

/// True when `body` begins (after leading whitespace) with `/note` followed by
/// whitespace or end-of-string (ASCII case-insensitive).
pub fn starts_with_note(body: &str) -> bool {
    let trimmed = body.trim_start();
    if trimmed.len() < 5 {
        return false;
    }
    let Some((prefix, rest)) = trimmed.as_bytes().split_at_checked(5) else {
        return false;
    };
    prefix.eq_ignore_ascii_case(b"/note") && rest.first().is_none_or(|b| b.is_ascii_whitespace())
}

fn is_mention_boundary_prev(b: u8) -> bool {
    !b.is_ascii_alphanumeric() && b != b'_' && b != b'.'
}

fn is_handle_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Extract deduplicated lowercase `@handle` tokens from `body`, preserving
/// first-seen order and ignoring email addresses like `user@example.com`.
pub fn parse_mentions(body: &str, explicit_silent: bool) -> ParsedMentions {
    let silent = explicit_silent || starts_with_note(body);
    let bytes = body.as_bytes();
    let mut handles = Vec::new();
    let mut seen = HashSet::new();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'@' && (i == 0 || is_mention_boundary_prev(bytes[i - 1])) {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && is_handle_byte(bytes[end]) && end - start < 64 {
                end += 1;
            }
            if end > start {
                let handle = body[start..end].to_ascii_lowercase();
                if seen.insert(handle.clone()) {
                    handles.push(handle);
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }

    ParsedMentions { silent, handles }
}

/// Canonical slug for a display name (`"Mei Lin"` -> `"mei-lin"`).
pub fn slugify_name(name: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
            pending_dash = false;
        } else if !out.is_empty() {
            pending_dash = true;
        }
    }
    out
}

/// Match a human display name (`"Mei Lin"`) or single-word name against a parsed handle.
pub fn human_handle_matches(name: &str, handle: &str) -> bool {
    let target = handle.trim().trim_start_matches('@').to_ascii_lowercase();
    let trimmed = name.trim();
    if trimmed.is_empty() || target.is_empty() {
        return false;
    }
    if slugify_name(trimmed) == target {
        return true;
    }
    let compact: String = trimmed
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if compact == target {
        return true;
    }
    trimmed
        .split_whitespace()
        .next()
        .is_some_and(|first| slugify_name(first) == target || first.eq_ignore_ascii_case(&target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mentions_ignoring_emails_and_dedupes() {
        let parsed = parse_mentions(
            "Ping @Designer, email foo@example.com or a.b@bar.org, and @mei-lin plus @designer",
            false,
        );
        assert!(!parsed.silent);
        assert_eq!(parsed.handles, vec!["designer", "mei-lin"]);
    }

    #[test]
    fn detects_slash_note_prefix() {
        assert!(starts_with_note("/note heads up @ops"));
        assert!(starts_with_note("  /NOTE\nquiet note"));
        assert!(starts_with_note("/note"));
        assert!(!starts_with_note("/notes something"));
        let parsed = parse_mentions("/note @ops fyi", false);
        assert!(parsed.silent);
        assert_eq!(parsed.handles, vec!["ops"]);
    }

    #[test]
    fn matches_human_handles() {
        assert!(human_handle_matches("Mei Lin", "mei"));
        assert!(human_handle_matches("Mei Lin", "mei-lin"));
        assert!(human_handle_matches("Mei Lin", "meilin"));
        assert!(!human_handle_matches("Mei Lin", "lin"));
        assert!(!human_handle_matches("", "mei"));
    }
}
