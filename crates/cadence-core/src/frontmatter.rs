//! `---` YAML frontmatter + Markdown body split shared by every host.
use crate::error::{Error, Result};

/// Split `---\n<yaml>\n---\n<body>`; the opening fence must be the very
/// first line. Returns the frontmatter text and the body.
pub fn split_front(text: &str) -> Result<(&str, &str)> {
    let text = text.strip_prefix("\u{feff}").unwrap_or(text);
    if !text.starts_with("---\n") && !text.starts_with("---\r\n") {
        return Err(Error::rejected(
            "Issue file must start with a '---' frontmatter fence",
        ));
    }
    let after_open = if text.starts_with("---\r\n") { 5 } else { 4 };
    let rest = &text[after_open..];
    for (idx, _) in rest.match_indices("---") {
        // The closing fence must sit on its own line.
        let before_ok = idx == 0 || rest.as_bytes()[idx - 1] == b'\n';
        let after = &rest[idx + 3..];
        let after_ok = after.is_empty()
            || after.starts_with('\n')
            || after.starts_with("\r\n")
            || after.starts_with(' ');
        if before_ok && after_ok {
            let yaml = &rest[..idx];
            // The fence's own line ending, then one blank separator
            // line — a body that wants to start blank writes two.
            let body = after
                .strip_prefix("\r\n")
                .or_else(|| after.strip_prefix('\n'))
                .unwrap_or(after);
            let body = body
                .strip_prefix("\r\n")
                .or_else(|| body.strip_prefix('\n'))
                .unwrap_or(body);
            return Ok((yaml, body));
        }
    }
    Err(Error::rejected(
        "Issue file is missing the closing '---' frontmatter fence",
    ))
}
