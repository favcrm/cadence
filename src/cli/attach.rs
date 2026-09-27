//! CAD-535: `cadence attach` — moved verbatim from src/main.rs.

use super::{atty_stdin, group_root_of, print_json};
use cadence_agent::adapter::registry::{self, Attach};
use cadence_agent::client;
use cadence_agent::error::{Error, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) fn run(state_dir: PathBuf, name: Option<String>, print: bool) -> Result<i32> {
    attach_command(&state_dir, name, print)
}

/// Print or exec the native attach for an agent's live endpoint.
/// `pty` attaches this terminal to the cadence-owned tmux pane;
/// `managed-ws` shells out to `codex resume --remote`.
pub(crate) fn attach_agent(state_dir: &Path, alias: &str, run: bool) -> Result<i32> {
    let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))?;
    let agent = &show["agent"];
    let kind = agent["endpoint_kind"].as_str().unwrap_or_default();
    let provider = agent["provider"].as_str().unwrap_or_default();
    let attach = registry::spec_opt(provider, kind)
        .map(|s| s.attach)
        .unwrap_or(Attach::None);
    if attach == Attach::Headless {
        if provider == "devin" && kind == "cloud" {
            let endpoint = agent["endpoint"].as_str().unwrap_or("");
            print_json(&json!({
                "alias": alias,
                "endpoint_kind": kind,
                "endpoint": endpoint,
                "note": "devin cloud has no local terminal — open the session URL",
                "observe": format!("cadence events --follow {alias}"),
                "inspect": format!("cadence agent show {alias}"),
            }));
            return Ok(0);
        }
        // A managed Claude endpoint is a headless stream-json process —
        // there is no terminal surface to attach. The explanation is
        // printed, never exec'd.
        let thread = agent["thread_id"].as_str().unwrap_or_default();
        print_json(&json!({
            "alias": alias,
            "endpoint_kind": kind,
            "note": "managed claude is a headless stream-json process — \
                     nothing to attach",
            "observe": format!("cadence events --follow {alias}"),
            "inspect": format!("cadence agent show {alias}"),
            "manual": format!(
                "to drive the session by hand: `cadence agent stop {alias}` \
                 then `claude --resume {thread}` in its cwd"),
        }));
        return Ok(0);
    }
    if !matches!(attach, Attach::Tmux | Attach::ProviderTui(_)) {
        return Err(Error::rejected(format!(
            "Agent '{alias}' uses endpoint kind '{kind}' — nothing to \
             attach; `cadence send {alias} --text '…'` still reaches it"
        )));
    }
    let state = agent["state"].as_str().unwrap_or_default();
    if matches!(state, "stopped" | "offline") {
        return Err(Error::rejected(format!(
            "Agent '{alias}' is {state} — resume it first: \
             `cadence agent resume {alias}`"
        )));
    }
    let endpoint = agent["endpoint"].as_str().ok_or_else(|| {
        // An unreconciled `unknown` fences the agent — reconcile first,
        // resume second; anything else just needs the resume.
        if show["unknown"].as_i64().unwrap_or(0) > 0 {
            Error::rejected(format!(
                "Agent '{alias}' is fenced by an unreconciled unknown message — \
                 resume refused. {} {}",
                cadence_agent::daemon::unknown_inspect_lead(),
                cadence_agent::daemon::unknown_recovery_note()
            ))
        } else {
            Error::rejected(format!(
                "Agent '{alias}' has no live endpoint — resume it with \
                 `cadence agent resume {alias}`"
            ))
        }
    })?;
    let thread = agent["thread_id"]
        .as_str()
        .ok_or_else(|| Error::rejected("Agent has no native thread yet"))?;
    if attach == Attach::Tmux {
        // tmux://<socket>/<session> — attach is a view of
        // the owned pane, not a takeover of anything else.
        let (socket, session) = endpoint
            .strip_prefix("tmux://")
            .and_then(|rest| rest.split_once('/'))
            .ok_or_else(|| Error::internal("malformed tmux endpoint"))?;
        if run {
            let status = cadence_agent::reaper::status(Command::new("tmux").args([
                "-L",
                socket,
                "attach-session",
                "-t",
                session,
            ]))?;
            return Ok(status.code().unwrap_or(1));
        }
        print_json(&json!({
            "alias": alias,
            "endpoint": endpoint,
            "thread_id": thread,
            "command": format!("tmux -L {socket} attach-session -t {session}"),
            "note": "Attach shows the live pane; terminal echo is not \
                     agent receipt — message state remains authoritative.",
        }));
        return Ok(0);
    }
    let Attach::ProviderTui(program) = attach else {
        return Err(Error::internal("unreachable: attach arm narrowed above"));
    };
    if run {
        let status = cadence_agent::reaper::status(
            Command::new(program).args(["resume", "--remote", endpoint, thread]),
        )?;
        return Ok(status.code().unwrap_or(1));
    }
    print_json(&json!({
        "alias": alias,
        "endpoint": endpoint,
        "thread_id": thread,
        "command": format!("{program} resume --remote {endpoint} {thread}"),
        "note": "Attach shows the native thread; terminal echo is not \
                 agent receipt — message state remains authoritative.",
    }));
    Ok(0)
}

/// Live agents with an attachable endpoint (pty or managed-ws).
pub(crate) fn attachable(state_dir: &Path) -> Result<Vec<Value>> {
    let list = client::rpc(state_dir, "agent_list", json!({}))?;
    let agents = list["agents"].as_array().cloned().unwrap_or_default();
    Ok(agents
        .into_iter()
        .filter(|a| {
            registry::attachable(
                a["provider"].as_str().unwrap_or_default(),
                a["endpoint_kind"].as_str().unwrap_or_default(),
            ) && a["endpoint"].is_string()
        })
        .collect())
}

pub(crate) fn print_attachable(agents: &[Value]) {
    // Group-aware order: each root row is followed by its members, so a
    // worker is always identifiable under its PM.
    let mut ordered: Vec<&Value> = agents.iter().collect();
    ordered.sort_by(|a, b| {
        let (ra, rb) = (group_root_of(a), group_root_of(b));
        let root_a = a["alias"].as_str() == Some(ra);
        let root_b = b["alias"].as_str() == Some(rb);
        ra.cmp(rb)
            .then(root_b.cmp(&root_a))
            .then(a["alias"].as_str().cmp(&b["alias"].as_str()))
    });
    print_json(&json!({
        "attachable": ordered
            .iter()
            .map(|a| json!({
                "alias": a["alias"],
                "provider": a["provider"],
                "group": group_root_of(a),
                "group_root": a["alias"].as_str() == Some(group_root_of(a)),
                "session": a["session_id"],
                "endpoint": a["endpoint"],
                "attach": format!("cadence attach {}", a["alias"].as_str().unwrap_or_default()),
            }))
            .collect::<Vec<_>>(),
    }));
}

/// `cadence attach [name]`: resolve an alias, a provider-native id, or
/// a provider name with exactly one live agent — never guessing — then
/// exec the same attach `agent attach --run` performs (`--print`
/// prints the command instead). With no name, list live attachable
/// agents; attach only when exactly one exists.
pub(crate) fn attach_command(state_dir: &Path, name: Option<String>, print: bool) -> Result<i32> {
    let alias = match name {
        Some(name) => {
            if let Ok(show) = client::rpc(state_dir, "agent_show", json!({"alias": name})) {
                show["agent"]["alias"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            } else {
                // Provider-name sugar: unambiguous only when exactly one
                // live agent of that provider exists.
                let live: Vec<String> = attachable(state_dir)?
                    .iter()
                    .filter(|a| a["provider"].as_str() == Some(name.as_str()))
                    .filter_map(|a| a["alias"].as_str().map(str::to_string))
                    .collect();
                match live.len() {
                    0 => {
                        return Err(Error::rejected(format!(
                            "Unknown agent '{name}' — no alias, native id, or \
                             single live provider match"
                        )))
                    }
                    1 => live.into_iter().next().unwrap(),
                    _ => {
                        return Err(Error::rejected(format!(
                            "'{name}' matches {} live agents: {} — name one \
                             explicitly",
                            live.len(),
                            live.join(", ")
                        )))
                    }
                }
            }
        }
        None => {
            let live = attachable(state_dir)?;
            if live.len() == 1 {
                live[0]["alias"].as_str().unwrap_or_default().to_string()
            } else {
                print_attachable(&live);
                return Ok(0);
            }
        }
    };
    // Exec only where this terminal can — same rule as launches and
    // `resume`: stdin a TTY and not inside tmux. Otherwise print the
    // attach command (`--print` forces that even on a usable TTY).
    let can_exec = !print && atty_stdin() && std::env::var_os("TMUX").is_none();
    attach_agent(state_dir, &alias, can_exec)
}
