//! The role's launch environment as a type (CAD-2239).
//!
//! `RoleEnv` can only be built by [`reviewer_gh_env`]: its field is private to
//! this module, which no sibling adapter can reach. Both transports take a
//! `&RoleEnv` at launch and apply it last, after their scrub and after every
//! other pair, so a local launch that skips the role environment does not
//! compile. `StdioAdapter::launch_protected` is the one exception: it clears
//! the environment and passes no tokens, so there is nothing to pin.

use std::process::Command;

use super::ProviderEnv;
use crate::error::Result;

/// A role that reads as a review. One predicate for the `gh` pin and for the
/// reviewer-pane refusal, so they never disagree.
pub(crate) fn is_reviewer_role(role: &str) -> bool {
    role.to_ascii_lowercase().contains("review")
}

/// The role's environment pairs: `gh` pinned for a review role, empty
/// otherwise. Only [`reviewer_gh_env`] constructs it.
#[derive(Debug)]
pub struct RoleEnv(Vec<(String, String)>);

impl RoleEnv {
    /// The pairs, for a launch that cannot take a `Command` (a tmux pane).
    pub fn pairs(&self) -> &[(String, String)] {
        &self.0
    }

    /// Apply the pairs to a child. Call it last.
    pub fn apply(&self, command: &mut Command) {
        for (key, value) in &self.0 {
            command.env(key, value);
        }
    }
}

/// A reviewer agent's `gh` environment (CAD-844).
///
/// A managed worker keeps `HOME`, so `gh` would otherwise read the operator's
/// saved login under `~/.config/gh` and could post a PR comment or a status
/// with the operator's account. A reviewer gets a private, empty
/// `GH_CONFIG_DIR` under its own agent directory instead, plus
/// `CADENCE_REVIEWER_GH_TOKEN` as `GH_TOKEN` when the operator has provisioned
/// a read-only token. With no token `gh` is unauthenticated — the fail-closed
/// direction, never the operator's account. The token's scope is the
/// operator's responsibility and nothing here copies their login.
pub(crate) fn reviewer_gh_env(
    env: &ProviderEnv,
    role: &str,
    agent_dir: &std::path::Path,
) -> Result<RoleEnv> {
    // Fail closed on spelling: any role that reads as a review gets the pin,
    // and a variant like `Reviewer` never falls through to the operator's
    // saved login. Pinning a role that turns out not to review is harmless —
    // it costs an empty, unauthenticated `gh`.
    if !is_reviewer_role(role) {
        return Ok(RoleEnv(Vec::new()));
    }
    let dir = agent_dir.join("gh-review");
    super::ensure_private_dir(&dir)?;
    let mut pairs = vec![(
        "GH_CONFIG_DIR".to_string(),
        dir.to_string_lossy().to_string(),
    )];
    // `gh` prefers these over the config file, so an inherited value would
    // defeat the config pin. Always override both: the provisioned read-only
    // token when present, an empty value (unset to `gh`) when not.
    let token = env
        .var("CADENCE_REVIEWER_GH_TOKEN")
        .filter(|value| !value.trim().is_empty());
    pairs.push(("GH_TOKEN".to_string(), token.unwrap_or_default()));
    // The enterprise-host variants are read the same way for a GHE host.
    for name in [
        "GITHUB_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
    ] {
        pairs.push((name.to_string(), String::new()));
    }
    Ok(RoleEnv(pairs))
}
