//! Actual credential-stdin CLI lifecycle against an unarmed embedded provider.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::contract_fixture::FakePlatform;
use common::{daemon_opts, TestDaemon};
use serde_json::{json, Value};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const FIRST: &str = concat!("cadp_cli_fixture_", "a1b2c3d4e5f6");
const SECOND: &str = concat!("cadp_cli_rotated_", "z9y8x7w6v5");
fn fixture() -> TestDaemon {
    let mut opts = daemon_opts();
    opts.test_seam = false;
    opts.platforms
        .insert("fixture".into(), Arc::new(FakePlatform::standard()));
    TestDaemon::start_opts(opts)
}
fn cli(d: &TestDaemon, args: &[&str], input: &str) -> Value {
    let private = tempfile::tempdir_in(d.dir.path()).unwrap();
    let script = private.path().join("operator-cli.py");
    let input_path = private.path().join("credential-input");
    let out = private.path().join("result.json");
    // This caller is off the test/daemon ancestry before it invokes the CLI.
    // No CADENCE_TEST_AS, token environment variable or assertion frame exists.
    let code = common::op::lineage_script(
        "import json,os,subprocess,sys,time
input_path,out,runner=sys.argv[1:4]
argv=sys.argv[4:]",
        r#"assert not any(k.startswith('CADENCE_TEST') for k in os.environ)
p = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
stdout, stderr = p.communicate(open(input_path,'rb').read(), timeout=20)
with open(out+'.tmp','w') as f:
    json.dump({'rc':p.returncode,'stdout':stdout.decode(errors='replace'),'stderr':stderr.decode(errors='replace')},f)
os.rename(out+'.tmp',out)"#,
    );
    std::fs::write(&script, code).unwrap();
    std::fs::write(&input_path, input).unwrap();
    let mut cmd = Command::new("setsid");
    cmd.args(["-f", "python3"])
        .arg(&script)
        .arg(&input_path)
        .arg(&out)
        .arg(std::process::id().to_string())
        .arg(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .arg("connection")
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", private.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let debug = format!("{cmd:?}");
    assert!(!debug.contains(FIRST));
    assert!(!debug.contains(SECOND));
    assert!(cmd.status().unwrap().success());
    let deadline = Instant::now() + Duration::from_secs(25);
    while !out.exists() {
        assert!(Instant::now() < deadline, "actual CLI did not return");
        std::thread::sleep(Duration::from_millis(20));
    }
    let text = std::fs::read_to_string(out).unwrap();
    assert!(!text.contains(FIRST), "CLI leaked first credential");
    assert!(!text.contains(SECOND), "CLI leaked rotated credential");
    serde_json::from_str(&text).unwrap()
}
fn success(d: &TestDaemon, args: &[&str], input: &str) -> Value {
    let result = cli(d, args, input);
    assert_eq!(result["rc"], 0, "CLI {args:?} failed: {result}");
    serde_json::from_str(result["stdout"].as_str().unwrap()).unwrap()
}
#[test]
fn cad688_actual_operator_cli_stdin_preserves_id_rotation_and_revocation() {
    let d = fixture();
    let providers = success(&d, &["providers"], "");
    assert!(providers["providers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["provider"] == "fixture"));
    let listed = success(&d, &["ls"], "");
    assert!(listed["connections"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["provider"] == "local" && c["account"] == "local"));
    let db = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    let grants_before: i64 = db
        .query_row("SELECT count(*) FROM platform_grants", [], |r| r.get(0))
        .unwrap();
    let args = [
        "create",
        "fixture",
        "--account",
        "cli-work",
        "--scope",
        "widgets:read",
        "--token-stdin",
        "--accept-same-uid-risk",
    ];
    let created = success(&d, &args, &format!("{FIRST}\n"))["connection"].clone();
    let id = created["id"].as_str().unwrap();
    assert!(!id.is_empty());
    assert_eq!(created["account"], "cli-work");
    assert_eq!(created["provider"], "fixture");
    assert_eq!(success(&d, &["show", id], "")["connection"], created);
    assert_eq!(success(&d, &["check", id], "")["connection"]["id"], id);
    let rotated =
        success(&d, &["rotate", id, "--token-stdin"], &format!("{SECOND}\n"))["connection"].clone();
    assert_eq!(rotated["id"], id);
    assert_eq!(rotated["account"], created["account"]);
    assert!(rotated["revision"].as_u64().unwrap() > created["revision"].as_u64().unwrap());
    assert_eq!(success(&d, &["show", id], "")["connection"], rotated);
    assert_eq!(success(&d, &["revoke", id], "")["revoked"], true);
    let missing = cli(&d, &["show", id], "");
    assert_ne!(missing["rc"], 0);
    let replacement = success(&d, &args, &format!("{FIRST}\n"))["connection"].clone();
    assert_ne!(replacement["id"], id);
    let stale = cli(&d, &["rotate", id, "--token-stdin"], &format!("{SECOND}\n"));
    assert_ne!(stale["rc"], 0);
    assert_eq!(
        success(&d, &["show", replacement["id"].as_str().unwrap()], "")["connection"],
        replacement
    );
    let grants_after: i64 = db
        .query_row("SELECT count(*) FROM platform_grants", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        grants_after, grants_before,
        "CLI management granted a worker"
    );
    let events: String = db
        .query_row(
            "SELECT COALESCE(group_concat(payload),'') FROM events",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!events.contains(FIRST));
    assert!(!events.contains(SECOND));
    // CLI envelope stays metadata-only, not an application binding.
    assert!(replacement.get("token").is_none());
    assert!(replacement.get("run_id").is_none());
    assert_eq!(
        d.operator_rpc(
            "connection_show",
            json!({"connection_id":replacement["id"]})
        )
        .unwrap()["connection"],
        replacement
    );
}
