//! The one integration suite (CAD-1970).
//!
//! The integration target also carries acceptance checks for changed gates.

#[test]
fn an_identifier_is_validated_before_anything_uses_it() {
    assert!(cadence_agent::proto::identifier("cad1234", "issue id").is_ok());
    assert!(cadence_agent::proto::identifier("not a valid id", "issue id").is_err());
}

// CAD-1981 acceptance, authored by the ticket author; implementers must not edit it.
#[test]
fn audit_verdicts_respect_the_invocations_organization() {
    use std::process::{Command, Output};

    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config");
    let notes = home.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    let head = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    std::fs::write(
        notes.join("20261101-120000-synthetic-verdict.md"),
        format!(
            "# Verdict: CAD-999 Standards review — pass\n\
             > Issue: CAD-999\n\
             > From: synthetic-reviewer\n\n\
             ## Verdict\n\
             pass — PR #5, head {head}\n\n\
             Risk: auto\n"
        ),
    )
    .unwrap();

    let run = |prefix: &[&str], org_env: Option<&str>, managed: bool| -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cadence"));
        cmd.env_clear()
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_STATE_HOME", home.path().join("state"))
            .args(prefix)
            .args([
                "audit", "verdicts", "--issue", "CAD-999", "--pr", "5", "--head", head,
            ])
            .arg("--notes-dir")
            .arg(&notes);
        if let Some(org) = org_env {
            cmd.env("CADENCE_ORG", org);
        }
        if managed {
            cmd.env("CADENCE_ALIAS", "synthetic-agent");
        }
        cadence_agent::reaper::output(&mut cmd).unwrap()
    };
    let local_pass = |out: Output| {
        assert!(
            out.status.success(),
            "local audit failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["verdicts"][0]["reviewer"], "synthetic-reviewer");
        assert_eq!(value["verdicts"][0]["outcome"], "pass");
    };
    let no_local_pass = |out: Output| {
        assert!(
            !out.status.success(),
            "hosted selection returned local output: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(!String::from_utf8_lossy(&out.stdout).contains("synthetic-reviewer"));
    };

    // No daemon or org registry is present; local parsing must still work.
    let without_home = cadence_agent::reaper::output(
        Command::new(env!("CARGO_BIN_EXE_cadence"))
            .env_clear()
            .args([
                "audit", "verdicts", "--issue", "CAD-999", "--pr", "5", "--head", head,
            ])
            .arg("--notes-dir")
            .arg(&notes),
    )
    .unwrap();
    local_pass(without_home);
    let invalid_config = cadence_agent::reaper::output(
        Command::new(env!("CARGO_BIN_EXE_cadence"))
            .env_clear()
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", "relative-config")
            .args([
                "audit", "verdicts", "--issue", "CAD-999", "--pr", "5", "--head", head,
            ])
            .arg("--notes-dir")
            .arg(&notes),
    )
    .unwrap();
    no_local_pass(invalid_config);
    local_pass(run(&["--org", "local"], None, false));
    no_local_pass(run(&["--org", "missing-hosted"], None, false));
    no_local_pass(run(&[], Some("missing-hosted"), false));
    no_local_pass(run(&["--org", "missing-hosted"], None, true));

    // A saved local default must not override this invocation's hosted choice.
    let registry = config.join("cadence").join("orgs.json");
    std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
    // An unreadable saved selection is not proof that this is a local org.
    std::os::unix::fs::symlink("missing-registry", &registry).unwrap();
    no_local_pass(run(&[], None, false));
    std::fs::remove_file(&registry).unwrap();

    std::fs::write(
        &registry,
        serde_json::to_vec(&serde_json::json!({
            "selected": {"org": "local"},
            "connections": [
                {
                    "selection": {"org": "local"},
                    "destination": {
                        "mode": "local",
                        "state_dir": home.path().join("local-state"),
                        "tracker_dir": home.path().join("tracker")
                    }
                },
                {
                    "selection": {"org": "remote-test"},
                    "destination": {
                        "mode": "remote",
                        "endpoint": "https://example.invalid",
                        "org_id": "ws_synthetic"
                    }
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    local_pass(run(&[], None, false));
    no_local_pass(run(&["--org", "remote-test"], None, false));
    no_local_pass(run(&[], Some("remote-test"), false));
    local_pass(run(&["--org", "local"], Some("remote-test"), false));
    assert!(
        !config.join("cadence").join("orgs.lock").exists(),
        "a read-only verdict must not create a registry lock"
    );

    // A saved hosted default also must not fall back to local notes.
    let mut saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
    saved["selected"]["org"] = serde_json::json!("remote-test");
    std::fs::write(&registry, serde_json::to_vec(&saved).unwrap()).unwrap();
    no_local_pass(run(&[], None, false));
    local_pass(run(&["--org", "local"], None, false));

    // Explicit local-instance pins and explicit orgs must not be mixed.
    let state = home.path().join("pinned");
    let conflict = run(
        &[
            "--state-dir",
            state.to_str().unwrap(),
            "--org",
            "remote-test",
        ],
        None,
        false,
    );
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("--org conflicts"));
    assert!(!String::from_utf8_lossy(&conflict.stdout).contains("synthetic-reviewer"));
}

// AOS-473: the verdicts document names its origin, and a local read never
// pretends to be repo-scoped. The gate applies its attested counting rule only
// to a document that says `hosted`; a local document (or an older client's,
// which has no marker) keeps today's rule, so the marker has to be real.
#[test]
fn audit_verdicts_names_its_origin_and_a_local_read_refuses_a_repo() {
    use std::process::{Command, Output};

    let home = tempfile::tempdir().unwrap();
    let notes = home.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    let head = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    std::fs::write(
        notes.join("20261102-120000-synthetic-verdict.md"),
        format!(
            "# Verdict: CAD-998 Standards review — pass\n\
             > Issue: CAD-998\n\
             > From: synthetic-reviewer\n\n\
             ## Verdict\n\
             pass — PR #5, head {head}\n\n\
             Risk: auto\n"
        ),
    )
    .unwrap();

    let run = |extra: &[&str]| -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cadence"));
        cmd.env_clear()
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("XDG_STATE_HOME", home.path().join("state"))
            .args([
                "audit", "verdicts", "--issue", "CAD-998", "--pr", "5", "--head", head,
            ])
            .arg("--notes-dir")
            .arg(&notes)
            .args(extra);
        cadence_agent::reaper::output(&mut cmd).unwrap()
    };

    let local = run(&[]);
    assert!(
        local.status.success(),
        "local audit failed: {}",
        String::from_utf8_lossy(&local.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&local.stdout).unwrap();
    assert_eq!(doc["origin"], "local");
    assert_eq!(doc["verdicts"][0]["reviewer"], "synthetic-reviewer");

    // A local note names no repository: asking for one is a refusal, never a
    // silently unfiltered answer.
    let scoped = run(&["--repo", "favcrm/cadence"]);
    assert!(!scoped.status.success(), "a local read honoured --repo");
    assert!(String::from_utf8_lossy(&scoped.stderr).contains("--repo"));
    assert!(scoped.stdout.is_empty());
}
