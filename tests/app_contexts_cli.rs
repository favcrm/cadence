//! Actual unarmed operator CLI, private defaults files and revision CAS.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, TestDaemon};
use serde_json::{json, Value};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
fn cli(d: &TestDaemon, args: &[&str]) -> Value {
    let private = tempfile::tempdir_in(d.dir.path()).unwrap();
    let script = private.path().join("context-cli.py");
    let out = private.path().join("result.json");
    let code = common::op::lineage_script(
        "import json,os,subprocess,sys,time
out,runner=sys.argv[1:3]
argv=sys.argv[3:]",
        r#"assert not any(k.startswith('CADENCE_TEST') for k in os.environ)
r=subprocess.run(argv,stdin=subprocess.DEVNULL,capture_output=True,timeout=20)
with open(out+'.tmp','w') as f:
    json.dump({'rc':r.returncode,'stdout':r.stdout.decode(errors='replace'),'stderr':r.stderr.decode(errors='replace')},f)
os.rename(out+'.tmp',out)"#,
    );
    std::fs::write(&script, code).unwrap();
    let mut cmd = Command::new("setsid");
    cmd.args(["-f", "python3"])
        .arg(script)
        .arg(&out)
        .arg(std::process::id().to_string())
        .arg(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .arg("app")
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", private.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    assert!(cmd.status().unwrap().success());
    let deadline = Instant::now() + Duration::from_secs(25);
    while !out.exists() {
        assert!(
            Instant::now() < deadline,
            "actual context CLI did not return"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap()
}
fn success(d: &TestDaemon, args: &[&str]) -> Value {
    let result = cli(d, args);
    assert_eq!(result["rc"], 0, "CLI {args:?}: {result}");
    serde_json::from_str(result["stdout"].as_str().unwrap()).unwrap()
}
#[test]
fn cad690_actual_operator_cli_defaults_cas_and_archived_history_roundtrip() {
    let root = tempfile::tempdir().unwrap();
    let pm = Pm::init(&root.path().join("pm")).unwrap();
    let mut opts = daemon_opts();
    opts.test_seam = false;
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
    let d = TestDaemon::start_opts(opts);
    let bundle = root.path().join("bundle");
    std::fs::create_dir_all(bundle.join("workflows")).unwrap();
    let original = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("apps/local-content");
    std::fs::copy(original.join("app.md"), bundle.join("app.md")).unwrap();
    let text = std::fs::read_to_string(original.join("workflows/draft.md")).unwrap();
    let safe = text.replace("source: { ask:", "source: { context_default: true, ask:");
    assert_ne!(safe, text);
    std::fs::write(bundle.join("workflows/draft.md"), safe).unwrap();
    let installed = d
        .operator_rpc("app_workspace_install", json!({"source":bundle}))
        .unwrap();
    let install = installed["install_id"].as_str().unwrap();
    let defaults = root.path().join("defaults.json");
    std::fs::write(
        &defaults,
        json!({"source":"Client A source facts"}).to_string(),
    )
    .unwrap();
    let file = defaults.to_str().unwrap();
    let args = [
        "context",
        "create",
        install,
        "--label",
        "Client A",
        "--defaults",
        file,
        "--request-id",
        "cli-context-a",
    ];
    let created = success(&d, &args)["context"].clone();
    let id = created["id"].as_str().unwrap();
    assert!(!id.is_empty());
    assert_eq!(created["install_id"], install);
    assert_eq!(
        created["config"]["input_defaults"]["source"],
        "Client A source facts"
    );
    assert_eq!(
        success(&d, &args)["context"],
        created,
        "same request changed immutable identity"
    );
    assert_eq!(
        success(&d, &["context", "show", install, id])["context"],
        created
    );
    let listed = success(&d, &["context", "ls", install]);
    assert!(listed["contexts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == id));
    let old_revision = created["revision"].as_u64().unwrap().to_string();
    std::fs::write(
        &defaults,
        json!({"source":"Client A updated source"}).to_string(),
    )
    .unwrap();
    let updated = success(
        &d,
        &[
            "context",
            "set",
            install,
            id,
            "--expected-revision",
            &old_revision,
            "--label",
            "Client A updated",
            "--defaults",
            file,
        ],
    )["context"]
        .clone();
    assert_eq!(updated["id"], id);
    assert!(updated["revision"].as_u64().unwrap() > created["revision"].as_u64().unwrap());
    assert_ne!(updated["digest"], created["digest"]);
    let stale = cli(
        &d,
        &[
            "context",
            "set",
            install,
            id,
            "--expected-revision",
            &old_revision,
            "--label",
            "Stale",
            "--defaults",
            file,
        ],
    );
    assert_ne!(stale["rc"], 0);
    let stale_archive = cli(
        &d,
        &[
            "context",
            "archive",
            install,
            id,
            "--expected-revision",
            &old_revision,
        ],
    );
    assert_ne!(stale_archive["rc"], 0);
    assert_eq!(
        success(&d, &["context", "show", install, id])["context"],
        updated,
        "stale CLI mutated configuration"
    );
    let revision = updated["revision"].as_u64().unwrap().to_string();
    let archived = success(
        &d,
        &[
            "context",
            "archive",
            install,
            id,
            "--expected-revision",
            &revision,
        ],
    )["context"]
        .clone();
    assert_eq!(archived["state"], "archived");
    assert_eq!(
        success(&d, &["context", "show", install, id])["context"],
        archived,
        "operator historical context disappeared"
    );
    let empty = success(
        &d,
        &[
            "context",
            "create",
            install,
            "--label",
            "No defaults",
            "--request-id",
            "cli-empty",
        ],
    )["context"]
        .clone();
    assert_eq!(empty["config"]["input_defaults"], json!({}));
    std::fs::write(
        &defaults,
        json!({"source":"x".repeat(32*1024+1)}).to_string(),
    )
    .unwrap();
    let huge = cli(
        &d,
        &[
            "context",
            "create",
            install,
            "--label",
            "Too large",
            "--defaults",
            file,
            "--request-id",
            "cli-huge",
        ],
    );
    assert_ne!(huge["rc"], 0);
    let after = success(&d, &["context", "ls", install]);
    assert_eq!(
        after["contexts"].as_array().unwrap().len(),
        2,
        "failed CLI created a context"
    );
    let db = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    for table in [
        "platform_grants",
        "platform_effects",
        "platform_defaults",
        "app_runs",
    ] {
        let n: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "context CLI enabled execution or account authority");
    }
    assert!(!pm.dir.join("site").exists());
}
