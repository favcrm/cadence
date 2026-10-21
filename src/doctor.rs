//! Environment checks: state directory, storage, provider CLIs.
//! Reports capabilities honestly — an absent optional tool is a fact,
//! not a failure.

pub mod host;

/// The daemon's provider-command resolution record (CAD-1247): what
/// the daemon computed for each provider command under its OWN PATH
/// at start, so `doctor --host` — usually run from a shell whose PATH
/// differs from the daemon's — reports the daemon's verdict, not the
/// doctor process's. `<state_dir>/provider-commands.json`.
pub const PROVIDER_COMMANDS_FILE: &str = "provider-commands.json";

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

/// Where the control plane and its lease come from, and which endpoint
/// kinds this build supports. Parse only: never acquire a lease, open the
/// live store or contact the board.
fn containers(pm_dir: Option<&Path>) -> Value {
    let endpoint_kinds = registry::endpoint_kind_ids();
    let Some(pm_dir) = pm_dir else {
        return json!({
            "control_plane": "unknown",
            "lease": "unknown",
            "hosted_config": false,
            "endpoint_kinds": endpoint_kinds,
            "warnings": ["no tracker: cannot classify the control plane"],
        });
    };
    let hosted = match crate::lease::read_hosted(pm_dir) {
        Ok(hosted) => hosted,
        Err(error) => {
            // `read_hosted` errors can contain the absolute pm.yaml path and
            // raw YAML values (serde echoes an invalid value). Report the
            // category only: doctor output is not a place to disclose either.
            let warning = if error.to_string().contains("[hosted]") {
                "pm.yaml [hosted] is malformed; values were not reported"
            } else {
                "pm.yaml is unreadable or invalid; values were not reported"
            };
            return json!({
                "control_plane": "unknown",
                "lease": "unknown",
                "hosted_config": false,
                "endpoint_kinds": endpoint_kinds,
                "warnings": [warning],
            });
        }
    };
    let mut control_plane = "local";
    let mut lease = "none";
    let mut warnings = Vec::new();
    if let Some(hosted) = &hosted {
        if let Some(spec) = hosted.lease.as_deref() {
            // Match `lease::Spec::parse`: trim first; only the exact
            // host-bound endpoint is hosted; `https` and other HTTP origins
            // are refused by `acquire`, so they must not read as hosted here.
            let raw = spec.trim();
            match raw {
                "" | "none" | "off" => {}
                "http://lease.internal"
                | "http://lease.internal/"
                | "http://lease.internal/renew" => {
                    control_plane = "hosted";
                    lease = "hosted";
                }
                _ => {
                    if let Some(path) = raw.strip_prefix("file:") {
                        if path.trim().is_empty() {
                            control_plane = "unknown";
                            lease = "unknown";
                            warnings.push("hosted.lease 'file:' needs a path".to_owned());
                        } else {
                            lease = "file";
                            warnings.push(
                                "hosted.lease is a file test double, not a hosted lease".to_owned(),
                            );
                        }
                    } else {
                        control_plane = "unknown";
                        lease = "unknown";
                        warnings.push(
                            "hosted.lease is not file:<path>, http://lease.internal or off"
                                .to_owned(),
                        );
                    }
                }
            }
        }
    }
    json!({
        "control_plane": control_plane,
        "lease": lease,
        "hosted_config": hosted.is_some(),
        "endpoint_kinds": endpoint_kinds,
        "warnings": warnings,
    })
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
    let pm_dir = pm["path"].as_str().map(std::path::PathBuf::from);
    let pm_present = pm["present"].as_bool().unwrap_or(false);
    checks["pm"] = pm;
    checks["containers"] = containers(pm_dir.as_deref());
    // Each spec's doctor capabilities gate on its probe binaries all
    // being present; daemon/board-level extras stay hand-listed.
    let mut caps = json!({});
    for spec in registry::SPECS {
        let ok = spec
            .probe_bins
            .iter()
            .all(|(p, _)| checks[p]["present"].as_bool().unwrap_or(false));
        for cap in spec.doctor_caps {
            // Several endpoints share a capability (pi and omp both provide
            // `managed_pi_rpc`): availability is "any provider with a present
            // probe", never the last spec's answer.
            let available = caps[*cap].as_bool().unwrap_or(false);
            caps[*cap] = json!(available || ok);
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
            "containers reports the configured control plane and lease kind; it never acquires a lease",
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

    fn containers_with_pm(text: &str) -> Value {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pm.yaml"), text).unwrap();
        containers(Some(dir.path()))
    }

    #[test]
    fn containers_without_a_tracker_is_unknown() {
        let value = containers(None);
        assert_eq!(value["control_plane"], "unknown");
        assert_eq!(value["lease"], "unknown");
        assert_eq!(value["hosted_config"], false);
        let kinds: Vec<&str> = value["endpoint_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kind| kind.as_str().unwrap())
            .collect();
        assert!(kinds.windows(2).all(|pair| pair[0] <= pair[1]), "{kinds:?}");
        for expected in ["pty", "managed", "cloud"] {
            assert!(
                kinds.contains(&expected),
                "{expected} missing from {kinds:?}"
            );
        }
    }

    #[test]
    fn containers_without_hosted_is_local_none() {
        let value = containers_with_pm("key: cadence\n");
        assert_eq!(value["control_plane"], "local");
        assert_eq!(value["lease"], "none");
        assert_eq!(value["hosted_config"], false);
        assert!(value["warnings"].as_array().unwrap().is_empty());
    }

    #[test]
    fn containers_hosted_http_lease_is_hosted() {
        for lease in [
            "http://lease.internal",
            "http://lease.internal/",
            "http://lease.internal/renew",
        ] {
            let value = containers_with_pm(&format!("hosted:\n  lease: {lease}\n"));
            assert_eq!(value["control_plane"], "hosted", "{lease}");
            assert_eq!(value["lease"], "hosted", "{lease}");
            assert_eq!(value["hosted_config"], true, "{lease}");
            assert!(value["warnings"].as_array().unwrap().is_empty(), "{lease}");
        }
    }

    #[test]
    fn containers_file_lease_is_local_with_a_warning() {
        let value = containers_with_pm("hosted:\n  lease: file:/tmp/lease.json\n");
        assert_eq!(value["control_plane"], "local");
        assert_eq!(value["lease"], "file");
        assert_eq!(value["hosted_config"], true);
        assert!(value["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("test double")));
    }

    #[test]
    fn containers_file_lease_without_a_path_is_unknown() {
        let value = containers_with_pm("hosted:\n  lease: 'file:'\n");
        assert_eq!(value["control_plane"], "unknown");
        assert_eq!(value["lease"], "unknown");
        assert!(value["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("needs a path")));
    }

    #[test]
    fn containers_https_or_foreign_http_lease_is_unknown() {
        for lease in [
            "https://lease.internal",
            "http://example.test/renew",
            "http://lease.internal:1234",
            "bogus",
        ] {
            let value = containers_with_pm(&format!("hosted:\n  lease: {lease}\n"));
            assert_eq!(value["control_plane"], "unknown", "{lease}");
            assert_eq!(value["lease"], "unknown", "{lease}");
            assert!(
                value["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|warning| warning.as_str().unwrap().contains("not file:<path>")),
                "{lease}: {value}"
            );
        }
    }

    #[test]
    fn containers_lease_off_is_local_none_with_hosted_config() {
        for text in [
            "hosted:\n  lease: off\n",
            "hosted:\n  lease: none\n",
            "hosted:\n  lease: ' none '\n",
            "hosted:\n  lease: ''\n",
            "hosted: {}\n",
        ] {
            let value = containers_with_pm(text);
            assert_eq!(value["control_plane"], "local", "{text}");
            assert_eq!(value["lease"], "none", "{text}");
            assert_eq!(value["hosted_config"], true, "{text}");
            assert!(value["warnings"].as_array().unwrap().is_empty(), "{text}");
        }
    }

    #[test]
    fn containers_malformed_hosted_is_unknown_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pm.yaml"), "hosted: nope\n").unwrap();
        let value = containers(Some(dir.path()));
        assert_eq!(value["control_plane"], "unknown");
        assert_eq!(value["lease"], "unknown");
        assert_eq!(value["hosted_config"], false);
        let warning = value["warnings"][0].as_str().unwrap();
        assert!(warning.contains("malformed"), "{warning}");
        assert!(
            !warning.contains(dir.path().to_str().unwrap()),
            "warning leaked the PM path: {warning}"
        );
    }

    #[test]
    fn containers_malformed_hosted_warning_redacts_values() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pm.yaml"),
            "hosted:\n  lease_ttl_secs: SUPERSECRET\n",
        )
        .unwrap();
        let value = containers(Some(dir.path()));
        assert_eq!(value["control_plane"], "unknown");
        assert_eq!(value["lease"], "unknown");
        let warning = value["warnings"][0].as_str().unwrap();
        assert!(
            !warning.contains("SUPERSECRET"),
            "warning leaked a value: {warning}"
        );
        assert!(
            !warning.contains(dir.path().to_str().unwrap()),
            "warning leaked the PM path: {warning}"
        );
        assert!(warning.contains("malformed"), "{warning}");
    }

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
        assert!(report["checks"]["containers"]["control_plane"].is_string());
        assert!(report["checks"]["containers"]["endpoint_kinds"].is_array());
        assert!(report["checks"]["containers"]["warnings"].is_array());
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
