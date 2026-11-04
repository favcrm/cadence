//! Commands a refusal prints for a person to paste.
//!
//! Refusals reach stderr as JSON, which rewrites `\`, `"` and control
//! characters, so a command pasted as printed is not the command that was built
//! if it contains any of them. `'\''` becomes `'\\''`, which ends the quote and
//! runs the rest of a crafted path. Shell syntax has no readable way to write
//! `'` inside quotes without one of those characters, so the rule is simple: a
//! printed command must be unchanged by JSON encoding, otherwise print none.

use std::path::Path;

/// Characters that read differently from what they do: a bidi override can make
/// a command look unlike the bytes it runs, and these also break a line.
fn deceptive(c: char) -> bool {
    matches!(c,
        '\u{00ad}' | '\u{061c}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
        | '\u{2060}'..='\u{206f}' | '\u{feff}')
}

/// One single-quoted argument, or `None` when the value has no safe printed form.
pub fn arg(value: &str) -> Option<String> {
    let safe = !value
        .chars()
        .any(|c| matches!(c, '\'' | '\\' | '"') || c.is_control() || deceptive(c));
    safe.then(|| format!("'{value}'"))
}

/// [`arg`] for a path. A path that is not valid UTF-8 has none: its lossy display
/// would name a different path.
pub fn path_arg(path: &Path) -> Option<String> {
    arg(path.to_str()?)
}

/// A backticked command around one path. When the path has no safe printed form
/// the command keeps a placeholder with no shell meaning, so pasting it fails
/// harmlessly, and the path is named as text.
pub fn with_path(before: &str, path: &Path, after: &str) -> String {
    match path_arg(path) {
        Some(quoted) => format!("`{before} {quoted}{after}`"),
        None => format!(
            "`{before} THAT_PATH{after}` with THAT_PATH set to {} (it cannot be printed as a \
             safe command)",
            path.display()
        ),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    /// The rule itself: JSON encoding leaves a pasteable command unchanged.
    pub(crate) fn survives_json(text: &str) -> bool {
        serde_json::to_string(text).unwrap() == format!("\"{text}\"")
    }

    /// Paths with no safe printed form.
    pub(crate) const UNPRINTABLE: [&str; 9] = [
        "/private/it's",
        "/private/a'; touch owned; '",
        "/private/back\\slash",
        "/private/dq\"uote",
        "/private/new\nline",
        "/private/esc\u{1b}[31m",
        "/private/c1\u{9b}31m",
        "/private/bidi\u{202e}txt",
        "/private/line\u{2028}sep",
    ];

    #[test]
    fn a_printable_value_is_quoted_and_survives_json() {
        for value in [
            "/private/has space",
            "/private/$HOME;x`y`",
            "/private/#hash",
            "/private/-n",
            "/private/&|<>*?[a]",
            "/private/工作",
        ] {
            let quoted = arg(value).unwrap();
            assert_eq!(quoted, format!("'{value}'"));
            assert!(survives_json(&quoted), "{quoted}");
        }
    }

    #[test]
    fn an_unprintable_value_has_no_argument() {
        for value in UNPRINTABLE {
            assert_eq!(arg(value), None, "{value:?}");
        }
    }

    #[test]
    fn a_path_that_is_not_utf8_has_no_argument() {
        assert_eq!(
            path_arg(Path::new(OsStr::from_bytes(b"/private/\xff"))),
            None
        );
    }

    #[test]
    fn with_path_prints_a_runnable_command_only_when_safe() {
        let shown = with_path("git -C", Path::new("/private/has space"), " status");
        assert_eq!(shown, "`git -C '/private/has space' status`");
        assert!(survives_json(&shown), "{shown}");
        for value in UNPRINTABLE {
            let shown = with_path("git -C", Path::new(value), " status");
            assert!(shown.starts_with("`git -C THAT_PATH status`"), "{shown}");
            assert!(shown.contains("cannot be printed"), "{shown}");
        }
    }
}
