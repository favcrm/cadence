//! CAD-535: `cadence skill` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum SkillAction {
    /// Write the vendored SKILL.md and create the `cadence` symlinks.
    /// Existing real dirs/files named `cadence` are left alone.
    Install,
    /// Report whether the installed copy matches the binary's vendored
    /// skill and which link dirs are wired.
    Status,
}

pub(super) fn run(_state_dir: PathBuf, action: SkillAction) -> Result<i32> {
    let home = home_dir()?;
    match action {
        SkillAction::Install => {
            let report = cadence_agent::skill::sync(&home, true)?;
            print_json(&report);
        }
        SkillAction::Status => print_json(&cadence_agent::skill::status(&home)),
    }
    Ok(0)
}
