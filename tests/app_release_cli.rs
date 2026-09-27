//! Actual operator CLI releases exact reviewed material without project grants.
#![allow(clippy::disallowed_methods)]
mod common;
use common::{
    app_release::{Release, A},
    TestDaemon,
};
use serde_json::Value;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
fn cli(d: &TestDaemon, args: &[&str]) -> Value {
    let private = tempfile::tempdir_in(d.dir.path()).unwrap();
    let script = private.path().join("release-cli.py");
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
            "actual release CLI did not return"
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
fn cad692_actual_operator_cli_binding_cas_and_exact_release_roundtrip() {
    let r = Release::new();
    let context = r.context("CLI Client", A, "cli-context");
    let install = r.install["install_id"].as_str().unwrap();
    let ctx = context["id"].as_str().unwrap();
    let args = [
        "binding",
        "create",
        install,
        "--context-id",
        ctx,
        "--slot",
        "publication",
        "--connection-id",
        &r.connection,
        "--request-id",
        "cli-binding",
    ];
    let binding = success(&r.daemon, &args)["binding"].clone();
    assert_eq!(success(&r.daemon, &args)["binding"], binding);
    let id = binding["id"].as_str().unwrap();
    assert_eq!(
        success(&r.daemon, &["binding", "show", install, id])["binding"],
        binding
    );
    assert_eq!(
        success(&r.daemon, &["binding", "ls", install, "--context-id", ctx])["bindings"][0],
        binding
    );
    let updated = success(
        &r.daemon,
        &[
            "binding",
            "set",
            install,
            id,
            "--expected-revision",
            "1",
            "--connection-id",
            &r.connection,
        ],
    )["binding"]
        .clone();
    assert_eq!(updated["revision"], 2);
    assert_ne!(
        cli(
            &r.daemon,
            &["binding", "revoke", install, id, "--expected-revision", "1"]
        )["rc"],
        0
    );
    assert_eq!(
        success(&r.daemon, &["binding", "show", install, id])["binding"],
        updated
    );
    let run = r.complete(&context, "cli-run");
    let run_id = run["id"].as_str().unwrap();
    let artifact = run["artifacts"][0]["id"].as_str().unwrap();
    let effect = success(
        &r.daemon,
        &[
            "effect",
            "stage",
            run_id,
            "--artifact-id",
            artifact,
            "--slot",
            "publication",
            "--request-id",
            "cli-stage",
            "--title",
            "CLI reviewed draft",
        ],
    )["effect"]
        .clone();
    assert_eq!(effect["state"], "waiting");
    assert!(r.items().as_array().unwrap().is_empty());
    let eid = effect["effect_id"].as_str().unwrap();
    assert_eq!(
        success(&r.daemon, &["effect", "show", eid])["effect"],
        effect
    );
    assert_eq!(
        success(
            &r.daemon,
            &["effect", "ls", "--install-id", install, "--context-id", ctx]
        )["effects"][0],
        effect
    );
    assert_ne!(
        cli(&r.daemon, &["effect", "accept", eid, "--digest", "wrong"])["rc"],
        0
    );
    assert_ne!(
        cli(
            &r.daemon,
            &[
                "effect",
                "stage",
                run_id,
                "--artifact-id",
                artifact,
                "--slot",
                "publication",
                "--request-id",
                "forged",
                "--title",
                "Forged",
                "--body",
                "caller text"
            ]
        )["rc"],
        0
    );
    assert_eq!(
        success(&r.daemon, &["effect", "show", eid])["effect"],
        effect
    );
    assert!(r.items().as_array().unwrap().is_empty());
    let done = success(
        &r.daemon,
        &[
            "effect",
            "accept",
            eid,
            "--digest",
            effect["digest"].as_str().unwrap(),
        ],
    );
    assert_eq!(done["effect"]["state"], "done");
    assert_eq!(r.items().as_array().unwrap().len(), 1);
    assert!(r.artifact(&run)["text"].as_str().unwrap().contains(A));
    assert!(
        cadence_agent::issue::project::list(&r.root.path().join("pm"))
            .unwrap()
            .is_empty()
    );
    let db = rusqlite::Connection::open(r.daemon.state.join("cadence.sqlite3")).unwrap();
    let grants: i64 = db
        .query_row("SELECT count(*) FROM platform_grants", [], |row| row.get(0))
        .unwrap();
    assert_eq!(grants, 0, "CLI leaked ambient worker grants");
}
