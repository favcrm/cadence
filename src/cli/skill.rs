//! CAD-535: `cadence skill` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum SkillAction {
    /// Install bundled public guidance, or a named skill and its guidance
    /// dependencies. Existing real provider directories/files are left alone.
    Install {
        #[arg(value_parser = cadence_agent::skill::SKILL_NAMES)]
        name: Option<String>,
    },
    /// Inspect every bundled guidance file and each provider's discovery link.
    Status,
}

pub(super) fn run(_state_dir: PathBuf, action: SkillAction) -> Result<i32> {
    let home = home_dir()?;
    match action {
        SkillAction::Install { name } => {
            let report = cadence_agent::skill::sync(&home, true, name.as_deref())?;
            print_json(&report);
        }
        SkillAction::Status => print_json(&cadence_agent::skill::status(&home)),
    }
    Ok(0)
}
