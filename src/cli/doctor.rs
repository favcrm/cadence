//! CAD-535: `cadence doctor` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(state_dir: PathBuf, host: bool, json: bool, reclaim_plan: bool) -> Result<i32> {
    if host {
        return cadence_agent::doctor::host::cli(&state_dir, json, reclaim_plan);
    }
    let report = cadence_agent::doctor::run(&state_dir)?;
    print_json(&report);
    let ok = report
        .pointer("/checks/storage/ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(if ok { 0 } else { 1 })
}
