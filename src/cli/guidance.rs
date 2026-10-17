//! Native update guidance post-step (CAD-1472).
//!
//! Client-only update never reaches this module. `--check` and
//! `update status` only call [`readonly`]. A sandbox profile does not
//! write the shared home. Guidance failure is a report, not a binary
//! rollback and not a daemon restart.

use cadence_agent::skill::ActivatedGuidance;
use cadence_agent::update::RunReport;
use cadence_agent::upgrade::Layout;

use super::*;

pub(super) fn readonly(home: &Path) -> Value {
    let status = cadence_agent::skill::status(home);
    json!({
        "guidance": status["guidance"],
        "repair": status["repair"],
        "attention": status["attention"],
        "status": status,
        "wrote": false,
        "target_guidance": "not-verified",
        "note": "target guidance is not verified until that build is activated",
        "live_session": "not-checked",
        "live_session_note": status["live_session_note"],
        "daemon_restarted_for_skills": false,
    })
}

pub(super) fn after_update(
    state_dir: &Path,
    layout: &Layout,
    report: &RunReport,
    home: &Path,
) -> Value {
    let daemon = daemon_scope(state_dir, home);
    if cadence_agent::sandbox::profile().is_some() {
        return json!({
            "guidance": "not-checked",
            "wrote": false,
            "skipped": true,
            "reason": "sandbox profile never writes the shared home",
            "repair": "cadence skill install",
            "live_session": "not-checked",
            "daemon_restarted_for_skills": false,
            "daemon": daemon,
            "status": cadence_agent::skill::status(home),
        });
    }
    match cadence_agent::skill::activated_guidance(
        report.rolled_back,
        &report.health,
        &report.restart,
    ) {
        ActivatedGuidance::CurrentProcess => {
            let mut body = sync_already_current(layout, report, home, state_dir);
            body["daemon_restarted_for_skills"] = json!(false);
            body["daemon"] = daemon;
            body
        }
        ActivatedGuidance::Release(sha) => {
            let binary = layout.binary(&sha);
            let mut body = cadence_agent::skill::invoke_activated_sync(&binary, home, state_dir);
            body["source"] = json!("activated-binary");
            body["activated_sha"] = json!(sha);
            body["daemon_restarted_for_skills"] = json!(false);
            body["daemon"] = daemon;
            body
        }
        ActivatedGuidance::Unavailable => json!({
            "guidance": "not-checked",
            "wrote": false,
            "reason": "activated build was not identified; guidance was not written",
            "live_session": "not-checked",
            "daemon_restarted_for_skills": false,
            "daemon": daemon,
        }),
    }
}

/// An already-current update has not switched builds. This process may
/// write only when it is the active release file. A different release
/// file is invoked so that build's bytes are used. A missing release is
/// left unwritten and reported `not-checked`: a deleted binary can still
/// be the running daemon, and this process must not substitute its own.
fn sync_already_current(
    layout: &Layout,
    report: &RunReport,
    home: &Path,
    state_dir: &Path,
) -> Value {
    if !report.check.up_to_date {
        return unavailable_guidance(
            "activated build was not identified; guidance was not written",
        );
    }
    let binary = layout.binary(&report.check.target);
    if same_executable(&binary) {
        let mut body = reconcile_current(home);
        body["source"] = json!("current-process");
        return body;
    }
    if binary.is_file() {
        let mut body = cadence_agent::skill::invoke_activated_sync(&binary, home, state_dir);
        body["source"] = json!("activated-binary");
        body["activated_sha"] = json!(report.check.target);
        return body;
    }
    json!({
        "guidance": "not-checked",
        "wrote": false,
        "reason": format!(
            "activated binary {} is not a file; guidance was not written",
            binary.display()
        ),
        "repair": "cadence skill install",
        "live_session": "not-checked",
        "activated_sha": report.check.target,
    })
}

fn unavailable_guidance(reason: &str) -> Value {
    json!({
        "guidance": "not-checked",
        "wrote": false,
        "reason": reason,
        "repair": "cadence skill install",
        "live_session": "not-checked",
    })
}

fn same_executable(binary: &Path) -> bool {
    let Ok(current) = std::env::current_exe() else {
        return false;
    };
    let Ok(binary) = binary.canonicalize() else {
        return false;
    };
    current.canonicalize().ok().as_deref() == Some(binary.as_path())
}

fn reconcile_current(home: &Path) -> Value {
    match cadence_agent::skill::sync(home, false, None) {
        Ok(sync) => {
            let status = cadence_agent::skill::status(home);
            json!({
                "wrote": sync["wrote"],
                "sync": sync,
                "status": status,
                "guidance": status["guidance"],
                "repair": status["repair"],
                "attention": status["attention"],
                "live_session": "not-checked",
                "live_session_note": status["live_session_note"],
            })
        }
        Err(error) => json!({
            "guidance": "needs-attention",
            "wrote": false,
            "error": error.to_string(),
            "repair": "cadence skill install",
            "live_session": "not-checked",
            "status": cadence_agent::skill::status(home),
        }),
    }
}

fn report_axis<'a>(guidance: &'a Value, key: &str) -> Option<&'a str> {
    guidance.get(key).and_then(Value::as_str).or_else(|| {
        guidance
            .get("status")
            .and_then(|status| status.get(key))
            .and_then(Value::as_str)
    })
}

pub(super) fn lines(guidance: &Value) -> Vec<String> {
    let state = guidance["guidance"].as_str().unwrap_or("not-checked");
    let mut out = vec![format!("guidance: {state}")];
    if let Some(state) = report_axis(guidance, "file_match") {
        out.push(format!("guidance: file match {state}"));
    }
    if let Some(state) = report_axis(guidance, "provider_discovery") {
        out.push(format!("guidance: provider discovery {state}"));
    }
    if let Some(repair) = guidance["repair"].as_str() {
        out.push(format!("guidance: repair with `{repair}`"));
    }
    if let Some(note) = guidance["attention"].as_str() {
        out.push(format!("guidance: {note}"));
    }
    if let Some(note) = guidance["note"].as_str() {
        out.push(format!("guidance: {note}"));
    }
    if let Some(reason) = guidance["reason"].as_str() {
        out.push(format!("guidance: {reason}"));
    }
    let session = guidance["live_session_note"]
        .as_str()
        .or_else(|| guidance["status"]["live_session_note"].as_str());
    if let Some(session) = session {
        out.push(format!("guidance: {session}"));
    }
    if let Some(daemon) = guidance.get("daemon").filter(|value| value.is_object()) {
        if let Some(reason) = daemon.get("reason").and_then(Value::as_str) {
            out.push(format!("guidance: {reason}"));
        }
        if daemon.get("build_differs_from_cli") == Some(&Value::Bool(true)) {
            out.push("guidance: daemon build differs from this command".to_string());
        }
        if let Some(home) = daemon.get("home").and_then(Value::as_str) {
            if home != "same-as-cli" {
                out.push(format!("guidance: daemon home {home}"));
            }
        }
    }
    out
}

fn daemon_scope(state_dir: &Path, cli_home: &Path) -> Value {
    let info = match cadence_agent::client::rpc_timeout(
        state_dir,
        "daemon_info",
        json!({}),
        Duration::from_secs(2),
    ) {
        Ok(info) => info,
        Err(_) => {
            return json!({
                "guidance": "not-checked",
                "reason": "no daemon answered for this state dir",
            });
        }
    };
    let build = info.get("build_commit").cloned().unwrap_or(Value::Null);
    let mut out = json!({
        "build": build,
        "cli_build": cadence_agent::overview::BUILD_COMMIT,
        "guidance": "not-checked",
        "build_differs_from_cli": build.as_str() != Some(cadence_agent::overview::BUILD_COMMIT),
    });
    let Some(pid) = info["pid"].as_u64() else {
        out["reason"] = json!("daemon pid was not reported; its home was not modified");
        return out;
    };
    match same_scope_home(pid) {
        ScopeHome::Same(path) if same_path(&path, cli_home) => {
            out["home"] = json!("same-as-cli");
            out["note"] =
                json!("same account and home as this command. Live sessions are not reloaded.");
        }
        ScopeHome::Same(path) => {
            out["home"] = json!(path);
            out["reason"] = json!("daemon home differs from this command and was not modified");
        }
        ScopeHome::OtherAccount => {
            out["reason"] = json!("daemon account differs; its home was not read or modified");
        }
        ScopeHome::Unreadable => {
            out["reason"] = json!("daemon home was not observable and was not modified");
        }
    }
    out
}

enum ScopeHome {
    // Only Linux can read another process's account and home. On other
    // targets `same_scope_home` always returns `Unreadable`, so these
    // variants exist for the shared match and are never constructed.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Same(PathBuf),
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    OtherAccount,
    Unreadable,
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
    left == right
}

#[cfg(target_os = "linux")]
fn same_scope_home(pid: u64) -> ScopeHome {
    let status = match std::fs::read_to_string(format!("/proc/{pid}/status")) {
        Ok(status) => status,
        Err(_) => return ScopeHome::Unreadable,
    };
    let uid = status
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .and_then(|line| line.split_whitespace().nth(1));
    let Some(uid) = uid else {
        return ScopeHome::Unreadable;
    };
    if uid != unsafe { libc::getuid() }.to_string() {
        return ScopeHome::OtherAccount;
    }
    let env = match std::fs::read(format!("/proc/{pid}/environ")) {
        Ok(env) => env,
        Err(_) => return ScopeHome::Unreadable,
    };
    for entry in env.split(|byte| *byte == 0) {
        if let Some(rest) = entry.strip_prefix(b"HOME=") {
            if let Ok(home) = String::from_utf8(rest.to_vec()) {
                return ScopeHome::Same(PathBuf::from(home));
            }
        }
    }
    ScopeHome::Unreadable
}

#[cfg(not(target_os = "linux"))]
fn same_scope_home(_pid: u64) -> ScopeHome {
    ScopeHome::Unreadable
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadence_agent::update::{CheckReport, RunReport};
    use std::os::unix::fs::PermissionsExt;

    fn quiet_check(target: &str, up_to_date: bool) -> CheckReport {
        CheckReport {
            current: Some(target.to_string()),
            current_version: None,
            target: target.to_string(),
            target_version: None,
            up_to_date,
            changes: Vec::new(),
            schema_current: None,
            schema_target: None,
            migration: false,
            lease: None,
            waiters: Vec::new(),
            blockers: Vec::new(),
            refusals: Vec::new(),
            older: false,
        }
    }

    fn report(
        target: &str,
        up_to_date: bool,
        rolled_back: bool,
        health: Value,
        restart: Value,
    ) -> RunReport {
        RunReport {
            check: quiet_check(target, up_to_date),
            lease: None,
            backup: None,
            install: None,
            drain: json!({"waited": false}),
            restart,
            health,
            rolled_back,
            pruned: Vec::new(),
            lines: Vec::new(),
        }
    }

    fn layout(root: &Path) -> Layout {
        Layout {
            releases: root.join("releases"),
            link: root.join("bin/cadence"),
        }
    }

    fn write_stub(path: &Path, marker: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{marker}' > \"$HOME/who-ran\"\nprintf '%s\\n' '{{\"wrote\":true,\"guidance\":\"current\",\"live_session\":\"not-checked\"}}'\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn already_current_repairs_guidance_once_and_does_not_restart_the_daemon() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&state).unwrap();
        cadence_agent::skill::sync(&home, false, None).unwrap();
        let reference = home.join(".agents/skills/company-outputs/references/workflow.md");
        let original = std::fs::read(&reference).unwrap();
        std::fs::write(&reference, "stale reference\n").unwrap();
        let missing = home.join(".agents/skills/company-outputs/references/quotation.md");
        std::fs::remove_file(&missing).unwrap();
        let link = home.join(".claude/skills/cadence");
        std::fs::remove_file(&link).unwrap();
        let installed = home.join(".agents/skills/cadence/SKILL.md");
        assert_eq!(
            std::fs::read(&installed).unwrap(),
            cadence_agent::skill::SKILL_MD.as_bytes(),
            "a fresh reader of the installed skill sees the embedded bytes"
        );

        let target = "dddddddddddddddddddddddddddddddddddddddd";
        let places = layout(root.path());
        let binary = places.binary(target);
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        // This process is the active release, so its embedded bytes are
        // the right ones. A symlink keeps that identity check honest.
        std::os::unix::fs::symlink(std::env::current_exe().unwrap(), &binary).unwrap();
        let report = report(target, true, false, Value::Null, Value::Null);
        let first = after_update(&state, &places, &report, &home);
        assert_eq!(first["source"], "current-process");
        assert_eq!(first["wrote"], true);
        assert_eq!(first["daemon_restarted_for_skills"], false);
        assert_eq!(first["live_session"], "not-checked");
        assert_eq!(std::fs::read(&reference).unwrap(), original);
        assert!(missing.is_file());
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(first["status"]["file_match"], "current");
        assert_eq!(first["status"]["provider_discovery"], "current");

        let unchanged = std::fs::metadata(&reference).unwrap().modified().unwrap();
        let second = after_update(&state, &places, &report, &home);
        assert_eq!(second["wrote"], false);
        assert_eq!(second["daemon_restarted_for_skills"], false);
        assert_eq!(
            std::fs::metadata(&reference).unwrap().modified().unwrap(),
            unchanged
        );
    }

    #[test]
    fn missing_active_release_does_not_write_this_process_bytes() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&state).unwrap();
        cadence_agent::skill::sync(&home, false, None).unwrap();
        let reference = home.join(".agents/skills/company-outputs/references/workflow.md");
        std::fs::write(&reference, "stale reference\n").unwrap();
        let target = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let report = report(target, true, false, Value::Null, Value::Null);
        let body = after_update(&state, &layout(root.path()), &report, &home);
        assert_eq!(body["guidance"], "not-checked");
        assert_eq!(body["wrote"], false);
        assert_eq!(body["repair"], "cadence skill install");
        assert_eq!(body["activated_sha"], target);
        assert!(body.get("source").is_none());
        assert_eq!(std::fs::read(&reference).unwrap(), b"stale reference\n");
        assert!(lines(&body)
            .iter()
            .any(|line| line == "guidance: not-checked"));
        assert!(!lines(&body)
            .iter()
            .any(|line| line.contains("file match current")));
    }

    #[test]
    fn already_current_different_binary_uses_that_binary_not_this_process() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&state).unwrap();
        let target = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let places = layout(root.path());
        write_stub(&places.binary(target), "activated");
        let report = report(target, true, false, Value::Null, Value::Null);
        let body = after_update(&state, &places, &report, &home);
        assert_eq!(body["source"], "activated-binary");
        assert_eq!(body["daemon_restarted_for_skills"], false);
        assert_eq!(
            std::fs::read_to_string(home.join("who-ran")).unwrap(),
            "activated\n"
        );
        assert!(
            !home.join(".agents/skills/cadence/SKILL.md").exists(),
            "the updater process must not write its own embedded guidance"
        );
    }

    #[test]
    fn rollback_syncs_the_restored_build_not_the_failed_candidate() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&state).unwrap();
        let places = layout(root.path());
        let failed = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let restored = "cccccccccccccccccccccccccccccccccccccccc";
        write_stub(&places.binary(failed), "failed");
        write_stub(&places.binary(restored), "restored");

        let automatic = report(
            failed,
            false,
            true,
            json!({"ok": false, "build": failed, "rolled_back_to": restored}),
            Value::Null,
        );
        let body = after_update(&state, &places, &automatic, &home);
        assert_eq!(body["activated_sha"], restored);
        assert_eq!(body["daemon_restarted_for_skills"], false);
        assert_eq!(
            std::fs::read_to_string(home.join("who-ran")).unwrap(),
            "restored\n"
        );
        assert!(!home.join(".agents/skills/cadence/SKILL.md").exists());

        std::fs::remove_file(home.join("who-ran")).unwrap();
        let explicit = report(
            failed,
            false,
            true,
            json!({"ok": true, "build": restored}),
            json!({"sha": restored}),
        );
        let body = after_update(&state, &places, &explicit, &home);
        assert_eq!(body["activated_sha"], restored);
        assert_eq!(
            std::fs::read_to_string(home.join("who-ran")).unwrap(),
            "restored\n"
        );
    }

    #[test]
    fn readonly_check_and_status_do_not_write_or_verify_the_target() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        cadence_agent::skill::sync(&home, false, None).unwrap();
        let reference = home.join(".agents/skills/company-outputs/references/workflow.md");
        let changed = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        let file = std::fs::File::options()
            .write(true)
            .open(&reference)
            .unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(changed)
                .set_accessed(changed),
        )
        .unwrap();
        drop(file);

        let report = readonly(&home);
        assert_eq!(report["wrote"], false);
        assert_eq!(report["target_guidance"], "not-verified");
        assert_eq!(report["live_session"], "not-checked");
        assert_eq!(report["guidance"], "current");
        assert_eq!(report["status"]["file_match"], "current");
        assert_eq!(report["status"]["provider_discovery"], "current");
        assert_eq!(
            std::fs::metadata(&reference).unwrap().modified().unwrap(),
            changed
        );
        let rendered = lines(&report);
        assert!(rendered
            .iter()
            .any(|line| line == "guidance: file match current"));
        assert!(rendered
            .iter()
            .any(|line| line == "guidance: provider discovery current"));
        assert!(rendered.iter().any(|line| line.contains("not verified")));

        std::fs::write(&reference, "leave this stale\n").unwrap();
        let stale = readonly(&home);
        assert_eq!(stale["wrote"], false);
        assert_eq!(stale["target_guidance"], "not-verified");
        assert_eq!(stale["status"]["file_match"], "needs-attention");
        assert_eq!(stale["status"]["provider_discovery"], "current");
        assert_eq!(std::fs::read(&reference).unwrap(), b"leave this stale\n");
    }

    #[test]
    fn default_text_names_a_daemon_scope_that_differs_or_was_not_observed() {
        let mut guidance = json!({
            "guidance": "current",
            "file_match": "current",
            "provider_discovery": "current",
            "daemon": {
                "guidance": "not-checked",
                "reason": "daemon home differs from this command and was not modified",
                "home": "/tmp/other-home",
                "build_differs_from_cli": true
            }
        });
        let rendered = lines(&guidance);
        assert!(rendered.iter().any(|line| line == "guidance: current"));
        assert!(rendered.iter().any(|line| {
            line == "guidance: daemon home differs from this command and was not modified"
        }));
        assert!(rendered
            .iter()
            .any(|line| line == "guidance: daemon build differs from this command"));
        assert!(rendered
            .iter()
            .any(|line| line == "guidance: daemon home /tmp/other-home"));

        guidance["daemon"] = json!({
            "guidance": "not-checked",
            "home": "same-as-cli",
            "build_differs_from_cli": false
        });
        let same = lines(&guidance);
        assert!(
            !same.iter().any(|line| line.contains("daemon")),
            "a matching daemon scope is not a second guidance claim: {same:?}"
        );
    }
}
