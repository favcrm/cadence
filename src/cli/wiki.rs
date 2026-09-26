//! CAD-580: `cadence wiki` — the wiki v1 store verbs (git text +
//! content-addressed blobs) over the daemon's wiki RPCs.

use super::*;

pub(super) fn run(state_dir: PathBuf, action: cadence_agent::wiki::cli::WikiAction) -> Result<i32> {
    cadence_agent::wiki::cli::run(&action, &state_dir)
}
