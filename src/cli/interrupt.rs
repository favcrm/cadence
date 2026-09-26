//! CAD-535: `cadence interrupt` — moved verbatim from src/main.rs.

use super::*;

pub(super) fn run(state_dir: PathBuf, alias: String, wait: u64) -> Result<i32> {
    let answer = client::rpc(
        &state_dir,
        "interrupt",
        json!({"alias": alias, "wait": wait}),
    )?;
    print_json(&answer);
    Ok(0)
}
