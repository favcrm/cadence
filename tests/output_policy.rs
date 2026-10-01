//! CAD-877: one stdout JSON policy — pretty on a TTY, compact when
//! piped, `CADENCE_JSON=pretty|compact` overrides; `issue ls` (and the
//! other table-by-default listers) emit JSON when piped.
#![allow(clippy::disallowed_methods)]
mod board_common;
use board_common::*;

use serde_json::Value;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn base(pm: &Path, state: &Path, args: &[&str], json: Option<&str>) -> Command {
    let mut cmd = Command::new(bin());
    cmd.arg("--state-dir")
        .arg(state)
        .args(args)
        .env("CADENCE_PM_DIR", pm)
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_JSON");
    if let Ok(home) = std::env::var("HOME") {
        cmd.env("HOME", home);
    }
    if let Some(v) = json {
        cmd.env("CADENCE_JSON", v);
    }
    cmd
}

/// Piped stdout (the agent case).
fn piped(pm: &Path, state: &Path, args: &[&str], json: Option<&str>) -> String {
    let out = base(pm, state, args, json).output().unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Stdout on a real pty (the human case).
fn on_tty(pm: &Path, state: &Path, args: &[&str]) -> String {
    let (mut m, mut s) = (0, 0);
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut m,
                &mut s,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        },
        0
    );
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
    let mut cmd = base(pm, state, args, None);
    cmd.stdout(Stdio::from(slave));
    let mut child = cmd.spawn().unwrap();
    drop(cmd); // closes our copy of the slave
    let mut text = Vec::new();
    let mut f = std::fs::File::from(master);
    let mut buf = [0u8; 4096];
    loop {
        match std::io::Read::read(&mut f, &mut buf) {
            Ok(0) | Err(_) => break, // EIO once the slave closes
            Ok(n) => text.extend_from_slice(&buf[..n]),
        }
    }
    assert!(child.wait().unwrap().success());
    String::from_utf8_lossy(&text).replace("\r\n", "\n")
}

fn fixture() -> (TempDir, TempDir) {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    (pm, state)
}

#[test]
fn piped_json_is_one_document_per_line() {
    let (pm, state) = fixture();
    for args in [
        &["issue", "show", "CAD-1", "--json"][..],
        &["issue", "log", "CAD-1"][..],
        &["issue", "ls", "--summary", "--json"][..],
        &["issue", "ls", "--json"][..],
    ] {
        let out = piped(pm.path(), state.path(), args, None);
        assert_eq!(out.trim_end().lines().count(), 1, "{args:?}: {out}");
        assert!(out.ends_with('\n'), "{args:?}");
        serde_json::from_str::<Value>(&out).unwrap();
    }
}

#[test]
fn pretty_override_and_tty_give_the_pretty_form_same_document() {
    let (pm, state) = fixture();
    let args = ["issue", "show", "CAD-1", "--json"];
    let compact = piped(pm.path(), state.path(), &args, None);
    let pretty = piped(pm.path(), state.path(), &args, Some("pretty"));
    assert!(pretty.lines().count() > 5, "{pretty}");
    assert_eq!(
        compact,
        piped(pm.path(), state.path(), &args, Some("compact"))
    );
    // Same document, same field names and order — only whitespace differs.
    let strip = |s: &str| s.split_whitespace().collect::<String>();
    assert_eq!(strip(&compact), strip(&pretty));
    // TTY is unchanged: pretty, with no override.
    let tty = on_tty(pm.path(), state.path(), &args);
    assert_eq!(tty, pretty, "TTY output must stay pretty");
}

#[test]
fn issue_ls_is_json_when_piped_and_a_table_on_a_tty() {
    let (pm, state) = fixture();
    let out = piped(pm.path(), state.path(), &["issue", "ls"], None);
    let v: Value = serde_json::from_str(&out).expect("piped `issue ls` is JSON");
    let ids: Vec<&str> = v["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"CAD-1") && ids.contains(&"CAD-3"), "{ids:?}");
    assert_eq!(out.trim_end().lines().count(), 1, "{out}");
    // A TTY still gets the table.
    let tty = on_tty(pm.path(), state.path(), &["issue", "ls"]);
    assert!(tty.lines().any(|l| l.starts_with("ID")), "{tty}");
    assert!(serde_json::from_str::<Value>(&tty).is_err(), "{tty}");
    // The table can still be forced onto a pipe.
    let out = piped(pm.path(), state.path(), &["issue", "ls"], Some("table"));
    assert!(out.lines().any(|l| l.starts_with("ID")), "{out}");
}

#[test]
fn other_table_listers_are_json_when_piped() {
    let (pm, state) = fixture();
    for args in [&["issue", "epic", "ls"][..], &["milestone", "ls"][..]] {
        let out = piped(pm.path(), state.path(), args, None);
        serde_json::from_str::<Value>(&out)
            .unwrap_or_else(|e| panic!("{args:?} piped is not JSON: {e}: {out}"));
        assert_eq!(out.trim_end().lines().count(), 1, "{args:?}: {out}");
    }
}
