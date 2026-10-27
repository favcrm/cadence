//! Shared hosted claim context and delivery. Event receipt alone never calls this module.
use super::Kickoff;
use crate::error::{Error, Result};
use crate::remote_cli::{self, RemoteAnswer, RemoteTarget};
use serde_json::{json, Map, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Destination for this kickoff's context and handoff.
/// Org, endpoint, and workspace stay the invocation's original selection.
/// The member pin is the claimant captured when the coordinator opened.
/// A missing or one-sided pin is unavailable: the current saved login is
/// never substituted.
pub fn claimant_target(target: &RemoteTarget, kickoff: &Kickoff) -> Result<RemoteTarget> {
    let Some(subject_id) = kickoff.subject_id.filter(|value| !value.is_empty()) else {
        return Err(Error::rejected(
            "hosted assignment has no original member; context was not read and the local agent was not messaged",
        ));
    };
    let Some(issuer) = kickoff.issuer.filter(|value| !value.is_empty()) else {
        return Err(Error::rejected(
            "hosted assignment has no original issuer; context was not read and the local agent was not messaged",
        ));
    };
    Ok(RemoteTarget {
        org: target.org.clone(),
        endpoint: target.endpoint.clone(),
        org_id: target.org_id.clone(),
        identity: Some(remote_cli::CliIdentityPin {
            subject_id: subject_id.to_string(),
            issuer: issuer.to_string(),
        }),
    })
}

/// Reload the saved login against the claimant pin before a handoff that
/// does not itself open a remote verb. Renewal of that same member is
/// allowed; a different member or issuer fails inside credential load,
/// before any request is given that login's authority.
fn reload_claimant(target: &RemoteTarget) -> Result<()> {
    let dir = crate::remote_auth::auth_dir(None).map_err(|_| {
        Error::rejected(
            "remote credential directory is unavailable; the local agent was not messaged",
        )
    })?;
    remote_cli::cli_material(&dir, target)?;
    Ok(())
}

pub fn deliver_kickoff(
    target: &RemoteTarget,
    local_state: Option<PathBuf>,
    kickoff: &Kickoff,
    wake_timeout: u64,
) -> Result<Value> {
    let state = match local_state {
        Some(state) => state,
        None => crate::client::state_dir()?,
    };
    deliver_kickoff_with(target, &state, kickoff, wake_timeout, &|params| {
        crate::client::rpc(&state, "agent_send", params)
    })
}

/// The daemon uses its existing non-steering send path, not an invented
/// operator connection. CLI delivery uses the normal agent_send RPC above.
pub fn deliver_kickoff_with(
    target: &RemoteTarget,
    state: &Path,
    kickoff: &Kickoff,
    wake_timeout: u64,
    send: &dyn Fn(Value) -> Result<Value>,
) -> Result<Value> {
    let target = claimant_target(target, kickoff)?;
    let (text, note) = if let Some(text) =
        crate::local_assignment::reporting::stored_kickoff_text(state, kickoff)?
    {
        reload_claimant(&target)?;
        (text, context_note_path(state, kickoff))
    } else {
        let context = require_context(&target, kickoff, wake_timeout)?;
        let note = write_note(state, kickoff, &context)?;
        let text = kickoff_line(kickoff, &note, &context)?;
        crate::local_assignment::reporting::store_kickoff_binding(
            state,
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
        .filter(|alias| crate::proto::identifier(alias, "alias").is_ok())
    {
        params["reply_to"] = json!(reply_to);
    }
    reload_claimant(&target)?;
    let mut sent = send(params)?;
    sent["note"] = json!(note.display().to_string());
    Ok(sent)
}

pub fn require_context(
    target: &RemoteTarget,
    kickoff: &Kickoff,
    wake_timeout: u64,
) -> Result<Value> {
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

pub fn write_note(dir: &Path, kickoff: &Kickoff, context: &Value) -> Result<PathBuf> {
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
        .pointer("/issue_title")
        .and_then(Value::as_str)
        .unwrap_or("hosted issue");
    let title: String = title
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(180)
        .collect();
    let line = format!(
        "Hosted issue {} ({title}). Read {} for this assignment's issue, comments, and wiki index. Use its org selector explicitly for hosted ticket/wiki actions, verify organization_id and audience, and do not change global org selection. Worktree: {}. A managed endpoint ends its final answer with SHA:<40-or-64-lowercase-hex>. A PTY explicit reporter uses cadence message result {} --token <turn_id> --text '<summary>' --sha <40-or-64-lowercase-hex>.",
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
    // One budget covers the issue, its comments, and the wiki index. A later
    // read cannot restart the wake clock after an earlier read used it up.
    let Some(deadline) = Instant::now().checked_add(Duration::from_secs(wake_timeout)) else {
        return json!({
            "org": target.org,
            "organization_id": kickoff.organization_id,
            "audience": target.endpoint,
            "assignment_id": kickoff.assignment_id,
            "claim_id": kickoff.claim_id,
            "generation": kickoff.generation,
            "context_error": "invalid wake timeout; the local agent was not messaged"
        });
    };
    let show = remote_cli::call_verb(
        target,
        "issue_show",
        show_args,
        deadline,
        wake_timeout,
        |_| {},
    );
    let history = remote_cli::call_verb(
        target,
        "issue_history",
        history_args,
        deadline,
        wake_timeout,
        |_| {},
    );
    let wiki = remote_cli::call_verb(
        target,
        "enumerate",
        wiki_args,
        deadline,
        wake_timeout,
        |_| {},
    );
    let (show_ok, issue) = outcome(show);
    let (history_ok, history) = outcome(history);
    let (wiki_ok, wiki) = outcome(wiki);
    // The hosted CLI answers `issue_show` with the issue nested under `issue`;
    // a top-level title is accepted too. Resolve the title once, here, so the
    // kickoff line never has to search the stored envelope.
    let issue_title = issue
        .pointer("/issue/title")
        .or_else(|| issue.pointer("/title"))
        .and_then(Value::as_str)
        .unwrap_or("hosted issue")
        .to_string();
    let mut context = json!({
        "org": target.org,
        "organization_id": kickoff.organization_id,
        "audience": target.endpoint,
        "assignment_id": kickoff.assignment_id,
        "claim_id": kickoff.claim_id,
        "generation": kickoff.generation,
        "issue": issue,
        "issue_title": issue_title,
        "comments": history,
        "wiki": wiki,
        "report": {
            "managed_endpoint": "end the final answer with SHA:<40-or-64-lowercase-hex>",
            "pty": {
                "command": "cadence message result",
                "message": kickoff.message_id,
                "token": "<turn_id>",
                "sha": "<40-or-64-lowercase-hex>"
            }
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

pub fn redact_secrets(text: String) -> String {
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
