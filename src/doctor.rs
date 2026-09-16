//! Environment checks: state directory, storage, provider CLIs.
//! Reports capabilities honestly — an absent optional tool is a fact,
//! not a failure.

use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

use crate::error::Result;
use crate::store::Store;

fn command_version(program: &str, args: &[&str]) -> Value {
    match Command::new(program).args(args).output() {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            json!({"present": true, "version": text.trim()})
        }
        Ok(out) => json!({
            "present": true,
            "version": null,
            "note": format!("exited {}", out.status),
        }),
        Err(_) => json!({"present": false}),
    }
}

pub fn run(state_dir: &Path) -> Result<Value> {
    std::fs::create_dir_all(state_dir)?;
    let mut checks = json!({});
    checks["state_dir"] = json!({
        "path": state_dir,
        "writable": state_dir.is_dir(),
    });
    // Storage: open + migrate a probe database without touching live state.
    let probe = state_dir.join(".doctor-probe.sqlite3");
    let storage = match Store::open(&probe) {
        Ok(_) => json!({"ok": true}),
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    };
    let _ = std::fs::remove_file(&probe);
    let _ = std::fs::remove_file(probe.with_extension("sqlite3-wal"));
    let _ = std::fs::remove_file(probe.with_extension("sqlite3-shm"));
    checks["storage"] = storage;
    checks["codex"] = command_version("codex", &["--version"]);
    checks["devin"] = command_version("devin", &["--version"]);
    checks["tmux"] = command_version("tmux", &["-V"]);
    let codex_ok = checks["codex"]["present"].as_bool().unwrap_or(false);
    let devin_ok = checks["devin"]["present"].as_bool().unwrap_or(false);
    let tmux_ok = checks["tmux"]["present"].as_bool().unwrap_or(false);
    Ok(json!({
        "state_dir": state_dir,
        "checks": checks,
        "capabilities": {
            "managed_codex_stdio": codex_ok,
            "managed_codex_ws": codex_ok,
            "pty_devin_tmux": devin_ok && tmux_ok,
            "managed_devin_acp": false,
            "native_inbox_endpoint": false,
            "fake_provider_tests": true,
        },
        "notes": [
            "pty devin endpoint requires devin + tmux; submission is gated on an explicit operator ready claim",
            "PTY submission cannot establish provider receipt; only an explicit message ack/result report completes it",
            "Devin ACP, Claude and Cursor endpoints remain unimplemented",
            "fake endpoint_kind is a test fixture, not a provider",
        ],
    }))
}
