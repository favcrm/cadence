//! Independent CAD-1922 acceptance of consolidated-home config guards.
//! Exercise the real offline registry CLI; never start setup, a daemon,
//! a browser, or a grant. All child environment paths are disposable.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::Builder;

fn org_list(root: &Path, home: &Path, xdg_config: Option<&Path>) -> Output {
    let temp = root.join("tmp");
    let bin = root.join("empty-bin");
    fs::create_dir_all(&temp).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_cadence"));
    command
        .env_clear()
        .current_dir(home)
        .env("HOME", home)
        .env("TMPDIR", &temp)
        .env("PATH", &bin)
        .env("CADENCE_TEST_REAL_HOME", root.join("not-the-real-home"))
        .args(["org", "list"]);
    if let Some(config) = xdg_config {
        command.env("XDG_CONFIG_HOME", config);
    }
    cadence_agent::reaper::output(&mut command).expect("run isolated offline cadence org list")
}

fn text(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn symlinked_consolidated_config_is_refused_without_writing_its_target() {
    let root = Builder::new()
        .prefix("c1922-home-")
        .tempdir_in("/tmp")
        .unwrap();

    // Positive control: exactly the same offline command accepts a real
    // consolidated config directory and creates its own registry lock.
    let real_home = root.path().join("real-home");
    let real_config = real_home.join(".cadence/config");
    fs::create_dir_all(&real_config).unwrap();
    let control = org_list(root.path(), &real_home, None);
    assert_eq!(control.status.code(), Some(0), "{}", text(&control));
    assert!(
        String::from_utf8_lossy(&control.stdout).contains("\"connections\""),
        "the control must reach the registry command: {}",
        text(&control)
    );
    assert!(real_config.join("orgs.lock").is_file());
    assert!(!real_home.join(".cadence/state").exists());

    let linked_home = root.path().join("linked-home");
    let redirected = root.path().join("redirected-config");
    fs::create_dir_all(linked_home.join(".cadence")).unwrap();
    fs::create_dir(&redirected).unwrap();
    let config_link = linked_home.join(".cadence/config");
    symlink(&redirected, &config_link).unwrap();

    let refused = org_list(root.path(), &linked_home, None);
    let detail = text(&refused);
    assert!(
        matches!(refused.status.code(), Some(3 | 4)),
        "expected an actual refusal, got {:?}: {detail}",
        refused.status.code()
    );
    assert!(
        detail.to_ascii_lowercase().contains("symlink"),
        "the refusal must identify the symlink: {detail}"
    );
    assert!(
        detail.contains(config_link.to_str().unwrap()),
        "the refusal must name the linked config root: {detail}"
    );
    assert!(
        fs::symlink_metadata(&config_link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the guard must leave the planted link untouched"
    );
    assert!(
        fs::read_dir(&redirected).unwrap().next().is_none(),
        "the refused command wrote into the symlink target"
    );
    assert!(!linked_home.join(".cadence/state").exists());
}

#[test]
fn first_run_keeps_config_in_its_home_despite_an_exported_xdg_config_home() {
    let root = Builder::new()
        .prefix("c1922-xdg-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = root.path().join("home");
    let xdg = root.path().join("xdg");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&xdg).unwrap();
    assert!(xdg.is_absolute());
    assert!(!home.join(".cadence").exists());
    assert!(!xdg.join("cadence").exists());

    let output = org_list(root.path(), &home, Some(&xdg));
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("\"connections\""),
        "the command must reach the registry rather than return early: {}",
        text(&output)
    );
    let config = home.join(".cadence/config");
    assert!(config.is_dir(), "first-run config was not consolidated");
    assert!(
        config.join("orgs.lock").is_file(),
        "the real registry command did not use the consolidated config"
    );
    assert!(
        !xdg.join("cadence").exists(),
        "first run created legacy config evidence under XDG_CONFIG_HOME"
    );
    assert!(
        fs::read_dir(&xdg).unwrap().next().is_none(),
        "first run must leave the unrelated exported config root untouched"
    );
    assert!(!home.join(".cadence/state").exists());
    assert!(!home.join(".local/state/cadence").exists());
    assert!(!home.join("pm").exists());
}
