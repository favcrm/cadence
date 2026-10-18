//! The `[A-Za-z0-9_-]` identifier grammar, in one place.
//!
//! More than one producer has to agree on it. An attribution handle lands
//! in the store's comment-author / `<actor> via board` grammar (CAD-1300),
//! and a project-policy grant is only matchable by a caller whose key was
//! derived the same way. Three separate copies of this derivation is how a
//! member ends up refused on one surface and admitted on another, so the
//! grammar, the bound and the sanitizer live here (AOS-274).

/// The longest handle any derivation keeps. A longer subject is cut, so
/// nothing longer can ever be a caller key — the store refuses such a grant
/// rather than record one no caller could match.
pub(crate) const HANDLE_MAX: usize = 64;

/// `raw` in the identifier grammar: every character outside the grammar
/// becomes `_`, cut to [`HANDLE_MAX`]. Two subjects that differ only
/// outside the grammar collide, which is the point: the store holds one
/// spelling per member.
pub(crate) fn sanitized(raw: &str) -> String {
    raw.chars()
        .map(|c| if is_ident_char(c) { c } else { '_' })
        .take(HANDLE_MAX)
        .collect()
}

/// Is `raw` itself a handle: non-empty, at most `max` bytes, every
/// character inside the grammar.
pub(crate) fn is_handle(raw: &str, max: usize) -> bool {
    !raw.is_empty() && raw.len() <= max && raw.chars().all(is_ident_char)
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizing_replaces_everything_outside_the_grammar_and_cuts_at_the_bound() {
        assert_eq!(sanitized("usr_9"), "usr_9");
        assert_eq!(sanitized("auth0|usr:9/x"), "auth0_usr_9_x");
        assert_eq!(sanitized(""), "");
        assert_eq!(sanitized(&"s".repeat(100)), "s".repeat(HANDLE_MAX));
    }

    #[test]
    fn a_handle_is_nonempty_inside_the_grammar_and_within_the_bound() {
        assert!(is_handle("usr_9", HANDLE_MAX));
        assert!(is_handle(&"s".repeat(HANDLE_MAX), HANDLE_MAX));
        assert!(!is_handle(&"s".repeat(HANDLE_MAX + 1), HANDLE_MAX));
        assert!(!is_handle("", HANDLE_MAX));
        assert!(!is_handle("auth0|usr:9/x", HANDLE_MAX));
        assert!(!is_handle("usr 9", HANDLE_MAX));
        assert!(!is_handle("usr.9", HANDLE_MAX));
    }

    /// The two halves are inverses over the grammar: what the sanitizer
    /// produces, the validator accepts — a caller key can always be
    /// recorded as a grant.
    #[test]
    fn what_the_sanitizer_produces_the_validator_accepts() {
        for raw in ["usr_9", "auth0|usr:9/x", "é", "a b", &"s".repeat(200)] {
            let key = sanitized(raw);
            assert!(is_handle(&key, HANDLE_MAX), "{raw:?} -> {key:?}");
        }
    }
}
