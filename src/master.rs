//! The master agent (CAD-339): one per install, alias `master`, a
//! managed Claude session (Codex waits for a read-only sandbox) the
//! daemon starts from the agent files `agents/master/SOUL.md` and
//! `AGENT.md`.
//!
//! **Where the files live.** CAD-338 (the agent filesystem) is not
//! implemented yet, so this uses the smallest location its design
//! record names: `<pm>/agents/<slug>/` — the tracker dir (`~/pm` or
//! `CADENCE_PM_DIR`), which is already git and already has one writer.
//! `agents/` holds no `project.yaml`, so it is never read as a project.
//! The repo carries the default templates (`agents/master/`); the
//! daemon installs any that are missing on `master start`.
//!
//! **One writer.** SOUL.md and AGENT.md change only through
//! [`write_file`] (`cadence master edit`, daemon RPC `agent_file_write`),
//! which the daemon runs for the proven operator only — an agent, the
//! master included, is refused before anything is written. Each write
//! (and each install) records the files' digest in the state dir;
//! `master start` refuses files whose digest does not match — an edit
//! made around the writer is caught at the next launch instead of
//! becoming the master's instructions.
//!
//! **Launch hardening.** The master never implements, so its Claude
//! session runs in an empty cwd under the state dir, with no settings
//! files, hooks or MCP servers (`--restricted`, `--strict-mcp-config`),
//! only the Bash tool, `dontAsk`, and exactly the `cadence` subcommands
//! in [`CLAUDE_ALLOWED_TOOLS`]; forge and platform credentials are
//! dropped from its env ([`DENIED_ENV`], an empty `GH_CONFIG_DIR`). All
//! of it is keyed on the alias, not on stored params. The daemon's own
//! allowlist (`daemon::MASTER_ALLOWED`) is the second line. This is a
//! process guard, not a security boundary: a same-uid process can still
//! read credential files and edit the tracker by hand (see
//! docs/design/AGENT-FILESYSTEM.md).

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::issue::Pm;

/// The master's alias — and its agent slug.
pub const ALIAS: &str = "master";
/// The agent files the briefing is built from, in briefing order.
pub const FILES: [&str; 2] = ["SOUL.md", "AGENT.md"];
/// Size caps from the agent-filesystem design record (characters).
pub const SOUL_MAX_CHARS: usize = 4_000;
pub const AGENT_MAX_CHARS: usize = 20_000;

const SOUL_TEMPLATE: &str = include_str!("../agents/master/SOUL.md");
const AGENT_TEMPLATE: &str = include_str!("../agents/master/AGENT.md");

/// The exact `cadence` subcommands the master's Claude session may run
/// — its whole toolset. Nothing else is allowed: the session runs in
/// `dontAsk` mode, so anything not listed is denied without a prompt.
/// Never a bare `Bash(cadence *)`: `cadence build-slot run -- <argv>`
/// execs anything (review round 1, C1).
pub const CLAUDE_ALLOWED_TOOLS: &[&str] = &[
    "Bash(cadence issue ls)",
    "Bash(cadence issue ls *)",
    "Bash(cadence issue show *)",
    "Bash(cadence issue project ls)",
    "Bash(cadence issue project ls *)",
    "Bash(cadence plan show *)",
    "Bash(cadence plan propose *)",
    "Bash(cadence master dispatch *)",
    "Bash(cadence master escalate *)",
    "Bash(cadence master summary)",
    "Bash(cadence master summary *)",
    "Bash(cadence report file *)",
    "Bash(cadence agent list)",
    "Bash(cadence agent list *)",
    "Bash(cadence agent show *)",
    "Bash(cadence status)",
    "Bash(cadence status *)",
];

/// The only built-in tool the master's Claude session has (`--tools`):
/// Bash, narrowed by [`CLAUDE_ALLOWED_TOOLS`]. No Read/Edit/Write/Web.
pub const CLAUDE_TOOLS: &str = "Bash";

/// Tools the master's Claude session may never use, whatever its
/// stored params say — belt and braces over `--tools`/`dontAsk`.
pub const CLAUDE_DISALLOWED_TOOLS: &[&str] = &[
    "Edit",
    "Write",
    "MultiEdit",
    "NotebookEdit",
    "Bash(gh)",
    "Bash(gh *)",
    "Bash(git push *)",
    "Bash(git merge *)",
    "Bash(git commit *)",
];

/// Forge and platform credentials removed from the master's env. The
/// scrub is by name; a credential stored in a file (gh's `hosts.yml`,
/// ssh keys) is what the empty `GH_CONFIG_DIR` and the tool denials
/// cover, and what stays a documented gap.
pub const DENIED_ENV: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GITLAB_TOKEN",
    "GL_TOKEN",
    "SSH_AUTH_SOCK",
    "CLOUDFLARE_API_TOKEN",
    "CLOUDFLARE_API_KEY",
    "CF_API_TOKEN",
    "VERCEL_TOKEN",
    "NETLIFY_AUTH_TOKEN",
    "FLY_API_TOKEN",
    "NPM_TOKEN",
    "CARGO_REGISTRY_TOKEN",
];

pub fn is_master(alias: &str) -> bool {
    alias == ALIAS
}

/// Env the master's provider gets on top of the usual identity pair:
/// `gh` finds no stored login (an empty config dir under the state dir),
/// git never prompts for credentials, and the tracker is named
/// explicitly — the master's cwd is not the tracker.
pub fn env_overrides(state_dir: &Path, pm_dir: Option<&Path>) -> Vec<(String, String)> {
    let gh = state_dir.join("master").join("no-forge");
    let _ = std::fs::create_dir_all(&gh);
    let mut env = vec![
        (
            "GH_CONFIG_DIR".to_string(),
            gh.to_string_lossy().to_string(),
        ),
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
    ];
    if let Some(pm) = pm_dir {
        env.push((
            "CADENCE_PM_DIR".to_string(),
            pm.to_string_lossy().to_string(),
        ));
    }
    env
}

/// The master's working directory: an empty folder under the state dir
/// (review round 1, I4). Never the tracker or a repo — Claude would load
/// a CLAUDE.md, `.mcp.json`, hooks or `.claude/settings*.json` any
/// agent can plant there.
pub fn workdir(state_dir: &Path) -> PathBuf {
    state_dir.join("master").join("cwd")
}

/// `<pm>/agents/<slug>` — the agent's folder.
pub fn agent_dir(pm_dir: &Path, slug: &str) -> PathBuf {
    pm_dir.join("agents").join(slug)
}

fn check_slug(slug: &str) -> Result<()> {
    if slug.is_empty()
        || slug.len() > 64
        || !slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(Error::rejected(format!(
            "Bad agent slug '{slug}' — lowercase letters, digits and '-'"
        )));
    }
    Ok(())
}

/// Refuse a name outside [`FILES`] and text over its cap. Refused, never
/// truncated.
pub fn check_file(name: &str, text: &str) -> Result<()> {
    let cap = match name {
        "SOUL.md" => SOUL_MAX_CHARS,
        "AGENT.md" => AGENT_MAX_CHARS,
        other => {
            return Err(Error::rejected(format!(
                "'{other}' is not an agent file — one of {}",
                FILES.join(", ")
            )))
        }
    };
    if text.trim().is_empty() {
        return Err(Error::rejected(format!("{name} is empty")));
    }
    let chars = text.chars().count();
    if chars > cap {
        return Err(Error::rejected(format!(
            "{name} is {chars} characters — the cap is {cap}; shorten it"
        )));
    }
    Ok(())
}

/// The folder, refusing symlinks anywhere on `agents/<slug>` — the
/// writer never follows a link out of the tracker.
fn real_dir(pm_dir: &Path, slug: &str) -> Result<PathBuf> {
    check_slug(slug)?;
    let agents = pm_dir.join("agents");
    let dir = agent_dir(pm_dir, slug);
    for p in [&agents, &dir] {
        if p.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            return Err(Error::rejected(format!(
                "{} is a symlink — agent files are never written through links",
                p.display()
            )));
        }
    }
    Ok(dir)
}

fn default_template(slug: &str, name: &str) -> Option<&'static str> {
    match (slug, name) {
        (ALIAS, "SOUL.md") => Some(SOUL_TEMPLATE),
        (ALIAS, "AGENT.md") => Some(AGENT_TEMPLATE),
        _ => None,
    }
}

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    if path.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{} is a symlink — refusing to write through it",
            path.display()
        )));
    }
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// Install the repo's default templates for every missing file of the
/// master, in one tracker commit. Existing files are never touched.
/// Returns the names installed (empty when nothing was missing).
pub fn install_defaults(pm: &Pm, actor: &str) -> Result<Vec<String>> {
    let dir = real_dir(&pm.dir, ALIAS)?;
    let _lock = pm.lock()?;
    let missing: Vec<&str> = FILES
        .iter()
        .copied()
        .filter(|name| dir.join(name).symlink_metadata().is_err())
        .collect();
    if missing.is_empty() {
        return Ok(vec![]);
    }
    std::fs::create_dir_all(&dir)?;
    let mut written = Vec::new();
    let undo = |written: &[PathBuf]| {
        for p in written {
            let _ = std::fs::remove_file(p);
        }
    };
    for name in &missing {
        let text = default_template(ALIAS, name).unwrap_or_default();
        let path = dir.join(name);
        if let Err(e) = write_atomic(&path, text) {
            undo(&written);
            return Err(e);
        }
        written.push(path);
    }
    if let Err(e) = crate::issue::write::commit(
        pm,
        &format!("agents/{ALIAS}: install default {}", missing.join(", ")),
        &[],
        actor,
    ) {
        undo(&written);
        return Err(e);
    }
    Ok(missing.iter().map(|s| s.to_string()).collect())
}

/// Replace one agent file — the only writer of SOUL.md and AGENT.md.
/// The daemon calls it for the proven operator only. Validated and
/// secret-scanned before anything is written; one tracker commit.
pub fn write_file(pm: &Pm, slug: &str, name: &str, text: &str, actor: &str) -> Result<Value> {
    check_file(name, text)?;
    let warnings = crate::secret::guard(&format!("agents/{slug}/{name}"), text)?;
    let dir = real_dir(&pm.dir, slug)?;
    let _lock = pm.lock()?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    let before = std::fs::read(&path).ok();
    write_atomic(&path, text)?;
    if let Err(e) =
        crate::issue::write::commit(pm, &format!("agents/{slug}: write {name}"), &[], actor)
    {
        // Put the old file back — a refused commit leaves no write.
        match &before {
            Some(old) => {
                let _ = std::fs::write(&path, old);
            }
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
        return Err(e);
    }
    let mut out = json!({
        "agent": slug,
        "file": name,
        "path": path,
        "changed": before.as_deref() != Some(text.as_bytes()),
    });
    if !warnings.is_empty() {
        out["secret_warnings"] = crate::secret::warnings_json(&warnings);
    }
    Ok(out)
}

/// Read the agent's files in [`FILES`] order, refusing symlinks and
/// files over their caps. `all` requires every file; otherwise a
/// missing one is skipped.
pub fn read_files(pm_dir: &Path, slug: &str, all: bool) -> Result<Vec<(String, String)>> {
    let dir = real_dir(pm_dir, slug)?;
    let mut out = Vec::new();
    for name in FILES {
        let path = dir.join(name);
        let meta = path.symlink_metadata();
        if meta.as_ref().is_ok_and(|m| m.is_symlink()) {
            return Err(Error::rejected(format!(
                "{} is a symlink — agent files are never read through links",
                path.display()
            )));
        }
        if meta.is_err() && !all {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::rejected(format!("cannot read {}: {e}", path.display())))?;
        check_file(name, &text)?;
        out.push((name.to_string(), text));
    }
    Ok(out)
}

/// sha256 of one file's text.
pub fn digest(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn record_path(state_dir: &Path) -> PathBuf {
    state_dir.join("agent-files.json")
}

fn load_records(state_dir: &Path) -> Value {
    std::fs::read_to_string(record_path(state_dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

/// Remember the digest of `name` the operator's writer (or the
/// installer) left.
pub fn record(state_dir: &Path, slug: &str, name: &str, digest: &str) -> Result<()> {
    let mut all = load_records(state_dir);
    if !all[slug].is_object() {
        all[slug] = json!({});
    }
    all[slug][name] = json!({"digest": digest, "at": crate::issue::time::now_epoch()});
    let path = record_path(state_dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&all)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// The launch check: each file must be the one the operator's writer
/// (or the installer) last recorded. It checks every file before it
/// records anything, so a refusal writes nothing. A file with no record
/// yet (placed before this check existed, or just installed) is
/// trusted once and recorded.
pub fn verify(state_dir: &Path, slug: &str, files: &[(String, String)]) -> Result<()> {
    let all = load_records(state_dir);
    let mut unrecorded = Vec::new();
    for (name, text) in files {
        let now = digest(text);
        match all[slug][name]["digest"].as_str() {
            None => unrecorded.push((name, now)),
            Some(known) if known == now => {}
            Some(_) => {
                return Err(Error::invalid(
                    "agent_files_changed",
                    format!(
                        "agents/{slug}/{name} changed outside `cadence master edit` — refusing \
                         to brief the master from it. Review it, then re-save it as the \
                         operator: `cadence master edit {name} --file <path>`"
                    ),
                ))
            }
        }
    }
    for (name, d) in unrecorded {
        record(state_dir, slug, name, &d)?;
    }
    Ok(())
}

/// Longest escalation summary the operator is shown.
pub const ESCALATION_SUMMARY_MAX: usize = 4_000;

fn escalations_path(state_dir: &Path) -> PathBuf {
    state_dir.join("escalations.json")
}

/// The escalations the daemon recorded (CAD-339), keyed
/// `<issue>/<question report>`: `{issue, question, summary, by, at}`.
/// Only the daemon's `question_escalate` writes this file — a report
/// file can never put a question in front of the operator.
pub fn escalations(state_dir: &Path) -> serde_json::Map<String, Value> {
    std::fs::read_to_string(escalations_path(state_dir))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Record one escalation; refuses a question already escalated. The
/// caller (the daemon) serializes writers.
pub fn record_escalation(state_dir: &Path, key: &str, record: Value) -> Result<()> {
    let mut all = escalations(state_dir);
    if all.contains_key(key) {
        return Err(Error::rejected(format!(
            "{key} is already escalated to the operator"
        )));
    }
    all.insert(key.to_string(), record);
    let path = escalations_path(state_dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&Value::Object(all))?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// The briefing text: SOUL.md then AGENT.md, verbatim and byte-stable
/// (the prefix caches), under one heading naming their source.
pub fn compose(files: &[(String, String)]) -> String {
    let mut out = String::from(
        "# Master briefing\n\nBuilt by the daemon from agents/master/ (SOUL.md, AGENT.md). \
         Only the operator changes these files.\n",
    );
    for (name, text) in files {
        out.push_str(&format!("\n<!-- agents/master/{name} -->\n"));
        out.push_str(text.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_fit_their_caps() {
        check_file("SOUL.md", SOUL_TEMPLATE).unwrap();
        check_file("AGENT.md", AGENT_TEMPLATE).unwrap();
        assert!(check_file("MEMORY.md", "x").is_err());
        assert!(check_file("SOUL.md", " \n").is_err());
        let long = "x".repeat(SOUL_MAX_CHARS + 1);
        let err = check_file("SOUL.md", &long).unwrap_err().to_string();
        assert!(err.contains("cap is 4000"), "{err}");
    }

    #[test]
    fn verify_trusts_once_then_refuses_a_changed_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let soul = |t: &str| vec![("SOUL.md".to_string(), t.to_string())];
        verify(tmp.path(), "master", &soul("v1")).unwrap();
        verify(tmp.path(), "master", &soul("v1")).unwrap();
        // A newly present file is trusted once; a changed one refuses —
        // and the refusal records nothing for the files beside it.
        let both = vec![
            ("SOUL.md".to_string(), "v2".to_string()),
            ("AGENT.md".to_string(), "a1".to_string()),
        ];
        let err = verify(tmp.path(), "master", &both).unwrap_err().to_string();
        assert!(
            err.contains("agents/master/SOUL.md changed outside"),
            "{err}"
        );
        assert!(load_records(tmp.path())["master"]["AGENT.md"].is_null());
        record(tmp.path(), "master", "SOUL.md", &digest("v2")).unwrap();
        verify(tmp.path(), "master", &both).unwrap();
        assert!(load_records(tmp.path())["master"]["AGENT.md"].is_object());
    }

    #[test]
    fn escalations_record_once() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(escalations(tmp.path()).is_empty());
        record_escalation(tmp.path(), "D-1/q.md", json!({"summary": "s"})).unwrap();
        let err = record_escalation(tmp.path(), "D-1/q.md", json!({}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("already escalated"), "{err}");
        assert_eq!(escalations(tmp.path())["D-1/q.md"]["summary"], "s");
    }

    #[test]
    fn compose_keeps_file_order_and_text() {
        let files = vec![
            ("SOUL.md".to_string(), "soul text\n".to_string()),
            ("AGENT.md".to_string(), "agent text".to_string()),
        ];
        let text = compose(&files);
        let soul = text.find("soul text").unwrap();
        let agent = text.find("agent text").unwrap();
        assert!(soul < agent, "{text}");
        assert!(text.contains("<!-- agents/master/AGENT.md -->"));
    }

    #[test]
    fn slugs_and_symlinks_are_refused() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(real_dir(tmp.path(), "../x").is_err());
        assert!(real_dir(tmp.path(), "Master").is_err());
        std::fs::create_dir_all(tmp.path().join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("elsewhere"), tmp.path().join("agents"))
            .unwrap();
        let err = real_dir(tmp.path(), "master").unwrap_err().to_string();
        assert!(err.contains("symlink"), "{err}");
    }
}
