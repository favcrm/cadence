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
    /// This does not write, and it does not claim a live session reloaded.
    Status,
    /// Read a bundled skill without writing anything: a JSON manifest of
    /// its files and byte counts, or with `--file` that file's exact bytes.
    Show {
        #[arg(value_parser = cadence_agent::skill::SKILL_NAMES)]
        name: String,
        /// A bundled file path, for example `scripts/next.py`.
        #[arg(long)]
        file: Option<String>,
    },
    /// Non-force refresh used by native `cadence update` so the activated
    /// binary writes its own embedded bytes. Not a second installer:
    /// `cadence skill install` remains the manual repair. Matching files
    /// and foreign provider entries are left unchanged.
    #[command(hide = true)]
    Sync,
}

/// Read the copy bundled in this executable, without consulting home or state.
pub(super) fn show(name: &str, file: Option<&str>) -> Result<i32> {
    match cadence_agent::skill::show(name, file)? {
        cadence_agent::skill::ShowOutput::Manifest(manifest) => print_json(&manifest),
        cadence_agent::skill::ShowOutput::File(text) => cadence_agent::output::print_text(text),
    }
    Ok(0)
}

pub(super) fn run(_state_dir: PathBuf, action: SkillAction) -> Result<i32> {
    match action {
        SkillAction::Install { name } => {
            let home = home_dir()?;
            let report = cadence_agent::skill::sync(&home, true, name.as_deref())?;
            print_json(&report);
        }
        SkillAction::Status => print_json(&cadence_agent::skill::status(&home_dir()?)),
        SkillAction::Show { name, file } => return show(&name, file.as_deref()),
        SkillAction::Sync => {
            let home = home_dir()?;
            if cadence_agent::sandbox::profile().is_some() {
                print_json(&json!({
                    "skipped": true,
                    "wrote": false,
                    "reason": "sandbox profile never writes the shared home",
                    "guidance": "not-checked",
                    "repair": "cadence skill install",
                    "live_session": "not-checked",
                    "status": cadence_agent::skill::status(&home),
                }));
                return Ok(0);
            }
            match cadence_agent::skill::sync(&home, false, None) {
                Ok(sync) => {
                    let status = cadence_agent::skill::status(&home);
                    print_json(&json!({
                        "skipped": false,
                        "wrote": sync["wrote"],
                        "sync": sync,
                        "guidance": status["guidance"],
                        "repair": status["repair"],
                        "attention": status["attention"],
                        "live_session": "not-checked",
                        "live_session_note": status["live_session_note"],
                        "status": status,
                    }));
                }
                Err(error) => {
                    let status = cadence_agent::skill::status(&home);
                    print_json(&json!({
                        "skipped": false,
                        "wrote": false,
                        "error": error.to_string(),
                        "guidance": "needs-attention",
                        "repair": "cadence skill install",
                        "live_session": "not-checked",
                        "live_session_note": status["live_session_note"],
                        "status": status,
                    }));
                }
            }
        }
    }
    Ok(0)
}
