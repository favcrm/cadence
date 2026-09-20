//! `cadence report` — intake: a question, feedback, idea or bug
//! becomes a tracker issue with context, instead of dying in a
//! terminal scrollback. Routing is by kind, not by cwd: `question`,
//! `feedback` and `bug` are about cadence itself and file into the
//! `cadence` project from wherever the operator stands; `idea` is
//! about the project being worked on and resolves like `issue new`
//! (cwd repo, `CADENCE_PROJECT`, `--project` — which always wins).
//! `--issue` files a comment on an existing issue instead.
//!
//! Every report also (a) sends one line to the project's PM inbox
//! when one is resolvable — the reporter's `upstream` first, then the
//! project's `team.yaml` `roles.pm.alias` — and (b) surfaces as an
//! Overview `needs_me` row of kind `intake` while the issue sits in
//! `backlog` (see `src/overview.rs`). The report never fails because
//! notification did — the issue file is the durable record.
//!
//! Anything written — body, cwd, repo, actor — passes through the
//! shared argv scrubber (`doctor::host::redact_argv` applied per
//! whitespace token): a report must never carry a credential.

use std::path::Path;

use serde_json::{json, Value};

use crate::client;
use crate::doctor::host::redact_argv;
use crate::error::{Error, Result};
use crate::issue::{board, model, project, time, write, Pm};

/// `report`'s kinds — the routing key. `value_enum` keeps clap in
/// sync; `as_str` is the stored tag/comment kind.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Kind {
    Question,
    Feedback,
    Idea,
    Bug,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Question => "question",
            Kind::Feedback => "feedback",
            Kind::Idea => "idea",
            Kind::Bug => "bug",
        }
    }
    /// Kinds that concern cadence itself — always the `cadence`
    /// project regardless of cwd.
    fn about_cadence(self) -> bool {
        matches!(self, Kind::Question | Kind::Feedback | Kind::Bug)
    }
    fn default_priority(self) -> &'static str {
        match self {
            Kind::Bug => "P2",
            _ => "P3",
        }
    }
}

/// A credential-shaped token inside any captured string is replaced
/// before it reaches the tracker. Whitespace-split so `k=v`, `--k=v`
/// and bare token shapes all hit the same scrubber as process argv.
fn scrub(text: &str) -> String {
    redact_argv(&text.split_whitespace().collect::<Vec<_>>())
}

/// The routing decision: which project the report files into.
fn target_project(pm: &Pm, kind: Kind, flag: Option<&str>, cwd: &Path) -> Result<project::Project> {
    // --project always wins, whatever the kind.
    if let Some(name) = flag {
        return project::resolve(&pm.dir, Some(name), cwd);
    }
    if kind.about_cadence() {
        return project::list(&pm.dir)?
            .into_iter()
            .find(|p| p.key == "cadence")
            .ok_or_else(|| {
                Error::rejected(
                    "No 'cadence' project on this tracker — register one with \
                     `cadence issue project add cadence --prefix <P>` or pass --project",
                )
            });
    }
    // `idea` — the cwd's project; resolve()'s error already names
    // --project and lists the known keys.
    project::resolve(&pm.dir, None, cwd)
}

/// Who filed it: the cadence alias inside a pane, else the OS user.
fn actor_of() -> String {
    std::env::var("CADENCE_ALIAS")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("USER").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "operator".to_string())
}

/// The automatic context block — everything an issue needs to locate
/// the reporter's world without another round-trip. Daemon build comes
/// from `daemon_info`; unreachable records that fact, never fails.
fn context_block(state_dir: &Path, cwd: &Path) -> String {
    let mut lines = vec![format!("- actor: {}", scrub(&actor_of()))];
    lines.push(format!("- cwd: {}", scrub(&cwd.to_string_lossy())));
    if let Some((root, remote)) = project::repo_identity(cwd) {
        let branch = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let mut repo = format!("- repo: {}", scrub(&root.to_string_lossy()));
        if !branch.is_empty() {
            repo.push_str(&format!(" (branch {})", scrub(&branch)));
        }
        if let Some(r) = remote {
            repo.push_str(&format!(" remote {}", scrub(&r)));
        }
        lines.push(repo);
    }
    lines.push(format!("- cadence: {}", env!("CARGO_PKG_VERSION")));
    let daemon = client::rpc(state_dir, "daemon_info", json!({}))
        .ok()
        .and_then(|i| {
            i["build_commit"]
                .as_str()
                .map(str::to_string)
                .or_else(|| Some("unknown".to_string()))
        })
        .unwrap_or_else(|| "unreachable".to_string());
    lines.push(format!("- daemon: {}", scrub(&daemon)));
    format!("## Report context\n\n{}\n", lines.join("\n"))
}

/// Resolve the PM inbox to notify, best-effort: the reporter's
/// `upstream` when reporting from inside a pane, else the project's
/// `team.yaml` `roles.pm.alias` (ADR 0001), else none — a missing PM
/// never blocks the report.
fn pm_inbox(pm: &Pm, project: &project::Project, state_dir: &Path) -> Option<String> {
    if let Ok(alias) = std::env::var("CADENCE_ALIAS") {
        if let Ok(show) = client::rpc(state_dir, "agent_show", json!({"alias": alias})) {
            if let Some(up) = show["agent"]["params"]["upstream"].as_str() {
                if !up.is_empty() {
                    return Some(up.to_string());
                }
            }
        }
    }
    let team = pm.dir.join(&project.key).join("team.yaml");
    if let Ok(text) = std::fs::read_to_string(&team) {
        let y: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
        return y["roles"]["pm"]["alias"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
    None
}

/// Best-effort one-line heads-up to the PM inbox. The return value is
/// what the caller records — failures are data, never report errors.
fn notify_pm(state_dir: &Path, inbox: Option<&str>, line: &str, reply_to: Option<&str>) -> Value {
    let Some(alias) = inbox else {
        return json!(null);
    };
    match client::rpc(
        state_dir,
        "agent_send",
        json!({"alias": alias, "text": line, "reply_to": reply_to}),
    ) {
        Ok(_) => json!({"to": alias, "sent": true}),
        Err(e) => json!({"to": alias, "sent": false, "error": e.to_string()}),
    }
}

/// `cadence report` — file one. `body` is the already-read report
/// text (`-m`, `--file` or stdin); the caller owns stdin/TTY policy.
#[allow(clippy::too_many_arguments)]
pub fn file(
    pm: &Pm,
    kind: Kind,
    project_flag: Option<&str>,
    issue_id: Option<&str>,
    priority: Option<&str>,
    body: &str,
    actor: &str,
    state_dir: &Path,
    cwd: &Path,
) -> Result<Value> {
    let body = scrub(body);
    if body.trim().is_empty() {
        return Err(Error::rejected("Report body is empty — pass -m or --file"));
    }
    let context = context_block(state_dir, cwd);
    let reporter = std::env::var("CADENCE_ALIAS")
        .ok()
        .filter(|s| !s.is_empty());

    // --issue: the report lands as a comment on an existing issue —
    // no new issue, no routing decision; the issue's own project holds
    // it. The PM heads-up still goes out.
    if let Some(id) = issue_id {
        let text = format!("{body}\n\n{context}");
        let out = write::add_comment(pm, id, &text, None, Some(kind.as_str()), None, actor)?;
        let (proj, _) = write::issue_dir(pm, id)?;
        let inbox = pm_inbox(pm, &proj, state_dir);
        let notified = notify_pm(
            state_dir,
            inbox.as_deref(),
            &format!("{id}: {} comment — {}", kind.as_str(), first_line(&body)),
            reporter.as_deref(),
        );
        let mut out = out;
        out["kind"] = json!(kind.as_str());
        out["project"] = json!(proj.key);
        out["notified"] = notified;
        return Ok(out);
    }

    let project = target_project(pm, kind, project_flag, cwd)?;
    let priority = priority.unwrap_or_else(|| kind.default_priority());
    model::check_priority(priority)?;
    let tags = write::check_tags(&project, &["intake".to_string(), kind.as_str().to_string()])?;

    let title = first_line(&body);
    if title.is_empty() {
        return Err(Error::rejected("Report needs a first line as its title"));
    }
    let rest = body.split_once('\n').map(|x| x.1).unwrap_or("").trim();
    let issue_body = if rest.is_empty() {
        format!("{title}\n\n{context}")
    } else {
        format!("{title}\n\n{rest}\n\n{context}")
    };

    let _lock = pm.lock()?;
    let id = format!(
        "{}-{}",
        project.prefix,
        write::next_id(&pm.dir.join(&project.key), &project.prefix)?
    );
    let dir = pm.dir.join(&project.key).join(&id);
    if dir.exists() {
        return Err(Error::rejected(format!(
            "Issue '{id}' already exists at {}",
            dir.display()
        )));
    }
    let mut front = model::Front::new(&id, title, &time::iso(time::now_epoch()));
    front.priority = priority.to_string();
    front.tags = tags;
    std::fs::create_dir_all(dir.join("comments"))?;
    std::fs::create_dir_all(dir.join("artifacts"))?;
    write::save_front(&dir, &front, &issue_body)?;
    let issues = board::load_all(&pm.dir, None)?;
    if let Err(e) = write::check_structure(&issues, &id) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }
    write::commit(
        pm,
        &format!("{id}: report {}", kind.as_str()),
        &[&id],
        actor,
    )?;

    let inbox = pm_inbox(pm, &project, state_dir);
    let notified = notify_pm(
        state_dir,
        inbox.as_deref(),
        &format!("{id}: new {} — {}", kind.as_str(), title),
        reporter.as_deref(),
    );
    Ok(json!({
        "id": id, "project": project.key, "kind": kind.as_str(),
        "priority": priority, "status": "backlog",
        "path": dir, "committed": true, "notified": notified,
    }))
}

fn first_line(body: &str) -> &str {
    body.lines().next().unwrap_or("").trim()
}

/// `cadence report ls [--kind K] [--project P]` — open intake: issues
/// tagged `intake` that are not done/dropped, newest first.
pub fn ls(pm: &Pm, kind: Option<Kind>, project: Option<&str>) -> Result<Value> {
    let issues = board::load_all(&pm.dir, None)?;
    let mut rows: Vec<Value> = issues
        .iter()
        .filter(|i| i.front.tags.iter().any(|t| t == "intake"))
        .filter(|i| !matches!(i.front.status.as_str(), "done" | "dropped"))
        .filter(|i| {
            kind.map(|k| i.front.tags.iter().any(|t| t == k.as_str()))
                .unwrap_or(true)
        })
        .filter(|i| project.map(|p| i.project == p).unwrap_or(true))
        .map(|i| {
            let kind_tag = i
                .front
                .tags
                .iter()
                .find(|t| *t != "intake")
                .cloned()
                .unwrap_or_default();
            json!({
                "id": i.front.id, "project": i.project, "kind": kind_tag,
                "status": i.front.status, "priority": i.front.priority,
                "title": i.front.title, "created": i.front.created,
                "owner": i.front.owner,
            })
        })
        .collect();
    rows.sort_by(|a, b| b["id"].as_str().cmp(&a["id"].as_str()));
    Ok(json!({"reports": rows, "count": rows.len()}))
}

/// `cadence report show <ID>` — one intake issue, body included.
pub fn show(pm: &Pm, id: &str) -> Result<Value> {
    let issue = board::find_issue(&pm.dir, id)?;
    let body = issue.body;
    Ok(json!({
        "id": issue.front.id, "project": issue.project,
        "status": issue.front.status, "priority": issue.front.priority,
        "title": issue.front.title, "tags": issue.front.tags,
        "owner": issue.front.owner, "created": issue.front.created,
        "body": body,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_redacts_token_and_flag_shapes() {
        let secret = format!("ghp_{}", "a".repeat(36));
        let out = scrub(&format!("leaked {secret} and --api-key={secret}"));
        assert!(!out.contains(&secret), "{out}");
        assert!(out.contains("[REDACTED]"), "{out}");
        assert_eq!(scrub("ordinary words only"), "ordinary words only");
    }

    #[test]
    fn kind_defaults_and_routing_class() {
        assert_eq!(Kind::Bug.default_priority(), "P2");
        assert_eq!(Kind::Idea.default_priority(), "P3");
        assert!(Kind::Bug.about_cadence() && Kind::Question.about_cadence());
        assert!(!Kind::Idea.about_cadence());
    }
}
