//! CAD-647: the source-checkout bridge does not access a daemon or PM.
// A test binary never runs the daemon reaper.
#![allow(clippy::disallowed_methods)]
use std::fs;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn cad647_app_dev_bridge_scrubs_credentials_without_reading_production_state() {
    let source = TempDir::new().unwrap();
    fs::create_dir(source.path().join("scripts")).unwrap();
    fs::create_dir(source.path().join("ui")).unwrap();
    fs::write(source.path().join("ui/package.json"), "{}").unwrap();
    fs::write(source.path().join("scripts/app-dev.mjs"), r#"
const forbidden=['CADENCE_ALIAS','CADENCE_STATE_DIR','CADENCE_PM_DIR','VITE_PRIVATE_SENTINEL','OPENAI_API_KEY','NODE_OPTIONS'];
if(forbidden.some(key=>key in process.env)) process.exit(42);
if(process.argv.slice(2).join(' ')!=='social-content --port 3186 --host 127.0.0.1') process.exit(43);
"#).unwrap();
    let state = source.path().join("must-not-adopt-state");
    fs::create_dir(&state).unwrap();
    let marker = state.join(".cadence-sandbox");
    fs::write(&marker, "poisoned sandbox profile").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cadence"))
        .args(["app", "dev", "social-content", "--source"])
        .arg(source.path())
        .env("CADENCE_ALIAS", "fixture-agent")
        .env("CADENCE_STATE_DIR", &state)
        .env("CADENCE_PM_DIR", source.path().join("must-not-open-pm"))
        .env("VITE_PRIVATE_SENTINEL", "do-not-forward")
        .env("OPENAI_API_KEY", "do-not-forward")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(marker).unwrap(),
        "poisoned sandbox profile"
    );
    assert_eq!(fs::read_dir(state).unwrap().count(), 1);
}

#[test]
fn cad647_app_dev_refuses_production_port_before_executing_a_source() {
    let output = Command::new(env!("CARGO_BIN_EXE_cadence"))
        .args([
            "app",
            "dev",
            "social-content",
            "--source",
            "/does-not-exist",
            "--port",
            "3010",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("3110..=3199"));
}
