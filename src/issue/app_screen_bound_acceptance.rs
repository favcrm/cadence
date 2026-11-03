//! CAD-2008 acceptance: a screen is an independently packaged frame, so its
//! assets are bounded by the package aggregate (2 MiB) — not by the old
//! 400 KiB script / 128 KiB leaf / 512 KiB package triple — and the rendered
//! frame document is no longer capped at 768 KiB.
//!
//! Every test drives the seams a consumer actually crosses, never a private
//! helper:
//!
//! * [`crate::issue::app_screen_pkg::extract`] — the install/mount re-proof of
//!   one `screens/<tag>/` package over the live bundle members;
//! * [`crate::issue::app_screen_decl::validate_map`] — the declaration↔asset
//!   guard itself (supplied bounds, then grammar, then digest match);
//! * [`crate::daemon::render_package_frame_html`] and
//!   [`crate::daemon::render_frame_html`] — the two frame-document entry
//!   points (board mount route and preview export);
//! * [`crate::issue::app::validate_texts`] — the whole-app install gate that
//!   keeps the finite 2 MiB bundle budget.
//!
//! The fixtures are synthetic but genuine: every leaf body is generated to an
//! exact byte length and the declaration's `size`/`sha256` are computed from
//! those same bytes, so admission is proved against real digests and a tamper
//! is a real digest mismatch. Sizes straddle the *former* published caps while
//! staying inside the retained 2 MiB package and whole-app budgets; no
//! assertion restates an implementation constant.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::daemon::{render_frame_html, render_package_frame_html};
use crate::issue::app;
use crate::issue::app_screen_decl::validate_map;
use crate::issue::app_screen_pkg::{extract, tags_in};

const KIB: usize = 1024;
/// The retained whole-package/whole-app budget the new screen leaf bounds
/// share. Written here as the ticket states it, not read from the crate.
const PACKAGE_BUDGET: usize = 2 * 1024 * KIB;
/// The `screens.json` declaration bound the contract keeps tight while asset
/// leaves grow — also stated here, never read from the crate.
const SCREEN_MANIFEST_CAP: usize = 32 * KIB;
/// The frame-document cap this ticket removes.
const FORMER_FRAME_CAP: usize = 768 * KIB;

/// One synthetic screen package: a tag plus the exact leaf bodies it will
/// declare. Every declaration is derived from these bodies, so the fixture is
/// internally consistent until a test deliberately tampers with it.
struct Screen {
    tag: String,
    contract: &'static str,
    assets: BTreeMap<String, String>,
}

impl Screen {
    fn new(tag: impl Into<String>) -> Self {
        Screen {
            tag: tag.into(),
            contract: "app-screens/v1",
            assets: BTreeMap::new(),
        }
    }

    /// The same declaration under the action-capable contract: a v2 screen
    /// with no `may`, no `tools` and no host pin stays read-only and valid.
    fn v2(mut self) -> Self {
        self.contract = "app-screens/v2";
        self
    }

    fn leaf(mut self, name: &str, body: String) -> Self {
        assert!(
            self.assets.insert(name.to_string(), body).is_none(),
            "duplicate fixture leaf {name}"
        );
        self
    }

    /// The `app-screens/v1` declaration for exactly these leaves — every
    /// size and digest computed from the body that will be supplied.
    fn manifest(&self, app: &str) -> String {
        let assets: Vec<serde_json::Value> = self
            .assets
            .iter()
            .map(|(name, body)| {
                serde_json::json!({
                    "name": name,
                    "media_type": media_type(name),
                    "sha256": digest(body),
                    "size": body.len(),
                })
            })
            .collect();
        serde_json::json!({
            "contract": self.contract,
            "app": app,
            "entry": "client.js",
            "assets": assets,
            "provenance": {
                "source_digest": digest("acceptance-source"),
                "sdk_digest": digest("acceptance-sdk"),
                "toolchain_digest": digest("acceptance-toolchain"),
            },
        })
        .to_string()
    }

    /// The declaration and the asset map the guard consumes directly.
    fn parts(&self, app: &str) -> (String, BTreeMap<String, String>) {
        (self.manifest(app), self.assets.clone())
    }

    /// The bundle members this package contributes.
    fn members(&self, app: &str) -> Vec<(String, String)> {
        let mut members = vec![(
            format!("screens/{}/screens.json", self.tag),
            self.manifest(app),
        )];
        members.extend(
            self.assets
                .iter()
                .map(|(name, body)| (format!("screens/{}/{}", self.tag, name), body.clone())),
        );
        members
    }
}

/// The `screens/<tag>/` members of a bundle — the live snapshot `extract`
/// re-proves, with no `app.md` (legacy extraction fixtures keep working).
fn bundle(app: &str, screens: &[&Screen]) -> BTreeMap<String, String> {
    screens
        .iter()
        .flat_map(|screen| screen.members(app))
        .collect()
}

/// A whole app: the manifest, one workflow, and the screen packages — the
/// shape `validate_texts` admits.
fn install_bundle(app: &str, screens: &[&Screen]) -> Vec<(String, String)> {
    let mut files = vec![
        ("app.md".to_string(), app_md(app)),
        ("workflows/do.md".to_string(), WORKFLOW_MD.to_string()),
    ];
    for screen in screens {
        files.extend(screen.members(app));
    }
    files
}

fn app_md(app: &str) -> String {
    format!(
        "---\napp: {app}\ntitle: Acceptance app\nversion: '1'\n---\n\n# Guide\n\n\
         A screen-only acceptance fixture.\n"
    )
}

const WORKFLOW_MD: &str =
    "---\ntitle: Do\ngoal: g\n---\n\n## Step\nagent: a\n\nDo.\n\n### Acceptance\n- [ ] done\n";

/// Write one local app source tree — the manifest, one workflow, and the
/// screen packages' members — so a catalog/frame-kit consumer can snapshot it.
/// Every member is asserted to be a regular file: the nonregular/hard-link
/// refusal must never be what a fixture accidentally triggers.
fn write_source(dir: &Path, app: &str, screens: &[&Screen]) -> std::path::PathBuf {
    let source = dir.join("app");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("app.md"), app_md(app)).unwrap();
    std::fs::create_dir_all(source.join("workflows")).unwrap();
    std::fs::write(source.join("workflows/do.md"), WORKFLOW_MD).unwrap();
    for screen in screens {
        for (rel, body) in screen.members(app) {
            let path = source.join(&rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, body).unwrap();
            assert!(
                std::fs::symlink_metadata(&path).unwrap().is_file(),
                "{rel} is a regular file, so the regular-file refusal cannot apply"
            );
        }
    }
    source
}

fn media_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, ext)| ext) {
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        _ => unreachable!("fixture leaf {name} has no mapped media type"),
    }
}

fn digest(body: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(body.as_bytes()))
}

/// A deterministic ASCII body of exactly `bytes` bytes, bracketed by markers
/// so a render can be proved to carry the approved bytes verbatim. `unit`
/// must be ASCII and free of the frame's forbidden raw-text tokens.
fn body(bytes: usize, unit: &str, start: &str, end: &str) -> String {
    assert!(
        unit.is_ascii() && start.is_ascii() && end.is_ascii(),
        "fixture text must be ASCII"
    );
    assert!(
        bytes >= start.len() + end.len() + unit.len(),
        "fixture body of {bytes} bytes is too small for its markers"
    );
    let mut out = String::with_capacity(bytes);
    out.push_str(start);
    while out.len() + end.len() + unit.len() <= bytes {
        out.push_str(unit);
    }
    let gap = bytes - end.len() - out.len();
    out.push_str(&unit[..gap]);
    out.push_str(end);
    debug_assert_eq!(out.len(), bytes);
    out
}

fn js_body(bytes: usize) -> String {
    body(
        bytes,
        "var __cadenceAcceptance=(__cadenceAcceptance||0)+1;",
        "/*cadence-acceptance:js:begin*/",
        "/*cadence-acceptance:js:end*/",
    )
}

fn css_body(bytes: usize) -> String {
    body(
        bytes,
        ".cadence-acceptance-root{display:block;margin:0;}",
        "/*cadence-acceptance:css:begin*/",
        "/*cadence-acceptance:css:end*/",
    )
}

fn svg_body(bytes: usize) -> String {
    body(
        bytes,
        "<rect x=\"0\" y=\"0\" width=\"1\" height=\"1\"/>",
        "[svg-acceptance-begin]",
        "[svg-acceptance-end]",
    )
}

fn json_body(bytes: usize) -> String {
    body(
        bytes,
        "{\"acceptance\":0},",
        "[json-acceptance-begin]",
        "[json-acceptance-end]",
    )
}

/// The consumer-visible failure this ticket exists to fix: a package whose
/// script is over the former 400 KiB cap, whose stylesheet is over the former
/// 128 KiB cap, and whose assets together are over the former 512 KiB package
/// total — yet comfortably inside the 2 MiB budget — must be admitted and
/// rendered, with both approved bodies reaching the frame byte for byte.
#[test]
fn cad2008_large_valid_screen_package_is_admitted_and_rendered_intact() {
    let js = js_body(700 * KIB);
    let css = css_body(300 * KIB);
    let combined = js.len() + css.len();
    assert!(
        combined > 512 * KIB && combined < PACKAGE_BUDGET,
        "fixture must straddle the former package total and stay inside the budget"
    );
    let screen = Screen::new("main")
        .leaf("client.js", js.clone())
        .leaf("styles.css", css.clone());
    let files = bundle("demo", &[&screen]);

    let package = extract(&files, "main")
        .expect("a valid package inside the package budget is admitted for mount");
    assert_eq!(
        package.assets.get("client.js").map(String::as_str),
        Some(js.as_str()),
        "the checked package carries the exact approved script"
    );
    assert_eq!(
        package.assets.get("styles.css").map(String::as_str),
        Some(css.as_str()),
        "the checked package carries the exact approved stylesheet"
    );
    assert_eq!(package.app, "demo");

    let html = render_package_frame_html(
        "csp-nonce",
        "main",
        "bridge-nonce",
        7,
        &package,
        "https://board.example",
    )
    .expect("a package inside the budget renders a frame");
    assert!(
        html.len() > FORMER_FRAME_CAP,
        "the rendered document crosses the former 768 KiB frame cap ({} bytes)",
        html.len()
    );
    assert!(
        html.contains(js.as_str()),
        "the approved script bytes reach the frame verbatim"
    );
    assert!(
        html.contains(css.as_str()),
        "the approved stylesheet bytes reach the frame verbatim"
    );
}

/// The preview-export entry point builds the same document from explicit
/// css/js strings. An approved document above the former 768 KiB cap must
/// render, and it must still be the approved bytes — the cap was a size
/// refusal, never a reason to rewrite content.
#[test]
fn cad2008_frame_document_is_not_capped_at_the_former_768_kib() {
    let js = js_body(600 * KIB);
    let css = css_body(200 * KIB);
    assert!(js.len() + css.len() > FORMER_FRAME_CAP);

    let html = render_frame_html(
        "csp-nonce",
        "main",
        "bridge-nonce",
        3,
        &css,
        &js,
        "https://board.example",
    )
    .expect("an approved document above 768 KiB renders");
    assert!(
        html.len() > FORMER_FRAME_CAP,
        "document is {} bytes",
        html.len()
    );
    assert!(html.contains(js.as_str()));
    assert!(html.contains(css.as_str()));
}

/// The package budget is the whole-package aggregate, and it still refuses.
/// Both fixtures use leaves that are individually inside the budget; only the
/// sum differs, so a regression that drops the aggregate check cannot pass by
/// leaning on a per-leaf cap.
#[test]
fn cad2008_screen_assets_over_the_package_budget_are_refused() {
    let within = Screen::new("main")
        .leaf("client.js", js_body(900 * KIB))
        .leaf("styles.css", css_body(900 * KIB));
    let (manifest, assets) = within.parts("demo");
    assert!(assets.values().map(String::len).sum::<usize>() < PACKAGE_BUDGET);
    assert!(
        validate_map(&manifest, &assets).is_ok(),
        "1800 KiB of screen assets is inside the package budget"
    );

    let over = Screen::new("main")
        .leaf("client.js", js_body(1100 * KIB))
        .leaf("styles.css", css_body(1100 * KIB));
    let (manifest, assets) = over.parts("demo");
    assert!(assets.values().map(String::len).sum::<usize>() > PACKAGE_BUDGET);
    assert!(
        validate_map(&manifest, &assets).is_err(),
        "2200 KiB of screen assets must refuse at the package aggregate"
    );
}

/// Integrity is unchanged by the widening: a same-length body swap is caught
/// by the sha256 at the guard, at the mount re-proof, and at the frame seam,
/// while the untampered package is admitted. Sizes stay inside the former
/// caps so this proof is independent of the budget change.
#[test]
fn cad2008_tampered_screen_bytes_are_refused_at_every_consumer_seam() {
    let js = js_body(300 * KIB);
    let css = css_body(100 * KIB);
    let screen = Screen::new("main")
        .leaf("client.js", js.clone())
        .leaf("styles.css", css);
    let files = bundle("demo", &[&screen]);

    let package = extract(&files, "main").expect("the untampered package is admitted");
    assert_eq!(
        package.assets.get("client.js").map(String::as_str),
        Some(js.as_str())
    );

    // Same length, different bytes: only the digest can catch this.
    let index = js.len() / 2;
    let mut bytes = js.clone().into_bytes();
    let original = bytes[index];
    bytes[index] = if original == b'Z' { b'Q' } else { b'Z' };
    let tampered = String::from_utf8(bytes).expect("the tamper stays ASCII");
    assert_eq!(tampered.len(), js.len());
    assert_ne!(tampered, js);

    let mut assets = package.assets.clone();
    assets.insert("client.js".to_string(), tampered.clone());
    assert!(
        validate_map(&package.manifest, &assets).is_err(),
        "a same-length body swap must refuse at the declaration guard"
    );

    let mut tampered_files = files.clone();
    tampered_files.insert("screens/main/client.js".to_string(), tampered.clone());
    assert!(
        extract(&tampered_files, "main").is_err(),
        "the mount re-proof refuses the tampered bundle"
    );

    let mut mutated = package;
    mutated.assets.insert("client.js".to_string(), tampered);
    assert!(
        render_package_frame_html(
            "csp-nonce",
            "main",
            "bridge-nonce",
            4,
            &mutated,
            "https://board.example",
        )
        .is_err(),
        "the frame seam re-proves the declaration instead of trusting the caller"
    );
}

/// Member-name safety at the public seam: only flat `<stem>.<js|css|svg|json>`
/// leaves under a legal tag dir are addressable. A nested member, a
/// traversal-shaped leaf, an undeclared extra leaf, an uppercase stem and an
/// illegal tag all refuse rather than being filtered into something servable.
#[test]
fn cad2008_unsafe_screen_member_names_are_refused() {
    let screen = Screen::new("main").leaf("client.js", js_body(4 * KIB));
    let files = bundle("demo", &[&screen]);
    assert!(extract(&files, "main").is_ok());

    let mut nested = files.clone();
    nested.insert("screens/main/nested/client.js".to_string(), js_body(KIB));
    assert!(
        extract(&nested, "main").is_err(),
        "a nested member under the tag dir is not a flat screen leaf"
    );

    let mut traversal = files.clone();
    traversal.insert("screens/main/../client.js".to_string(), js_body(KIB));
    assert!(
        extract(&traversal, "main").is_err(),
        "a traversal-shaped leaf is refused, never resolved"
    );

    let mut undeclared = files.clone();
    undeclared.insert("screens/main/extra.js".to_string(), js_body(KIB));
    assert!(
        extract(&undeclared, "main").is_err(),
        "an undeclared leaf beside the declared set refuses"
    );

    let mut uppercase = files.clone();
    uppercase.insert("screens/main/Client.js".to_string(), js_body(KIB));
    assert!(
        extract(&uppercase, "main").is_err(),
        "an uppercase stem is not the bundle's leaf grammar"
    );

    assert!(
        extract(&files, "Main").is_err(),
        "an illegal tag is refused before any member is read"
    );
    let mut bad_tag = files.clone();
    bad_tag.insert(
        "screens/Main/screens.json".to_string(),
        screen.manifest("demo"),
    );
    assert!(
        tags_in(&bad_tag).is_err(),
        "the tag scan refuses an illegal tag dir"
    );
}

/// Old packages keep their existing behavior: a small v1 package with no
/// `app.md` still extracts and renders, carrying its approved assets verbatim.
#[test]
fn cad2008_existing_small_screen_packages_keep_rendering() {
    let js = js_body(2 * KIB);
    let css = css_body(512);
    let screen = Screen::new("main")
        .leaf("client.js", js.clone())
        .leaf("styles.css", css.clone());
    let files = bundle("demo", &[&screen]);

    let package = extract(&files, "main").expect("a legacy small package is admitted");
    let html = render_package_frame_html(
        "csp-nonce",
        "main",
        "bridge-nonce",
        1,
        &package,
        "https://board.example",
    )
    .expect("a legacy small package renders");
    assert!(html.contains(js.as_str()));
    assert!(html.contains(css.as_str()));
}

/// `screens.json` stays its own tightly bounded manifest: widening the asset
/// budget must not widen the declaration's byte cap.
#[test]
fn cad2008_screen_manifest_keeps_its_own_tight_byte_bound() {
    let screen = Screen::new("main").leaf("client.js", js_body(4 * KIB));
    let (manifest, assets) = screen.parts("demo");
    assert!(validate_map(&manifest, &assets).is_ok());

    let padded = format!("{manifest}{}", " ".repeat(SCREEN_MANIFEST_CAP + KIB));
    assert!(
        validate_map(&padded, &assets).is_err(),
        "an oversized screens.json refuses before any parse"
    );
}

/// The install gate admits the same large package: a bundle that installs is
/// the only bundle that can be mounted, so the widened screen budget has to
/// hold where the whole app is measured.
#[test]
fn cad2008_install_admits_a_large_screen_package_inside_the_whole_app_budget() {
    let screen = Screen::new("main")
        .leaf("client.js", js_body(700 * KIB))
        .leaf("styles.css", css_body(300 * KIB));
    let files = install_bundle("demo", &[&screen]);
    let total: usize = files.iter().map(|(_, text)| text.len()).sum();
    assert!(total < PACKAGE_BUDGET);

    let agents = HashSet::new();
    app::validate_texts(files, &agents, &[])
        .expect("a 1000 KiB screen package installs under the 2 MiB app budget");
}

/// The whole-app budget is retained, and it is the aggregate — not a per-leaf
/// or per-package cap — that refuses. Two independently valid packages fit;
/// adding a stylesheet to each pushes every leaf and every package inside its
/// own budget while the app crosses 2 MiB.
#[test]
fn cad2008_install_keeps_the_finite_whole_app_budget() {
    let agents = HashSet::new();
    let alpha = Screen::new("alpha").leaf("client.js", js_body(900 * KIB));
    let beta = Screen::new("beta").leaf("client.js", js_body(900 * KIB));
    let fits = install_bundle("demo", &[&alpha, &beta]);
    assert!(fits.iter().map(|(_, text)| text.len()).sum::<usize>() < PACKAGE_BUDGET);
    app::validate_texts(fits, &agents, &[])
        .expect("two 900 KiB screen packages fit the whole-app budget");

    let alpha = alpha.leaf("styles.css", css_body(300 * KIB));
    let beta = beta.leaf("styles.css", css_body(300 * KIB));
    let over = install_bundle("demo", &[&alpha, &beta]);
    let total: usize = over.iter().map(|(_, text)| text.len()).sum();
    assert!(total > PACKAGE_BUDGET);
    for screen in [&alpha, &beta] {
        let package_total: usize = screen.assets.values().map(String::len).sum();
        assert!(
            package_total < PACKAGE_BUDGET,
            "each package stays inside its own budget"
        );
    }
    let error = match app::validate_texts(over, &agents, &[]) {
        Err(error) => error,
        Ok(_) => panic!("a whole app over 2 MiB must refuse at install"),
    };
    let message = error.to_string();
    let bound = PACKAGE_BUDGET.to_string();
    assert!(
        message.contains(bound.as_str()),
        "the install refusal names the {bound}-byte aggregate bound: {message}"
    );
}

/// Every admitted screen leaf extension shares the package budget, not only
/// scripts and stylesheets: an `svg`/`json` leaf above the former 128 KiB flat
/// cap is a screen asset like any other.
#[test]
fn cad2008_screen_leaf_extensions_share_the_package_budget() {
    let svg = svg_body(200 * KIB);
    let json = json_body(200 * KIB);
    let screen = Screen::new("main")
        .leaf("client.js", js_body(4 * KIB))
        .leaf("icon.svg", svg.clone())
        .leaf("data.json", json.clone());

    let (manifest, assets) = screen.parts("demo");
    let checked = validate_map(&manifest, &assets)
        .expect("svg/json leaves are screen assets on the package budget");
    assert_eq!(checked.asset_count(), 3);

    let files = bundle("demo", &[&screen]);
    let package = extract(&files, "main").expect("the package is admitted whole");
    assert_eq!(
        package.assets.get("icon.svg").map(String::as_str),
        Some(svg.as_str())
    );
    assert_eq!(
        package.assets.get("data.json").map(String::as_str),
        Some(json.as_str())
    );
}

/// The local folder a consumer actually loads is snapshotted through the
/// descriptor-relative catalog reader, and the frame-kit export is that
/// consumer: `write_frame_kit` reads every member of the source tree with the
/// per-member cap the bundle grammar applies. An oversized *regular* screen
/// file must refuse with an actionable bound — the member's relative path and
/// the numeric cap — not the generic "not a regular file" refusal and not a
/// silent truncation. The oversized file is one byte past the package budget,
/// so the reported number is the applicable cap, not an incidental size.
#[test]
fn cad2008_local_catalog_refuses_oversized_screen_file_with_path_and_cap() {
    let dir = tempfile::TempDir::new().unwrap();
    let screen = Screen::new("main")
        .v2()
        .leaf("client.js", js_body(PACKAGE_BUDGET + 1));
    let source = write_source(dir.path(), "demo", &[&screen]);

    let out = dir.path().join("frame-kit");
    let error = crate::app_preview_export::write_frame_kit(
        &out,
        Some(&source),
        "https://board.example",
        false,
    )
    .expect_err("an oversized regular screen file must refuse the local frame-kit load");

    let message = error.to_string();
    let cap = PACKAGE_BUDGET.to_string();
    assert!(
        message.contains("screens/main/client.js"),
        "the refusal names the oversized member's relative path: {message}"
    );
    assert!(
        message.contains(cap.as_str()),
        "the refusal names the applicable cap ({cap} bytes): {message}"
    );
    assert!(!out.exists(), "a refused load creates no export directory");
}

/// The local snapshot behind the frame-kit export bounds the whole source
/// tree, not one member at a time: a bundle whose every file is inside its own
/// cap but whose total is over the package budget must refuse with the numeric
/// bound named, so the consumer can tell which limit it crossed.
#[test]
fn cad2008_local_snapshot_refuses_over_budget_app_with_numeric_bound() {
    let dir = tempfile::TempDir::new().unwrap();
    let screen = Screen::new("main")
        .v2()
        .leaf("client.js", js_body(1100 * KIB))
        .leaf("styles.css", css_body(1100 * KIB));
    let source = write_source(dir.path(), "demo", &[&screen]);

    let out = dir.path().join("frame-kit");
    let error = crate::app_preview_export::write_frame_kit(
        &out,
        Some(&source),
        "https://board.example",
        false,
    )
    .expect_err("a source over the whole-app budget must refuse the local frame-kit load");

    let message = error.to_string();
    let bound = PACKAGE_BUDGET.to_string();
    assert!(
        message.contains(bound.as_str()),
        "the snapshot refusal names the {bound}-byte aggregate bound: {message}"
    );
    assert!(!out.exists(), "a refused load creates no export directory");
}

/// The install gate names the offending member and its own cap when one file
/// is over its bound while the app total is not: an operator can act on the
/// diagnostic without guessing which limit applies.
#[test]
fn cad2008_install_names_the_oversized_member_and_its_cap() {
    let screen = Screen::new("main").leaf("client.js", js_body(4 * KIB));
    let mut files = install_bundle("demo", &[&screen]);
    let (manifest, _) = screen.parts("demo");
    let padded = format!("{manifest}{}", " ".repeat(SCREEN_MANIFEST_CAP + KIB));
    let entry = files
        .iter_mut()
        .find(|(rel, _)| rel.as_str() == "screens/main/screens.json")
        .expect("the install bundle carries the screen declaration");
    entry.1 = padded;
    let total: usize = files.iter().map(|(_, text)| text.len()).sum();
    assert!(total < PACKAGE_BUDGET);

    let agents = HashSet::new();
    let error = match app::validate_texts(files, &agents, &[]) {
        Err(error) => error,
        Ok(_) => panic!("a screen declaration over its own cap must refuse at install"),
    };
    let message = error.to_string();
    let cap = SCREEN_MANIFEST_CAP.to_string();
    assert!(
        message.contains("screens/main/screens.json"),
        "the install refusal names the oversized member: {message}"
    );
    assert!(
        message.contains(cap.as_str()),
        "the install refusal names the member's {cap}-byte cap: {message}"
    );
}

/// A JS body that is valid UTF-8 with multi-byte characters: its byte length
/// and its character count differ, so a size fact that counted characters
/// cannot stand in for the installed bytes.
fn utf8_js_body() -> String {
    concat!(
        "/*cadence-acceptance:utf8:begin*/",
        "var greeting=\"héllo → 世界 🙂\";",
        "/*cadence-acceptance:utf8:end*/",
    )
    .to_string()
}

/// The host-reported facts for these exact bodies: byte length and sha256.
fn facts_of(bodies: &BTreeMap<String, String>) -> serde_json::Value {
    let mut facts = serde_json::Map::new();
    for (name, body) in bodies {
        facts.insert(
            name.clone(),
            serde_json::json!({ "size": body.len(), "sha256": digest(body) }),
        );
    }
    serde_json::Value::Object(facts)
}

/// One `app.screen.integrity` request, encoded exactly as the shared ABI takes
/// it: one declaration, the facts for its bodies, and the number of screen
/// packages the bundle declares.
fn integrity_request_counting(
    manifest: &str,
    facts: &serde_json::Value,
    screen_count: u64,
) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "v": 1,
        "op": "app.screen.integrity",
        "input": {
            "manifest": manifest,
            "assets": facts,
            "screen_count": screen_count,
        },
    }))
    .expect("the request is bounded JSON")
}

/// The single-package request every other acceptance case sends.
fn integrity_request(manifest: &str, facts: &serde_json::Value) -> Vec<u8> {
    integrity_request_counting(manifest, facts, 1)
}

/// Run one request through the shared evaluator and decode its answer.
fn run_op(request: &[u8]) -> serde_json::Value {
    serde_json::from_slice(&cadence_core::evaluate::evaluate(request))
        .expect("the evaluator always answers JSON")
}

/// AOS-479: one validator serves both hosts, so one declaration must get the
/// same answer from the native body path (`validate_map`), the native fact path
/// (`validate_facts`) and the hosted operation (`app.screen.integrity`). The
/// body carries multi-byte UTF-8, and the facts must report its real byte
/// length — a character count is refused.
#[test]
fn cad2008_shared_core_admits_one_declaration_from_bodies_and_facts() {
    let body = utf8_js_body();
    assert!(
        body.len() > body.chars().count(),
        "the fixture must carry multi-byte UTF-8 so bytes and characters differ"
    );
    let screen = Screen::new("main").leaf("client.js", body.clone());
    let (manifest, bodies) = screen.parts("demo");

    let checked =
        validate_map(&manifest, &bodies).expect("the native body path admits the declaration");
    assert_eq!(checked.app(), "demo");
    assert_eq!(checked.asset_count(), 1);

    let mut native_facts = BTreeMap::new();
    native_facts.insert(
        "client.js".to_string(),
        crate::issue::app_screen_decl::ScreenAssetFact {
            size: body.len() as u64,
            sha256: digest(&body),
        },
    );
    let from_facts = crate::issue::app_screen_decl::validate_facts(&manifest, &native_facts)
        .expect("the native fact path admits the same declaration");
    assert_eq!(from_facts.app(), "demo");

    let answer = run_op(&integrity_request(&manifest, &facts_of(&bodies)));
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["op"], "app.screen.integrity");
    assert_eq!(answer["value"]["app"], "demo");

    // The same declaration with a character-count fact does not describe the
    // installed bytes, and the shared validator refuses it.
    let mut characters = facts_of(&bodies);
    characters["client.js"]["size"] = serde_json::json!(body.chars().count());
    let refused = run_op(&integrity_request(&manifest, &characters));
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["code"], "invalid_manifest");

    // A declaration that counted characters instead of bytes is not the
    // installed body either: the native body path refuses it, and so does the
    // operation run over the same bytes.
    let mut declared = serde_json::from_str::<serde_json::Value>(&manifest)
        .expect("the fixture declaration is JSON");
    declared["assets"][0]["size"] = serde_json::json!(body.chars().count());
    let declared = declared.to_string();
    assert!(validate_map(&declared, &bodies).is_err());
    let refused = run_op(&integrity_request(&declared, &facts_of(&bodies)));
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["code"], "invalid_manifest");
}

/// AOS-479: a same-length body swap keeps every declared size true, so only
/// the digest can catch it — and the hosted fact path catches it while the
/// declared bytes still pass, so the refusal is the digest and not the shape.
#[test]
fn cad2008_shared_core_refuses_same_length_tampered_bytes_from_facts() {
    let body = js_body(64 * KIB);
    let screen = Screen::new("main").leaf("client.js", body.clone());
    let (manifest, bodies) = screen.parts("demo");
    assert!(validate_map(&manifest, &bodies).is_ok());

    let mut bytes = body.clone().into_bytes();
    let index = bytes.len() / 2;
    let original = bytes[index];
    bytes[index] = if original == b'Z' { b'Q' } else { b'Z' };
    let tampered = String::from_utf8(bytes).expect("the tamper stays ASCII");
    assert_eq!(tampered.len(), body.len());
    assert_ne!(tampered, body);

    let swapped = facts_of(&BTreeMap::from([("client.js".to_string(), tampered)]));
    assert_eq!(swapped["client.js"]["size"], body.len() as u64);
    let refused = run_op(&integrity_request(&manifest, &swapped));
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["code"], "invalid_manifest");

    let accepted = run_op(&integrity_request(&manifest, &facts_of(&bodies)));
    assert_eq!(accepted["ok"], true, "{accepted}");
}

/// AOS-479: the facts are the caller's report, so the shared validator refuses
/// facts that cannot describe the declared bytes — a wrong digest, a malformed
/// digest, a missing fact and an undeclared extra fact — while the same
/// declaration with correct facts passes.
#[test]
fn cad2008_shared_core_refuses_bad_missing_and_extra_facts() {
    let body = js_body(4 * KIB);
    let screen = Screen::new("main").leaf("client.js", body);
    let (manifest, bodies) = screen.parts("demo");
    let good = facts_of(&bodies);
    assert_eq!(run_op(&integrity_request(&manifest, &good))["ok"], true);

    let mut wrong = good.clone();
    wrong["client.js"]["sha256"] = serde_json::json!(format!("sha256:{}", "0".repeat(64)));
    let mut malformed = good.clone();
    malformed["client.js"]["sha256"] = serde_json::json!("not-a-digest");
    let mut extra = good.clone();
    extra["styles.css"] = serde_json::json!({ "size": 0, "sha256": digest("") });

    for (case, facts) in [
        ("wrong digest", wrong),
        ("malformed digest", malformed),
        ("missing fact", serde_json::json!({})),
        ("extra fact", extra),
    ] {
        let refused = run_op(&integrity_request(&manifest, &facts));
        assert_eq!(refused["ok"], false, "{case}: {refused}");
        assert_eq!(refused["code"], "invalid_manifest", "{case}: {refused}");
    }
}

/// AOS-479: `tools` is the one map-valued declaration field, so a repeated
/// alias would otherwise collapse to its last value. The shared validator
/// refuses the duplicate through both entry points, while the same declaration
/// carrying one alias is valid.
#[test]
fn cad2008_shared_core_refuses_a_duplicate_tool_alias_in_v2() {
    let body = js_body(4 * KIB);
    let screen = Screen::new("main").leaf("client.js", body);
    let (v1, bodies) = screen.parts("demo");
    let with_tools = |tools: &str| {
        v1.replacen("\"app-screens/v1\"", "\"app-screens/v2\"", 1)
            .replacen(
                '{',
                &format!("{{\"host_contract\":\"screen-media.v1\",\"tools\":{tools},"),
                1,
            )
    };

    let unique = with_tools("{\"media.read\":\"catalog\"}");
    let duplicate = with_tools("{\"media.read\":\"catalog\",\"media.read\":\"catalog\"}");
    // The injection replaces the declaration's opening brace, so the object it
    // opens is closed by the declaration's own final brace. A fixture that
    // closed it early would be malformed JSON and would refuse for the wrong
    // reason, so both declarations are proved well-formed first.
    for declaration in [&unique, &duplicate] {
        assert!(
            serde_json::from_str::<serde_json::Value>(declaration).is_ok(),
            "the injected declaration must be well-formed JSON: {declaration}"
        );
    }
    assert!(validate_map(&unique, &bodies).is_ok(), "{unique}");
    let accepted = run_op(&integrity_request(&unique, &facts_of(&bodies)));
    assert_eq!(accepted["ok"], true, "{accepted}");

    assert!(validate_map(&duplicate, &bodies).is_err());
    let refused = run_op(&integrity_request(&duplicate, &facts_of(&bodies)));
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["code"], "invalid_manifest");
}

/// `count` legitimate screen packages, each a valid declaration over the same
/// real body, tagged so the bundle carries distinct packages.
fn screen_packages(count: usize, body: &str) -> Vec<Screen> {
    (0..count)
        .map(|index| Screen::new(format!("pkg{index}")).leaf("client.js", body.to_string()))
        .collect()
}

/// AOS-479: native package admission bounds one bundle at eight screen
/// packages, and the shared operation carries the same bound in
/// `screen_count`, so eight legitimate declarations — each a valid package over
/// real body facts — pass both.
#[test]
fn cad2008_eight_screen_packages_are_admitted_by_native_and_core() {
    let body = js_body(4 * KIB);
    let screens = screen_packages(8, &body);
    let files = bundle("demo", &screens.iter().collect::<Vec<_>>());
    let tags = tags_in(&files).expect("eight screen packages are a legal bundle");
    assert_eq!(tags.len(), 8);
    assert!(
        extract(&files, &tags[0]).is_ok(),
        "each package is a valid declaration over its own bytes"
    );

    let (manifest, bodies) = screens[0].parts("demo");
    let accepted = run_op(&integrity_request_counting(
        &manifest,
        &facts_of(&bodies),
        8,
    ));
    assert_eq!(accepted["ok"], true, "{accepted}");
    assert_eq!(accepted["value"]["app"], "demo");
}

/// AOS-479: nine packages exceed native's bound, and the operation refuses the
/// same set instead of admitting the declaration out of context. The identical
/// declaration and facts still pass as a one-package bundle, so the refusal is
/// the count and not the package.
#[test]
fn cad2008_nine_screen_packages_are_refused_by_native_and_core() {
    let body = js_body(4 * KIB);
    let screens = screen_packages(9, &body);
    let files = bundle("demo", &screens.iter().collect::<Vec<_>>());
    assert!(
        tags_in(&files).is_err(),
        "nine screen packages exceed the native bound"
    );

    let (manifest, bodies) = screens[0].parts("demo");
    let facts = facts_of(&bodies);
    let accepted = run_op(&integrity_request(&manifest, &facts));
    assert_eq!(accepted["ok"], true, "{accepted}");

    // A screen operation always represents at least one package: the same
    // declaration and facts are refused as a zero-package set.
    let zero = run_op(&integrity_request_counting(&manifest, &facts, 0));
    assert_eq!(zero["ok"], false, "{zero}");

    // The count is a u64 on every host. A value past a 32-bit `usize` is still
    // a well-formed command, so it must refuse as an out-of-range package
    // count — the package-set answer — never as a malformed command, and the
    // answer cannot differ between x64 and wasm32.
    let past_u32 = run_op(&integrity_request_counting(
        &manifest,
        &facts,
        4_294_967_296,
    ));
    assert_eq!(past_u32["ok"], false, "{past_u32}");
    assert_eq!(past_u32["code"], "invalid_manifest", "{past_u32}");

    let refused = run_op(&integrity_request_counting(&manifest, &facts, 9));
    assert_eq!(refused["ok"], false, "{refused}");
}
