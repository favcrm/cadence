//! CAD-535: `cadence export` — moved verbatim from src/main.rs.
//! CAD-1163: `--portable --tracker <PATH>` writes a coordination-only
//! `cadence.portable/1` package instead of the legacy bundle. `--app`
//! selections are refused before anything is read or written.

use super::*;

pub(super) fn run(
    state_dir: PathBuf,
    out: PathBuf,
    portable: bool,
    tracker: Option<PathBuf>,
    apps: Vec<String>,
    snapshot_working_tree: bool,
) -> Result<i32> {
    if !portable {
        print_json(&cadence_agent::backup::export(&state_dir, &out)?);
        return Ok(0);
    }
    let tracker = tracker.ok_or_else(|| Error::usage("--portable needs --tracker <PATH>"))?;
    let opts = cadence_agent::backup::portable::PortableExportOptions {
        scope: cadence_agent::backup::portable::Scope::Coordination,
        tracker,
    };
    let report = if snapshot_working_tree {
        cadence_agent::backup::portable::export_portable_snapshot_selected(
            &state_dir, &out, &opts, &apps,
        )?
    } else {
        cadence_agent::backup::portable::export_portable_selected(&state_dir, &out, &opts, &apps)?
    };
    print_json(&report);
    Ok(0)
}
