//! CAD-535: `cadence export` — moved verbatim from src/main.rs.
//! CAD-1163: `--portable --tracker <PATH>` writes a coordination-only
//! `cadence.portable/1` package instead of the legacy bundle. `--app`
//! selections are refused before anything is read or written.
//! CAD-1580: `--redact-store-secrets` is the operator's explicit opt-in
//! to redact blocking credential-shaped spans inside the private export
//! copy; it never rewrites the live store.

use super::*;

pub(super) fn run(
    state_dir: PathBuf,
    out: PathBuf,
    portable: bool,
    tracker: Option<PathBuf>,
    apps: Vec<String>,
    snapshot_working_tree: bool,
    redact_store_secrets: bool,
) -> Result<i32> {
    if !portable {
        print_json(&cadence_agent::backup::export(
            &state_dir,
            &out,
            redact_store_secrets,
        )?);
        return Ok(0);
    }
    let tracker = tracker.ok_or_else(|| Error::usage("--portable needs --tracker <PATH>"))?;
    let opts = cadence_agent::backup::portable::PortableExportOptions {
        scope: cadence_agent::backup::portable::Scope::Coordination,
        tracker,
        redact_store_secrets,
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
