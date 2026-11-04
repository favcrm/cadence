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
        | '\u{2060}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}'
        | '\u{e0000}'..='\u{e007f}')
}

/// Whether text is safe to show on one line of something a person may paste:
/// no line break can start a new command, and nothing disguises what follows.
pub(crate) fn plain_text(value: &str) -> bool {
    !value.chars().any(|c| c.is_control() || deceptive(c))
}

/// One single-quoted argument, or `None` when the value has no safe printed form.
pub(crate) fn arg(value: &str) -> Option<String> {
    let safe = plain_text(value) && !value.chars().any(|c| matches!(c, '\'' | '\\' | '"'));
    safe.then(|| format!("'{value}'"))
}

/// [`arg`] for a path. A path that is not valid UTF-8 has none: its lossy display
/// would name a different path.
pub(crate) fn path_arg(path: &Path) -> Option<String> {
    arg(path.to_str()?)
}

/// One piece of a printed command. Only `Fixed` is emitted as written, and it must
/// be `'static`, so a runtime value cannot reach a command unquoted.
pub(crate) enum Part<'a> {
    Fixed(&'static str),
    Value(&'a str),
    Path(&'a Path),
}

/// The command text, plus a note naming any value that has no safe printed form.
/// Such a value becomes a `VALUE_n` placeholder with no shell meaning. Pasting it
/// cannot act on the real path, though a command may still run against the
/// placeholder, which is why the note says what to set it to.
fn render(parts: &[Part]) -> (String, String) {
    let mut command = String::new();
    let mut unprintable = Vec::new();
    for part in parts {
        let (quoted, shown) = match part {
            Part::Fixed(text) => {
                command.push_str(text);
                continue;
            }
            Part::Value(value) => (arg(value), value.to_string()),
            Part::Path(path) => (path_arg(path), path.display().to_string()),
        };
        match quoted {
            Some(quoted) => command.push_str(&quoted),
            None => {
                let name = format!("VALUE_{}", unprintable.len() + 1);
                command.push_str(&name);
                unprintable.push(format!("{name} set to {shown}"));
            }
        }
    }
    let note = if unprintable.is_empty() {
        String::new()
    } else {
        format!(
            " with {} (it cannot be printed as a safe command)",
            unprintable.join(" and ")
        )
    };
    (command, note)
}

/// A backticked command for a sentence.
pub(crate) fn command(parts: &[Part]) -> String {
    let (command, note) = render(parts);
    format!("`{command}`{note}")
}

/// A command with nothing around it, for a field or a line of its own.
pub(crate) fn bare_command(parts: &[Part]) -> String {
    let (command, note) = render(parts);
    format!("{command}{note}")
}

/// [`command`] for the common shape: fixed words, one path, fixed words.
pub(crate) fn with_path(before: &'static str, path: &Path, after: &'static str) -> String {
    command(&[
        Part::Fixed(before),
        Part::Fixed(" "),
        Part::Path(path),
        Part::Fixed(after),
    ])
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
            assert!(shown.starts_with("`git -C VALUE_1 status`"), "{shown}");
            assert!(shown.contains("cannot be printed"), "{shown}");
        }
    }

    /// Every runtime value is quoted or replaced, wherever it sits in the command.
    #[test]
    fn every_value_in_a_command_is_quoted_or_replaced() {
        let parts = |root, name| {
            bare_command(&[
                Part::Fixed("git -C "),
                Part::Path(Path::new(root)),
                Part::Fixed(" worktree remove "),
                Part::Value(name),
            ])
        };
        assert_eq!(
            parts("/repo", "x; touch owned"),
            "git -C '/repo' worktree remove 'x; touch owned'"
        );
        let shown = parts("/re'po", "x'; touch owned; '");
        assert!(
            shown.starts_with("git -C VALUE_1 worktree remove VALUE_2 with VALUE_1 set to"),
            "{shown}"
        );
        assert!(shown.contains("cannot be printed"), "{shown}");
    }

    #[test]
    fn plain_text_refuses_what_could_start_a_new_line_or_disguise_one() {
        assert!(plain_text("/private/it's a path;$x"));
        for value in [
            "a\ntouch owned",
            "a\rb",
            "a\u{1b}[31m",
            "a\u{202e}b",
            "a\u{2028}b",
        ] {
            assert!(!plain_text(value), "{value:?}");
        }
    }
}
