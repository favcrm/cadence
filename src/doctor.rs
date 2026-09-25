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
    match crate::reaper::output(Command::new(program).args(args)) {
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
    // Empty probe_bins would mark devin_cloud present. The capability is
    // credentials, not a binary: presence of both values, never the values.
    // A blank CADENCE_* override blocks the process DEVIN_* variable, matching
    // the adapter.
    caps["devin_cloud"] = json!(devin_cloud_present(
        credential_present("CADENCE_DEVIN_API_KEY", "DEVIN_API_KEY"),
        credential_present("CADENCE_DEVIN_ORG_ID", "DEVIN_ORG_ID"),
    ));
    caps["managed_devin_acp"] = json!(false);
    caps["issue_folders"] = json!(pm_present);
    caps["ui_board"] = json!(pm_present);
    // CAD-547: installed apps' connection slots — each `needs.connections`
    // slot's binding is checked against the connections the daemon
    // registers (plus the built-in `local`); an unreachable daemon
    // reports the check as unavailable, never as "unknown connection".
    if pm_present {
        let known = crate::client::rpc(state_dir, "daemon_info", json!({}))
            .ok()
            .and_then(|v| v["connections"].as_array().cloned())
            .map(|a| {
                let mut set: std::collections::HashSet<String> = a
                    .iter()
                    .filter_map(|c| c.as_str().map(str::to_string))
                    .collect();
                set.insert(crate::issue::app::LOCAL_CONNECTION.to_string());
                set
            });
        if let Ok(dir) = crate::issue::default_dir() {
            if let Ok(pm) = crate::issue::Pm::at(&dir) {
                checks["apps"] = crate::issue::app::doctor(&pm.dir, known.as_ref());
            }
        }
    }
    Ok(json!({
        "state_dir": state_dir,
        "checks": checks,
        "capabilities": caps,
        "notes": [
            "pty devin endpoint requires devin + tmux; submission is gated on an explicit operator ready claim",
            "PTY submission cannot establish provider receipt; only an explicit message ack/result report completes it",
            "managed claude runs headless stream-json; the turn's result text is the report — no message result needed",
            "Devin ACP and Cursor endpoints remain unimplemented",
            "devin cloud requires DEVIN_API_KEY and DEVIN_ORG_ID (CADENCE_DEVIN_API_KEY and CADENCE_DEVIN_ORG_ID override them); doctor reports presence only and never the values",
            "fake endpoint_kind is a test fixture, not a provider",
            "cadence issue reads/writes ~/pm (CADENCE_PM_DIR) directly — no daemon needed",
            "cadence ui serves the read-only board on loopback; --features ui embeds the SPA",
        ],
    }))
}

/// Both credentials must be present. Callers pass booleans only — this
/// never sees the secret values.
pub fn devin_cloud_present(has_key: bool, has_org: bool) -> bool {
    has_key && has_org
}

fn credential_present(cadence_name: &str, plain: &str) -> bool {
    match std::env::var(cadence_name) {
        Ok(value) => !value.trim().is_empty(),
        Err(_) => std::env::var(plain).is_ok_and(|value| !value.trim().is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devin_cloud_presence_requires_both_credentials() {
        assert!(!devin_cloud_present(false, false));
        assert!(!devin_cloud_present(true, false));
        assert!(!devin_cloud_present(false, true));
        assert!(devin_cloud_present(true, true));
    }

    #[test]
    fn doctor_reports_devin_cloud_without_credential_values() {
        let dir = tempfile::tempdir().unwrap();
        let report = run(dir.path()).unwrap();
        assert!(report["capabilities"]["devin_cloud"].is_boolean());
        let notes = report["notes"].to_string();
        assert!(notes.contains("never the values"), "{notes}");
        assert!(notes.contains("DEVIN_API_KEY"));
        for name in [
            "DEVIN_API_KEY",
            "CADENCE_DEVIN_API_KEY",
            "DEVIN_ORG_ID",
            "CADENCE_DEVIN_ORG_ID",
        ] {
            if let Ok(value) = std::env::var(name) {
                let value = value.trim();
                if value.len() >= 8 {
                    assert!(
                        !report.to_string().contains(value),
                        "doctor report included {name}"
                    );
                }
            }
        }
    }
}
