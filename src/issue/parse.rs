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

/// One checklist item from the unique level-two `Acceptance` section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptanceItem {
    pub text: String,
    pub checked: bool,
}

impl AcceptanceItem {
    /// The readback shape `issue show --json`, `issue acceptance` and
    /// `dispatch` all report: `done` mirrors `checked` for readers
    /// that speak in completion rather than checkbox terms.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "text": self.text,
            "checked": self.checked,
            "done": self.checked,
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct LineRange {
    start: usize,
    content_end: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug)]
struct AcceptanceSection {
    heading_end: usize,
    end: usize,
}

fn line_ranges(text: &str) -> Vec<LineRange> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let end = start + line.len();
        let content_end = if line.ends_with("\r\n") {
            end - 2
        } else if line.ends_with('\n') {
            end - 1
        } else {
            end
        };
        ranges.push(LineRange {
            start,
            content_end,
            end,
        });
        start = end;
    }
    if text.is_empty() {
        ranges.push(LineRange {
            start: 0,
            content_end: 0,
            end: 0,
        });
    }
    ranges
}

/// Return the heading level and title for a Markdown ATX heading.
/// Closing `#` characters are ignored, matching normal Markdown syntax.
pub(crate) fn heading(line: &str) -> Option<(usize, &str)> {
    let line = markdown_line(line)?;
    let level = line.bytes().take_while(|b| *b == b'#').count();
    if level == 0 {
        return None;
    }
    let rest = &line[level..];
    if !rest.is_empty()
        && !rest
            .chars()
            .next()
            .is_some_and(|character| character.is_whitespace())
    {
        return None;
    }
    let trimmed = rest.trim();
    let without_closing = trimmed.trim_end_matches('#');
    let title = if without_closing.len() < trimmed.len()
        && without_closing
            .chars()
            .last()
            .is_some_and(char::is_whitespace)
    {
        without_closing.trim_end()
    } else {
        trimmed
    };
    Some((level, title))
}

/// Markdown ATX headings and fenced blocks may be indented by at most three
/// spaces. Four-space examples are indented code and must not become syntax.
fn markdown_line(line: &str) -> Option<&str> {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    (indent <= 3).then(|| &line[indent..])
}

/// Return a fence marker for a line that starts a fenced code block.
fn fence(line: &str) -> Option<(u8, usize)> {
    let line = markdown_line(line)?;
    let marker = line.as_bytes().first().copied()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let count = line.bytes().take_while(|b| *b == marker).count();
    (count >= 3).then_some((marker, count))
}

/// A closing fence may only have whitespace after its marker. Opening fences
/// can carry an optional language or other info string, so that rule belongs
/// here rather than in `fence`.
fn is_closing_fence(line: &str, marker: u8, count: usize) -> bool {
    let Some(line) = markdown_line(line) else {
        return false;
    };
    let marker_count = line.bytes().take_while(|byte| *byte == marker).count();
    line.as_bytes().first().copied() == Some(marker)
        && marker_count >= count
        && line
            .get(marker_count..)
            .is_some_and(|trailing| trailing.chars().all(char::is_whitespace))
}

/// Tracks fenced code blocks line by line, so Markdown readers outside
/// this module (plan files, CAD-359) skip headings and checkboxes inside
/// a fence exactly as the acceptance reader does.
#[derive(Default)]
pub(crate) struct Fences(Option<(u8, usize)>);

impl Fences {
    /// True when `line` (without its line ending) is a fence marker or
    /// sits inside a fenced block — i.e. it is not Markdown syntax.
    pub(crate) fn is_code(&mut self, line: &str) -> bool {
        if let Some((marker, count)) = fence(line) {
            match self.0 {
                Some((open_marker, open_count))
                    if open_marker == marker && is_closing_fence(line, marker, open_count) =>
                {
                    self.0 = None;
                }
                None => self.0 = Some((marker, count)),
                _ => {}
            }
            return true;
        }
        self.0.is_some()
    }
}

/// Locate level-two `Acceptance` sections outside fenced code blocks.
/// Sections end at the next heading of level two or higher.
fn acceptance_sections(body: &str) -> Vec<AcceptanceSection> {
    let mut sections: Vec<AcceptanceSection> = Vec::new();
    let mut current: Option<usize> = None;
    let mut in_fence: Option<(u8, usize)> = None;

    for line in line_ranges(body) {
        let text = &body[line.start..line.content_end];
        if let Some((marker, count)) = fence(text) {
            match in_fence {
                Some((open_marker, open_count))
                    if open_marker == marker && is_closing_fence(text, marker, open_count) =>
                {
                    in_fence = None;
                }
                None => in_fence = Some((marker, count)),
                _ => {}
            }
            continue;
        }
        if in_fence.is_some() {
            continue;
        }
        let Some((level, title)) = heading(text) else {
            continue;
        };
        if level > 2 {
            continue;
        }
        if let Some(index) = current.take() {
            sections[index].end = line.start;
        }
        if level == 2 && title.eq_ignore_ascii_case("acceptance") {
            sections.push(AcceptanceSection {
                heading_end: line.end,
                end: body.len(),
            });
            current = Some(sections.len() - 1);
        }
    }
    sections
}

/// Parse one Markdown checklist line. Empty checklist stubs are excluded.
fn checkbox_item(line: &str) -> Option<AcceptanceItem> {
    let line = markdown_line(line)?;
    let rest = line.strip_prefix("- [")?;
    let marked = match rest.as_bytes().first() {
        Some(b'x') | Some(b'X') if rest.as_bytes().get(1) == Some(&b']') => true,
        Some(b' ') if rest.as_bytes().get(1) == Some(&b']') => false,
        _ => return None,
    };
    let suffix = &rest[2..];
    if !suffix.is_empty() && !suffix.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let text = suffix.trim();
    (!text.is_empty()).then(|| AcceptanceItem {
        text: text.to_string(),
        checked: marked,
    })
}

/// Read acceptance items from the unique level-two `Acceptance` section.
/// A duplicate heading is treated as malformed and produces no items on the
/// read side; mutation uses `replace_acceptance` to report the error.
pub fn acceptance_items(body: &str) -> Vec<AcceptanceItem> {
    let sections = acceptance_sections(body);
    if sections.len() != 1 {
        return Vec::new();
    }
    let section = sections[0];
    let section_body = &body[section.heading_end..section.end];
    let mut items = Vec::new();
    let mut in_fence: Option<(u8, usize)> = None;
    for line in line_ranges(section_body) {
        let text = &section_body[line.start..line.content_end];
        if let Some((marker, count)) = fence(text) {
            match in_fence {
                Some((open_marker, open_count))
                    if open_marker == marker && is_closing_fence(text, marker, open_count) =>
                {
                    in_fence = None;
                }
                None => in_fence = Some((marker, count)),
                _ => {}
            }
            continue;
        }
        if in_fence.is_none() {
            if let Some(item) = checkbox_item(text) {
                items.push(item);
            }
        }
    }
    items
}

/// Validate the existing body before replacing its acceptance section.
pub fn parse_acceptance_section(body: &str) -> Result<Vec<AcceptanceItem>> {
    let sections = acceptance_sections(body);
    match sections.as_slice() {
        [] => Ok(Vec::new()),
        [_] => Ok(acceptance_items(body)),
        _ => Err(Error::rejected(
            "Issue has duplicate level-two Acceptance sections — remove the duplicate before authoring acceptance",
        )),
    }
}

/// Parse a replacement file. Every nonblank line must be a nonempty checkbox
/// with explicit checked or unchecked state; this keeps malformed input from
/// being silently authored as a criterion.
pub fn parse_acceptance_input(text: &str) -> Result<Vec<AcceptanceItem>> {
    let mut items = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Some(item) = checkbox_item(line) else {
            return Err(Error::rejected(format!(
                "Acceptance input line {} must be `- [ ] text` or `- [x] text`",
                line_no + 1
            )));
        };
        items.push(item);
    }
    if items.is_empty() {
        return Err(Error::rejected(
            "Acceptance input is empty — provide at least one nonempty checklist item",
        ));
    }
    Ok(items)
}

fn preferred_newline(body: &str) -> &'static str {
    if body.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

fn rendered_acceptance(items: &[AcceptanceItem], newline: &str) -> Result<String> {
    if items.is_empty() {
        return Err(Error::rejected(
            "Acceptance input is empty — provide at least one nonempty checklist item",
        ));
    }
    let mut out = String::new();
    for item in items {
        if item.text.trim().is_empty() || item.text.contains('\r') || item.text.contains('\n') {
            return Err(Error::rejected(
                "Acceptance items must have nonempty single-line text",
            ));
        }
        out.push_str(if item.checked { "- [x] " } else { "- [ ] " });
        out.push_str(item.text.trim());
        out.push_str(newline);
    }
    Ok(out)
}

/// Replace the unique acceptance section, or append one when it is absent.
/// The surrounding body and its line-ending style are preserved.
pub fn replace_acceptance(body: &str, items: &[AcceptanceItem]) -> Result<String> {
    let sections = acceptance_sections(body);
    if sections.len() > 1 {
        return Err(Error::rejected(
            "Issue has duplicate level-two Acceptance sections — remove the duplicate before authoring acceptance",
        ));
    }
    let newline = preferred_newline(body);
    let checklist = rendered_acceptance(items, newline)?;
    if let Some(section) = sections.first() {
        let mut out = String::with_capacity(body.len() + checklist.len());
        out.push_str(&body[..section.heading_end]);
        out.push_str(newline);
        out.push_str(&checklist);
        out.push_str(newline);
        out.push_str(&body[section.end..]);
        return Ok(out);
    }

    let mut out = body.to_string();
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push_str(newline);
        }
        out.push_str(newline);
    }
    out.push_str("## Acceptance");
    out.push_str(newline);
    out.push_str(newline);
    out.push_str(&checklist);
    Ok(out)
}

/// Serialise frontmatter + body back to the file text. YAML output is
/// deterministic (struct field order) so diffs stay minimal.
pub fn render(front: &impl serde::Serialize, body: &str) -> Result<String> {
    let yaml = serde_yaml::to_string(front)
        .map_err(|e| Error::internal(format!("cannot serialise frontmatter: {e}")))?;
    let body = body.strip_prefix('\n').unwrap_or(body);
    Ok(format!("---\n{yaml}---\n\n{body}"))
}

/// Markdown acceptance checkboxes in a body: `(done, total)`. A
/// checkbox with no text (`- [ ]`) is a template stub, not an item.
pub fn checkbox_progress(body: &str) -> (u64, u64) {
    let mut done = 0u64;
    let mut total = 0u64;
    for line in body.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("- [") {
            let marked = match rest.as_bytes().first() {
                Some(b'x') | Some(b'X') if rest.as_bytes().get(1) == Some(&b']') => Some(true),
                Some(b' ') if rest.as_bytes().get(1) == Some(&b']') => Some(false),
                _ => None,
            };
            let Some(marked) = marked else {
                continue;
            };
            if rest[2..].trim().is_empty() {
                continue;
            }
            total += 1;
            done += marked as u64;
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
        // A bare `- [ ]` is a template stub — no text, not an item.
        assert_eq!(checkbox_progress("- [ ] \n- [ ]\n"), (0, 0));
        assert_eq!(checkbox_progress("- [ ] real\n- [ ]\n"), (0, 1));
    }

    #[test]
    fn acceptance_is_scoped_and_skips_fenced_examples() {
        let body = concat!(
            "- [x] unrelated before\n",
            "## Acceptance\n",
            "- [X] first outcome\n",
            "- [ ] second outcome\n",
            "- [ ]\n",
            "\x60\x60\x60markdown\n",
            "## Acceptance\n",
            "- [x] fenced example\n",
            "\x60\x60\x60\n",
            "### Notes\n",
            "- [ ] third outcome\n",
            "## Other\n",
            "- [x] unrelated after\n",
        );
        assert_eq!(
            acceptance_items(body),
            vec![
                AcceptanceItem {
                    text: "first outcome".into(),
                    checked: true,
                },
                AcceptanceItem {
                    text: "second outcome".into(),
                    checked: false,
                },
                AcceptanceItem {
                    text: "third outcome".into(),
                    checked: false,
                },
            ]
        );
        // The compatibility counter remains global and still sees every
        // nonempty checkbox, including unrelated and fenced examples.
        assert_eq!(checkbox_progress(body), (4, 6));
    }

    #[test]
    fn acceptance_supports_crlf_and_stops_at_level_two_or_higher() {
        let body = "## Acceptance\r\n- [x] first\r\n## Notes\r\n- [ ] ignored\r\n";
        assert_eq!(
            acceptance_items(body),
            vec![AcceptanceItem {
                text: "first".into(),
                checked: true,
            }]
        );
    }

    #[test]
    fn acceptance_ignores_indented_code_and_requires_heading_closing_space() {
        let body = concat!(
            "    ## Acceptance\n",
            "    - [x] indented example\n",
            "## Acceptance#\n",
            "- [x] hash is part of this title\n",
            "## Acceptance###\n",
            "- [x] hashes are part of this title\n",
            "## Acceptance ###\n",
            "- [ ] real outcome\n",
            "## Notes\n",
        );
        assert_eq!(
            acceptance_items(body),
            vec![AcceptanceItem {
                text: "real outcome".into(),
                checked: false,
            }]
        );
    }

    #[test]
    fn acceptance_requires_whitespace_after_fence_closer() {
        let body = concat!(
            "## Acceptance\n",
            "```markdown\n",
            "- [x] fenced example\n",
            "``` with trailing text\n",
            "- [x] still fenced\n",
            "````\n",
            "- [ ] visible outcome\n",
            "## Notes\n",
        );
        assert_eq!(
            acceptance_items(body),
            vec![AcceptanceItem {
                text: "visible outcome".into(),
                checked: false,
            }]
        );
    }

    #[test]
    fn acceptance_rejects_duplicate_sections_and_bad_input() {
        let duplicate = "## Acceptance\n- [ ] first\n## Acceptance\n- [ ] second\n";
        assert!(parse_acceptance_section(duplicate).is_err());
        assert!(replace_acceptance(
            duplicate,
            &[AcceptanceItem {
                text: "replacement".into(),
                checked: false,
            }]
        )
        .is_err());
        for input in [
            "",
            "\n  \n",
            "- [] missing state\n",
            "- [ ]missing separator\n",
            "plain text\n",
        ] {
            assert!(parse_acceptance_input(input).is_err(), "{input:?}");
        }
    }

    #[test]
    fn acceptance_replacement_preserves_body_and_inserts_when_absent() {
        let body = concat!(
            "Intro\r\n\r\n",
            "    ## Acceptance\r\n",
            "    - [x] indented code example\r\n",
            "    ```markdown\r\n",
            "    - [x] indented fenced example\r\n",
            "    ```\r\n",
            "## Acceptance\r\n",
            "- [ ] old\r\n\r\n",
            "## Notes\r\nKeep this\r\n",
        );
        let changed = replace_acceptance(
            body,
            &[
                AcceptanceItem {
                    text: "new one".into(),
                    checked: true,
                },
                AcceptanceItem {
                    text: "new two".into(),
                    checked: false,
                },
            ],
        )
        .unwrap();
        assert!(changed.contains("Intro\r\n\r\n"));
        assert!(changed.contains("    ## Acceptance\r\n    - [x] indented code example\r\n"));
        assert!(
            changed.contains("    ```markdown\r\n    - [x] indented fenced example\r\n    ```\r\n")
        );
        assert!(changed.contains("- [x] new one\r\n- [ ] new two\r\n"));
        assert!(changed.contains("## Notes\r\nKeep this\r\n"));
        let inserted = replace_acceptance(
            "Body\n",
            &[AcceptanceItem {
                text: "criterion".into(),
                checked: false,
            }],
        )
        .unwrap();
        assert!(inserted.ends_with("## Acceptance\n\n- [ ] criterion\n"));
    }
}
