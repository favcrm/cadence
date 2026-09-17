//! `issue.md` / comment files: `---` YAML frontmatter + Markdown body.

use crate::error::{Error, Result};
use crate::issue::model::{CommentFront, Front};

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

pub fn parse_issue(text: &str) -> Result<(Front, String)> {
    let (yaml, body) = split_front(text)?;
    let front: Front = serde_yaml::from_str(yaml)
        .map_err(|e| Error::rejected(format!("issue frontmatter is not valid YAML: {e}")))?;
    Ok((front, body.to_string()))
}

pub fn parse_comment(text: &str) -> Result<(CommentFront, String)> {
    let (yaml, body) = split_front(text)?;
    let front: CommentFront = serde_yaml::from_str(yaml)
        .map_err(|e| Error::rejected(format!("comment frontmatter is not valid YAML: {e}")))?;
    Ok((front, body.to_string()))
}

/// Serialise frontmatter + body back to the file text. YAML output is
/// deterministic (struct field order) so diffs stay minimal.
pub fn render(front: &impl serde::Serialize, body: &str) -> Result<String> {
    let yaml = serde_yaml::to_string(front)
        .map_err(|e| Error::internal(format!("cannot serialise frontmatter: {e}")))?;
    let body = body.strip_prefix('\n').unwrap_or(body);
    Ok(format!("---\n{yaml}---\n\n{body}"))
}

/// Markdown acceptance checkboxes in a body: `(done, total)`.
pub fn checkbox_progress(body: &str) -> (u64, u64) {
    let mut done = 0u64;
    let mut total = 0u64;
    for line in body.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("- [") {
            match rest.as_bytes().first() {
                Some(b'x') | Some(b'X') if rest.as_bytes().get(1) == Some(&b']') => {
                    done += 1;
                    total += 1;
                }
                Some(b' ') if rest.as_bytes().get(1) == Some(&b']') => total += 1,
                _ => {}
            }
        }
    }
    (done, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\nid: CAD-16\ntitle: x\nstatus: doing\npriority: P1\ncreated: 2026-09-17T16:01:00Z\n---\n\nBody line\n- [x] a\n- [ ] b\n";

    #[test]
    fn round_trip() {
        let (front, body) = parse_issue(SAMPLE).unwrap();
        assert_eq!(front.id, "CAD-16");
        assert_eq!(front.status, "doing");
        assert!(body.starts_with("Body line"));
        let text = render(&front, &body).unwrap();
        let (front2, body2) = parse_issue(&text).unwrap();
        assert_eq!(front2.title, "x");
        assert_eq!(body2, body);
    }

    #[test]
    fn rejects_missing_fence() {
        assert!(parse_issue("no frontmatter").is_err());
        assert!(parse_issue("---\nid: CAD-1\n").is_err());
    }

    #[test]
    fn checkboxes() {
        let (_, body) = parse_issue(SAMPLE).unwrap();
        assert_eq!(checkbox_progress(&body), (1, 2));
        assert_eq!(checkbox_progress("none"), (0, 0));
        assert_eq!(checkbox_progress("- [X] caps\n- [] no\n"), (1, 1));
    }
}
