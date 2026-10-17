//! Hosted report body for `hosted-cadence-local-assignment.v1`.
//!
//! The board accepts at most 8,192 UTF-8 bytes of request JSON and at most
//! 4,000 Unicode scalar values of `text` (Zod code-point length). Optional
//! `text` and `reported_head_sha` are omitted, not sent as null.
//!
//! `BoardHttp` uses ureq 3 `send_json` and `SendBody::from_json`, which
//! serializes with `serde_json::to_vec_pretty` (two-space indent, space
//! after each colon). The byte budget is that pretty encoding, counted
//! without building the body until the payload is known to fit. Manual
//! oversize fails here as `payload_too_large` and is not sent.
//! `bounded_report_text` only chooses a summary string; it does not write
//! the durable local result.

use crate::error::{Error, Result};
use serde_json::{json, Value};

use super::VERSION;

const MAX_BODY_BYTES: usize = 8192;
const MAX_TEXT_CODEPOINTS: usize = 4_000;

pub(super) struct ReportIdentity<'a> {
    pub organization_id: &'a str,
    pub claim_id: &'a str,
    pub generation: u64,
}

pub(super) fn report_body(
    identity: &ReportIdentity<'_>,
    state: &str,
    text: Option<&str>,
    sha: Option<&str>,
) -> Result<Value> {
    let text_escaped = match text {
        Some(text) => Some(measured_text(text)?),
        None => None,
    };
    let encoded = pretty_len(identity, state, text_escaped, sha);
    if encoded > MAX_BODY_BYTES {
        return Err(payload_too_large());
    }
    let body = assemble(identity, state, text, sha);
    debug_assert_eq!(
        encoded,
        serde_json::to_vec_pretty(&body)
            .map(|bytes| bytes.len())
            .unwrap_or(0)
    );
    Ok(body)
}

/// `reference` is copied into the truncation note only when the full text
/// does not fit. It is not parsed: a reserved message id or an assignment
/// id both work, including characters that JSON must escape. Those
/// escapes count toward the same 8,192-byte pretty body and 4,000-code-point
/// caps. The note does not claim a local message already stores the text.
pub(super) fn bounded_report_text(
    identity: &ReportIdentity<'_>,
    state: &str,
    sha: Option<&str>,
    reference: &str,
    text: &str,
) -> Result<String> {
    let fixed = pretty_len(identity, state, Some(0), sha);
    if fixed > MAX_BODY_BYTES {
        return Err(payload_too_large());
    }
    let budget = MAX_BODY_BYTES - fixed;
    let (notice_cps, notice_escaped, notice_bytes) = notice_widths(reference);
    let notice_fits = notice_cps <= MAX_TEXT_CODEPOINTS && notice_escaped <= budget;

    let mut codepoints = 0usize;
    let mut escaped = 0usize;
    let mut cut = 0usize;
    let mut entire = true;
    for (index, ch) in text.char_indices() {
        let next_cps = codepoints + 1;
        let next_escaped = escaped + escaped_char_len(ch);
        if next_cps > MAX_TEXT_CODEPOINTS || next_escaped > budget {
            entire = false;
            break;
        }
        codepoints = next_cps;
        escaped = next_escaped;
        if notice_fits
            && next_cps + notice_cps <= MAX_TEXT_CODEPOINTS
            && next_escaped + notice_escaped <= budget
        {
            cut = index + ch.len_utf8();
        }
    }
    if entire {
        return Ok(text.to_string());
    }
    if !notice_fits {
        return Err(payload_too_large());
    }
    let mut summary = String::with_capacity(cut + notice_bytes);
    summary.push_str(&text[..cut]);
    summary.push_str(NOTICE_PREFIX);
    summary.push_str(reference);
    summary.push_str(NOTICE_SUFFIX);
    Ok(summary)
}

const NOTICE_PREFIX: &str =
    "[truncated to fit the hosted report; inspect the local result or command error for ";
const NOTICE_SUFFIX: &str = "]";
const NOTICE_PREFIX_CPS: usize = 83;
const NOTICE_SUFFIX_CPS: usize = 1;
const NOTICE_PREFIX_ESCAPED: usize = 83;
const NOTICE_SUFFIX_ESCAPED: usize = 1;

fn notice_widths(reference: &str) -> (usize, usize, usize) {
    debug_assert_eq!(
        (NOTICE_PREFIX_CPS, NOTICE_PREFIX_ESCAPED),
        piece_widths(NOTICE_PREFIX)
    );
    debug_assert_eq!(
        (NOTICE_SUFFIX_CPS, NOTICE_SUFFIX_ESCAPED),
        piece_widths(NOTICE_SUFFIX)
    );
    let (reference_cps, reference_escaped) = piece_widths(reference);
    let codepoints = NOTICE_PREFIX_CPS
        .saturating_add(reference_cps)
        .saturating_add(NOTICE_SUFFIX_CPS);
    let escaped = NOTICE_PREFIX_ESCAPED
        .saturating_add(reference_escaped)
        .saturating_add(NOTICE_SUFFIX_ESCAPED);
    let bytes = NOTICE_PREFIX
        .len()
        .saturating_add(reference.len())
        .saturating_add(NOTICE_SUFFIX.len());
    (codepoints, escaped, bytes)
}

fn piece_widths(value: &str) -> (usize, usize) {
    let mut codepoints = 0usize;
    let mut escaped = 0usize;
    for ch in value.chars() {
        codepoints += 1;
        escaped = escaped.saturating_add(escaped_char_len(ch));
    }
    (codepoints, escaped)
}

#[cfg(test)]
fn truncation_notice(reference: &str) -> String {
    let mut notice =
        String::with_capacity(NOTICE_PREFIX.len() + reference.len() + NOTICE_SUFFIX.len());
    notice.push_str(NOTICE_PREFIX);
    notice.push_str(reference);
    notice.push_str(NOTICE_SUFFIX);
    notice
}

fn payload_too_large() -> Error {
    Error::invalid(
        "payload_too_large",
        "hosted report is over 8192 JSON bytes or 4000 text code points",
    )
}

fn assemble(
    identity: &ReportIdentity<'_>,
    state: &str,
    text: Option<&str>,
    sha: Option<&str>,
) -> Value {
    let mut body = json!({
        "version": VERSION,
        "organization_id": identity.organization_id,
        "claim_id": identity.claim_id,
        "expected_generation": identity.generation,
        "state": state,
    });
    if let Some(text) = text {
        body["text"] = json!(text);
    }
    if let Some(sha) = sha {
        body["reported_head_sha"] = json!(sha);
    }
    body
}

/// Escaped length of `text`, or `payload_too_large` once either producer
/// cap is already exceeded by the text alone.
fn measured_text(text: &str) -> Result<usize> {
    let mut codepoints = 0usize;
    let mut escaped = 0usize;
    for ch in text.chars() {
        codepoints += 1;
        if codepoints > MAX_TEXT_CODEPOINTS {
            return Err(payload_too_large());
        }
        escaped += escaped_char_len(ch);
        if escaped > MAX_BODY_BYTES {
            return Err(payload_too_large());
        }
    }
    Ok(escaped)
}

fn escaped_char_len(ch: char) -> usize {
    match ch {
        '"' | '\\' | '\u{0008}' | '\t' | '\n' | '\u{000c}' | '\r' => 2,
        control if (control as u32) < 0x20 => 6,
        other => other.len_utf8(),
    }
}

fn escaped_len(value: &str) -> usize {
    let mut len = 0usize;
    for &byte in value.as_bytes() {
        let add = match byte {
            0x08 | 0x09 | 0x0A | 0x0C | 0x0D | b'"' | b'\\' => 2,
            0x00..=0x1F => 6,
            _ => 1,
        };
        len = len.saturating_add(add);
    }
    len
}

fn decimal_digits(mut value: u64) -> usize {
    let mut digits = 1usize;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

/// Byte length of the pretty JSON object ureq will send.
fn pretty_len(
    identity: &ReportIdentity<'_>,
    state: &str,
    text_escaped: Option<usize>,
    sha: Option<&str>,
) -> usize {
    let mut fields = 5usize;
    let mut inner = 0usize;
    inner = inner.saturating_add(str_field("claim_id", escaped_len(identity.claim_id)));
    inner = inner.saturating_add(num_field(
        "expected_generation",
        decimal_digits(identity.generation),
    ));
    inner = inner.saturating_add(str_field(
        "organization_id",
        escaped_len(identity.organization_id),
    ));
    if let Some(sha) = sha {
        inner = inner.saturating_add(str_field("reported_head_sha", escaped_len(sha)));
        fields += 1;
    }
    inner = inner.saturating_add(str_field("state", escaped_len(state)));
    if let Some(escaped) = text_escaped {
        inner = inner.saturating_add(str_field("text", escaped));
        fields += 1;
    }
    inner = inner.saturating_add(str_field("version", escaped_len(VERSION)));
    // `{` + each field's newline and two-space indent + commas + `\n}`.
    let structural = fields.saturating_mul(4).saturating_add(2);
    inner.saturating_add(structural)
}

fn str_field(key: &str, escaped: usize) -> usize {
    key.len().saturating_add(6).saturating_add(escaped)
}

fn num_field(key: &str, digits: usize) -> usize {
    key.len() + 4 + digits
}

#[cfg(test)]
fn posted(body: &Value) -> Vec<u8> {
    serde_json::to_vec_pretty(body).expect("pretty report json")
}

#[cfg(test)]
fn text_room(identity: &ReportIdentity<'_>, state: &str, sha: Option<&str>) -> usize {
    let empty = report_body(identity, state, Some(""), sha).expect("empty text fits");
    MAX_BODY_BYTES - posted(&empty).len()
}

#[cfg(test)]
fn org_for_unit(unit_bytes: usize) -> String {
    let mut org = "o".repeat(4_200);
    loop {
        let identity = ReportIdentity {
            organization_id: &org,
            claim_id: "c",
            generation: 1,
        };
        let room = text_room(&identity, "result", None);
        if room.is_multiple_of(unit_bytes) {
            return org;
        }
        org.push('o');
    }
}

#[cfg(test)]
fn assert_unit_boundary(unit: char, unit_bytes: usize) {
    assert_eq!(escaped_char_len(unit), unit_bytes);
    let org = org_for_unit(unit_bytes);
    let identity = ReportIdentity {
        organization_id: &org,
        claim_id: "c",
        generation: 1,
    };
    let room = text_room(&identity, "result", None);
    let count = room / unit_bytes;
    assert!(count <= MAX_TEXT_CODEPOINTS);
    let fit: String = std::iter::repeat_n(unit, count).collect();
    let body = report_body(&identity, "result", Some(&fit), None).expect("just fits");
    assert_eq!(posted(&body).len(), MAX_BODY_BYTES);
    assert_eq!(body["text"].as_str(), Some(fit.as_str()));

    let over: String = std::iter::repeat_n(unit, count + 1).collect();
    let rejected = report_body(&identity, "result", Some(&over), None).expect_err("one over");
    assert_eq!(rejected.code(), Some("payload_too_large"));

    let message_id = "msg\"\\\u{0001}测";
    assert_eq!(
        bounded_report_text(&identity, "result", None, message_id, &fit).expect("exact fit"),
        fit
    );
    let source = over.clone();
    let summary = bounded_report_text(&identity, "result", None, message_id, &source)
        .expect("truncated summary");
    assert_eq!(source, over, "full local text is not modified");
    let notice = truncation_notice(message_id);
    let prefix = summary
        .strip_suffix(&notice)
        .expect("honest local-message reference");
    assert!(source.starts_with(prefix));
    assert!(prefix.len() < source.len());
    assert!(summary.chars().count() <= MAX_TEXT_CODEPOINTS);
    let accepted = report_body(&identity, "result", Some(&summary), None).expect("summary posts");
    assert!(posted(&accepted).len() <= MAX_BODY_BYTES);
    let next = source[prefix.len()..]
        .chars()
        .next()
        .expect("one more unit");
    let mut longer = prefix.to_string();
    longer.push(next);
    longer.push_str(&notice);
    assert!(
        report_body(&identity, "result", Some(&longer), None).is_err(),
        "summary keeps the longest prefix that still fits"
    );
}

#[test]
fn canonical_body_matches_v1_and_omits_absent_fields() {
    let identity = ReportIdentity {
        organization_id: "ws_org",
        claim_id: "hlc_claim",
        generation: 7,
    };
    let omitted = report_body(&identity, "result", None, None).expect("body");
    assert_eq!(
        String::from_utf8(posted(&omitted)).expect("utf-8"),
        format!(
            "{{\n  \"claim_id\": \"hlc_claim\",\n  \"expected_generation\": 7,\n  \
             \"organization_id\": \"ws_org\",\n  \"state\": \"result\",\n  \
             \"version\": \"{VERSION}\"\n}}"
        )
    );
    assert!(omitted.get("text").is_none());
    assert!(omitted.get("reported_head_sha").is_none());

    let empty = report_body(&identity, "result", Some(""), None).expect("empty text");
    assert_eq!(empty["text"].as_str(), Some(""));

    let text = "说\"\\\n\u{0001}";
    let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let full = report_body(&identity, "result", Some(text), Some(sha)).expect("full");
    assert_eq!(
        String::from_utf8(posted(&full)).expect("utf-8"),
        "{\n  \"claim_id\": \"hlc_claim\",\n  \"expected_generation\": 7,\n  \
         \"organization_id\": \"ws_org\",\n  \
         \"reported_head_sha\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\n  \
         \"state\": \"result\",\n  \"text\": \"说\\\"\\\\\\n\\u0001\",\n  \
         \"version\": \"hosted-cadence-local-assignment.v1\"\n}"
    );
    assert_eq!(full["text"].as_str(), Some(text));
    assert_eq!(full["reported_head_sha"].as_str(), Some(sha));
}

#[test]
fn manual_limits_hit_exact_byte_and_codepoint_boundaries() {
    assert_unit_boundary('a', 1);
    assert_unit_boundary('"', 2);
    assert_unit_boundary('\\', 2);
    assert_unit_boundary('\u{0001}', 6);
    assert_unit_boundary('测', 3);
    assert_unit_boundary('\u{10000}', 4);

    let identity = ReportIdentity {
        organization_id: "o",
        claim_id: "c",
        generation: 1,
    };
    let overhead =
        posted(&report_body(&identity, "result", Some(""), None).expect("overhead")).len();
    assert!(overhead + MAX_TEXT_CODEPOINTS < MAX_BODY_BYTES);
    let fit = "a".repeat(MAX_TEXT_CODEPOINTS);
    let body = report_body(&identity, "result", Some(&fit), None).expect("4000");
    assert_eq!(posted(&body).len(), overhead + MAX_TEXT_CODEPOINTS);
    assert_eq!(body["text"].as_str().unwrap().chars().count(), 4_000);
    let over = "a".repeat(MAX_TEXT_CODEPOINTS + 1);
    assert_eq!(
        report_body(&identity, "result", Some(&over), None)
            .expect_err("4001")
            .code(),
        Some("payload_too_large")
    );
    let summary =
        bounded_report_text(&identity, "result", None, "local-4001", &over).expect("summary");
    let notice = truncation_notice("local-4001");
    assert_eq!(&summary[summary.len() - notice.len()..], notice.as_str());
    assert!(summary.chars().count() <= MAX_TEXT_CODEPOINTS);
    let accepted = report_body(&identity, "result", Some(&summary), None).expect("fits");
    assert!(posted(&accepted).len() <= MAX_BODY_BYTES);
    assert_ne!(summary, over);
}

#[test]
fn metadata_and_escaping_consume_the_same_byte_budget() {
    let org = org_for_unit(1);
    let identity = ReportIdentity {
        organization_id: &org,
        claim_id: "c",
        generation: 9,
    };
    let room = text_room(&identity, "result", None);
    let text = "a".repeat(room);
    assert_eq!(
        posted(&report_body(&identity, "result", Some(&text), None).expect("gen 9")).len(),
        MAX_BODY_BYTES
    );
    let wider = ReportIdentity {
        organization_id: &org,
        claim_id: "c",
        generation: 10,
    };
    assert_eq!(
        report_body(&wider, "result", Some(&text), None)
            .expect_err("extra generation digit")
            .code(),
        Some("payload_too_large")
    );

    let sha = "b".repeat(40);
    let with_sha = text_room(&identity, "result", Some(&sha));
    assert_eq!(room - with_sha, 27 + sha.len());
    let sha_text = "a".repeat(with_sha);
    assert_eq!(
        posted(
            &report_body(&identity, "result", Some(&sha_text), Some(&sha)).expect("sha just fits")
        )
        .len(),
        MAX_BODY_BYTES
    );
    assert!(report_body(&identity, "result", Some(&text), Some(&sha)).is_err());

    let quoted = ReportIdentity {
        organization_id: &org,
        claim_id: "c\"c",
        generation: 9,
    };
    assert_eq!(
        report_body(&quoted, "result", Some(&text), None)
            .expect_err("escaped claim id")
            .code(),
        Some("payload_too_large")
    );
    let longer_org = format!("{org}o");
    let grown = ReportIdentity {
        organization_id: &longer_org,
        claim_id: "c",
        generation: 9,
    };
    assert!(report_body(&grown, "result", Some(&text), None).is_err());

    let probe = "o".repeat(7_000);
    let probe_id = ReportIdentity {
        organization_id: &probe,
        claim_id: "c",
        generation: 1,
    };
    let bare = posted(&report_body(&probe_id, "result", None, None).expect("bare")).len();
    let exact_org = "o".repeat(7_000 + MAX_BODY_BYTES - bare);
    let exact = ReportIdentity {
        organization_id: &exact_org,
        claim_id: "c",
        generation: 1,
    };
    assert_eq!(
        posted(&report_body(&exact, "result", None, None).expect("metadata just fits")).len(),
        MAX_BODY_BYTES
    );
    let mut one_over = exact_org.clone();
    one_over.push('o');
    let over = ReportIdentity {
        organization_id: &one_over,
        claim_id: "c",
        generation: 1,
    };
    assert_eq!(
        report_body(&over, "result", None, None)
            .expect_err("metadata one over")
            .code(),
        Some("payload_too_large")
    );
    assert!(report_body(&exact, "result", Some(""), None).is_err());
    assert_eq!(
        bounded_report_text(&exact, "result", None, "m", "")
            .expect_err("no room for a text field")
            .code(),
        Some("payload_too_large")
    );
}

#[test]
fn automatic_summary_skips_the_notice_until_text_needs_it() {
    let identity = ReportIdentity {
        organization_id: "o",
        claim_id: "c",
        generation: 1,
    };
    let kept = "  \t说\"\\";
    assert_eq!(
        bounded_report_text(&identity, "blocked", None, "unused-id", kept).expect("fits"),
        kept
    );
    let huge_id = "m".repeat(5_000);
    assert_eq!(
        bounded_report_text(&identity, "blocked", None, &huge_id, kept).expect("notice unused"),
        kept
    );
    let over = "a".repeat(MAX_TEXT_CODEPOINTS + 1);
    assert_eq!(
        bounded_report_text(&identity, "blocked", None, &huge_id, &over)
            .expect_err("notice cannot fit")
            .code(),
        Some("payload_too_large")
    );
}
