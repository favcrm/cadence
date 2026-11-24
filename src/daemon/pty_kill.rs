//! Daemon-only explicit termination of an agent's tmux session.

use std::path::Path;
use std::process::Command;

use crate::adapter::{pty, ProviderEnv};

pub(super) fn kill_pane(state_dir: &Path, alias: &str, env: &ProviderEnv, agent_uid: Option<u32>) {
    if let Some(uid) = agent_uid {
        pty::kill_pane_checked(state_dir, alias, env, uid);
        return;
    }
    // Legacy single-UID operation: the daemon owns this private socket.
    let tmux = env
        .var("CADENCE_TMUX_COMMAND")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "tmux".to_string());
    let socket = pty::tmux_socket(state_dir);
    let _ = crate::reaper::output(Command::new(tmux).arg("-L").arg(socket).args([
        "kill-session",
        "-t",
        alias,
    ]));
}

#[cfg(test)]
mod tests {
    #[test]
    fn legacy_same_uid_stop_kills_session() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("tmux-double");
        let marker = dir.path().join("killed");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ncase \"$*\" in\n  *kill-session*) touch '{}' ;;\nesac\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = crate::adapter::ProviderEnv::default();
        env.set("CADENCE_TMUX_COMMAND", script.to_string_lossy());
        super::kill_pane(dir.path(), "worker", &env, None);
        assert!(marker.exists(), "legacy same-uid stop changed");
    }
}
