//! CAD-1129 H1: the built-in catalog — `workspace-apps/*` embedded in
//! the binary at build time, so the catalog works offline and is
//! reviewed with the host. `catalog.json` lists the entries; each
//! entry's bundle ships as `include_str!` rows, giving `files()` the
//! same `rel-path → text` snapshot the installer validates.
//!
//! A Git-sourced app is the other catalog source (Explorer → "Add from
//! Git URL", operator only). There is no third-party store in v1.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::Deserialize;

use crate::error::{Error, Result};

/// `workspace-apps/catalog.json` — the embedded index.
const INDEX: &str = include_str!("../../../workspace-apps/catalog.json");

#[derive(Deserialize)]
struct Index {
    schema: u32,
    entries: Vec<IndexEntry>,
}

#[derive(Deserialize)]
struct IndexEntry {
    id: String,
    featured: bool,
    dir: String,
}

/// One built-in catalog entry: its id, featured flag, and the bundle
/// snapshot the host embedded at build time.
pub struct Builtin {
    pub id: &'static str,
    pub featured: bool,
    pub files: BTreeMap<String, String>,
}

fn bundle(files: &[(&str, &str)]) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(rel, text)| (rel.to_string(), text.to_string()))
        .collect()
}

/// The embedded bundles, keyed by catalog id. A file added to a
/// `workspace-apps/<app>/` directory must be listed here — the build
/// refuses (compile error) when a bundle's `app.md` names an app the
/// index does not carry, and `load()` refuses a `catalog.json` entry
/// whose `dir` has no embedded files.
static BUNDLES: LazyLock<BTreeMap<&'static str, BTreeMap<String, String>>> = LazyLock::new(|| {
    let mut bundles = BTreeMap::new();
    bundles.insert(
        "crm",
        bundle(&[
            ("app.md", include_str!("../../../workspace-apps/crm/app.md")),
            (
                "app-chat.json",
                include_str!("../../../workspace-apps/crm/app-chat.json"),
            ),
            (
                "workflows/email-brief.md",
                include_str!("../../../workspace-apps/crm/workflows/email-brief.md"),
            ),
            (
                "rubrics/email.md",
                include_str!("../../../workspace-apps/crm/rubrics/email.md"),
            ),
            (
                "assets/crm.svg",
                include_str!("../../../workspace-apps/crm/assets/crm.svg"),
            ),
        ]),
    );
    bundles.insert(
        "social-content",
        bundle(&[
            (
                "app.md",
                include_str!("../../../workspace-apps/social-content/app.md"),
            ),
            (
                "app-chat.json",
                include_str!("../../../workspace-apps/social-content/app-chat.json"),
            ),
            (
                "workflows/facebook.md",
                include_str!("../../../workspace-apps/social-content/workflows/facebook.md"),
            ),
            (
                "workflows/image-instagram.md",
                include_str!("../../../workspace-apps/social-content/workflows/image-instagram.md"),
            ),
            (
                "workflows/image-manual.md",
                include_str!("../../../workspace-apps/social-content/workflows/image-manual.md"),
            ),
            (
                "workflows/instagram.md",
                include_str!("../../../workspace-apps/social-content/workflows/instagram.md"),
            ),
            (
                "workflows/source-instagram.md",
                include_str!(
                    "../../../workspace-apps/social-content/workflows/source-instagram.md"
                ),
            ),
            (
                "rubrics/brand.md",
                include_str!("../../../workspace-apps/social-content/rubrics/brand.md"),
            ),
            (
                "assets/social-content.svg",
                include_str!("../../../workspace-apps/social-content/assets/social-content.svg"),
            ),
        ]),
    );
    bundles
});

/// Every built-in entry, in `catalog.json` order. Refuses a malformed
/// index, a duplicate id, or an entry with no embedded bundle.
pub fn list() -> Result<Vec<Builtin>> {
    static PARSED: LazyLock<Result<Index>> = LazyLock::new(|| {
        serde_json::from_str::<Index>(INDEX)
            .map_err(|e| Error::internal(format!("workspace-apps/catalog.json is malformed: {e}")))
    });
    let index = match &*PARSED {
        Ok(index) => index,
        Err(e) => return Err(Error::internal(e.to_string())),
    };
    if index.schema != 1 || index.entries.is_empty() {
        return Err(Error::internal(
            "workspace-apps/catalog.json needs schema 1 and at least one entry",
        ));
    }
    let mut entries = Vec::new();
    for entry in &index.entries {
        let id: &'static str = match BUNDLES.keys().find(|k| **k == entry.id) {
            Some(id) => id,
            None => {
                return Err(Error::internal(format!(
                    "catalog entry '{}' has no embedded bundle under workspace-apps/",
                    entry.id
                )))
            }
        };
        if !crate::issue::model::valid_tag(&entry.id) {
            return Err(Error::internal(format!(
                "catalog entry id '{}' is not tag-shaped",
                entry.id
            )));
        }
        let files = BUNDLES
            .get(id)
            .cloned()
            .ok_or_else(|| Error::internal("embedded bundle missing"))?;
        let _ = entry.dir;
        entries.push(Builtin {
            id,
            featured: entry.featured,
            files,
        });
    }
    Ok(entries)
}

/// One entry by catalog id.
pub fn get(id: &str) -> Result<Option<Builtin>> {
    Ok(list()?.into_iter().find(|b| b.id == id))
}
