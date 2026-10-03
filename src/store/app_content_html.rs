//! Host sanitiser for operator-pasted email HTML (CAD-1056).
//!
//! The operator may paste HTML; the host never trusts it. Untrusted
//! input passes through an allowlist (ammonia, an html5ever-based
//! sanitiser) and then through host-owned passes the allowlist cannot
//! express: link and image URL schemes, tracking-pixel removal and
//! footer-spoof refusal. The stored value is the sanitised output;
//! the raw paste is never kept. The unsubscribe footer is appended by
//! the renderer after this fragment, so the fragment can neither
//! remove nor impersonate it.
use super::app_content::FOOTER_NOTE;
use super::*;
use std::collections::{HashMap, HashSet};

/// Raw pasted HTML bound (bytes).
pub const HTML_INPUT_BYTES: usize = 200_000;
/// Sanitised HTML bound (bytes) — what is stored and rendered.
pub const HTML_STORED_BYTES: usize = 100_000;
/// Plain-text override bound (bytes).
pub const TEXT_OVERRIDE_BYTES: usize = 30_000;

const REFUSED: &str = "email HTML exceeds its supported shape or bounds";

const TAGS: &[&str] = &[
    "a",
    "p",
    "br",
    "hr",
    "div",
    "span",
    "h1",
    "h2",
    "h3",
    "h4",
    "ul",
    "ol",
    "li",
    "strong",
    "b",
    "em",
    "i",
    "u",
    "small",
    "blockquote",
    "table",
    "thead",
    "tbody",
    "tr",
    "td",
    "th",
    "img",
];

/// Elements whose content is dropped with the tag (the rest keep
/// their text and lose only the markup).
const CLEAN_CONTENT: &[&str] = &[
    "script",
    "style",
    "iframe",
    "object",
    "embed",
    "noscript",
    "template",
    "svg",
    "math",
    "applet",
    "frame",
    "frameset",
    "textarea",
    "select",
    "option",
    "head",
    "title",
    "xmp",
    "noembed",
    "noframes",
    "plaintext",
];

const STYLE_PROPERTIES: &[&str] = &[
    "color",
    "background-color",
    "font-size",
    "font-weight",
    "font-style",
    "text-align",
    "text-decoration",
    "line-height",
    "margin",
    "padding",
    "width",
    "max-width",
];

fn scheme_ok(value: &str, schemes: &[&str]) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    !lower.chars().any(|c| c.is_control() || c.is_whitespace())
        && schemes.iter().any(|s| lower.starts_with(s))
}

fn filter_attribute<'u>(
    tag: &str,
    name: &str,
    value: &'u str,
) -> Option<std::borrow::Cow<'u, str>> {
    let keep = match (tag, name) {
        ("a", "href") => scheme_ok(value, &["https://", "http://", "mailto:"]),
        ("img", "src") => scheme_ok(value, &["https://"]),
        (_, "style") => {
            let lower = value.to_ascii_lowercase();
            ![
                "url(",
                "expression",
                "javascript",
                "@import",
                "\\",
                "/*",
                "&",
            ]
            .iter()
            .any(|bad| lower.contains(bad))
        }
        _ => true,
    };
    keep.then(|| value.into())
}

fn builder() -> ammonia::Builder<'static> {
    let mut b = ammonia::Builder::empty();
    b.tags(TAGS.iter().copied().collect::<HashSet<_>>())
        .clean_content_tags(CLEAN_CONTENT.iter().copied().collect::<HashSet<_>>())
        .tag_attributes(
            [
                ("a", ["href", "title"].into_iter().collect::<HashSet<_>>()),
                (
                    "img",
                    ["src", "alt", "width", "height"].into_iter().collect(),
                ),
                (
                    "td",
                    ["colspan", "rowspan", "align", "valign"]
                        .into_iter()
                        .collect(),
                ),
                (
                    "th",
                    ["colspan", "rowspan", "align", "valign"]
                        .into_iter()
                        .collect(),
                ),
                (
                    "table",
                    ["width", "align", "cellpadding", "cellspacing", "border"]
                        .into_iter()
                        .collect(),
                ),
            ]
            .into_iter()
            .collect::<HashMap<_, _>>(),
        )
        .generic_attributes(["style"].into_iter().collect())
        .filter_style_properties(STYLE_PROPERTIES.iter().copied().collect())
        .url_schemes(["https", "http", "mailto"].into_iter().collect())
        .url_relative(ammonia::UrlRelative::Deny)
        .link_rel(Some("noopener noreferrer nofollow"))
        .strip_comments(true)
        .attribute_filter(filter_attribute);
    b
}

/// Value of `name="..."` inside one serialised start tag.
fn attr<'t>(tag: &'t str, name: &str) -> Option<&'t str> {
    let needle = format!(" {name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')?;
    Some(&tag[start..start + end])
}

fn is_pixel(tag: &str) -> bool {
    ["width", "height"].iter().any(|name| {
        attr(tag, name)
            .and_then(|v| v.trim_end_matches("px").parse::<f64>().ok())
            .is_some_and(|n| n <= 1.0)
    })
}

/// Drop every `<img>` the allowlist left behind that has no `https`
/// source or is a 1x1 (or smaller) tracking pixel. The input is
/// ammonia's own serialisation, so attributes are double-quoted and
/// `>` never appears inside a value.
fn drop_unsafe_images(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find("<img") {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let Some(end) = tail.find('>') else {
            // Unterminated tag: nothing safe to keep.
            return out;
        };
        let tag = &tail[..=end];
        let keep = attr(tag, "src").is_some_and(|s| s.starts_with("https://")) && !is_pixel(tag);
        if keep {
            out.push_str(tag);
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Sanitise pasted HTML into the stored fragment. Refusals never echo
/// content.
pub fn sanitize_html(input: &str) -> Result<String> {
    if input.trim().is_empty() || input.len() > HTML_INPUT_BYTES {
        return Err(Error::rejected(REFUSED));
    }
    let clean = builder().clean(input).to_string();
    let clean = drop_unsafe_images(&clean);
    let clean = clean.trim().to_string();
    if clean.is_empty() || clean.len() > HTML_STORED_BYTES {
        return Err(Error::rejected(REFUSED));
    }
    refuse_footer_spoof(&clean)?;
    Ok(clean)
}

/// The host owns the unsubscribe footer: operator content may not
/// carry the host footer sentence. Links to the host's own
/// unsubscribe endpoint are refused by [`refuse_host_unsubscribe`],
/// which needs the live binding and origin facts.
fn refuse_footer_spoof(html: &str) -> Result<()> {
    if html
        .to_ascii_lowercase()
        .contains(&FOOTER_NOTE.to_ascii_lowercase())
    {
        return Err(Error::rejected(SPOOF));
    }
    Ok(())
}

const SPOOF: &str = "email content may not carry its own unsubscribe footer; the host adds it";

/// One host-built unsubscribe URL shape, normalised for comparison.
#[derive(Clone, Debug)]
pub struct HostEndpoint {
    host: String,
    path: String,
    /// `true`: every path under `path` is the endpoint (the send-time
    /// `{origin}/unsubscribe/<token>` family). `false`: only `path`
    /// itself, and an empty path only counts with a `token` query.
    under: bool,
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(v) = hex {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_ascii_lowercase()
}

/// Fully decode (three rounds, so double and triple encoding cannot
/// hide a path) and lowercase.
fn decode_all(text: &str) -> String {
    let mut cur = text.to_string();
    for _ in 0..3 {
        let next = percent_decode(&cur);
        if next == cur.to_ascii_lowercase() {
            return next;
        }
        cur = next;
    }
    cur
}

/// Split an absolute `http(s)` URL into (host, path, query), all
/// decoded and lowercased; the path loses repeated and trailing
/// slashes, the host loses a default port.
fn split_url(url: &str) -> Option<(String, String, String)> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))?;
    let rest = rest.split('#').next().unwrap_or("");
    let (head, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (host, path) = head.split_at(head.find('/').unwrap_or(head.len()));
    let host = decode_all(host.rsplit('@').next().unwrap_or(""));
    let host = host
        .strip_suffix(":443")
        .or_else(|| host.strip_suffix(":80"))
        .unwrap_or(&host)
        .to_string();
    let mut path = decode_all(path).replace('\\', "/");
    while path.contains("//") {
        path = path.replace("//", "/");
    }
    let path = path.trim_end_matches('/').to_string();
    Some((host, path, decode_all(query)))
}

impl HostEndpoint {
    /// A saved binding (or preview) base: `{base}?token=RECIPIENT`.
    pub fn binding(base: &str) -> Option<Self> {
        let (host, path, _) = split_url(base)?;
        Some(Self {
            host,
            path,
            under: false,
        })
    }

    /// The configured send origin: `{origin}/unsubscribe/<token>`.
    pub fn origin(origin: &str) -> Option<Self> {
        let (host, path, _) = split_url(origin)?;
        Some(Self {
            host,
            path: format!("{path}/unsubscribe"),
            under: true,
        })
    }

    fn matches(&self, host: &str, path: &str, query: &str) -> bool {
        if host != self.host {
            return false;
        }
        if self.under {
            return path == self.path || path.starts_with(&format!("{}/", self.path));
        }
        if self.path.is_empty() {
            return path.is_empty() && query.contains("token=");
        }
        path == self.path
    }
}

fn entity_decode(href: &str) -> String {
    href.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Refuse operator links that target the host's own unsubscribe
/// endpoint (same host, any scheme, case, percent-encoding, trailing
/// slash or query) or carry the host's per-recipient placeholder.
/// Ordinary third-party links are untouched.
pub fn refuse_host_unsubscribe(html: &str, endpoints: &[HostEndpoint]) -> Result<()> {
    let mut rest = html;
    while let Some(at) = rest.find("<a ") {
        let tail = &rest[at..];
        let end = tail.find('>').unwrap_or(tail.len());
        if let Some(href) = attr(&tail[..end], "href") {
            if let Some((host, path, query)) = split_url(&entity_decode(href)) {
                let marker = query.split('&').any(|pair| {
                    pair.split_once('=')
                        .is_some_and(|(k, v)| k == "token" && v == "recipient")
                });
                if marker || endpoints.iter().any(|e| e.matches(&host, &path, &query)) {
                    return Err(Error::rejected(SPOOF));
                }
            }
        }
        rest = &tail[end..];
    }
    Ok(())
}

/// Plain-text alternative generated from sanitised HTML: tags drop,
/// block boundaries become newlines, links keep their target.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::new();
    let mut href: Option<String> = None;
    let mut rest = html;
    while let Some(at) = rest.find('<') {
        out.push_str(&decode(&rest[..at]));
        let tail = &rest[at..];
        let end = tail.find('>').map_or(tail.len(), |e| e + 1);
        let tag = &tail[..end];
        let name: String = tag
            .trim_start_matches(['<', '/'])
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        let closing = tag.starts_with("</");
        match (name.as_str(), closing) {
            ("a", false) => href = attr(tag, "href").map(decode),
            ("a", true) => {
                if let Some(target) = href.take() {
                    out.push_str(" (");
                    out.push_str(&target);
                    out.push(')');
                }
            }
            ("br", _)
            | ("hr", _)
            | ("p" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4", true) => out.push('\n'),
            ("td" | "th", true) => out.push(' '),
            _ => {}
        }
        rest = &tail[end..];
    }
    out.push_str(&decode(rest));
    let mut lines: Vec<&str> = out.lines().map(str::trim_end).collect();
    lines.dedup_by(|a, b| a.is_empty() && b.is_empty());
    lines.join("\n").trim().to_string()
}

fn decode(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(input: &str) -> String {
        sanitize_html(input).unwrap()
    }

    fn assert_inert(out: &str) {
        let lower = out.to_ascii_lowercase();
        for bad in [
            "<script",
            "<style",
            "<iframe",
            "<object",
            "<embed",
            "<form",
            "<input",
            "<button",
            "<meta",
            "<svg",
            "<link",
            "onerror",
            "onload",
            "onclick",
            "javascript:",
            "data:",
            "vbscript:",
            "alert(",
            "<!--",
            "srcdoc",
        ] {
            assert!(!lower.contains(bad), "{bad} survived in {out}");
        }
    }

    #[test]
    fn xss_vector_corpus_is_neutralised() {
        for vector in [
            "<script>alert(1)</script><p>ok</p>",
            "<p onclick=\"alert(1)\">ok</p>",
            "<img src=\"https://e.example/a.png\" onerror=\"alert(1)\" alt=x>",
            "<svg onload=alert(1)><circle/></svg><p>ok</p>",
            "<a href=\"javascript:alert(1)\">x</a><p>ok</p>",
            "<a href=\"  JaVaScRiPt:alert(1)\">x</a><p>ok</p>",
            "<a href=\"java&#x0A;script:alert(1)\">x</a><p>ok</p>",
            "<a href=\"&#106;avascript:alert(1)\">x</a><p>ok</p>",
            "<a href=\"data:text/html;base64,PHNjcmlwdD4=\">x</a><p>ok</p>",
            "<img src=\"data:image/png;base64,AAAA\"><p>ok</p>",
            "<iframe src=\"https://e.example\"></iframe><p>ok</p>",
            "<iframe srcdoc=\"<script>alert(1)</script>\"></iframe><p>ok</p>",
            "<object data=\"x.swf\"></object><embed src=\"x.swf\"><p>ok</p>",
            "<form action=\"https://e.example\"><input name=a><button>go</button></form><p>ok</p>",
            "<meta http-equiv=\"refresh\" content=\"0;url=https://e.example\"><p>ok</p>",
            "<style>p{background:url(javascript:alert(1))}</style><p>ok</p>",
            "<p style=\"background:url(javascript:alert(1))\">ok</p>",
            "<p style=\"position:fixed;top:0;left:0;width:100%;height:100%\">ok</p>",
            "<!--[if mso]><script>alert(1)</script><![endif]--><p>ok</p>",
            "<!-- <img src=x onerror=alert(1)> --><p>ok</p>",
            "<noscript><p title=\"</noscript><img src=x onerror=alert(1)>\"></noscript><p>ok</p>",
            "<math><mtext><style><img src=x onerror=alert(1)></style></mtext></math><p>ok</p>",
            "<base href=\"https://e.example/\"><link rel=stylesheet href=\"https://e.example/x.css\"><p>ok</p>",
        ] {
            let out = clean(vector);
            assert_inert(&out);
            assert!(out.contains("ok") || out.contains('x') || out.contains("<a"), "{vector} -> {out}");
        }
    }

    #[test]
    fn links_allow_http_https_mailto_only_and_images_https_only() {
        let out = clean(
            "<a href=\"https://e.example/a\">a</a><a href=\"http://e.example/b\">b</a>\
             <a href=\"mailto:x@e.example\">c</a><a href=\"//e.example/d\">d</a>\
             <a href=\"/rel\">e</a><a href=\"ftp://e.example/f\">f</a>\
             <img src=\"https://e.example/i.png\" alt=\"ok\" width=\"300\">\
             <img src=\"http://e.example/j.png\"><img src=\"//e.example/k.png\"><img>",
        );
        for kept in [
            "https://e.example/a",
            "http://e.example/b",
            "mailto:x@e.example",
            "https://e.example/i.png",
        ] {
            assert!(out.contains(kept), "{kept} lost: {out}");
        }
        for gone in [
            "//e.example/d",
            "\"/rel\"",
            "ftp:",
            "http://e.example/j.png",
            "//e.example/k.png",
        ] {
            assert!(!out.contains(gone), "{gone} survived: {out}");
        }
        assert_eq!(out.matches("<img").count(), 1, "{out}");
        assert!(out.contains("noopener"), "links lack rel: {out}");
    }

    #[test]
    fn tracking_pixels_are_removed() {
        for pixel in [
            "<img src=\"https://t.example/p.gif\" width=\"1\" height=\"1\">",
            "<img src=\"https://t.example/p.gif\" width=\"1\">",
            "<img src=\"https://t.example/p.gif\" height=\"0\">",
            "<img src=\"https://t.example/p.gif\" width=\"1px\" height=\"1px\" alt=\"\">",
        ] {
            let out = clean(&format!("<p>hello</p>{pixel}"));
            assert!(!out.contains("<img"), "pixel survived: {out}");
        }
        assert!(clean(
            "<p>x</p><img src=\"https://e.example/hero.png\" width=\"600\" alt=\"hero\">"
        )
        .contains("hero.png"));
    }

    #[test]
    fn style_is_limited_to_the_property_allowlist() {
        let out = clean("<p style=\"color:red;position:fixed;top:0\">ok</p>");
        assert!(out.contains("color:red"), "{out}");
        assert!(!out.contains("position") && !out.contains("top"), "{out}");
        // Any value that can fetch or script drops the whole attribute.
        for bad in [
            "background-color:red;background-image:url(https://t.example/x)",
            "color:red;width:expression(alert(1))",
            "color:\\72ed",
        ] {
            let out = clean(&format!("<p style=\"{bad}\">ok</p>"));
            assert_eq!(out, "<p>ok</p>", "{bad} -> {out}");
        }
    }

    #[test]
    fn footer_sentence_is_refused() {
        assert!(sanitize_html(&format!("<p>hi</p><p>{FOOTER_NOTE}</p>")).is_err());
        assert!(sanitize_html(&format!("<p>{}</p>", FOOTER_NOTE.to_uppercase())).is_err());
    }

    fn endpoints() -> Vec<HostEndpoint> {
        vec![
            HostEndpoint::binding("https://cadence.invalid/unsubscribe").unwrap(),
            HostEndpoint::binding("https://example.com/unsub").unwrap(),
            HostEndpoint::binding("https://bare.example").unwrap(),
            HostEndpoint::origin("https://send.example.com/base/").unwrap(),
        ]
    }

    fn link(href: &str) -> String {
        clean(&format!("<p><a href=\"{href}\">x</a></p>"))
    }

    #[test]
    fn host_unsubscribe_links_are_refused_in_every_shape() {
        for href in [
            "https://cadence.invalid/unsubscribe?token=RECIPIENT",
            "https://cadence.invalid/unsubscribe",
            "https://cadence.invalid/unsubscribe/",
            "http://cadence.invalid/unsubscribe",
            "HTTPS://CADENCE.INVALID/Unsubscribe?x=1&token=abc",
            "https://cadence.invalid/%75nsubscribe",
            "https://cadence.invalid/%2575nsubscribe",
            "https://cadence.invalid//unsubscribe",
            "https://cadence.invalid:443/unsubscribe",
            "https://example.com/unsub?token=RECIPIENT&a=1",
            "http://example.com/unsub/",
            "https://bare.example?token=RECIPIENT",
            "https://bare.example/?token=abc",
            "https://send.example.com/base/unsubscribe/abc123",
            "http://send.example.com/base/unsubscribe/abc123/",
            "https://send.example.com/base/%55nsubscribe/abc",
            // The per-recipient placeholder is host-private anywhere.
            "https://anywhere.example/x?token=RECIPIENT",
            "https://anywhere.example/x?a=1&token=%52ecipient",
        ] {
            let out = link(href);
            assert!(
                refuse_host_unsubscribe(&out, &endpoints()).is_err(),
                "{href} admitted -> {out}"
            );
        }
    }

    #[test]
    fn ordinary_third_party_links_are_allowed() {
        for href in [
            "https://news.example/article?utm=unsubscribe-tips",
            "https://news.example/unsubscribe-guide",
            "https://news.example/unsubscribe",
            "https://news.example/recipient-stories?token=abc",
            "https://cadence.invalid/blog/unsubscribe-tips",
            "https://cadence.invalid/",
            "https://bare.example/pricing",
            "https://example.com/unsubscribe",
            "https://send.example.com/base/news",
            "https://send.example.com/other/unsubscribe/abc",
            "mailto:help@example.com?subject=unsubscribe",
        ] {
            let out = link(href);
            refuse_host_unsubscribe(&out, &endpoints())
                .unwrap_or_else(|_| panic!("{href} refused -> {out}"));
        }
    }

    #[test]
    fn bounds_and_empty_refuse() {
        assert!(sanitize_html("").is_err());
        assert!(sanitize_html("   ").is_err());
        assert!(sanitize_html("<script>alert(1)</script>").is_err());
        assert!(sanitize_html(&"<p>x</p>".repeat(HTML_INPUT_BYTES)).is_err());
        assert!(sanitize_html(&format!("<p>{}</p>", "x".repeat(HTML_STORED_BYTES))).is_err());
    }

    #[test]
    fn structure_cannot_escape_the_host_wrapper() {
        let out = clean("<p>a</p></td></tr></table><div>b</div></body></html><table><tr><td>c</td></tr></table>");
        for open in ["table", "tr", "td", "div", "p"] {
            assert_eq!(
                out.matches(&format!("<{open}")).count(),
                out.matches(&format!("</{open}>")).count(),
                "unbalanced {open}: {out}"
            );
        }
        assert!(!out.contains("</body") && !out.contains("</html"), "{out}");
    }

    #[test]
    fn text_is_generated_from_sanitised_html() {
        let text = html_to_text(&clean(
            "<h1>Hi</h1><p>Read <a href=\"https://e.example/a?x=1&y=2\">this</a> &amp; that</p>",
        ));
        assert!(text.contains("Hi\n"), "{text:?}");
        assert!(
            text.contains("Read this (https://e.example/a?x=1&y=2) & that"),
            "{text:?}"
        );
        assert!(!text.contains('<'));
    }
}
