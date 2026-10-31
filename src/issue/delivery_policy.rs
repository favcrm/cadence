//! `PROJECT.md` `delivery:` reader and the approval store, over the portable
//! policy model in `cadence-core`.
//!
//! CAD-1946: the model, its parser and the merge rules moved to
//! `cadence_core::delivery` so the hosted tenant evaluates with the same code
//! as the CLI. This module keeps only what needs a filesystem: the strict
//! `load` and the approval rows.

use std::path::Path;

use serde_json::json;

use crate::error::{Error, Result};
use crate::issue::work;

pub use cadence_core::delivery::*;

/// Strict load: no file/section is `Ok(None)`; a symlinked, unreadable
/// or malformed one refuses — an approval never guesses.
pub fn load(pm_dir: &Path, key: &str) -> Result<Option<DeliveryPolicy>> {
    parse_project(key, work::read_project_md(pm_dir, key)?.as_deref())
        .map_err(|e| Error::rejected(e.to_string()))
}

/// Every project's latest `project_work_approve` delivery half, from the
/// daemon. An unreachable daemon is an empty map — unapproved reads as the
/// defaults, the fail-safe direction.
pub fn fetch_approvals(state_dir: &Path) -> DeliveryApprovals {
    crate::client::rpc(state_dir, "project_work_approvals", json!({}))
        .ok()
        .and_then(|v| v["approvals"].as_object().cloned())
        .map(|m| {
            m.into_iter()
                .filter_map(|(k, v)| approved_from(&v).map(|a| (k, a)))
                .collect()
        })
        .unwrap_or_default()
}
