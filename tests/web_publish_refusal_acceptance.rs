//! CAD-1429 Publish Web: independent CLI refusal acceptance.
//!
//! Proves the local logged-out gate: every `cadence web` action refuses
//! before any request is built, with no auth material on disk. Needs no
//! network and no credential.
//!
//! Remote-scope cases (live device grant, TLS hook) are planned on
//! CAD-1429 and depend on the library-test `WebIssuerTls` seam — when they
//! land they belong beside this file.
//!
//! Binding boundary (stated plainly): this file compiles only into the
//! library unit-test path, where `cfg!(test)` admits the loopback issuer pin.
//! It is NOT evidence about the production `cadence` binary, which rejects
//! loopback http and trusts only webpki roots. Production binary TLS is not
//! proved here.
#![cfg(all(unix, feature = "test-seam"))]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_cadence");

/// Fresh private temp root per case, so no real config directory is touched.
fn private_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("cad1429-acc-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for sub in ["home", "xdg", "tmp", "state"] {
        std::fs::create_dir_all(root.join(sub)).expect("create private root");
    }
    root
}

/// Run one `cadence web <args>` invocation with the environment fully
/// isolated. Nothing is inherited from the operator's shell.
fn web(root: &Path, args: &[&str]) -> Output {
    // Global `--org` only; `--state-dir` conflicts with `--org` by design, so
    // the org path resolves through the isolated XDG config instead.
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env("TMPDIR", root.join("tmp"))
        .env_remove("CADENCE_STATE_DIR")
        .env_remove("CADENCE_PM_DIR")
        .env_remove("CADENCE_ALIAS")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cadence_agent::reaper::output(&mut cmd).expect("spawn cadence web")
}

/// No credential or pending record may exist after a refused call.
fn no_auth_material(root: &Path) -> bool {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, found);
            } else {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with("cli-") || name.starts_with("web-") {
                    found.push(p);
                }
            }
        }
    }
    let mut found = Vec::new();
    walk(&root.join("xdg"), &mut found);
    walk(&root.join("home"), &mut found);
    found.is_empty()
}

/// Logged-out gate: all seven web actions refuse before any request is built.
/// This is the one case that runs without the TLS hook or a device grant.
#[test]
fn logged_out_every_web_action_refuses_with_no_auth_material() {
    let root = private_root("logged-out");
    let cases: &[&[&str]] = &[
        &["prepare", "--name", "acc-site", "index.html"],
        &["publish", "rel_33333333"],
        &["status", "acc-site"],
        &["releases", "acc-site"],
        &["submissions", "acc-site"],
        &["rollback", "acc-site", "rel_33333333"],
        &["unpublish", "acc-site"],
    ];
    for args in cases {
        // Global flags come before the subcommand: `--org acme web <action>`.
        let mut full: Vec<&str> = vec!["--org", "acme", "web"];
        full.extend_from_slice(args);
        let out = web(&root, &full);
        let code = out.status.code().unwrap_or(-1);
        assert_eq!(code, 3, "args {args:?} must refuse with exit 3");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("nothing was sent"),
            "args {args:?} must state nothing was sent: {stderr}"
        );
    }
    assert!(
        no_auth_material(&root),
        "a refused call wrote credential or pending material"
    );
    let _ = std::fs::remove_dir_all(&root);
}
