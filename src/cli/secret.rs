//! CAD-535: `cadence secret` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum SecretAction {
    /// Scan text for credential-shaped strings: the vendored gitleaks
    /// rule pack plus cadence's bare-token and argv rules. Prints JSON
    /// findings `{rule, line, column, redacted, severity, fingerprint}`,
    /// never the value. Exit 0 when clean or warn-only, 1 on any
    /// blocking finding. `<state dir>/secret-allowlist.toml` (operator
    /// edited) drops allowlisted findings.
    Scan {
        /// Scan this file (its path also scopes path-specific rules);
        /// else stdin.
        #[arg(long)]
        file: Option<PathBuf>,
    },
}

pub(super) fn run(state_dir: PathBuf, action: SecretAction) -> Result<i32> {
    match action {
        SecretAction::Scan { file } => {
            use cadence_agent::secret;
            let text = match &file {
                Some(path) => String::from_utf8_lossy(&std::fs::read(path).map_err(|e| {
                    Error::rejected(format!("Cannot read {}: {e}", path.display()))
                })?)
                .into_owned(),
                None => {
                    if atty_stdin() {
                        return Err(Error::rejected("Pipe text on stdin or pass --file"));
                    }
                    let mut bytes = Vec::new();
                    std::io::stdin().read_to_end(&mut bytes)?;
                    String::from_utf8_lossy(&bytes).into_owned()
                }
            };
            let allow = secret::Allowlist::load(&state_dir)?;
            let path = file.as_ref().map(|p| p.to_string_lossy().into_owned());
            let (report, blocking) = secret::report(&text, path.as_deref(), &allow)?;
            print_json(&report);
            Ok(if blocking { 1 } else { 0 })
        }
    }
}
