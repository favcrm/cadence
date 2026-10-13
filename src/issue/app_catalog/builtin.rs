//! CAD-1422: the host's embedded built-in catalog is retired. Detached
//! app repositories are the canonical source of truth (installed from a
//! trusted path or Git checkout); discovery and marketplace installs
//! moved to the AgenticOS Apps page. The host no longer ships
//! `workspace-apps/` bundles and embeds nothing.
//!
//! `Builtin`, `list` and `get` remain as a compatibility shell so
//! callers that enumerate the catalog or resolve an id keep their
//! signature — they now answer "no distributions". A `builtin:<id>`
//! source is a refused historical source form, never a lookup: `get`
//! only runs through `retired()`, which returns the explicit retirement
//! refusal. `Source::Builtin` rows recorded by older hosts still
//! deserialize — their stored bundles keep working unchanged.

use std::collections::BTreeMap;

use crate::error::{Error, Result};

/// The refusal every `builtin:<id>` path resolves through. The id is
/// never a bundle lookup — the catalog is gone, known and unknown ids
/// get the same retired answer.
fn retired(id: &str) -> Error {
    Error::rejected(format!(
        "unknown built-in catalog id '{id}' — the built-in app catalog was \
         retired (CAD-1422); install apps from the detached app repositories \
         or the AgenticOS marketplace, and manage existing installs from Apps"
    ))
}

/// One historical built-in catalog entry. Retired: kept only so the
/// `Builtin`/`list`/`get` interface survives — never installable
/// content (`files` is always empty).
#[allow(dead_code)]
pub struct Builtin {
    pub id: &'static str,
    pub featured: bool,
    pub files: BTreeMap<String, String>,
}

/// The shipped catalog. The catalog is retired, so this is always
/// empty — the interface stays for callers that enumerate it.
pub fn list() -> Result<Vec<Builtin>> {
    Ok(Vec::new())
}

/// Resolve a catalog id. Always the retired refusal: a `builtin:` source
/// is a historical form, not a live distribution, and there is no
/// embedded bundle to answer for any id.
pub fn get(id: &str) -> Result<Option<Builtin>> {
    Err(retired(id))
}
