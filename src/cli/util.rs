//! CAD-1002: small loose helpers (git, worktree, duration, verbs) — moved
//! verbatim from `cli/mod.rs` (CAD-984 PR-2).

use super::*;

/// Run a git subcommand in `dir`, returning stdout or a rejected error
/// carrying stderr.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = cadence_agent::reaper::output(Command::new("git").arg("-C").arg(dir).args(args))
        .map_err(|_| Error::rejected("`git` is required and was not found on PATH"))?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `git worktree add <root>/.cadence/wt/<name> -b cadence/<name>` — the
/// new checkout becomes the agent's cwd. Shared with `issue start`
/// through `cadence_agent::worktree`.
pub(crate) fn create_worktree(base: &Path, name: &str) -> Result<PathBuf> {
    cadence_agent::worktree::create_worktree(base, name)
}

/// `--older-than` duration: bare seconds or an s/m/h/d-suffixed value.
pub(crate) fn parse_duration(text: &str) -> Result<f64> {
    let (num, mult) = match text.chars().last() {
        Some('s') => (&text[..text.len() - 1], 1.0),
        Some('m') => (&text[..text.len() - 1], 60.0),
        Some('h') => (&text[..text.len() - 1], 3600.0),
        Some('d') => (&text[..text.len() - 1], 86400.0),
        _ => (text, 1.0),
    };
    let secs = num
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(|v| v * mult);
    secs.ok_or_else(|| {
        Error::rejected(format!(
            "Invalid duration '{text}' — use seconds or a suffix: 30m, 12h, 7d"
        ))
    })
}

/// This binary's top-level verbs — setup names a fix only by a verb
/// that exists.
pub(crate) fn cli_verbs() -> Vec<String> {
    use clap::CommandFactory;
    Cli::command()
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect()
}
