//! Wiki page reading, historical revision reads, and bounded range slicing.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::Pm;

use super::blob::blob_read_range;
use super::{
    admitted, allowed, blob_meta, classify, memory_file, no_such_page, normalize, profile_file,
    resolve, rev_bytes, rev_of, revision, transfer, vault_dir, Caller, Op, View,
    NATIVE_TOOL_LINE_BOUND, TEXT_CAP, VIEW_CAP,
};

/// One bounded read of an original text page. `offset` is a UTF-8 byte
/// index. `bytes` is the maximum length; `None` returns the longest
/// slice from `offset` that fits [`NATIVE_TOOL_LINE_BOUND`]. `rev`,
/// when set, must be the original page revision — a mismatch returns
/// no body.
pub struct BodyRange<'a> {
    pub offset: u64,
    pub bytes: Option<u64>,
    pub rev: Option<&'a str>,
}

struct TextPage {
    path: String,
    text: String,
    /// Revision of the whole original, not of the returned slice.
    rev: String,
}

enum Opened {
    Dir { path: String },
    Blob(Value),
    Text(TextPage),
}

/// `wiki read` — text as `{kind:"text",text,rev}`; a blob page as
/// `{kind:"blob",sha256,size,mime,name,rev}` (rev = the pointer's);
/// a directory as `{kind:"dir"}`. View paths read their source.
/// The text `rev` is the original body's revision.
pub fn read(pm: &Pm, caller: &Caller, path: &str) -> Result<Value> {
    match open_page(pm, caller, path)? {
        Opened::Dir { path } => Ok(json!({"path": path, "kind": "dir"})),
        Opened::Blob(value) => Ok(value),
        Opened::Text(page) => Ok(json!({
            "path": page.path,
            "kind": "text",
            "text": page.text,
            "rev": page.rev,
            "sha256": transfer::sha256_bytes(page.text.as_bytes()),
        })),
    }
}

/// Read `path` at `at`. This does not call [`read`] when the selector
/// misses: an unknown, ambiguous, or non-commit name is an error.
pub fn read_revision(
    pm: &Pm,
    caller: &Caller,
    path: &str,
    at: &str,
    range: Option<BodyRange<'_>>,
) -> Result<Value> {
    if !admitted(pm, caller, path) {
        return Err(no_such_page(path));
    }
    let pinned = revision::pin(pm, caller, path, at)?;
    let mut value = match (&pinned.body, range) {
        (revision::Body::Dir, Some(_)) => {
            return Err(Error::rejected(format!(
                "wiki revision '{path}' refused: a range read needs a text page or blob"
            )))
        }
        (revision::Body::Dir, None) => json!({
            "path": pinned.path,
            "kind": "dir",
            "rev": Value::Null,
            "sha256": Value::Null,
        }),
        (revision::Body::Text(text), None) => json!({
            "path": pinned.path,
            "kind": "text",
            "text": text,
            "rev": pinned.rev,
            "sha256": pinned.sha256,
        }),
        (revision::Body::Text(text), Some(range)) => {
            let page = TextPage {
                path: pinned.path.clone(),
                text: text.clone(),
                rev: pinned.rev.clone(),
            };
            text_read_range(page, range)?
        }
        (revision::Body::Blob(blob), None) => json!({
            "path": pinned.path,
            "kind": "blob",
            "sha256": blob.sha256,
            "size": blob.size,
            "mime": blob.mime,
            "name": blob.name,
            "rev": pinned.rev,
        }),
        (revision::Body::Blob(blob), Some(range)) => {
            let meta = json!({
                "path": pinned.path,
                "kind": "blob",
                "sha256": blob.sha256,
                "size": blob.size,
                "mime": blob.mime,
                "name": blob.name,
                "rev": pinned.rev,
            });
            blob_read_range(pm, &meta, range)?
        }
    };
    if let Some(obj) = value.as_object_mut() {
        // `rev` is the selector the caller asked for. A commit name is not
        // rewritten to the live FNV token, or the board cannot tell this
        // page from latest. `git` stays the resolved commit.
        obj.insert("rev".to_string(), json!(at));
        obj.insert("at".to_string(), json!(at));
        obj.insert("git".to_string(), json!(pinned.git));
        if obj.get("sha256").is_none() {
            obj.insert("sha256".to_string(), json!(pinned.sha256));
        }
    }
    if value.get("range").is_some() || value.get("data_base64").is_some() {
        let len = serde_json::to_vec(&value)
            .map(|body| body.len())
            .unwrap_or(usize::MAX);
        if len.saturating_add(1) >= NATIVE_TOOL_LINE_BOUND {
            return Err(Error::rejected(
                "wiki revision refused: the historical range does not fit the native tool line bound",
            ));
        }
    }
    Ok(value)
}

/// Historical Wiki original for memory evidence, bounded before any read.
///
/// Same authorization and sandbox path as [`read_revision`], with these
/// restrictions that keep the read answerable inside a fixed budget:
///
/// - `at` MUST be an explicit 40-hex commit selector. An `fnv1a:` token is
///   never resolved here (that resolution inspects up to [`super::HISTORY_CAP`]
///   historical bodies), and an unknown or other name is refused — never
///   substituted with the newest commit.
/// - A text original whose Git blob size is above `max_bytes` (or above
///   [`TEXT_CAP`]) is refused from the size probe alone; its content is never
///   read.
/// - A blob page whose pointer records a size above `max_bytes` is refused
///   before its content-addressed object is opened or read. Within the bound
///   the object is opened once under the store's custody checks, at most
///   `size + 1` bytes are read, the length must match the pointer, and the
///   digest is verified over exactly the bytes returned.
///
/// A miss never falls back to the live page. `Ok(None)` is the
/// definite-absence answer: the pinned commit was read and demonstrably holds
/// neither a text page nor a blob pointer at that path. An unknown or
/// non-commit selector, an ACL or view refusal, a directory, an over-limit
/// original, and every custody failure stay `Err`.
///
/// A text reply is `{path, kind:"text", text, rev, at, git, sha256}`; a blob
/// reply is `{path, kind:"blob", sha256, size, mime, name, data_base64, rev,
/// at, git}`. `rev`/`at` are the requested commit; `sha256` is the digest of
/// the returned original (the text bytes, or the verified blob bytes).
pub fn read_revision_for_evidence(
    pm: &Pm,
    caller: &Caller,
    path: &str,
    at: &str,
    max_bytes: usize,
) -> Result<Option<Value>> {
    let Some(found) = revision::evidence_text(pm, caller, path, at, max_bytes)? else {
        return Ok(None);
    };
    Ok(Some(match found.body {
        revision::EvidenceBody::Text(text) => json!({
            "path": found.path,
            "kind": "text",
            "text": text,
            "rev": at,
            "at": at,
            "git": found.git,
            "sha256": found.sha256,
        }),
        revision::EvidenceBody::Blob {
            size,
            mime,
            name,
            data_base64,
        } => json!({
            "path": found.path,
            "kind": "blob",
            "sha256": found.sha256,
            "size": size,
            "mime": mime,
            "name": name,
            "data_base64": data_base64,
            "rev": at,
            "at": at,
            "git": found.git,
        }),
    }))
}

/// Bounded original-body read. Same authorization and sandbox path as
/// [`read`]. A text page adds `text` and `range`. A blob page keeps the
/// pointer metadata and adds `range` plus `data_base64` (standard
/// base64 of that byte slice). A directory is refused. The reply does
/// not copy the page or raise [`TEXT_CAP`].
pub fn read_range(pm: &Pm, caller: &Caller, path: &str, range: BodyRange<'_>) -> Result<Value> {
    match open_page(pm, caller, path)? {
        Opened::Text(page) => text_read_range(page, range),
        Opened::Blob(meta) => blob_read_range(pm, &meta, range),
        Opened::Dir { .. } => Err(Error::rejected(format!(
            "wiki read '{path}' refused: a range read needs a text page or blob — \
             directories have no original text range"
        ))),
    }
}

fn text_read_range(page: TextPage, range: BodyRange<'_>) -> Result<Value> {
    if let Some(want) = range.rev {
        if want != page.rev {
            return Err(Error::rejected(format!(
                "wiki read '{}' refused: revision '{want}' is not the original \
                 revision '{}' — ranges are not combined across revisions",
                page.path, page.rev
            )));
        }
    }
    let total = page.text.len() as u64;
    if range.offset > total {
        return Err(Error::rejected(format!(
            "wiki read '{}' refused: offset {} is past the original {total} bytes",
            page.path, range.offset
        )));
    }
    let start = range.offset as usize;
    if !page.text.is_char_boundary(start) {
        return Err(Error::rejected(format!(
            "wiki read '{}' refused: offset {start} is inside a UTF-8 character",
            page.path
        )));
    }
    let end = match range.bytes {
        Some(n) => explicit_end(&page, start, n)?,
        None => fitting_end(&page, start)?,
    };
    Ok(ranged_value(&page, start, end))
}

/// Optional JSON integer field. Absent is `None`. A float, a negative,
/// or a non-number is refused — never coerced.
pub fn json_u64(params: &Value, key: &str) -> Result<Option<u64>> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_u64()
            .ok_or_else(|| {
                Error::rejected(format!(
                    "wiki read refused: '{key}' must be a non-negative integer"
                ))
            })
            .map(Some),
        Some(_) => Err(Error::rejected(format!(
            "wiki read refused: '{key}' must be a non-negative integer"
        ))),
    }
}

fn open_page(pm: &Pm, caller: &Caller, path: &str) -> Result<Opened> {
    if !admitted(pm, caller, path) {
        return Err(no_such_page(path));
    }
    let norm = normalize(path)?;
    if norm.is_empty() {
        return Err(Error::rejected(
            "wiki read '' refused: the root is a directory",
        ));
    }
    let segs: Vec<&str> = norm.split('/').collect();
    allowed(pm, caller, Op::Read, &segs)?;
    let vault = vault_dir(pm)?;

    match classify(&segs) {
        View::Profile { alias } => {
            if segs.len() != 4 {
                return Err(Error::rejected(format!(
                    "wiki read '{norm}' refused: profile/ reads are profile/<file>"
                )));
            }
            let file = profile_file(pm, &alias, segs[3])?;
            let meta = std::fs::metadata(&file)?;
            if meta.len() > VIEW_CAP {
                return Err(Error::rejected(format!(
                    "wiki read '{norm}' refused: file over the {VIEW_CAP}-byte cap"
                )));
            }
            let text = std::fs::read_to_string(&file)
                .map_err(|e| Error::rejected(format!("wiki read '{norm}': {e}")))?;
            return Ok(text_page(norm, text));
        }
        View::Memory { alias } => {
            if segs.len() != 5 {
                return Err(Error::rejected(format!(
                    "wiki read '{norm}' refused: memory/ reads are \
                     memory/<project>/<lesson>.md"
                )));
            }
            let file = memory_file(pm, &alias, segs[3], segs[4])?;
            let text = std::fs::read_to_string(&file)
                .map_err(|e| Error::rejected(format!("wiki read '{norm}': {e}")))?;
            return Ok(text_page(norm, text));
        }
        View::Real => {}
    }

    let abs = resolve(&vault, &norm)?;
    match abs.symlink_metadata() {
        Ok(m) if m.is_dir() => Ok(Opened::Dir { path: norm }),
        Ok(m) if m.is_file() => {
            if m.len() > TEXT_CAP {
                return Err(Error::rejected(format!(
                    "wiki read '{norm}' refused: file over the {TEXT_CAP}-byte cap"
                )));
            }
            let text = std::fs::read_to_string(&abs).map_err(|_| {
                Error::rejected(format!(
                    "wiki read '{norm}' refused: not UTF-8 text (is it a blob?)"
                ))
            })?;
            Ok(text_page(norm, text))
        }
        Ok(_) => Err(Error::rejected(format!("wiki read '{norm}': not a file"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A blob page: <path>.blob holds the pointer.
            let pointer = vault.join(format!("{norm}.blob"));
            match pointer.symlink_metadata() {
                Ok(m) if m.is_file() && !m.file_type().is_symlink() => {
                    let meta = blob_meta(&pointer)?;
                    Ok(Opened::Blob(json!({
                        "path": norm,
                        "kind": "blob",
                        "sha256": meta.get("sha256").cloned().unwrap_or(Value::Null),
                        "size": meta.get("size").cloned().unwrap_or(Value::Null),
                        "mime": meta.get("mime").cloned().unwrap_or(Value::Null),
                        "name": meta.get("name").cloned().unwrap_or(Value::Null),
                        "rev": rev_of(&pointer)?,
                    })))
                }
                _ => Err(Error::rejected(format!("wiki read '{norm}': no such file"))),
            }
        }
        Err(e) => Err(e.into()),
    }
}

fn text_page(path: String, text: String) -> Opened {
    let rev = rev_bytes(text.as_bytes());
    Opened::Text(TextPage { path, text, rev })
}

fn ranged_value(page: &TextPage, start: usize, end: usize) -> Value {
    json!({
        "path": page.path,
        "kind": "text",
        "rev": page.rev,
        "text": &page.text[start..end],
        "range": {
            "start": start,
            "end": end,
            "total": page.text.len(),
        }
    })
}

pub(super) fn oversize(path: &str) -> Error {
    Error::rejected(format!(
        "wiki read '{path}' refused: the range exceeds the \
         {NATIVE_TOOL_LINE_BOUND}-byte native tool line bound"
    ))
}

/// Bytes one UTF-8 byte contributes inside a `serde_json` string.
/// Controls use the short escapes (`\n`, `\r`, `\t`, `\b`, `\f`); every
/// other byte below 0x20 is `\u00XX`.
fn json_byte_escape_len(b: u8) -> usize {
    match b {
        b'"' | b'\\' | 0x08 | b'\t' | b'\n' | 0x0c | b'\r' => 2,
        0x00..=0x1F => 6,
        _ => 1,
    }
}

pub(super) fn json_string_len(s: &str) -> usize {
    let mut n = 2;
    for &b in s.as_bytes() {
        n += json_byte_escape_len(b);
    }
    n
}

fn decimal_len(n: usize) -> usize {
    if n == 0 {
        1
    } else {
        (n.ilog10() as usize) + 1
    }
}

/// Compact length of [`ranged_value`], matching `serde_json::to_string`.
/// `json!` maps sort keys, so the line is
/// `{"kind":"text","path":…,"range":{"end":…,"start":…,"total":…},"rev":…,"text":…}`.
fn compact_line_len(page: &TextPage, start: usize, end: usize) -> usize {
    "{\"kind\":\"text\",\"path\":".len()
        + json_string_len(&page.path)
        + ",\"range\":{\"end\":".len()
        + decimal_len(end)
        + ",\"start\":".len()
        + decimal_len(start)
        + ",\"total\":".len()
        + decimal_len(page.text.len())
        + "},\"rev\":".len()
        + json_string_len(&page.rev)
        + ",\"text\":".len()
        + json_string_len(&page.text[start..end])
        + "}".len()
}

fn line_fits(len: usize) -> bool {
    len + 1 < NATIVE_TOOL_LINE_BOUND
}

fn explicit_end(page: &TextPage, start: usize, bytes: u64) -> Result<usize> {
    let mut end = start.saturating_add(usize::try_from(bytes).unwrap_or(usize::MAX));
    if end > page.text.len() {
        end = page.text.len();
    }
    if !page.text.is_char_boundary(end) {
        return Err(Error::rejected(format!(
            "wiki read '{}' refused: range end {end} is inside a UTF-8 character",
            page.path
        )));
    }
    // The JSON line is longer than the raw slice, so a slice that already
    // meets the line bound cannot fit.
    if end - start + 1 >= NATIVE_TOOL_LINE_BOUND || !line_fits(compact_line_len(page, start, end)) {
        return Err(oversize(&page.path));
    }
    Ok(end)
}

/// Largest char-boundary end at or after `start` whose compact JSON
/// line fits the native tool bound. One forward pass; escaped length
/// is accumulated, not recomputed, and no JSON string is built.
fn fitting_end(page: &TextPage, start: usize) -> Result<usize> {
    let text = page.text.as_str();
    let limit = start
        .saturating_add(NATIVE_TOOL_LINE_BOUND.saturating_sub(1))
        .min(text.len());
    let base = compact_line_len(page, start, start) - decimal_len(start);
    let mut end = start;
    let mut escaped = 0usize;
    let mut i = start;
    while i < limit {
        let ch_end = i + text[i..].chars().next().map(char::len_utf8).unwrap_or(0);
        if ch_end > limit {
            break;
        }
        let mut add = 0;
        for &b in &text.as_bytes()[i..ch_end] {
            add += json_byte_escape_len(b);
        }
        let line = base + decimal_len(ch_end) + escaped + add;
        if !line_fits(line) {
            break;
        }
        escaped += add;
        end = ch_end;
        i = ch_end;
    }
    let line = base + decimal_len(end) + escaped;
    if !line_fits(line) {
        return Err(oversize(&page.path));
    }
    debug_assert_eq!(
        line,
        serde_json::to_string(&ranged_value(page, start, end))
            .map(|encoded| encoded.len())
            .unwrap_or(usize::MAX)
    );
    Ok(end)
}
