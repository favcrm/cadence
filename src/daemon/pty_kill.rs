//! Daemon-only explicit termination of an agent's tmux session.

use std::path::Path;
use std::process::Command;

use crate::adapter::{pty, ProviderEnv};

pub(super) fn kill_pane(state_dir: &Path, alias: &str, env: &ProviderEnv, agent_uid: Option<u32>) {
    kill_pane_with(
        state_dir,
        alias,
        env,
        agent_uid,
        pty::kill_pane_checked,
        |tmux, socket, target| {
            // Legacy single-UID operation: the daemon owns this private socket.
            let _ = crate::reaper::output(Command::new(tmux).arg("-L").arg(socket).args([
                "kill-session",
                "-t",
                target,
            ]));
        },
    );
}

fn kill_pane_with(
    state_dir: &Path,
    alias: &str,
    env: &ProviderEnv,
    agent_uid: Option<u32>,
    checked: impl FnOnce(&Path, &str, &ProviderEnv, u32),
    legacy_kill: impl FnOnce(&str, &str, &str),
) {
    if let Some(uid) = agent_uid {
        checked(state_dir, alias, env, uid);
        return;
    }
    let tmux = env
        .var("CADENCE_TMUX_COMMAND")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "tmux".to_string());
    let socket = pty::tmux_socket(state_dir);
    legacy_kill(&tmux, &socket, alias);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_same_uid_stop_kills_session() {
        let state_dir = Path::new("/tmp/cadence-pty-kill-state");
        let env = ProviderEnv::default();
        env.set("CADENCE_TMUX_COMMAND", "/custom/tmux");

        let mut legacy_call = None;
        kill_pane_with(
            state_dir,
            "worker",
            &env,
            None,
            |_, _, _, _| panic!("legacy same-uid stop must not invoke checked kill"),
            |tmux, socket, target| {
                legacy_call = Some((tmux.to_string(), socket.to_string(), target.to_string()));
            },
        );
        assert_eq!(
            legacy_call,
            Some((
                "/custom/tmux".to_string(),
                pty::tmux_socket(state_dir),
                "worker".to_string(),
            ))
        );

        let fallback_env = ProviderEnv::default();
        fallback_env.set("CADENCE_TMUX_COMMAND", "");
        let mut fallback_tmux = None;
        kill_pane_with(
            state_dir,
            "worker",
            &fallback_env,
            None,
            |_, _, _, _| panic!("legacy same-uid stop must not invoke checked kill"),
            |tmux, _, _| {
                fallback_tmux = Some(tmux.to_string());
            },
        );
        assert_eq!(fallback_tmux.as_deref(), Some("tmux"));
    }

    #[test]
    fn split_uid_stop_routes_to_checked_kill_only() {
        let state_dir = Path::new("/tmp/cadence-pty-kill-state");
        let env = ProviderEnv::default();

        let mut checked_call = None;
        kill_pane_with(
            state_dir,
            "worker",
            &env,
            Some(4242),
            |dir, alias, _, uid| {
                checked_call = Some((dir.to_path_buf(), alias.to_string(), uid));
            },
            |_, _, _| panic!("split-uid stop must not invoke legacy same-uid kill"),
        );
        assert_eq!(
            checked_call,
            Some((state_dir.to_path_buf(), "worker".to_string(), 4242))
        );
    }
}
