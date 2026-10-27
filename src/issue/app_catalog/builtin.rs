//! CAD-1422: the host's embedded built-in catalog is retired. Detached
//! app repositories are the canonical source of truth (installed from a
//! trusted path or Git checkout); discovery and marketplace installs
//! moved to the AgenticOS Apps page. The host no longer ships
//! `workspace-apps/` bundles and embeds nothing.
//!
//! Every `builtin:<id>` path resolves through one refusal: the id is
//! never a bundle lookup — the catalog is gone, known and unknown ids
//! get the same retired answer. A `builtin:<id>` source is a refused
//! historical source form. `Source::Builtin` rows recorded by older
//! hosts still deserialize — their stored bundles keep working
//! unchanged.
//!
//! The catalog interface itself is gone: nothing enumerates the
//! built-in catalog any more, so there is no `list` and no entry type
//! to keep alive.

use crate::error::{Error, Result};

/// The refusal every `builtin:<id>` path resolves through.
pub fn refuse_retired(id: &str) -> Result<()> {
    Err(Error::rejected(format!(
        "unknown built-in catalog id '{id}' — the built-in app catalog was \
         retired (CAD-1422); install apps from the detached app repositories \
         or the AgenticOS marketplace, and manage existing installs from Apps"
    )))
}
