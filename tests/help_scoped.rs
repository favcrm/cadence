// CAD-888: the real binary's help views. The in-process snapshots live
// in src/cli/help.rs; this proves the wiring: what `main` prints for
// the root help, the pane scope, `help <section>` and `help <verb>`.
use std::process::Command;

fn run(args: &[&str], alias: Option<&str>) -> (i32, String) {
    let home = tempfile::TempDir::new().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.args(args)
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin");
    if let Some(a) = alias {
        cmd.env("CADENCE_ALIAS", a);
    }
    let out = cadence_agent::reaper::output(&mut cmd).unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn root_help_is_the_grouped_core_list() {
    for flag in ["--help", "-h"] {
        let (code, help) = run(&[flag], None);
        assert_eq!(code, 0);
        assert!(help.len() < 4096, "{} bytes", help.len());
        assert_eq!(
            help.trim_end().lines().last().unwrap(),
            "More: cadence help operator | cadence help all"
        );
        for verb in ["self", "inbox", "issue", "plan", "send", "dispatch", "join"] {
            assert!(help.contains(&format!("  {verb} ")), "{verb}: {help}");
        }
        assert!(!help.contains("rollout"), "{help}");
    }
}

#[test]
fn a_pane_does_not_get_the_operator_pointer() {
    let (_, help) = run(&["--help"], Some("w1"));
    assert!(
        help.trim_end().ends_with("More: cadence help all"),
        "{help}"
    );
    assert!(!help.contains("help operator"), "{help}");
}

#[test]
fn help_sections_list_what_the_default_leaves_out() {
    let (code, operator) = run(&["help", "operator"], None);
    assert_eq!(code, 0);
    for verb in ["daemon", "update", "backup", "platform", "session"] {
        assert!(operator.contains(&format!("  {verb} ")), "{verb}");
    }
    let (code, all) = run(&["help", "all"], None);
    assert_eq!(code, 0);
    for verb in ["issue", "daemon", "rollout", "delivery", "job"] {
        assert!(all.contains(&format!("  {verb} ")), "{verb}");
    }
}

#[test]
fn help_for_a_verb_is_clap_help_and_hidden_verbs_run() {
    let (code, help) = run(&["help", "issue"], None);
    assert_eq!(code, 0);
    assert!(help.contains("Usage: cadence issue"), "{help}");
    let (code, help) = run(&["help", "rollout"], None);
    assert_eq!(code, 0);
    assert!(help.contains("Usage: cadence rollout"), "{help}");
    let (code, help) = run(&["backup", "--help"], None);
    assert_eq!(code, 0);
    assert!(help.contains("--keep"), "{help}");
}

#[test]
fn state_dir_may_follow_the_section_and_hidden_plumbing_stays_hidden() {
    let (code, all) = run(&["help", "all", "--state-dir", "/tmp/cad888-none"], None);
    assert_eq!(code, 0);
    assert!(all.contains("    issue epic stage"), "{all}");
    for hidden in [
        "confine",
        "mcp-agent",
        "mcp-permission",
        "master peek-grant",
    ] {
        assert!(!all.contains(hidden), "{hidden} listed");
    }
}
