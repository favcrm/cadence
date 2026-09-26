//! CAD-535: `cadence send` — moved verbatim from src/main.rs.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    state_dir: PathBuf,
    alias: String,
    text: Option<String>,
    file: Option<PathBuf>,
    message: Option<String>,
    reply_to: Option<String>,
    task: Option<String>,
    ready: bool,
    force: bool,
    nudge: bool,
    steer: SteerArgs,
) -> Result<i32> {
    // Identical path to `message send` — the verb form is sugar,
    // not a second implementation.
    let (result, _) = send_message(
        &state_dir, &alias, text, file, message, reply_to, ready, force, task, nudge, steer,
    )?;
    print_json(&result);
    Ok(0)
}
