use serde_json::json;
use crate::error::{Error, Result};
/// CAD-1123 HP1: the host UI kit every frame document carries — the
/// board's generated tokens (`design/tokens.css`, kept in step with
/// `ui/src/styles.css` by `scripts/design-kit --check`), the design kit's
/// component classes (`design/kit.css`: `.btn`, `.chip`, `.field`,
/// `.card`, `.tabs`, `.scrim`, `.toast`, …) and the frame additions
/// (`.drawer`, `.seg`, `.sheet`). Packages use it instead of vendoring
/// host CSS; its version is stamped on `<html data-cadence-kit>` and in
/// the `#cadence-screen-kit` JSON block.
pub const SCREEN_KIT_CSS: &str = concat!(
    include_str!("../../design/tokens.css"),
    "\n",
    include_str!("../../design/kit.css"),
    "\n",
    include_str!("screen_kit.css"),
    "\n",
    // CAD-1622: the shared drawer shell's rules, so any app screen can render
    // the shell the CRM app uses without shipping host CSS. The package the
    // CRM app consumes builds from these same files, so the two copies cannot
    // drift.
    include_str!("../../ui/screen-ui/src/drawer-base.css"),
    "\n",
    include_str!("../../ui/screen-ui/src/drawer.css"),
);
/// The kit contract version a package can feature-detect.
pub const SCREEN_KIT_VERSION: &str = "cadence-kit/1";

/// Rendered-frame HTML: the static wrapper document carrying the CSP
/// nonce, the bootstrap JSON block, the approved CSS and the approved
/// IIFE — every byte host-verified so the package's own text can never
/// break out of its script/style context. Assets share the app package bound.
///
/// `csp_nonce` admits the two stamped blocks only; `bridge_nonce` is the
/// non-authorizing init token the child echoes (distinct from the
/// FrameCap bearer in the URL); `parent_origin` is the server-derived
/// exact board origin the child must `postMessage` to. The approved
/// IIFE may intentionally add code or read what it was granted — this
/// wrapper only prevents *accidental* display-injection and
/// unapproved-code execution; it does not constrain what approved code
/// deliberately does. The mount point is a generic `<div id="root">` —
/// the frozen app's own IIFE auto-mounts `#root`/`[data-sc-root]`; the
/// host emits no app-specific symbol.
pub fn render_frame_html(
    csp_nonce: &str,
    tag: &str,
    bridge_nonce: &str,
    generation: u64,
    css: &str,
    js: &str,
    parent_origin: &str,
) -> Result<String> {
    render_frame_document(
        csp_nonce,
        tag,
        bridge_nonce,
        generation,
        (&[css], &[js]),
        parent_origin,
    )
}

/// Render an already-validated package without repeating declaration
/// validation or script/style end-token scans (CAD-2534).
#[doc(hidden)]
pub fn render_checked_package_frame_html(
    csp_nonce: &str,
    tag: &str,
    bridge_nonce: &str,
    generation: u64,
    checked: &crate::issue::app_screen_decl::IntegrityCheckedScreen,
    assets: &std::collections::BTreeMap<String, String>,
    parent_origin: &str,
) -> Result<String> {
    let mut styles = Vec::new();
    for (name, body) in assets {
        if name.ends_with(".js") {
            check_script_text(body)?;
        } else if name.ends_with(".css") {
            let css = check_style_text(body)?;
            // Preserve the legacy first-stylesheet selection while checking
            // every supplied stylesheet, including inert leaves.
            if styles.is_empty() {
                styles.push(css);
            }
        }
    }
    let scripts: Vec<&str> = match checked.startup() {
        Some(order) => order
            .iter()
            .map(|name| {
                assets
                    .get(name)
                    .map(String::as_str)
                    .ok_or_else(|| Error::rejected("screen script asset missing"))
            })
            .collect::<Result<Vec<_>>>()?,
        None => vec![assets
            .get(checked.entry())
            .map(String::as_str)
            .ok_or_else(|| Error::rejected("screen entry asset missing"))?],
    };
    assemble_frame_document(
        csp_nonce,
        tag,
        bridge_nonce,
        generation,
        (&styles, &scripts),
        parent_origin,
    )
}

/// Revalidate public, caller-constructible package bytes before selecting
/// scripts. The declaration alone supplies explicit order; absent startup
/// retains entry-only execution, regardless of the map's file order.
pub fn render_package_frame_html(
    csp_nonce: &str,
    tag: &str,
    bridge_nonce: &str,
    generation: u64,
    package: &crate::issue::app_screen_pkg::ScreenPackage,
    parent_origin: &str,
) -> Result<String> {
    let checked = crate::issue::app_screen_decl::validate_map(&package.manifest, &package.assets)?;
    render_checked_package_frame_html(
        csp_nonce,
        tag,
        bridge_nonce,
        generation,
        &checked,
        &package.assets,
        parent_origin,
    )
}

fn render_frame_document(
    csp_nonce: &str,
    tag: &str,
    bridge_nonce: &str,
    generation: u64,
    assets: (&[&str], &[&str]),
    parent_origin: &str,
) -> Result<String> {
    let (styles, scripts) = assets;
    for css in styles {
        check_style_text(css)?;
    }
    for js in scripts {
        check_script_text(js)?;
    }
    assemble_frame_document(
        csp_nonce,
        tag,
        bridge_nonce,
        generation,
        (styles, scripts),
        parent_origin,
    )
}

/// Assemble the static frame document; callers must have verified `styles` and
/// `scripts` with [`check_style_text`] and [`check_script_text`] first.
fn assemble_frame_document(
    csp_nonce: &str,
    tag: &str,
    bridge_nonce: &str,
    generation: u64,
    assets: (&[&str], &[&str]),
    parent_origin: &str,
) -> Result<String> {
    let (styles, scripts) = assets;
    let asset_bytes = styles
        .iter()
        .chain(scripts.iter())
        .try_fold(0usize, |total, text| total.checked_add(text.len()))
        .ok_or_else(|| Error::rejected("screen frame assets exceed the app package byte limit"))?;
    if asset_bytes > crate::issue::app::MAX_APP_BYTES as usize {
        return Err(Error::rejected(
            "screen frame assets exceed the app package byte limit",
        ));
    }
    let bootstrap_bytes = csp_nonce
        .len()
        .saturating_add(tag.len())
        .saturating_add(bridge_nonce.len())
        .saturating_add(parent_origin.len());
    if bootstrap_bytes > 4 * 1024 {
        return Err(Error::rejected(
            "screen frame bootstrap exceeds its byte limit",
        ));
    }
    let boot = json!({
        "tag": tag,
        "bridge_nonce": bridge_nonce,
        "generation": generation,
        "parent_origin": parent_origin,
    })
    .to_string();
    let boot = escape_json_script(&boot);
    // The kit version lives in its own block: the CAD-1006 boot block is
    // a closed shape that shipped packages validate key by key.
    let kit_boot = escape_json_script(&json!({ "kit": SCREEN_KIT_VERSION }).to_string());
    let csp_nonce = csp_nonce
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let capacity = asset_bytes
        + SCREEN_KIT_CSS.len()
        + boot.len()
        + kit_boot.len()
        + csp_nonce.len() * (2 + scripts.len())
        + (styles.len() + scripts.len()) * 32
        + 512;
    let mut html = String::with_capacity(capacity);
    html.push_str("<!doctype html><html data-cadence-kit=\"");
    html.push_str(SCREEN_KIT_VERSION);
    html.push_str("\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><script nonce=\"");
    html.push_str(&csp_nonce);
    html.push_str("\" type=\"application/json\" id=\"cadence-screen-boot\">");
    html.push_str(&boot);
    html.push_str("</script><script nonce=\"");
    html.push_str(&csp_nonce);
    html.push_str("\" type=\"application/json\" id=\"cadence-screen-kit\">");
    html.push_str(&kit_boot);
    html.push_str("</script><style id=\"cadence-kit\">");
    html.push_str(SCREEN_KIT_CSS);
    html.push_str("</style>");
    for css in styles {
        html.push_str("<style>");
        html.push_str(css);
        html.push_str("</style>");
    }
    html.push_str("</head><body><div id=\"root\"></div>");
    for js in scripts {
        html.push_str("<script nonce=\"");
        html.push_str(&csp_nonce);
        html.push_str("\">");
        html.push_str(js);
        html.push_str("</script>");
    }
    html.push_str("</body></html>");
    Ok(html)
}

/// `<script type="application/json">`-safe escaping for the bootstrap:
/// JSON string escaping with `\\uXXXX` — `<`,`>`,`&` and the JS line
/// separators become `\\u003c`/`\\u003e`/`\\u0026`/`\\u2028`/`\\u2029`
/// literals the JSON parser restores, while the raw-text tokenizer
/// never sees a `</script`/`<!--` token. NOT HTML entities — inside a
/// raw-text `<script>` an `&lt;` reaches `JSON.parse` as a literal and
/// corrupts the field; `\\u003c` is the only correct form here.
fn escape_json_script(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            _ => out.push(c),
        }
    }
    out
}

/// Emit the approved IIFE source EXACTLY — a raw-text `<script>` block
/// cannot contain `</script`, `<!--` or `-->` (the HTML parser would
/// terminate the block or open a comment), so the mount FAILS CLOSED on
/// a case-insensitive match rather than rewriting bytes (a `\\u003c`
/// backslash-escape would corrupt operators/regex/React `</tag>`).
/// Preserving the exact approved source is what makes the approved
/// bytes' sha256 the executed bytes.
fn check_script_text(text: &str) -> Result<&str> {
    let script_end = text.match_indices('<').any(|(index, _)| {
        text.as_bytes()
            .get(index..index + 8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"</script"))
    });
    if script_end || text.contains("<!--") || text.contains("-->") {
        return Err(Error::rejected(
            "screen entry contains a forbidden HTML-script end token",
        ));
    }
    Ok(text)
}

/// Emit the approved CSS EXACTLY — a `<style>` raw-text block cannot
/// contain a case-insensitive `</style` end token; fail closed instead
/// of rewriting (a rewritten stylesheet is not the approved bytes).
fn check_style_text(text: &str) -> Result<&str> {
    let style_end = text.match_indices('<').any(|(index, _)| {
        text.as_bytes()
            .get(index..index + 7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"</style"))
    });
    if style_end {
        return Err(Error::rejected(
            "screen stylesheet contains a forbidden </style end token",
        ));
    }
    Ok(text)
}
