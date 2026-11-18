//! CAD-2022 / CAD-2316: sanitized HTML5 + print CSS financial templates.

use std::collections::{BTreeSet, HashMap, HashSet};

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::Value;

use crate::error::{Error, Result};

use super::TemplateSpec;

pub const HTML_MAX_BYTES: usize = 512 * 1024;
pub const HTML_IMAGE_MAX_BYTES: usize = 512 * 1024;
pub const HTML_IMAGE_TOTAL_MAX_BYTES: usize = 2 * 1024 * 1024;

const SYS_KEYS: &str =
    "kind currency tax_rate discount_rate subtotal discount tax total has_unresolved_pricing";
const ITEM_KEYS: &str = "description quantity unit_price line_total";
const VOID_TAGS: &str = "br hr img col meta";
const WRAP_TAGS: &str = "html head body title meta style";
const BODY_TAGS: &str = "div section header footer main article aside nav h1 h2 h3 h4 h5 h6 p span strong b em i u s small sub sup mark br hr pre code blockquote ul ol li dl dt dd table thead tbody tfoot tr th td caption colgroup col figure figcaption address time abbr a img";
const SVG_TAGS: &str = "svg g path rect circle ellipse line polyline polygon text tspan defs lineargradient radialgradient stop clippath";
const SVG_DRAW: &str = "fill clip-path stroke stroke-width opacity transform";
const BAD_ATTRS: &str = "srcdoc formaction xlink:href srcset poster background ping integrity";
const SAFE_ATTRS: &str = "id class style title lang dir role aria-label aria-hidden";
const STYLE_PROPS: &str = "align-items background background-color border border-bottom border-collapse border-left border-radius border-right border-spacing border-top break-inside color display flex flex-direction font font-family font-size font-style font-weight gap grid-template-columns height justify-content letter-spacing line-height margin margin-bottom margin-left margin-right margin-top max-height max-width min-height min-width opacity padding padding-bottom padding-left padding-right padding-top page-break-after page-break-inside table-layout text-align text-decoration text-transform vertical-align white-space width";
const BAD_CSS: &str = "expression( -moz-binding behavior: javascript: vbscript: image-set( -webkit-image-set( cross-fade(";
const ENTITIES: &str = "amp:& lt:< gt:> quot:\" apos:' nbsp:\u{a0} colon:: semi:; tab:\t newline:\n sol:/ bsol:\\ lpar:( rpar:) equals:= num:#";

fn has_word(list: &str, w: &str) -> bool {
    list.split_ascii_whitespace().any(|x| x == w)
}
fn wset(s: &'static str) -> HashSet<&'static str> {
    s.split_ascii_whitespace().collect()
}
fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}
fn scan_name(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], b':' | b'-')) {
        i += 1;
    }
    i
}
fn erej(m: impl Into<String>) -> Error {
    Error::rejected(m)
}
fn rej<T>(m: impl Into<String>) -> Result<T> {
    Err(Error::rejected(m))
}
fn ensure(cond: bool, msg: impl Into<String>) -> Result<()> {
    if cond {
        Ok(())
    } else {
        rej(msg)
    }
}
macro_rules! chk {
    ($c:expr, $($a:tt)+) => { if !($c) { return rej(format!($($a)+)); } };
}
fn is_draw_svg(t: &str) -> bool {
    has_word(SVG_TAGS, t)
        && !matches!(
            t,
            "svg" | "defs" | "lineargradient" | "radialgradient" | "stop" | "clippath"
        )
}

fn tag_attrs_str(tag: &str) -> &'static str {
    match tag {
        "html" => "lang dir",
        "meta" => "charset",
        "a" => "href target rel",
        "img" => "src alt width height",
        "td" => "colspan rowspan align valign",
        "th" => "colspan rowspan scope align valign",
        "col" => "span width",
        "colgroup" => "span",
        "table" => "width align cellpadding cellspacing border",
        "time" => "datetime",
        "abbr" => "title",
        "svg" => "xmlns viewbox width height fill stroke stroke-width stroke-linecap stroke-linejoin preserveaspectratio",
        "path" => "d fill-rule clip-rule stroke-linecap stroke-linejoin",
        "rect" => "x y width height rx ry",
        "circle" => "cx cy r",
        "ellipse" => "cx cy rx ry",
        "line" => "x1 y1 x2 y2 stroke-linecap",
        "polyline" | "polygon" => "points",
        "text" | "tspan" => "x y dx dy text-anchor font-size font-family font-weight",
        "lineargradient" => "x1 y1 x2 y2 gradientunits gradienttransform",
        "radialgradient" => "cx cy r fx fy gradientunits gradienttransform",
        "stop" => "offset stop-color stop-opacity",
        "clippath" => "clippathunits",
        _ => "",
    }
}

fn has_ctrl(s: &str, allow_ws: bool) -> bool {
    let ok_ws = |b: u8| allow_ws && matches!(b, b'\t' | b'\n' | b'\r');
    s.bytes().any(|b| (b == 0x7f || b < 0x20) && !ok_ws(b))
}

fn decode_num(digits: &str, radix: u32, max_sig: usize) -> Result<char> {
    let sig = digits.trim_start_matches('0');
    let src = if sig.is_empty() { "0" } else { sig };
    let ch = (sig.len() <= max_sig)
        .then(|| u32::from_str_radix(src, radix).ok())
        .flatten()
        .and_then(char::from_u32)
        .filter(|c| *c != '\0');
    ch.ok_or_else(|| erej("disallowed numeric ref"))
}

fn decode_entities(raw: &str) -> Result<String> {
    let (mut out, mut i, b) = (String::with_capacity(raw.len()), 0, raw.as_bytes());
    while i < b.len() {
        if b[i] != b'&' {
            let ch = raw[i..].chars().next().expect("utf8");
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let (after, ab) = (&raw[i + 1..], &b[i + 1..]);
        let num = if ab.starts_with(b"#x") || ab.starts_with(b"#X") {
            Some((16, 6, 2))
        } else {
            ab.starts_with(b"#").then_some((10, 7, 1))
        };
        if let Some((rx, ms, ds)) = num {
            let n = ab[ds..]
                .iter()
                .take_while(|c| char::from(**c).is_digit(rx))
                .count();
            if n > 0 {
                let end = ds + n;
                out.push(decode_num(&after[ds..end], rx, ms)?);
                i += 1 + end + usize::from(ab.get(end) == Some(&b';'));
                continue;
            }
        } else if ab.first().is_some_and(u8::is_ascii_alphabetic) {
            let end = ab.iter().take_while(|c| c.is_ascii_alphanumeric()).count();
            let semi = ab.get(end) == Some(&b';');
            let nxt = ab.get(end + usize::from(semi));
            if semi || !nxt.is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'=') {
                let k = after[..end].to_ascii_lowercase();
                let found = ENTITIES.split(' ').find_map(|p| {
                    p.split_once(':')
                        .filter(|(x, _)| *x == k)
                        .and_then(|(_, v)| v.chars().next())
                });
                if let Some(ch) = found {
                    out.push(ch);
                    i += 1 + end + usize::from(semi);
                    continue;
                }
            }
        }
        out.push('&');
        i += 1;
    }
    Ok(out)
}

fn is_frag(v: &str) -> bool {
    let ok = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-');
    v.len() > 1 && v.starts_with('#') && v[1..].bytes().all(ok)
}

fn check_data_image(val: &str, total_img: &mut usize) -> Result<()> {
    let low = val.to_ascii_lowercase();
    let img = low
        .strip_prefix("data:image/")
        .and_then(|r| r.split_once(";base64,"));
    let Some((kind, _)) = img.filter(|(k, _)| matches!(*k, "png" | "jpeg" | "webp")) else {
        return rej("<img src> requires inline data:image/(png|jpeg|webp);base64");
    };
    let b64 = &val[19 + kind.len()..];
    let ok_c = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=');
    let cap = !b64.is_empty() && b64.len() <= HTML_IMAGE_MAX_BYTES * 4 / 3 + 4;
    chk!(
        cap && b64.bytes().all(ok_c),
        "<img src> invalid or oversized base64 image"
    );
    let dec = B64
        .decode(b64)
        .map_err(|_| erej("<img src> invalid base64"))?;
    chk!(
        !dec.is_empty() && dec.len() <= HTML_IMAGE_MAX_BYTES,
        "inline image out of bounds"
    );
    *total_img = total_img.saturating_add(dec.len());
    chk!(
        *total_img <= HTML_IMAGE_TOTAL_MAX_BYTES,
        "total inline images exceed limit"
    );
    Ok(())
}

fn validate_css(css: &str, lbl: &str, total_img: &mut usize) -> Result<()> {
    let bad_c = has_ctrl(css, true)
        || ["{{", "{%", "\\", "/*", "*/"]
            .iter()
            .any(|p| css.contains(p));
    chk!(!bad_c, "{lbl}: disallowed CSS syntax");
    let low = css.to_ascii_lowercase();
    let cmp: String = low.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if let Some(b) = BAD_CSS.split_ascii_whitespace().find(|b| cmp.contains(b)) {
        chk!(false, "{lbl}: forbidden CSS '{b}'");
    }
    for seg in low.split('@').skip(1) {
        let end = seg
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-')))
            .unwrap_or(seg.len());
        let nm = &seg[..end];
        chk!(
            nm.is_empty() || matches!(nm, "page" | "media"),
            "{lbl}: disallowed @{nm}"
        );
    }
    let (mut s, lb, cb) = (0, low.as_bytes(), css.as_bytes());
    while let Some(pos) = low[s..].find("url") {
        let after = s + pos + 3;
        let prev_ok =
            pos == 0 || (!lb[s + pos - 1].is_ascii_alphanumeric() && lb[s + pos - 1] != b'-');
        let cur = skip_ws(cb, after);
        if prev_ok && cb.get(cur) == Some(&b'(') {
            let rel = css[cur + 1..]
                .find(')')
                .ok_or_else(|| erej("unclosed url(...)"))?;
            let close = cur + 1 + rel;
            let inner = css[cur + 1..close].trim();
            chk!(
                !inner.contains('(') && !inner.contains(['\n', '\r']),
                "malformed url(...)"
            );
            let unq = ["\"", "'"]
                .iter()
                .find_map(|q| inner.strip_prefix(q)?.strip_suffix(q))
                .unwrap_or(inner)
                .trim();
            if !is_frag(unq) {
                check_data_image(unq, total_img)?;
            }
            s = close + 1;
        } else {
            s = after;
        }
    }
    Ok(())
}

fn validate_href(raw: &str) -> Result<()> {
    let dec = decode_entities(raw)?;
    chk!(!has_ctrl(&dec, false), "control chars in <a href>");
    let t = dec.trim();
    if t.starts_with('#') && is_frag(t) {
        return Ok(());
    }
    let low = t.to_ascii_lowercase();
    let m_ok = low
        .strip_prefix("mailto:")
        .is_some_and(|a| a.contains('@') && a.contains('.') && !a.chars().any(char::is_whitespace));
    chk!(
        (low.starts_with("https://") && t.len() > 8) || m_ok,
        "disallowed <a href>"
    );
    Ok(())
}

fn find_rawtext_close(tail: &str, tag: &str) -> Result<(usize, usize)> {
    let (low, needle, mut s) = (tail.to_ascii_lowercase(), format!("</{tag}"), 0);
    while let Some(rel) = low[s..].find(&needle) {
        let idx = s + rel;
        let after = &tail[idx + needle.len()..];
        let delim = after
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b'/' || *b == b'>');
        if delim {
            let gt = after
                .find('>')
                .filter(|g| after[..*g].bytes().all(|b| b.is_ascii_whitespace()))
                .ok_or_else(|| erej(format!("malformed </{tag}>")))?;
            return Ok((idx, idx + needle.len() + gt + 1));
        }
        s = idx + needle.len();
    }
    rej(format!("unclosed <{tag}>"))
}

fn known_scalar(spec: &TemplateSpec, k: &str) -> bool {
    has_word(SYS_KEYS, k)
        || spec.fields.iter().any(|f| f.key == k)
        || spec.fixed_text.contains_key(k)
}

fn validate_placeholders(html: &str, spec: &TemplateSpec) -> Result<()> {
    let (mut i, b, mut stack) = (0, html.as_bytes(), Vec::new());
    while i < b.len() {
        if b[i] == b'{' && matches!(b.get(i + 1), Some(b'{' | b'%')) {
            let (is_var, start) = (b[i + 1] == b'{', i + 2);
            let rel = html[start..]
                .find(if is_var { "}}" } else { "%}" })
                .ok_or_else(|| erej("unclosed delimiter"))?;
            let inner = &html[start..start + rel];
            chk!(
                !inner.contains("{{") && !inner.contains("{%"),
                "nested delimiters"
            );
            if is_var {
                let ex = inner.trim();
                chk!(!ex.is_empty(), "empty placeholder '{{ }}'");
                if let Some(f) = ex.strip_prefix("item.") {
                    chk!(
                        stack.contains(&true) && has_word(ITEM_KEYS, f),
                        "invalid item '{ex}'"
                    );
                } else {
                    chk!(known_scalar(spec, ex), "undeclared '{{{{ {ex} }}}}'");
                }
            } else {
                let parts: Vec<&str> = inner.split_whitespace().collect();
                match parts.as_slice() {
                    ["for", "item", "in", "items"] => stack.push(true),
                    ["endfor"] if stack.pop() == Some(true) => {}
                    ["if", k] | ["if", "not", k] if known_scalar(spec, k) || *k == "items" => {
                        stack.push(false)
                    }
                    ["endif"] if stack.pop() == Some(false) => {}
                    _ => chk!(false, "invalid directive '{{% {} %}}'", inner.trim()),
                }
            }
            i = start + rel + 2;
        } else if matches!(b[i], b'}' | b'%') && b.get(i + 1) == Some(&b'}') {
            chk!(false, "unmatched closing delimiter");
        } else {
            i += 1;
        }
    }
    chk!(stack.is_empty(), "unclosed template block");
    Ok(())
}

struct ParsedTag<'a> {
    name: String,
    attrs: Vec<(&'a str, &'a str)>,
    self_closing: bool,
    end: usize,
}

fn parse_open_tag(html: &str, lt: usize) -> Result<ParsedTag<'_>> {
    let (b, len, pos) = (html.as_bytes(), html.len(), lt + 1);
    chk!(
        pos < len && b[pos].is_ascii_alphabetic(),
        "unescaped '<' or malformed tag"
    );
    let ne = scan_name(b, pos);
    let name = html[pos..ne].to_ascii_lowercase();
    let (mut attrs, mut cur) = (Vec::new(), ne);
    while cur < len {
        cur = skip_ws(b, cur);
        if cur >= len {
            break;
        }
        if b[cur] == b'>' || (b[cur] == b'/' && b.get(cur + 1) == Some(&b'>')) {
            let sc = b[cur] == b'/';
            return Ok(ParsedTag {
                name,
                attrs,
                self_closing: sc,
                end: cur + usize::from(sc) + 1,
            });
        }
        if b[cur] == b'/' {
            cur += 1;
            continue;
        }
        let a0 = cur;
        while cur < len && !b[cur].is_ascii_whitespace() && !matches!(b[cur], b'=' | b'>' | b'/') {
            cur += 1;
        }
        let an = &html[a0..cur];
        chk!(!an.is_empty(), "malformed attr in <{name}>");
        cur = skip_ws(b, cur);
        let mut av = "";
        if b.get(cur) == Some(&b'=') {
            cur = skip_ws(b, cur + 1);
            if cur >= len {
                break;
            }
            let q = b[cur];
            if q == b'"' || q == b'\'' {
                let rel = html[cur + 1..]
                    .find(q as char)
                    .ok_or_else(|| erej(format!("unterminated '{an}'")))?;
                av = &html[cur + 1..cur + 1 + rel];
                cur += 2 + rel;
            } else {
                let v0 = cur;
                while cur < len && !b[cur].is_ascii_whitespace() && b[cur] != b'>' {
                    chk!(
                        !matches!(b[cur], b'"' | b'\'' | b'`'),
                        "bad quote in '{an}'"
                    );
                    cur += 1;
                }
                av = &html[v0..cur];
            }
        }
        attrs.push((an, av));
    }
    rej(format!("unclosed <{name}>"))
}

struct StructuralCheck {
    styles: Vec<String>,
    body_frag: String,
    no_comments: String,
    html_lang: String,
}

fn validate_tag_attrs(
    tag: &str,
    attrs: &[(&str, &str)],
    is_svg: bool,
    lang: &mut String,
    img_b: &mut usize,
) -> Result<()> {
    if tag == "meta" {
        let ok = attrs.len() == 1 && attrs[0].0.eq_ignore_ascii_case("charset");
        chk!(
            ok && decode_entities(attrs[0].1)?
                .trim()
                .eq_ignore_ascii_case("utf-8"),
            "only <meta charset=\"utf-8\"> allowed"
        );
        return Ok(());
    }
    let (ta, draw) = (tag_attrs_str(tag), is_draw_svg(tag));
    for &(ar, vr) in attrs {
        let attr = ar.to_ascii_lowercase();
        let allowed = has_word(SAFE_ATTRS, &attr)
            || has_word(ta, &attr)
            || (draw && has_word(SVG_DRAW, &attr));
        chk!(
            !attr.starts_with("on") && !has_word(BAD_ATTRS, &attr) && allowed,
            "disallowed attr '{attr}' on <{tag}>"
        );
        if attr == "lang" && tag == "html" {
            let c = vr.trim();
            if (1..=16).contains(&c.len())
                && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                *lang = c.to_string();
            }
        }
        let svg_paint = is_svg && matches!(attr.as_str(), "fill" | "stroke" | "clip-path");
        if attr == "style"
            || (attr == "href" && tag == "a")
            || (attr == "src" && tag == "img")
            || svg_paint
        {
            let dec = decode_entities(vr)?;
            chk!(
                !vr.contains("{{")
                    && !vr.contains("{%")
                    && !dec.contains("{{")
                    && !dec.contains("{%"),
                "placeholders disallowed in attr"
            );
            if attr == "href" && tag == "a" {
                validate_href(vr)?;
            } else if attr == "src" && tag == "img" {
                chk!(!has_ctrl(&dec, false), "control chars in <img src>");
                check_data_image(dec.trim(), img_b)?;
            } else if attr == "style" {
                validate_css(&dec, "inline style", img_b)?;
            } else {
                chk!(
                    !dec.contains('\\')
                        && !dec.contains("/*")
                        && !dec.contains("*/")
                        && !has_ctrl(&dec, false),
                    "CSS escapes/comments/controls disallowed in SVG '{attr}'"
                );
                if dec.to_ascii_lowercase().contains("url") {
                    let u = dec
                        .trim()
                        .strip_prefix("url(")
                        .and_then(|s| s.strip_suffix(')'));
                    chk!(
                        u.is_some_and(|id| is_frag(id.trim())),
                        "SVG '{attr}' url(...) must reference #id"
                    );
                }
            }
        }
        if attr == "rel" && tag == "a" {
            let rel_ok = decode_entities(vr)?.split_whitespace().all(|t| {
                matches!(
                    t.to_ascii_lowercase().as_str(),
                    "noopener" | "noreferrer" | "nofollow" | "external"
                )
            });
            chk!(rel_ok, "disallowed <a rel>");
        }
    }
    Ok(())
}

fn check_comment_end(after: &str, in_body: bool) -> Result<usize> {
    chk!(
        !after.starts_with('>') && !after.starts_with("->"),
        "malformed HTML comment"
    );
    let end = match (after.find("-->"), after.find("--!>")) {
        (Some(a), Some(b)) if b < a => return rej("malformed HTML comment"),
        (None, Some(_)) => return rej("malformed HTML comment"),
        (Some(a), _) if !after[..a].contains("--") => a,
        (Some(_), _) => return rej("disallowed '--' in comment"),
        (None, None) => return rej("unclosed HTML comment"),
    };
    chk!(!in_body, "comments disallowed in body");
    Ok(7 + end)
}

fn check_structure_and_extract(html: &str, img_b: &mut usize) -> Result<StructuralCheck> {
    let mut stack: Vec<(String, bool)> = Vec::new();
    let (mut svg_d, mut styles, mut spans) = (0usize, Vec::new(), Vec::new());
    let (mut nc, mut saw_body, mut saw_comment) = (String::with_capacity(html.len()), false, false);
    let (mut b0, mut b1, mut lang, mut i, b) = (None, None, "en".to_string(), 0, html.as_bytes());
    let no_cend = |t: &str| {
        ensure(
            !t.contains("-->") && !t.contains("--!>"),
            "unmatched comment",
        )
    };

    while i < b.len() {
        let Some(lt_rel) = html[i..].find('<') else {
            no_cend(&html[i..])?;
            nc.push_str(&html[i..]);
            break;
        };
        let (lt, t) = (i + lt_rel, &html[i..i + lt_rel]);
        no_cend(t)?;
        nc.push_str(t);
        let tail = &html[lt..];

        if let Some(after) = tail.strip_prefix("<!--") {
            saw_comment = true;
            i = lt + check_comment_end(after, saw_body && b1.is_none())?;
            continue;
        }
        if tail.to_ascii_lowercase().starts_with("<!doctype") {
            let gt = tail.find('>').ok_or_else(|| erej("unclosed <!DOCTYPE>"))?;
            nc.push_str(&tail[..=gt]);
            i = lt + gt + 1;
            continue;
        }
        chk!(
            !tail.starts_with("<!") && !tail.starts_with("<?"),
            "disallowed XML/CDATA"
        );
        if tail.starts_with("</") {
            let end = scan_name(b, lt + 2);
            let tag = html[lt + 2..end].to_ascii_lowercase();
            let cur = skip_ws(b, end);
            chk!(
                cur < b.len() && b[cur] == b'>' && !tag.is_empty(),
                "malformed </{tag}>"
            );
            let top = stack.pop().filter(|(t, _)| t == &tag);
            let (_, was_svg) = top.ok_or_else(|| erej(format!("mismatched </{tag}>")))?;
            if tag == "body" && b0.is_some() && b1.is_none() {
                b1 = Some(lt);
            }
            svg_d -= usize::from(was_svg);
            nc.push_str(&html[lt..=cur]);
            i = cur + 1;
            continue;
        }

        let p = parse_open_tag(html, lt)?;
        let (tag, is_svg) = (p.name.as_str(), has_word(SVG_TAGS, &p.name));
        chk!(
            is_svg || has_word(BODY_TAGS, tag) || has_word(WRAP_TAGS, tag),
            "disallowed <{tag}>"
        );
        let bad_svg = (tag == "svg" && svg_d > 0)
            || (tag != "svg" && is_svg && svg_d == 0)
            || (!is_svg && svg_d > 0);
        chk!(!bad_svg, "invalid <svg> nesting around <{tag}>");
        validate_tag_attrs(tag, &p.attrs, is_svg, &mut lang, img_b)?;

        if tag == "style" || tag == "title" {
            chk!(!p.self_closing, "<{tag}> cannot self-close");
            let (ilen, erel) = find_rawtext_close(&html[p.end..], tag)?;
            let (inner, end) = (&html[p.end..p.end + ilen], p.end + erel);
            if tag == "style" {
                let cmt = ["<!--", "-->", "--!>"].iter().any(|c| inner.contains(c));
                chk!(
                    !inner.to_ascii_lowercase().contains("</style") && !cmt,
                    "malformed <style>"
                );
                validate_css(inner, "<style>", img_b)?;
                styles.push(inner.trim().to_string());
                spans.push((lt, end));
            } else {
                chk!(
                    !inner.contains('<') && !has_ctrl(inner, true),
                    "invalid <title>"
                );
            }
            nc.push_str(&html[lt..end]);
            i = end;
            continue;
        }
        if tag == "body" {
            chk!(!saw_body, "multiple <body>");
            saw_body = true;
            if !p.self_closing {
                b0 = Some(p.end);
                stack.push(("body".into(), false));
            }
        } else if !has_word(VOID_TAGS, tag) && !p.self_closing {
            svg_d += usize::from(tag == "svg");
            stack.push((tag.into(), tag == "svg"));
        }
        nc.push_str(&html[lt..p.end]);
        i = p.end;
    }

    if let Some((u, _)) = stack.last() {
        return rej(format!("unclosed <{u}>"));
    }
    chk!(saw_body || !saw_comment, "comments disallowed in body");
    let body_frag = if saw_body {
        b0.zip(b1)
            .map(|(s, e)| html[s..e].trim().to_string())
            .unwrap_or_default()
    } else {
        let (mut frag, mut cur) = (String::new(), 0);
        for (s, e) in spans {
            if s >= cur {
                frag.push_str(&html[cur..s]);
                cur = e;
            }
        }
        frag.push_str(&html[cur..]);
        frag.trim().to_string()
    };
    Ok(StructuralCheck {
        styles,
        body_frag,
        no_comments: nc,
        html_lang: lang,
    })
}

fn ammonia_clean_body(body_frag: &str) -> Result<String> {
    let mut nonce = 0u64;
    while body_frag.contains(&format!("CADDIR_{nonce}_")) {
        nonce += 1;
    }
    let pfx = format!("CADDIR_{nonce}_");
    let (mut dirs, mut sh, mut rest) = (
        Vec::new(),
        String::with_capacity(body_frag.len()),
        body_frag,
    );
    while let Some(pos) = rest.find("{%") {
        sh.push_str(&rest[..pos]);
        let end = rest[pos + 2..]
            .find("%}")
            .ok_or_else(|| erej("unclosed '{%' directive"))?;
        dirs.push(rest[pos..pos + 4 + end].to_string());
        sh.push_str(&format!("<!--{pfx}{:06}-->", dirs.len() - 1));
        rest = &rest[pos + 4 + end..];
    }
    sh.push_str(rest);

    let mut tags = wset(BODY_TAGS);
    tags.extend(wset(SVG_TAGS));
    tags.extend(["linearGradient", "radialGradient", "clipPath"]);
    const CAMEL: &[(&str, Option<&str>, &[&str])] = &[
        ("svg", None, &["viewBox", "preserveAspectRatio"]),
        (
            "lineargradient",
            Some("linearGradient"),
            &["gradientUnits", "gradientTransform"],
        ),
        (
            "radialgradient",
            Some("radialGradient"),
            &["gradientUnits", "gradientTransform"],
        ),
        ("clippath", Some("clipPath"), &["clipPathUnits"]),
    ];
    let mut attrs: HashMap<&'static str, HashSet<&'static str>> = HashMap::new();
    for &t in &tags {
        let mut s: HashSet<&'static str> = tag_attrs_str(t)
            .split_ascii_whitespace()
            .filter(|&a| !(t == "a" && a == "rel"))
            .collect();
        if is_draw_svg(t) {
            s.extend(wset(SVG_DRAW));
        }
        if let Some((_, c, ex)) = CAMEL.iter().find(|(k, _, _)| *k == t) {
            s.extend(ex.iter().copied());
            if let Some(ct) = c {
                attrs.insert(ct, s.clone());
            }
        }
        if !s.is_empty() {
            attrs.insert(t, s);
        }
    }
    let mut out = ammonia::Builder::empty()
        .tags(tags)
        .tag_attributes(attrs)
        .generic_attributes(wset(SAFE_ATTRS))
        .filter_style_properties(wset(STYLE_PROPS))
        .url_schemes(["https", "mailto", "data"].into_iter().collect())
        .url_relative(ammonia::UrlRelative::PassThrough)
        .link_rel(Some("noopener noreferrer nofollow"))
        .strip_comments(false)
        .clean(&sh)
        .to_string();
    for (idx, d) in dirs.iter().enumerate() {
        out = out.replacen(&format!("<!--{pfx}{idx:06}-->"), d, 1);
    }
    Ok(out)
}

fn sanitize_doc(
    bytes: &[u8],
    label: &str,
    spec: Option<&TemplateSpec>,
) -> Result<(StructuralCheck, String)> {
    ensure(
        !bytes.is_empty() && bytes.len() <= HTML_MAX_BYTES,
        format!("{label} size out of bounds"),
    )?;
    let raw = std::str::from_utf8(bytes).map_err(|_| erej(format!("{label} must be UTF-8")))?;
    ensure(
        !raw.trim().is_empty() && !has_ctrl(raw, true),
        format!("{label} empty or control chars"),
    )?;
    let mut img = 0usize;
    let st = check_structure_and_extract(raw, &mut img)?;
    if let Some(s) = spec {
        validate_placeholders(&st.no_comments, s)?;
    }
    let body = ammonia_clean_body(&st.body_frag)?;
    let mut post_img = 0usize;
    let _ = check_structure_and_extract(&body, &mut post_img)?;
    if let Some(s) = spec {
        validate_placeholders(&body, s)?;
    }
    Ok((st, body))
}

/// Validate and canonicalize an HTML5 + print CSS financial template against `spec`.
pub fn validate_and_sanitize_html_template(bytes: &[u8], spec: &TemplateSpec) -> Result<String> {
    let (st, body) = sanitize_doc(bytes, "HTML template", Some(spec))?;
    let styles: String = st
        .styles
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(|s| format!("<style>{s}</style>\n"))
        .collect();
    Ok(format!("<!doctype html>\n<html lang=\"{}\">\n<head>\n<meta charset=\"utf-8\">\n{styles}</head>\n<body>\n{body}\n</body>\n</html>\n", st.html_lang))
}

/// Validate a `text/html` authoring reference snippet as static UTF-8 HTML.
pub fn validate_html_reference(bytes: &[u8]) -> Result<()> {
    sanitize_doc(bytes, "HTML reference", None).map(|_| ())
}

fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn scalar_text(val: Option<&Value>) -> String {
    match val {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

fn truthy(val: Option<&Value>) -> bool {
    match val {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
    }
}

fn find_block_end(src: &str, mut cur: usize, start: &str, end_t: &str) -> Result<(usize, usize)> {
    let mut depth = 1usize;
    while let Some(rel) = src[cur..].find("{%") {
        let open = cur + rel;
        let er = src[open + 2..]
            .find("%}")
            .ok_or_else(|| erej("unclosed '{%'"))?;
        let close = open + 4 + er;
        let kw = src[open + 2..open + 2 + er]
            .split_whitespace()
            .next()
            .unwrap_or("");
        if kw == start {
            depth += 1;
        } else if kw == end_t {
            depth -= 1;
            if depth == 0 {
                return Ok((open, close));
            }
        }
        cur = close;
    }
    rej(format!("unclosed '{{% {start} %}}'"))
}

fn render_segment(src: &str, ctx: &Value, item: Option<&Value>) -> Result<String> {
    let (mut out, mut cur) = (String::with_capacity(src.len()), 0);
    while cur < src.len() {
        let (ov, ot) = (src[cur..].find("{{"), src[cur..].find("{%"));
        if let Some(vr) = ov.filter(|v| ot.is_none_or(|t| *v < t)) {
            out.push_str(&src[cur..cur + vr]);
            let open = cur + vr + 2;
            let end = src[open..]
                .find("}}")
                .ok_or_else(|| erej("unclosed '{{'"))?;
            let ex = src[open..open + end].trim();
            let fv = ex
                .strip_prefix("item.")
                .map(|f| item.and_then(|o| o.get(f)));
            let val = scalar_text(fv.unwrap_or_else(|| ctx.get(ex)));
            out.push_str(&escape_html(&val));
            cur = open + end + 2;
        } else if let Some(tr) = ot {
            out.push_str(&src[cur..cur + tr]);
            let open = cur + tr + 2;
            let end = src[open..]
                .find("%}")
                .ok_or_else(|| erej("unclosed '{%'"))?;
            let (dir, after) = (src[open..open + end].trim(), open + end + 2);
            let parts: Vec<&str> = dir.split_whitespace().collect();
            match parts.as_slice() {
                ["for", "item", "in", "items"] => {
                    let (be, ep) = find_block_end(src, after, "for", "endfor")?;
                    if let Some(arr) = ctx.get("items").and_then(Value::as_array) {
                        for row in arr {
                            out.push_str(&render_segment(&src[after..be], ctx, Some(row))?);
                        }
                    }
                    cur = ep;
                }
                ["if", k] | ["if", "not", k] => {
                    let (be, ep) = find_block_end(src, after, "if", "endif")?;
                    if truthy(ctx.get(*k)) == (parts.len() == 2) {
                        out.push_str(&render_segment(&src[after..be], ctx, item)?);
                    }
                    cur = ep;
                }
                _ => return rej(format!("unsupported '{{% {dir} %}}'")),
            }
        } else {
            out.push_str(&src[cur..]);
            break;
        }
    }
    Ok(out)
}

/// Deterministic HTML renderer for validated financial templates.
pub fn render_html_template(template_html: &str, context: &Value, sample: bool) -> Result<String> {
    let mut rendered = render_segment(template_html, context, None)?;
    let _ = check_structure_and_extract(&rendered, &mut 0)?;
    if sample {
        let banner = "\n<div data-sample-banner=\"true\" style=\"background:#fef3c7;border:2px solid #d97706;color:#78350f;padding:10px 14px;margin:0 0 16px;font:600 13px/1.4 system-ui,sans-serif;\">SAMPLE PREVIEW — PLACEHOLDER DATA ONLY · NOT A DOCUMENT TO SEND</div>\n";
        let low = rendered.to_ascii_lowercase();
        let pos = low
            .find("<body")
            .and_then(|bp| rendered[bp..].find('>').map(|g| bp + g + 1));
        rendered.insert_str(pos.unwrap_or(0), banner);
    }
    Ok(rendered)
}

/// Extract valid scalar `{{ key }}` names from an HTML snippet.
pub fn extract_html_placeholders(html: &str) -> Vec<String> {
    let (mut out, mut cur) = (BTreeSet::new(), 0);
    while let Some(rel) = html[cur..].find("{{") {
        let start = cur + rel + 2;
        let Some(end) = html[start..].find("}}") else {
            break;
        };
        let k = html[start..start + end].trim();
        let ok = !k.is_empty() && !k.starts_with("item.") && !has_word(SYS_KEYS, k);
        if ok && k != "items" {
            out.insert(k.to_string());
        }
        cur = start + end + 2;
    }
    out.into_iter().collect()
}

/// Synthesize a safe A4 printable HTML5 starter template from reference intake provenance.
pub fn build_suggested_html_from_intake(file_name: &str, provenance: &Value) -> String {
    let ev = &provenance["evidence"];
    let mut notes_in = String::new();
    let push_p = |s: &mut String, t: &str| {
        if !t.trim().is_empty() {
            s.push_str(&format!("<p>{}</p>\n", escape_html(t.trim())));
        }
    };
    if let Some(arr) = ev["blocks"].as_array() {
        arr.iter()
            .take(40)
            .filter_map(|b| b["text"].as_str())
            .for_each(|t| push_p(&mut notes_in, t));
    } else if let Some(arr) = ev["pages"].as_array() {
        arr.iter()
            .take(10)
            .filter_map(|p| p["text"].as_str())
            .flat_map(|t| t.lines().take(20))
            .for_each(|l| push_p(&mut notes_in, l));
    }
    let notes = if notes_in.is_empty() {
        String::new()
    } else {
        format!("<section class=\"intake-notes\">{notes_in}</section>")
    };
    let mut phs = Vec::new();
    if let Some(arr) = ev["structure"]["placeholders"].as_array() {
        for k in arr
            .iter()
            .filter_map(Value::as_str)
            .filter(|k| !has_word(SYS_KEYS, k) && *k != "items")
        {
            let ek = escape_html(k);
            phs.push(format!("<p>{ek}: {{{{ {ek} }}}}</p>"));
        }
    }
    let rows = if phs.is_empty() {
        "<p>Customer: {{ customer }}</p>".into()
    } else {
        phs.join("\n")
    };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><style>@page{{size:A4;margin:18mm}}body{{font-family:system-ui,sans-serif;font-size:13px}}table{{width:100%;border-collapse:collapse;margin:12px 0}}th,td{{border-bottom:1px solid #e5e7eb;padding:6px;text-align:left}}.num{{text-align:right}}.totals{{margin-left:auto;width:220px}}</style></head><body><header><h1>{{{{ kind }}}}</h1><p class=\"source-ref\">Reference: {}</p></header><section>{rows}</section><table><thead><tr><th>Description</th><th class=\"num\">Qty</th><th class=\"num\">Unit</th><th class=\"num\">Total</th></tr></thead><tbody>{{% for item in items %}}<tr><td>{{{{ item.description }}}}</td><td class=\"num\">{{{{ item.quantity }}}}</td><td class=\"num\">{{{{ item.unit_price }}}}</td><td class=\"num\">{{{{ item.line_total }}}}</td></tr>{{% endfor %}}</tbody></table><div class=\"totals\"><p>Subtotal: {{{{ subtotal }}}} {{{{ currency }}}}</p><p>Discount: {{{{ discount }}}} {{{{ currency }}}}</p><p>Tax: {{{{ tax }}}} {{{{ currency }}}}</p><p><strong>Total: {{{{ total }}}} {{{{ currency }}}}</strong></p></div>{notes}</body></html>",
        escape_html(file_name)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business_templates::parse_spec;
    use serde_json::json;

    fn sample_spec() -> TemplateSpec {
        parse_spec(json!({"kind": "quotation", "fields": [{"key": "customer", "label": "Customer", "kind": "text", "required": true}], "fixed_text": {}, "calculation": {"confirmed": true, "currency": "HKD", "tax_rate_bps": 0, "discount_bps": 0}})).unwrap()
    }
    fn wrap(h: &str, b: &str) -> String {
        format!("<!doctype html><html><head><meta charset=\"utf-8\">{h}</head><body>{b}<p>{{{{ customer }}}}</p><ul>{{% for item in items %}}<li>{{{{ item.description }}}} {{{{ item.quantity }}}} {{{{ item.unit_price }}}}</li>{{% endfor %}}</ul></body></html>")
    }

    #[test]
    fn refuses_bypasses() {
        let s = sample_spec();
        assert!(
            validate_and_sanitize_html_template(wrap("<title>OK</title>", "").as_bytes(), &s)
                .is_ok()
        );
        for (h, b) in [
            ("<style></style x><script>alert(1)</script><style></style>", ""),
            ("<style></style/a><img src=\"https://attacker.invalid/s\"><style></style>", ""),
            ("<title><!--</title><script>alert(1)</script><title>--></title>", ""),
            ("<style><!--</style><script>alert(1)</script><style>--></style>", ""),
            ("", "<div title=\"<!--\"><script>alert(1)</script><div title=\"-->\">x</div>"),
            ("", "<!-- --!><script>alert(1)</script>-->"),
            ("", "<!--><script>alert(1)</script>-->"),
            ("", "<div style=\"background:&#x000000000000000075;rl(https://attacker.invalid/x)\">x</div>"),
            ("", "<div style=\"background:url&#40https://attacker.invalid/x)\">x</div>"),
            ("", "<svg viewBox=\"0 0 10 10\"><rect x=\"0\" y=\"0\" width=\"5\" height=\"5\" fill=\"&#x000000000000000075;rl(https://attacker.invalid/x#p)\"/></svg>"),
            ("", "<div style=\"font-family:'{{ customer }}'\">x</div>"),
            ("", "<a href=\"https://example.com/{{ customer }}\">x</a>"),
        ] {
            let v = wrap(h, b);
            assert!(validate_and_sanitize_html_template(v.as_bytes(), &s).is_err());
            assert!(validate_html_reference(v.as_bytes()).is_err(), "{v}");
        }
    }

    #[test]
    fn renders_svg_and_sentinel() {
        let s = sample_spec();
        let svg = r##"<svg viewBox="0 0 120 24" preserveAspectRatio="xMidYMid meet"><defs><linearGradient id="g1" gradientUnits="userSpaceOnUse"><stop offset="0%" stop-color="#111111"/></linearGradient><radialGradient id="g2" gradientTransform="scale(1)"><stop offset="100%" stop-color="#333333"/></radialGradient><clipPath id="c1" clipPathUnits="userSpaceOnUse"><rect x="0" y="0" width="60" height="24"/></clipPath></defs><rect width="120" height="24" fill="url(#g1)" clip-path="url(#c1)"/></svg>"##;
        let out = validate_and_sanitize_html_template(wrap("<title>T</title>", svg).as_bytes(), &s)
            .unwrap();
        for p in r##"viewBox="0 0 120 24"|preserveAspectRatio="xMidYMid meet"|<linearGradient|gradientUnits="userSpaceOnUse"|<radialGradient|gradientTransform="scale(1)"|<clipPath|clipPathUnits="userSpaceOnUse"|clip-path="url(#c1)""##.split('|') {
            assert!(out.contains(p), "missing {p}");
        }
        let ctx = json!({"kind": "quotation", "currency": "HKD", "customer": "{% Promo %} Northwind Ltd", "items": [{"description": "Discount 50%}", "quantity": "1", "unit_price": "100.00", "line_total": "100.00"}], "subtotal": "100.00", "discount": "0.00", "tax": "0.00", "total": "100.00"});
        let r = render_html_template(&out, &ctx, true).unwrap();
        assert!(r.contains("Discount 50%}") && r.contains("{% Promo %} Northwind Ltd"));
        assert!(r.contains("data-sample-banner=\"true\""));
        let frag = "<style>p { color: #111111; }</style >\n<p title=\"CADDIR_0_000000 &lt;style&gt;\">{{ customer }}</p>\n<ul>{% for item in items %}<li>{{ item.description }} {{ item.quantity }} {{ item.unit_price }}</li>{% endfor %}</ul>";
        let f = validate_and_sanitize_html_template(frag.as_bytes(), &s).unwrap();
        assert!(f.contains("<style>p { color: #111111; }</style>"));
        assert!(f.contains("CADDIR_0_000000") && f.contains("{% for item in items %}"));
    }
}
