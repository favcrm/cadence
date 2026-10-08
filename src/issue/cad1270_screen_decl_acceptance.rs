//! CAD-1270 acceptance check (ticket item 3): the shared bundle validator
//! `app::validate_texts` — the function `install`, `install-check` and the
//! offline `app check` all run — refuses a package whose `screens/<tag>/`
//! declaration fails the authoritative `app_screen_pkg::extract` /
//! `app_screen_decl::validate_map` integrity gate, and admits one that
//! passes it.
//!
//! This module is the non-implementer check the ticket requires for a
//! changed install-admission guard. The implementer wires it with
//! `#[cfg(test)] mod cad1270_screen_decl_acceptance;` inside
//! `crate::issue` and may not edit it.
//!
//! Every fixture carries a real, valid `workflows/do.md` so the CAD-1177
//! standalone-tools ("zero-workflow") admission exception can never
//! substitute for — or mask — missing screen-declaration validation:
//! the `has_tools_only_screen` branch only matters when
//! `workflow_count == 0`, and here it never is.
//!
//! No PM, state dir, daemon, network or operator session is touched:
//! `validate_texts` takes bundle bytes plus an empty agent registry —
//! exactly the offline surface `app check` exposes.

use super::*;

use crate::issue::app;
use serde_json::json;
use sha2::{Digest, Sha256};

/// A real, valid workflow — the same shape the app bundles ship. Its
/// presence forces `workflow_count > 0`, so nothing in the CAD-1177
/// tools-only path can rescue or mask the screen checks under test.
const WORKFLOW: &str = "---\ntitle: \"Post: {{topic}}\"\ngoal: \"Publish {{topic}}\"\n\
inputs:\n  topic: { ask: \"About what?\" }\n---\n\n\
Why.\n\n## Do {{topic}}\nagent: dev-1\nsize: S\n\nDo it.\n\n### Acceptance\n- [ ] done\n";

fn sha256(body: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(body.as_bytes()))
}

/// A valid `app.md` frontmatter+body. `needs.connections` is the empty
/// list; no capabilities/views/requires — the plainest bundle that
/// parses (mirrors workspace-apps fixture grammar).
const APP_MD: &str =
    "---\napp: fixture-app\ntitle: Fixture\nversion: '1'\nneeds:\n  connections: []\n---\n\nGuide.\n";

/// The v1 screen declaration `validate_map` accepts: exact contract,
/// `client.js` entry declared, each declared asset carrying the real
/// body's media/size/sha256, inert provenance digest syntax, empty `may`.
fn screen_decl(app: &str, assets: &[(&str, &str)]) -> String {
    let members: Vec<serde_json::Value> = assets
        .iter()
        .map(|(name, body)| {
            let media = match name.rsplit_once('.').map(|(_, e)| e).unwrap_or("") {
                "js" => "text/javascript",
                "css" => "text/css",
                "svg" => "image/svg+xml",
                "json" => "application/json",
                _ => "application/octet-stream",
            };
            json!({"name":name,"media_type":media,"sha256":sha256(body),"size":body.len()})
        })
        .collect();
    json!({"contract":"app-screens/v1","app":app,"entry":"client.js",
           "assets":members,
           "provenance":{"source_digest":sha256("s"),"sdk_digest":sha256("k"),"toolchain_digest":sha256("t")},
           "may":[]})
    .to_string()
}

/// A complete bundle as `(relpath, body)` pairs — app.md, one workflow
/// and a `screens/<tag>/` package built from `decl`/`assets`.
fn bundle(decl: &str, assets: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut files = vec![
        ("app.md".to_string(), APP_MD.to_string()),
        ("workflows/do.md".to_string(), WORKFLOW.to_string()),
        ("screens/board/screens.json".to_string(), decl.to_string()),
    ];
    for (name, body) in assets {
        files.push((format!("screens/board/{name}"), body.to_string()));
    }
    files
}

/// The empty agent registry an offline check always runs — the same
/// arguments `app check` passes.
fn validate(files: Vec<(String, String)>) -> Result<app::Validated> {
    app::validate_texts(files, &Default::default(), &[])
}

/// Positive control: a bundle whose screen declaration matches its
/// supplied bytes passes the shared validator whole. This proves the
/// fixture itself is sound — every refusal below is then attributable
/// to the mutated declaration, not a broken baseline.
#[test]
fn cad1270_valid_screen_package_passes_validate_texts() {
    let js = "console.log('x');";
    let css = "body{}";
    let files = bundle(
        &screen_decl("fixture-app", &[("client.js", js), ("styles.css", css)]),
        &[("client.js", js), ("styles.css", css)],
    );
    let validated = validate(files)
        .unwrap_or_else(|e| panic!("valid screen package must pass validate_texts: {e}"));
    assert_eq!(validated.manifest.app, "fixture-app");
}

/// A declaration the authoritative `validate_map` refuses must refuse
/// `validate_texts` even though every workflow parses and every cap is
/// met. Each mutation targets a different integrity clause: tampered
/// supplied bytes (sha256/size mismatch), malformed JSON, an undeclared
/// leaf smuggled beside declared ones, and a wrong contract version.
#[test]
fn cad1270_invalid_screen_declaration_refuses_validate_texts() {
    let js = "console.log('x');";

    // (a) Declared for `js` but the bundle ships tampered bytes —
    //     declared sha256/size no longer match the supplied body.
    let files = bundle(
        &screen_decl("fixture-app", &[("client.js", js)]),
        &[("client.js", "tampered();")],
    );
    assert!(
        validate(files).is_err(),
        "tampered screen asset must be refused"
    );

    // (b) screens.json that is not valid JSON at all.
    let files = bundle("{not json", &[("client.js", js)]);
    assert!(
        validate(files).is_err(),
        "malformed screens.json must be refused"
    );

    // (c) An undeclared leaf sitting next to the declared entry —
    //     the declared↔supplied set match must refuse it.
    let files = bundle(
        &screen_decl("fixture-app", &[("client.js", js)]),
        &[("client.js", js), ("extra.js", "e")],
    );
    assert!(
        validate(files).is_err(),
        "undeclared screen leaf must be refused"
    );

    // (d) A contract string outside {app-screens/v1, app-screens/v2}.
    let decl = screen_decl("fixture-app", &[("client.js", js)])
        .replace("app-screens/v1", "app-screens/v9");
    let files = bundle(&decl, &[("client.js", js)]);
    assert!(
        validate(files).is_err(),
        "unknown screen contract must be refused"
    );
}

/// The orphan-package case `tags_in` + `extract` alone cannot catch:
/// `screens/board/` carries a `client.js` leaf but NO `screens.json`
/// declaration, so `tags_in` (which enumerates only manifest-bearing
/// tags) yields nothing and a per-tag loop never visits it. The shared
/// gate must instead refuse any `screens/<tag>/` member set lacking its
/// declaration — install-check must not admit a screen package the
/// mount RPC would refuse for "no screens.json".
#[test]
fn cad1270_screen_dir_without_declaration_refuses_validate_texts() {
    let js = "console.log('x');";
    let files = vec![
        ("app.md".to_string(), APP_MD.to_string()),
        ("workflows/do.md".to_string(), WORKFLOW.to_string()),
        // screens/board/client.js present, screens/board/screens.json absent.
        ("screens/board/client.js".to_string(), js.to_string()),
    ];
    assert!(
        validate(files).is_err(),
        "a screens/<tag>/ package with no screens.json must be refused"
    );
}
