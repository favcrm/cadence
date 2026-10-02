//! CAD-1006: serve-time extraction + live integrity re-proof of one
//! `screens/<tag>/` package inside an installed workspace bundle.
//!
//! A screen package is a set of flat bundle members
//! `screens/<tag>/screens.json` (the `app-screens/v1` declaration,
//! [`crate::issue::app_screen_decl`]) plus its declared asset leaves
//! `screens/<tag>/<stem>.<js|css|svg|json>`. This module does ONE thing:
//! given the live `snapshot` of an installed bundle (the exact bytes
//! `bundle_digest` covers) and a tag, it re-runs the declaration
//! validator against the live members and returns the checked assets —
//! so a mounted frame executes only bytes whose sha256 equals the
//! currently-approved live digest.
//!
//! It grants no authority of its own: the caller (the daemon screen
//! verbs) has already proven the operator connection AND re-checked the
//! live `bundle_digest`/`app_capability_status` approval pin. The frame
//! is never served a partially-valid package — a manifest that fails
//! `validate_map`, a missing `screens.json`, or a member that is absent
//! from the live tree refuses the whole mount, never half of it.

use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::issue::{app_screen_decl, model};

/// The declaration file every screen tag must carry inside the bundle.
pub const SCREENS_DIR: &str = "screens";
/// The declaration filename (the app-screens/v1 manifest) within a tag dir.
pub const SCREEN_MANIFEST: &str = "screens.json";
/// Most screen tags one bundle may declare — the count a scan accepts.
const MAX_SCREENS: usize = 8;

/// One screen package, integrity-checked against the live bundle bytes.
/// Carries the validated assets and the declared app tag; never paths,
/// never authority.
#[derive(Debug)]
pub struct ScreenPackage {
    /// The bundle member prefix `screens/<tag>/` this package lives under.
    pub tag: String,
    /// `screens/<tag>/screens.json` verbatim — the validated declaration.
    pub manifest: String,
    /// Declared asset leaf name → body, integrity-checked against the
    /// declaration (`client.js` guaranteed present and JS-typed).
    pub assets: BTreeMap<String, String>,
    /// The declared `app` tag the package was reviewed as.
    pub app: String,
}

/// `true` when `tag` is a legal screen tag (the bundle's tag grammar).
pub fn valid_tag(tag: &str) -> bool {
    model::valid_tag(tag)
}

/// `true` when `leaf` is an admissible screen member name: the
/// declaration `screens.json`, or a flat `<stem>.<js|css|svg|json>`
/// leaf whose stem is `valid_tag`. Mirrors the validator's name
/// grammar (`check_asset_name`) so install-time admission and
/// serve-time integrity agree on the same allowed leaves.
pub fn leaf_ok(leaf: &str) -> bool {
    if leaf == SCREEN_MANIFEST {
        return true;
    }
    let Some((stem, ext)) = leaf.rsplit_once('.') else {
        return false;
    };
    matches!(ext, "js" | "css" | "svg" | "json") && model::valid_tag(stem)
}

/// The screen tags a live bundle carries — the `screens/<tag>/` dirs
/// that contain a `screens.json`, derived from the file list only.
/// Bounded at [`MAX_SCREENS`]; a malformed or oversized set refuses.
pub fn tags_in(files: &BTreeMap<String, String>) -> Result<Vec<String>> {
    let prefix = format!("{SCREENS_DIR}/");
    let mut tags = Vec::new();
    for name in files.keys() {
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Some((tag, leaf)) = rest.split_once('/') else {
            continue;
        };
        if leaf != SCREEN_MANIFEST {
            continue;
        }
        if !valid_tag(tag) {
            return Err(Error::rejected(format!(
                "screen tag {tag:?} is not a valid tag"
            )));
        }
        if !tags.contains(&tag.to_string()) {
            tags.push(tag.to_string());
            if tags.len() > MAX_SCREENS {
                return Err(Error::rejected("too many screen packages in one bundle"));
            }
        }
    }
    Ok(tags)
}

/// Re-prove one screen package against the live bundle snapshot.
///
/// Collects the `screens/<tag>/` members from `files`, refuses when the
/// `screens.json` declaration is absent, then runs
/// [`app_screen_decl::validate_map`] over the declaration and the leaf
/// map so every supplied byte is declared and every declared byte is
/// present with its sha256+size. Any refusal aborts the whole mount.
pub fn extract(files: &BTreeMap<String, String>, tag: &str) -> Result<ScreenPackage> {
    if !valid_tag(tag) {
        return Err(Error::rejected(format!("screen tag {tag:?} is not a valid tag")));
    }
    let prefix = format!("{SCREENS_DIR}/{tag}/");
    let manifest_key = format!("{prefix}{SCREEN_MANIFEST}");
    let manifest = files.get(&manifest_key).cloned().ok_or_else(|| {
        Error::rejected(format!("installation has no screen package '{tag}'"))
    })?;
    // Collect the leaf assets for this tag — every other member under
    // the prefix. The declaration decides which leaves are admitted;
    // an undeclared leaf present in the live tree refuses via the
    // declared↔supplied set match, so we supply exactly the leaves the
    // bundle carries (no extra filtering here — the validator is the gate).
    let mut assets: BTreeMap<String, String> = BTreeMap::new();
    for (name, text) in files {
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        if rest == SCREEN_MANIFEST {
            continue;
        }
        // A leaf is one flat segment — the bundle grammar already
        // guarantees this, but refuse a nested member rather than hand
        // the validator a name it cannot match.
        if rest.contains('/') {
            return Err(Error::rejected(format!(
                "screen package '{tag}' carries a nested member {name:?}"
            )));
        }
        assets.insert(rest.to_string(), text.clone());
    }
    let checked = app_screen_decl::validate_map(&manifest, &assets).map_err(|e| {
        Error::rejected(format!("screen package '{tag}' failed integrity: {e}"))
    })?;
    Ok(ScreenPackage {
        tag: tag.to_string(),
        manifest,
        assets,
        app: checked.app().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sha2::{Digest, Sha256};

    fn sha256(body: &str) -> String {
        format!("sha256:{:x}", Sha256::digest(body.as_bytes()))
    }

    fn decl(assets: &[(&str, &str)]) -> String {
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
        json!({"contract":"app-screens/v1","app":"crm","entry":"client.js",
               "assets":members,
               "provenance":{"source_digest":sha256("s"),"sdk_digest":sha256("k"),"toolchain_digest":sha256("t")},
               "may":[]})
        .to_string()
    }

    fn bundle_with(tag: &str, decl: &str, assets: &[(&str, &str)]) -> BTreeMap<String, String> {
        let mut files = BTreeMap::new();
        files.insert("app.md".to_string(), "m".to_string());
        files.insert(format!("screens/{tag}/screens.json"), decl.to_string());
        for (name, body) in assets {
            files.insert(format!("screens/{tag}/{name}"), body.to_string());
        }
        files
    }

    #[test]
    fn extract_runs_validate_map_against_live_bytes() {
        let js = "console.log('x');";
        let css = "body{}";
        let decl = decl(&[("client.js", js), ("styles.css", css)]);
        let files = bundle_with("day-cards", &decl, &[("client.js", js), ("styles.css", css)]);
        let pkg = extract(&files, "day-cards").unwrap();
        assert_eq!(pkg.tag, "day-cards");
        assert_eq!(pkg.app, "crm");
        assert_eq!(pkg.assets.len(), 2);
        assert_eq!(pkg.assets["client.js"], js);
    }

    #[test]
    fn extract_refuses_missing_manifest_tampered_asset_and_undeclared_leaf() {
        let js = "x";
        let decl = decl(&[("client.js", js)]);
        // No screens.json.
        let mut files = BTreeMap::new();
        files.insert(format!("screens/a/client.js"), js.to_string());
        assert!(extract(&files, "a").is_err());
        // Tampered asset body (declared hash is for different bytes).
        let files = bundle_with("a", &decl, &[("client.js", "tampered")]);
        assert!(extract(&files, "a").is_err());
        // Undeclared leaf present in the live tree.
        let mut files = bundle_with("a", &decl, &[("client.js", js)]);
        files.insert("screens/a/extra.js".to_string(), "e".to_string());
        assert!(extract(&files, "a").is_err());
        // Nested member under the tag prefix refuses outright.
        let mut files = bundle_with("a", &decl, &[("client.js", js)]);
        files.insert("screens/a/nested/x.js".to_string(), "e".to_string());
        assert!(extract(&files, "a").is_err());
    }

    #[test]
    fn tags_in_discovers_manifests_only_and_bounds() {
        let decl = decl(&[("client.js", "x")]);
        let mut files = BTreeMap::new();
        files.insert("screens/a/screens.json".to_string(), decl.clone());
        files.insert("screens/b/client.js".to_string(), "x".to_string()); // no manifest
        files.insert("screens/c/screens.json".to_string(), decl);
        let tags = tags_in(&files).unwrap();
        assert_eq!(tags, vec!["a".to_string(), "c".to_string()]);
    }
}
