//! Commands a refusal prints for a person to paste.
//!
//! Refusals reach stderr as JSON, which rewrites `\`, `"` and control
//! characters, so a command pasted as printed is not the command that was built
//! if it contains any of them. `'\''` becomes `'\\''`, which ends the quote and
//! runs the rest of a crafted path. Shell syntax has no readable way to write
//! `'` inside quotes without one of those characters, so the rule is simple: a
//! printed command must be unchanged by JSON encoding. A value that cannot meet
//! that is never printed in or beside the command, and [`shown`] names a path in the
//! sentence around it, since a person may paste the whole line.

use std::path::Path;

/// Characters that read differently from what they do: a bidi override can make
/// a command look unlike the bytes it runs, and these also break a line.
fn deceptive(c: char) -> bool {
    matches!(c,
        '\u{00ad}' | '\u{061c}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
        | '\u{2060}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}'
        | '\u{e0000}'..='\u{e007f}')
}

/// One single-quoted argument, or `None` when the value has no safe printed form.
/// A backtick is safe inside the quotes, but [`command`] wraps its output in backticks
/// and one in the value would end that span early. `$` is safe too, but a refusal
/// prints inside the double quotes of a JSON string, where it would expand.
pub(crate) fn arg(value: &str) -> Option<String> {
    let safe = !value
        .chars()
        .any(|c| matches!(c, '\'' | '\\' | '"' | '`' | '$') || c.is_control() || deceptive(c));
    safe.then(|| format!("'{value}'"))
}

/// [`arg`] for a path. A path that is not valid UTF-8 has none: its lossy display
/// would name a different path.
pub(crate) fn path_arg(path: &Path) -> Option<String> {
    arg(path.to_str()?)
}

/// A path in a sentence near a command. It reads as the path, quoted, so pasting the
/// whole line cannot run it unless it is the first word, so do not start a sentence
/// with it. A path that cannot be quoted safely shows as [`sanitized`].
pub(crate) fn shown(path: &Path) -> String {
    path_arg(path).unwrap_or_else(|| sanitized(&path.to_string_lossy()))
}

/// A value with each character that could mean something to a shell as `?`: close
/// enough to recognise, and inert without quotes.
pub(crate) fn sanitized(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "/._-+@:,%=".contains(c) {
                c
            } else {
                '?'
            }
        })
        .collect()
}

/// One piece of a printed command. Only `Fixed` is emitted as written, and it must
/// be `'static`, so a runtime value cannot reach a command unquoted.
pub(crate) enum Part<'a> {
    Fixed(&'static str),
    Value(&'a str),
    Path(&'a Path),
}

/// The command text, using a `VALUE_n` placeholder for each value with no safe
/// printed form, and how many placeholders it used. A refused value is never
/// repeated: anything near a command may be pasted or copied along with it.
fn render(parts: &[Part]) -> (String, usize) {
    let mut command = String::new();
    let mut placeholders = 0;
    for part in parts {
        let quoted = match part {
            Part::Fixed(text) => {
                command.push_str(text);
                continue;
            }
            Part::Value(value) => arg(value),
            Part::Path(path) => path_arg(path),
        };
        match quoted {
            Some(quoted) => command.push_str(&quoted),
            None => {
                placeholders += 1;
                command.push_str(&format!("VALUE_{placeholders}"));
            }
        }
    }
    (command, placeholders)
}

/// A backticked command for a sentence. A value with no safe printed form becomes
/// a placeholder with no shell meaning. Pasting it cannot act on the real value,
/// though a command may still run against the placeholder, so the note says so.
pub(crate) fn command(parts: &[Part]) -> String {
    let (command, placeholders) = render(parts);
    if placeholders == 0 {
        format!("`{command}`")
    } else {
        format!(
            "`{command}` (VALUE_ stands for a value that cannot be printed as a safe command; \
             do not paste this as written)"
        )
    }
}

/// A command with nothing around it, for a field a person copies or a line of its
/// own. `None` when any value has no safe printed form: the caller says it in words.
pub(crate) fn bare_command(parts: &[Part]) -> Option<String> {
    let (command, placeholders) = render(parts);
    (placeholders == 0).then_some(command)
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

    /// Run a whole message as a person pasting the whole line would, both bare and in
    /// the JSON a refusal prints as. Anything crafted to run would create `owned` in the
    /// working directory. `PATH` holds only `touch`: these messages name real commands,
    /// such as `cadence ui login --rotate`, which must never run from a test.
    pub(crate) fn assert_inert_when_pasted(message: &str) {
        let tools = tempfile::tempdir().unwrap();
        let touch = ["/usr/bin/touch", "/bin/touch"]
            .into_iter()
            .find(|path| Path::new(path).exists())
            .expect("touch");
        std::os::unix::fs::symlink(touch, tools.path().join("touch")).unwrap();
        let json = serde_json::to_string_pretty(&serde_json::json!({"error": message})).unwrap();
        for form in [message, json.as_str()] {
            let root = tempfile::tempdir().unwrap();
            let mut shell = std::process::Command::new("/bin/sh");
            shell
                .arg("-c")
                .arg(form)
                .env("PATH", tools.path())
                .current_dir(root.path());
            crate::reaper::output(&mut shell).unwrap();
            assert!(!root.path().join("owned").exists(), "{form:?}");
        }
    }

    /// Names built to run `touch owned` when printed raw in a sentence.
    pub(crate) const CRAFTED: [&str; 6] = [
        "x;touch owned;y",
        "x $(touch owned) y",
        "x`touch owned`y",
        "x'; touch owned; '",
        "x\ntouch owned\ny",
        "x\\'; touch owned; #",
    ];

    /// Paths with no safe printed form.
    pub(crate) const UNPRINTABLE: [&str; 11] = [
        "/private/x$(touch owned)",
        "/private/x`touch owned`",
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
            "/private/x;y",
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
            [
                Part::Fixed("git -C "),
                Part::Path(Path::new(root)),
                Part::Fixed(" worktree remove "),
                Part::Value(name),
            ]
        };
        assert_eq!(
            bare_command(&parts("/repo", "x; touch owned")).unwrap(),
            "git -C '/repo' worktree remove 'x; touch owned'"
        );
        let shown = command(&parts("/re'po", "x'; touch owned; '"));
        assert!(
            shown.starts_with("`git -C VALUE_1 worktree remove VALUE_2`"),
            "{shown}"
        );
    }

    #[test]
    fn a_path_in_a_sentence_is_inert_and_still_recognisable() {
        assert_eq!(shown(Path::new("/repo/has space")), "'/repo/has space'");
        assert_eq!(shown(Path::new("/repo/it's")), "/repo/it?s");
        assert_eq!(shown(Path::new("/repo/工作's")), "/repo/工作?s");
        assert_eq!(
            shown(Path::new(OsStr::from_bytes(b"/repo/\xff"))),
            "/repo/?"
        );
        for name in CRAFTED {
            let path = format!("/repo/{name}");
            assert_inert_when_pasted(&format!("left {} in place", shown(Path::new(&path))));
        }
    }

    /// A refused value appears nowhere in the output, not only outside the command:
    /// a person copies the whole field or the whole line.
    #[test]
    fn a_refused_value_appears_nowhere_in_the_output() {
        for value in UNPRINTABLE {
            let parts = [Part::Fixed("chmod 700 "), Part::Path(Path::new(value))];
            assert_eq!(bare_command(&parts), None, "{value:?}");
            let printed = command(&parts);
            assert!(!printed.contains(value), "{printed:?}");
            assert!(printed.contains("cannot be printed"), "{printed:?}");
            let printed = with_path("chmod 700", Path::new(value), "");
            assert!(!printed.contains(value), "{printed:?}");
        }
    }
}
