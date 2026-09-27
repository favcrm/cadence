//! CAD-630 foundation: explicit single-workspace installation catalog.
//! No daemon/HTTP routing, execution or grant translation is enabled here.
use std::path::Path;

use crate::error::{Error, Result};
use crate::issue::Pm;

#[derive(Debug, Default)]
pub struct Catalog;

#[derive(Debug)]
pub struct Installation {
    pub install_id: String,
    pub project: Option<String>,
}

/// The first migration seam is specified before its implementation.
pub fn migrate(_pm: &Pm, _state_dir: &Path) -> Result<Catalog> {
    Ok(Catalog)
}

impl Catalog {
    pub fn resolve_legacy(
        &self,
        _root: &Path,
        _project: &str,
        _name: &str,
    ) -> Result<Installation> {
        Err(Error::rejected("installation catalog is not implemented"))
    }
}
