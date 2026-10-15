//! CLI for hosted local-agent coordination (CAD-1460).
//!
//! These commands talk to
//! `/__platform/hosted-cadence/:organizationId/local-assignments`.
//! They do not forward `issue start` or `dispatch` into the container.
//! A claim is not execution until the configured local actor accepts the
//! issue note through the existing daemon send.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use cadence_agent::local_assignment::http::BoardHttp;
use cadence_agent::local_assignment::{Coordinator, Kickoff};
use cadence_agent::remote_cli::{self, RemoteAnswer, RemoteTarget};
use clap::Subcommand;
use serde_json::{json, Map, Value};

use super::{print_json, Error, Result};

#[derive(Subcommand)]
pub(crate) enum LocalAgentAction {
    /// Save this machine's local actor and claim queued work for you.
    Connect {
        /// Local daemon alias that will read the issue.
        #[arg(long)]
        alias: Option<String>,
        /// Local git checkout. Without it the alias is only saved.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Lane slug. Defaults to `work`.
        #[arg(long)]
        name: Option<String>,
    },
    /// Keep picking up later assignments until disconnected or interrupted.
    Watch,
    /// Stop local pickup without stopping actors or cancelling their claims.
    Disconnect,
    /// Show this machine's alias and the assignments the board returned.
    Status,
    /// Report a claim this machine already holds.
    Report {
        assignment: String,
        /// `running`, `blocked`, `failed`, or `result`.
        #[arg(long)]
        state: String,
        #[arg(long)]
        text: Option<String>,
        /// 40 or 64 lowercase hex characters. Required for result.
        #[arg(long)]
        head: Option<String>,
    },
    /// Move or cancel a live assignment.
    Reassign {
        assignment: String,
        /// Directory user id that should take the work.
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        cancel: bool,
    },
    /// Run the existing device grant once for one lane and store that credential.
    Tool {
        /// `provider.draft` or `publish.send`.
        #[arg(long)]
        lane: String,
    },
}

fn coordinator(target: &RemoteTarget) -> Result<Coordinator<BoardHttp>> {
    BoardHttp::open(target)?.into_coordinator()
}

pub(super) fn run(
    target: &RemoteTarget,
    action: &LocalAgentAction,
    wake_timeout: u64,
    local_state: Option<PathBuf>,
) -> Result<i32> {
    let mut coord = coordinator(target)?;
    let value = match action {
        LocalAgentAction::Connect { repo, name, alias } => {
            let mut deliver = |kickoff: &Kickoff| {
                deliver_kickoff(target, local_state.clone(), kickoff, wake_timeout)
            };
            let value = coord.connect(
                repo.as_deref(),
                name.as_deref(),
                alias.as_deref(),
                &mut deliver,
            )?;
            if let Some(dir) = local_state
                .clone()
                .or_else(|| cadence_agent::client::state_dir().ok())
            {
                ensure_report_driver(&dir);
            }
            value
        }
        LocalAgentAction::Watch => {
            let mut deliver = |kickoff: &Kickoff| {
                deliver_kickoff(target, local_state.clone(), kickoff, wake_timeout)
            };
            let report_state = local_state
                .clone()
                .or_else(|| cadence_agent::client::state_dir().ok());
            if let Some(dir) = &report_state {
                ensure_report_driver(dir);
            }
            // The coordinator stays open for the watch. Each later poll
            // reloads the saved login in place; it does not resolve another org.
            let mut previous = None;
            loop {
                let value = match coord.pickup(None, false, &mut deliver) {
                    Ok(value) => value,
                    Err(error) => {
                        if error.code() == Some("config_required") {
                            let status = coord.status()?;
                            if status["configured"] == false {
                                print_json(&status);
                                return Ok(if previous.is_some() { 0 } else { 5 });
                            }
                        }
                        return Err(error);
                    }
                };
                if value.get("guidance").is_some() {
                    print_json(&value);
                    return Ok(5);
                }
                if previous.as_ref() != Some(&value) {
                    print_json(&value);
                    previous = Some(value);
                }
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        }
        LocalAgentAction::Disconnect => coord.disconnect()?,
        LocalAgentAction::Status => coord.status()?,
        LocalAgentAction::Report {
            assignment,
            state,
            text,
            head,
        } => coord.report(assignment, state, text.as_deref(), head.as_deref())?,
        LocalAgentAction::Reassign {
            assignment,
            to,
            cancel,
        } => coord.reassign(assignment, to.as_deref(), *cancel)?,
        LocalAgentAction::Tool { lane } => coord.authorize_tool(lane)?,
    };
    finish(&value)
}

pub(super) fn dispatch(target: &RemoteTarget, issue: &str, to: &str) -> Result<i32> {
    finish(&coordinator(target)?.dispatch(issue, Some(to))?)
}

pub(super) fn start(
    target: &RemoteTarget,
    id: &str,
    repo: Option<&Path>,
    name: Option<&str>,
    wake_timeout: u64,
    local_state: Option<PathBuf>,
) -> Result<i32> {
    let Some(repo) = repo else {
        return Err(Error::usage(
            "hosted issue start requires --repo <local git checkout>; nothing was sent",
        ));
    };
    let mut deliver =
        |kickoff: &Kickoff| deliver_kickoff(target, local_state.clone(), kickoff, wake_timeout);
    let value = coordinator(target)?.start_here(id, repo, name, &mut deliver)?;
    if let Some(dir) = local_state.or_else(|| cadence_agent::client::state_dir().ok()) {
        ensure_report_driver(&dir);
    }
    finish(&value)
}

fn ensure_report_driver(state: &Path) {
    if cadence_agent::client::rpc_timeout(
        state,
        "health",
        json!({}),
        std::time::Duration::from_millis(200),
    )
    .is_ok()
    {
        return;
    }
    let _ = cadence_agent::client::daemon_start(state);
}

fn finish(value: &Value) -> Result<i32> {
    print_json(value);
    if value
        .get("refused")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty())
    {
        return Ok(5);
    }
    Ok(0)
}

fn deliver_kickoff(
    target: &RemoteTarget,
    local_state: Option<PathBuf>,
    kickoff: &Kickoff,
    wake_timeout: u64,
) -> Result<Value> {
    let state = match local_state {
        Some(dir) => dir,
        None => cadence_agent::client::state_dir()?,
    };
    let (text, note) = if let Some(text) =
        cadence_agent::local_assignment::reporting::stored_kickoff_text(&state, kickoff)?
    {
        (text, context_note_path(&state, kickoff))
    } else {
        let context = require_context(target, kickoff, wake_timeout)?;
        let note = write_note(&state, kickoff, &context)?;
        let text = kickoff_line(kickoff, &note, &context)?;
        cadence_agent::local_assignment::reporting::store_kickoff_binding(
            &state,
            &target.org,
            &target.endpoint,
            kickoff,
            &text,
        )?;
        (text, note)
    };
    let mut params = json!({
        "alias": kickoff.alias,
        "text": text,
        "message": kickoff.message_id,
    });
    if let Some(reply_to) = std::env::var("CADENCE_ALIAS")
        .ok()
        .filter(|alias| cadence_agent::proto::identifier(alias, "alias").is_ok())
    {
        params["reply_to"] = json!(reply_to);
    }
    let mut sent = cadence_agent::client::rpc(&state, "agent_send", params)?;
    sent["note"] = json!(note.display().to_string());
    Ok(sent)
}

fn require_context(target: &RemoteTarget, kickoff: &Kickoff, wake_timeout: u64) -> Result<Value> {
    let context = issue_context(target, kickoff, wake_timeout);
    if context.get("context_error").is_some() {
        return Err(Error::rejected(
            "hosted issue context was not read; the local agent was not messaged and the assignment was not left running",
        ));
    }
    Ok(context)
}

// A reassignment must not rewrite a snapshot an older local actor can still read.
fn context_note_path(dir: &Path, kickoff: &Kickoff) -> PathBuf {
    dir.join("hosted-assignments")
        .join(format!("{}.md", kickoff.message_id))
}

fn write_note(dir: &Path, kickoff: &Kickoff, context: &Value) -> Result<PathBuf> {
    let message = kickoff.message_id;
    if message.is_empty()
        || message.contains('/')
        || message.contains('\\')
        || message.contains('\0')
    {
        return Err(Error::rejected(
            "message id cannot name a local note; the actor was not messaged",
        ));
    }
    let folder = dir.join("hosted-assignments");
    fs::create_dir_all(&folder)?;
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o700))?;
    let path = context_note_path(dir, kickoff);
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(Error::rejected(
            "assignment note path is a symlink; the actor was not messaged",
        ));
    }
    let serialized = serde_json::to_string_pretty(context).map_err(|error| {
        Error::rejected(format!("hosted context could not be serialized: {error}"))
    })?;
    let body = redact_secrets(serialized);
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
    }
    Ok(path)
}

fn kickoff_line(kickoff: &Kickoff, note: &Path, context: &Value) -> Result<String> {
    let title = context
        .pointer("/issue/title")
        .and_then(Value::as_str)
        .unwrap_or("hosted issue");
    let title: String = title
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(180)
        .collect();
    let line = format!(
        "Hosted issue {} ({title}). Read {} for this assignment's issue, comments, and wiki index. Use its org selector explicitly for hosted ticket/wiki actions, verify organization_id and audience, and do not change global org selection. Worktree: {}. Report with cadence message result {} --token <turn_id> --text '<summary>' --sha <40-or-64-lowercase-hex>.",
        kickoff.issue_id,
        note.display(),
        kickoff.worktree,
        kickoff.message_id
    );
    if line.chars().any(char::is_control) || line.len() > 4000 {
        return Err(Error::rejected(
            "local actor message was not a single safe line; nothing was sent",
        ));
    }
    Ok(line)
}

fn issue_context(target: &RemoteTarget, kickoff: &Kickoff, wake_timeout: u64) -> Value {
    let mut show_args = Map::new();
    show_args.insert("id".into(), json!(kickoff.issue_id));
    show_args.insert("json".into(), json!(true));
    let mut history_args = Map::new();
    history_args.insert("id".into(), json!(kickoff.issue_id));
    history_args.insert("limit".into(), json!(20));
    let mut wiki_args = Map::new();
    wiki_args.insert("path".into(), json!(""));
    let show = remote_cli::call_verb(target, "issue_show", show_args, wake_timeout, |_| {});
    let history =
        remote_cli::call_verb(target, "issue_history", history_args, wake_timeout, |_| {});
    let wiki = remote_cli::call_verb(target, "enumerate", wiki_args, wake_timeout, |_| {});
    let (show_ok, issue) = outcome(show);
    let (history_ok, history) = outcome(history);
    let (wiki_ok, wiki) = outcome(wiki);
    let mut context = json!({
        "org": target.org,
        "organization_id": kickoff.organization_id,
        "audience": target.endpoint,
        "assignment_id": kickoff.assignment_id,
        "claim_id": kickoff.claim_id,
        "generation": kickoff.generation,
        "issue": issue,
        "comments": history,
        "wiki": wiki,
        "report": {
            "command": "cadence message result",
            "message": kickoff.message_id,
            "token": "<turn_id>",
            "sha": "<40-or-64-lowercase-hex>"
        }
    });
    if !show_ok || !history_ok || !wiki_ok {
        context["context_error"] = json!(
            "issue, comments, or wiki context was not fully read; the local agent was not messaged"
        );
    }
    context
}

fn outcome(answer: Result<RemoteAnswer>) -> (bool, Value) {
    match answer {
        Ok(RemoteAnswer::Ok(value)) => (true, value),
        Ok(RemoteAnswer::Conflict(_)) => (
            false,
            json!({"context_error": "hosted context conflicted; the local agent was not messaged"}),
        ),
        Err(error) => (false, json!({"context_error": brief(&error)})),
    }
}

fn brief(error: &Error) -> String {
    let text: String = error
        .to_string()
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(240)
        .collect();
    redact_secrets(text)
}

fn redact_secrets(text: String) -> String {
    let mut out: Option<String> = None;
    let mut copied = 0;
    for (start, _) in text.match_indices("hct_") {
        if start < copied {
            continue;
        }
        let after = &text[start + 4..];
        let length = after
            .chars()
            .take(43)
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
            .count();
        if length != 43 {
            continue;
        }
        let output = out.get_or_insert_with(|| String::with_capacity(text.len()));
        output.push_str(&text[copied..start]);
        output.push_str("hct_[redacted]");
        copied = start + 47;
    }
    match out {
        Some(mut output) => {
            output.push_str(&text[copied..]);
            output
        }
        None => text,
    }
}

#[cfg(test)]
mod context_snapshots {
    use super::*;

    #[test]
    fn a_new_generation_cannot_overwrite_an_older_actor_context() {
        let dir = tempfile::tempdir().expect("state");
        let mut kickoff = Kickoff {
            alias: "worker",
            issue_id: "CAD1460",
            assignment_id: "hla_00000000000000000000000000000001",
            worktree: "/tmp/cad1460-work",
            message_id: "0123456789abcdef0123456789abcdef",
            claim_id: "hlc_00000000000000000000000000000001",
            generation: 1,
            organization_id: "ws_cad1460",
            audience: "https://board.test",
            subject_id: None,
            issuer: None,
        };
        let first = write_note(dir.path(), &kickoff, &json!({"issue": "original work"}))
            .expect("first context");
        let original = fs::read(&first).expect("original snapshot");
        kickoff.message_id = "abcdef0123456789abcdef0123456789";
        kickoff.claim_id = "hlc_00000000000000000000000000000002";
        kickoff.generation = 2;
        let second = write_note(dir.path(), &kickoff, &json!({"issue": "reassigned work"}))
            .expect("replacement context");
        assert_ne!(first, second);
        assert_eq!(fs::read(first).expect("older actor context"), original);
    }

    #[test]
    fn redaction_preserves_org_selectors_but_hides_complete_credentials() {
        let credential = format!("hct_{}", "a".repeat(43));
        let redacted = redact_secrets(format!(
            "org=hct_company\ncredential={credential}\nwiki=global/hct_notes.md"
        ));
        assert!(redacted.contains("org=hct_company\n"));
        assert!(redacted.contains("wiki=global/hct_notes.md"));
        assert!(!redacted.contains(&credential));
    }
}
