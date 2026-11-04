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

// CAD-2001 acceptance, authored by the integrator under the recorded one-time
// exception; implementers must not edit it. Two projects may register the same
// checkout, but a cwd-only resolution must refuse rather than pick the first.
#[test]
fn a_shared_checkout_is_registered_by_both_projects_and_never_first_matched() {
    use cadence_agent::issue::{project, project_new, Pm};

    let tmp = tempfile::TempDir::new().unwrap();
    let pm = Pm::init(&tmp.path().join("pm")).unwrap();
    let repo = tmp.path().join("shared");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C")
            .arg(&repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args);
        let out = cadence_agent::reaper::output(&mut cmd).unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "synthetic\n").unwrap();
    git(&["add", "README.md"]);
    git(&["commit", "-q", "-m", "synthetic"]);
    let repo = repo.canonicalize().unwrap();

    for (key, prefix) in [("alpha", "ALP"), ("beta", "BET")] {
        let request = project_new::Request {
            key: key.into(),
            prefix: Some(prefix.into()),
            repo: Some(repo.clone()),
            ..project_new::Request::default()
        };
        project_new::run(&pm, &request, "operator", &[]).unwrap();
    }

    assert_eq!(
        project::claimants_for_cwd(&pm.dir, &repo),
        ["alpha", "beta"]
    );
    assert_eq!(project::key_for_cwd(&pm.dir, &repo), None);
    assert_eq!(
        project::resolve(&pm.dir, Some("alpha"), &repo).unwrap().key,
        "alpha"
    );
    assert_eq!(
        project::resolve(&pm.dir, Some("beta"), &repo).unwrap().key,
        "beta"
    );
}

// CAD-2014 acceptance, authored independently of the fix; implementers must
// not edit it. Native `job_list` emits `created` as a JSON number (the store
// column is REAL), so issue-bound job selection must order timestamps
// numerically. These checks drive the shared decision seam the CLI and the
// board read use: `board::outcomes_from_jobs` for the chosen job's outcome
// and `board::views_with_jobs` for the resulting issue readiness.
mod cad2014_job_readiness {
    use cadence_agent::issue::board::{self, Issue, JobOutcome, View};
    use cadence_agent::issue::model::Front;
    use serde_json::{json, Map, Value};
    use std::path::{Path, PathBuf};

    const NOTES: &str = "/nonexistent-cad2014-notes";

    fn job(id: &str, issue: &str, state: &str, created: f64, tasks: &[&str]) -> Value {
        json!({
            "id": id,
            "issue": issue,
            "state": state,
            "created": created,
            "tasks": tasks
                .iter()
                .map(|state| (state.to_string(), json!(1)))
                .collect::<Map<String, Value>>(),
        })
    }

    fn issue(id: &str, status: &str) -> Issue {
        let mut front = Front::new(id, "synthetic", "2026-11-03T00:00:00Z");
        front.status = status.to_string();
        Issue {
            project: "cadence".to_string(),
            dir: PathBuf::from("/nonexistent-cad2014-issue"),
            front,
            body: String::new(),
            comments: vec![],
            artifacts: vec![],
        }
    }

    fn view_of<'a>(views: &'a [View], id: &str) -> &'a View {
        views
            .iter()
            .find(|view| view.issue.front.id == id)
            .expect("issue view")
    }

    /// The reported bug: `created` values are numeric, so a string comparator
    /// ties them and the inverted ids pick the older job `z`. Its only task is
    /// the auto-created draft stub, which yields no outcome, so the issue
    /// falls through to its file `ready` status even though the newer job `a`
    /// has a blocked task.
    #[test]
    fn a_newer_blocked_job_is_not_hidden_by_an_older_unstarted_one() {
        let jobs = vec![
            job("z", "CAD-2014", "open", 1.0, &["draft"]),
            job("a", "CAD-2014", "open", 2.0, &["blocked"]),
        ];
        let outcomes = board::outcomes_from_jobs(&jobs);
        assert_eq!(outcomes.get("CAD-2014"), Some(&JobOutcome::Blocked));

        let views = board::views_with_jobs(
            Path::new(NOTES),
            vec![issue("CAD-2014", "ready")],
            &outcomes,
        );
        let view = view_of(&views, "CAD-2014");
        assert_eq!(view.blocked_reason, Some("job blocked"));
        assert!(
            view.blocked,
            "the newer job's blocked task must block the issue"
        );
        assert!(
            !view.ready,
            "a newer blocked job must not leave the issue ready"
        );
    }

    /// Numeric `created` is the ordering; an id that sorts the other way must
    /// not win. Both id directions are exercised so a fix cannot pass by
    /// continuing to sort ids.
    #[test]
    fn numeric_created_order_beats_an_inverted_job_id() {
        let newer_smaller_id = vec![
            job("z", "CAD-2014", "open", 1.0, &["running"]),
            job("a", "CAD-2014", "open", 2.0, &["blocked"]),
        ];
        assert_eq!(
            board::outcomes_from_jobs(&newer_smaller_id).get("CAD-2014"),
            Some(&JobOutcome::Blocked)
        );

        let newer_larger_id = vec![
            job("a", "CAD-2014", "open", 1.0, &["running"]),
            job("z", "CAD-2014", "open", 2.0, &["blocked"]),
        ];
        assert_eq!(
            board::outcomes_from_jobs(&newer_larger_id).get("CAD-2014"),
            Some(&JobOutcome::Blocked)
        );
    }

    /// Equal `created` keeps the established deterministic tie-break:
    /// descending id, so `z` (running) wins over `a` (blocked), and the result
    /// does not depend on the daemon's list order.
    #[test]
    fn equal_timestamps_use_a_stable_id_tie() {
        let jobs = vec![
            job("a", "CAD-2014", "open", 7.0, &["blocked"]),
            job("z", "CAD-2014", "open", 7.0, &["running"]),
        ];
        let forward = board::outcomes_from_jobs(&jobs);
        assert_eq!(forward.get("CAD-2014"), Some(&JobOutcome::Status("doing")));

        let mut reversed = jobs.clone();
        reversed.reverse();
        let backward = board::outcomes_from_jobs(&reversed);
        assert_eq!(backward.get("CAD-2014"), Some(&JobOutcome::Status("doing")));
    }

    /// The newest non-terminal job wins over a newer terminal one.
    #[test]
    fn a_newer_terminal_job_does_not_hide_an_older_active_job() {
        let jobs = vec![
            job("active", "CAD-2014", "open", 1.0, &["running"]),
            job("terminal", "CAD-2014", "done", 2.0, &["done"]),
        ];
        assert_eq!(
            board::outcomes_from_jobs(&jobs).get("CAD-2014"),
            Some(&JobOutcome::Status("doing"))
        );
    }

    /// With no live job, the numeric newest terminal job decides. A string
    /// comparator ties on created and picks the older failed job `z`, which
    /// yields no outcome at all.
    #[test]
    fn all_terminal_jobs_fall_back_to_the_newest_terminal_job() {
        let newest_done = vec![
            job("z", "CAD-2014", "failed", 1.0, &["failed"]),
            job("a", "CAD-2014", "done", 2.0, &["done"]),
        ];
        assert_eq!(
            board::outcomes_from_jobs(&newest_done).get("CAD-2014"),
            Some(&JobOutcome::Status("done"))
        );

        // A cancelled newest job reports no board outcome.
        let newest_cancelled = vec![
            job("z", "CAD-2014", "done", 1.0, &["done"]),
            job("a", "CAD-2014", "cancelled", 2.0, &["cancelled"]),
        ];
        assert_eq!(
            board::outcomes_from_jobs(&newest_cancelled).get("CAD-2014"),
            None
        );
    }

    /// Empty input and a single bound job are handled without inventing an
    /// issue entry; an unbound job is ignored.
    #[test]
    fn no_jobs_and_a_single_job_are_handled() {
        assert!(board::outcomes_from_jobs(&[]).is_empty());

        let single = vec![job("only", "CAD-2014", "open", 3.0, &["blocked"])];
        assert_eq!(
            board::outcomes_from_jobs(&single).get("CAD-2014"),
            Some(&JobOutcome::Blocked)
        );

        let unbound = vec![job("loose", "", "open", 4.0, &["blocked"])];
        assert!(board::outcomes_from_jobs(&unbound).is_empty());
    }

    /// Unsupported `created` values are not demoted to "oldest" while a
    /// numeric sibling wins: the original comparator kept its complete
    /// string/id ordering for the whole issue group whenever any `created` is
    /// nonnumeric. The missing/null/garbage timestamp therefore keeps `z`
    /// (blocked) ahead of `a` (created 2, draft-only). Demoting `z` would
    /// select `a`, yield no job outcome, and make the issue falsely ready.
    #[test]
    fn mixed_malformed_timestamps_keep_the_original_blocked_choice() {
        let mut missing = job("z", "CAD-2014", "open", 1.0, &["blocked"]);
        missing.as_object_mut().unwrap().remove("created").unwrap();
        let mut null = missing.clone();
        null["created"] = Value::Null;
        let mut garbage = missing.clone();
        garbage["created"] = json!("garbage");

        for blocked_z in [missing, null, garbage] {
            let jobs = vec![blocked_z, job("a", "CAD-2014", "open", 2.0, &["draft"])];
            let outcomes = board::outcomes_from_jobs(&jobs);
            assert_eq!(
                outcomes.get("CAD-2014"),
                Some(&JobOutcome::Blocked),
                "a nonnumeric created must not demote the blocked job"
            );

            let views = board::views_with_jobs(
                Path::new(NOTES),
                vec![issue("CAD-2014", "ready")],
                &outcomes,
            );
            let view = view_of(&views, "CAD-2014");
            assert!(view.blocked);
            assert!(
                !view.ready,
                "mixed malformed timestamps must not fake readiness"
            );
        }
    }
}

// CAD-2055 acceptance, authored independently of the implementation;
// implementers must not edit or weaken this real queue-guard check.
#[cfg(all(debug_assertions, feature = "test-seam"))]
#[test]
fn cad2055_native_queue_keeps_real_gates_after_test_job_retirement() {
    use base64::Engine;
    use cadence_agent::daemon::queue_authority_acceptance;
    use cadence_agent::error::{Error, Result};
    use serde_json::json;

    let repo = "fixture/queue";
    let base = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let rules = json!([{
        "type": "merge_queue",
        "ruleset_id": 7
    }]);
    let ruleset = json!({
        "id": 7,
        "enforcement": "active",
        "target": "branch",
        "current_user_can_bypass": "never",
        "bypass_actors": [],
        "rules": [{"type": "merge_queue"}]
    });
    let mut protection = json!({
        "enforce_admins": {"enabled": true},
        "required_status_checks": {
            "contexts": ["fmt", "clippy", "build", "ui"],
            "checks": [
                {"context": "fmt", "app_id": 15368},
                {"context": "clippy", "app_id": 15368},
                {"context": "build", "app_id": 15368},
                {"context": "ui", "app_id": 15368}
            ]
        }
    });
    // This is the current CI contract: four real jobs, no retired test job.
    let workflow = r#"on: [pull_request, merge_group]
jobs:
  fmt: {runs-on: ubuntu-22.04, steps: [{run: "true"}]}
  clippy: {runs-on: ubuntu-22.04, steps: [{run: "true"}]}
  build: {runs-on: ubuntu-22.04, steps: [{run: "true"}]}
  ui: {runs-on: ubuntu-22.04, steps: [{run: "true"}]}
"#;
    let contents = json!({
        "encoding": "base64",
        "content": base64::engine::general_purpose::STANDARD.encode(workflow)
    });
    let workflow_endpoint = format!("repos/{repo}/contents/.github/workflows/ci.yml?ref={base}");
    let read = |args: &[&str]| -> Result<String> {
        assert_eq!(args.len(), 2, "unexpected GitHub read: {args:?}");
        assert_eq!(args[0], "api", "unexpected GitHub command: {args:?}");
        match args[1] {
            "repos/fixture/queue/rulesets/7" => Ok(ruleset.to_string()),
            endpoint if endpoint == workflow_endpoint => Ok(contents.to_string()),
            _ => panic!("unexpected GitHub read: {args:?}"),
        }
    };

    queue_authority_acceptance(repo, base, &rules, Some(&protection), read)
        .expect("a protected four-gate workflow must not require the retired test job");

    // Operator settings can drop a still-existing gate. Keep their legacy
    // contexts internally consistent so only the real integration floor
    // can catch the missing ui protection, rather than an earlier check.
    protection["required_status_checks"]["checks"]
        .as_array_mut()
        .unwrap()
        .retain(|check| check["context"] != "ui");
    protection["required_status_checks"]["contexts"]
        .as_array_mut()
        .unwrap()
        .retain(|context| context != "ui");

    let error = queue_authority_acceptance(repo, base, &rules, Some(&protection), read)
        .expect_err("retiring test must not permit an unprotected real ui gate");
    match error {
        Error::Rejected(message) => assert_eq!(
            message,
            concat!(
                "delivery requirements: operator must protect the existing ui ",
                "integration gate before automatic landing"
            )
        ),
        other => panic!("expected a queue-guard rejection, got {other:?}"),
    }
}
