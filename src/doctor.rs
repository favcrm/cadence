//! Environment checks: state directory, storage, provider CLIs.
//! Reports capabilities honestly — an absent optional tool is a fact,
//! not a failure.

pub mod host;

use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

use crate::adapter::registry;
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
    // Provider binaries are probed from the registry's `probe_bins`
    // entries — a new provider lands here with its spec, not by
    // hand-listing a command_version call.
    for (program, args) in registry::SPECS
        .iter()
        .flat_map(|s| s.probe_bins.iter())
        .fold(std::collections::BTreeMap::new(), |mut m, (p, a)| {
            m.entry(*p).or_insert(*a);
            m
        })
    {
        checks[program] = command_version(program, args);
    }
    // The PM board: does the tracker dir exist, is it a git repo, and
    // does `issue lint` pass. Absent is a fact, not a failure.
    let pm = match crate::issue::default_dir().and_then(|d| crate::issue::Pm::at(&d)) {
        Ok(pm) => {
            let projects = crate::issue::project::list(&pm.dir)
                .map(|p| p.len())
                .unwrap_or(0);
            let issues = crate::issue::board::load_all(&pm.dir, None)
                .map(|i| i.len())
                .unwrap_or(0);
            let lint = crate::issue::lint::run(&pm, None).unwrap_or_default();
            json!({
                "present": true,
                "path": pm.dir,
                "git": pm.dir.join(".git").is_dir(),
                "projects": projects,
                "issues": issues,
                "lint_ok": lint["ok"],
                "lint_warnings": lint["warnings"].as_array().map(|w| w.len()).unwrap_or(0),
            })
        }
        Err(_) => json!({
            "present": false,
            "hint": "create it with `cadence issue init`",
        }),
    };
    let pm_present = pm["present"].as_bool().unwrap_or(false);
    checks["pm"] = pm;
    // Each spec's doctor capabilities gate on its probe binaries all
    // being present; daemon/board-level extras stay hand-listed.
    let mut caps = json!({});
    for spec in registry::SPECS {
        let ok = spec
            .probe_bins
            .iter()
            .all(|(p, _)| checks[p]["present"].as_bool().unwrap_or(false));
        for cap in spec.doctor_caps {
            caps[*cap] = json!(ok);
        }
    }
    caps["managed_devin_acp"] = json!(false);
    caps["issue_folders"] = json!(pm_present);
    caps["ui_board"] = json!(pm_present);
    Ok(json!({
        "state_dir": state_dir,
        "checks": checks,
        "capabilities": caps,
        "notes": [
            "pty devin endpoint requires devin + tmux; submission is gated on an explicit operator ready claim",
            "PTY submission cannot establish provider receipt; only an explicit message ack/result report completes it",
            "managed claude runs headless stream-json; the turn's result text is the report — no message result needed",
            "Devin ACP and Cursor endpoints remain unimplemented",
            "fake endpoint_kind is a test fixture, not a provider",
            "cadence issue reads/writes ~/pm (CADENCE_PM_DIR) directly — no daemon needed",
            "cadence ui serves the read-only board on loopback; --features ui embeds the SPA",
        ],
    }))
}
