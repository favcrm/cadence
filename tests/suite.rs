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

// CAD-2081 acceptance for workflow-issued review verdicts, authored from the ticket
// by someone other than the implementer; implementers must not edit it.
//
// Each case drives the real `scripts/enqueue-reviewed --dry-run` in a throwaway git
// checkout against a stubbed `gh` and `cadence` (tests/fixtures/cad2081_gate_stub.py).
// The hosted row reader is covered in src/delivery/hosted_gate.rs.
mod cad2081_workflow_verdicts {
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const STUB: &str = include_str!("fixtures/cad2081_gate_stub.py");
    const REPO: &str = "favcrm/cadence";
    const STD_FILE: &str = ".github/workflows/review-standards.yml";
    const SPEC_FILE: &str = ".github/workflows/review-spec.yml";
    const ALLOWLIST: &str = "docs/roles/review-workflow-principals.toml";
    const RISK_PATHS: &str = "docs/roles/risk-paths.toml";
    const ONE_REVIEW: &str = "docs/roles/one-review-paths.toml";
    const GOOD_ALLOWLIST: &str = "[principals.svc_std]\nkind = \"standards\"\n\
        workflow = \".github/workflows/review-standards.yml\"\n\
        [principals.svc_spec]\nkind = \"spec-security\"\n\
        workflow = \".github/workflows/review-spec.yml\"\n";

    struct Fixture {
        dir: tempfile::TempDir,
        base: String,
        head: String,
        self_edit_head: String,
        differs: String,
        no_file: String,
        tip: String,
        tip_same: String,
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = cadence_agent::reaper::output(
            Command::new("git")
                .current_dir(dir)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", dir)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args),
        )
        .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn put(dir: &Path, path: &str, text: &str) {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }

    fn commit(dir: &Path, message: &str) -> String {
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", message]);
        git(dir, &["rev-parse", "HEAD"])
    }

    impl Fixture {
        fn new() -> Fixture {
            Fixture::build(false)
        }

        /// The same repository with the standards workflow a symlink at the base.
        fn with_symlinked_workflow() -> Fixture {
            Fixture::build(true)
        }

        fn build(symlinked: bool) -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            // The repository, and its origin beside it: nothing but the repository's own
            // files may sit under the work tree, or `git add -A` would commit them.
            let work = dir.path().join("work");
            std::fs::create_dir(&work).unwrap();
            let d = work.as_path();
            git(d, &["init", "-q", "-b", "main"]);
            if symlinked {
                put(
                    d,
                    ".github/workflows/review-standards-impl.yml",
                    "standards workflow v1\n",
                );
                std::os::unix::fs::symlink("review-standards-impl.yml", d.join(STD_FILE)).unwrap();
            } else {
                put(d, STD_FILE, "standards workflow v1\n");
            }
            put(d, SPEC_FILE, "spec workflow v1\n");
            put(d, "README", "base\n");
            let base = commit(d, "base");
            // A superseded workflow, and one without the file, off the base.
            git(d, &["checkout", "-q", "-b", "old", &base]);
            put(d, STD_FILE, "standards workflow v0\n");
            let differs = commit(d, "old standards workflow");
            git(d, &["checkout", "-q", "-b", "gone", &base]);
            std::fs::remove_file(d.join(STD_FILE)).unwrap();
            let no_file = commit(d, "no standards workflow");
            // A PR that rewrites its own standards workflow.
            git(d, &["checkout", "-q", "-b", "selfedit", &base]);
            put(d, STD_FILE, "standards workflow v2, written by the PR\n");
            put(d, "notes/change.txt", "x\n");
            let self_edit_head = commit(d, "pr rewrites its own reviewer");
            // main has moved on since the PR's base: a live tip with a newer standards
            // workflow, published to a bare origin as main and fetched, so the gate can
            // fetch a commit by its sha as well as ask the API for it.
            git(d, &["checkout", "-q", "-b", "tipmain", &base]);
            if !symlinked {
                put(d, STD_FILE, "standards workflow v3, live on main\n");
            }
            let tip = commit(d, "main moves the standards workflow");
            let origin = dir.path().join("origin.git");
            git(d, &["init", "-q", "--bare", origin.to_str().unwrap()]);
            git(d, &["remote", "add", "origin", origin.to_str().unwrap()]);
            git(d, &["push", "-q", "origin", "tipmain:refs/heads/main"]);
            git(d, &["fetch", "-q", "origin"]);
            git(d, &["checkout", "-q", "-b", "tip2", &tip]);
            let tip_same = commit(d, "an unrelated change after the tip");
            git(d, &["checkout", "-q", "main"]);
            put(d, "notes/change.txt", "x\n");
            let head = commit(d, "ordinary change");
            Fixture {
                dir,
                base,
                head,
                self_edit_head,
                differs,
                no_file,
                tip,
                tip_same,
            }
        }

        fn work(&self) -> PathBuf {
            self.dir.path().join("work")
        }

        /// A head off the PR's base that changes only `path`.
        fn touch(&self, path: &str) -> String {
            let d = self.work();
            let name = format!("touch-{}", path.replace('/', "-"));
            git(&d, &["checkout", "-q", "-B", &name, &self.base]);
            put(&d, path, "// x\n");
            let head = commit(&d, &format!("touch {path}"));
            git(&d, &["checkout", "-q", "main"]);
            head
        }

        /// The base as it really is: the repository's own risk and one-review lists.
        fn base_files(&self) -> Value {
            let real = |path: &str| {
                std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
            };
            json!({
                ".github/workflows/ci.yml": "jobs:\n  ci:\n    runs-on: x\n",
                RISK_PATHS: real(RISK_PATHS),
                ONE_REVIEW: real(ONE_REVIEW),
                ALLOWLIST: GOOD_ALLOWLIST,
            })
        }

        fn wrow(&self, name: &str, stamp: &str, principal: &str, kind: &str) -> Value {
            json!({"name": name, "stamp": stamp, "reviewer": principal, "reviewer_id": principal,
                   "repo": REPO, "kind": kind, "outcome": "pass", "title_outcome": "pass",
                   "section_outcome": "pass", "risk": "auto", "identity_source": "workflow-oidc",
                   "run_id": "9001", "run_attempt": "1", "workflow_sha": self.tip,
                   "jti": "jti-0123456789ab", "base_sha": self.base})
        }

        fn std(&self) -> Value {
            self.wrow(
                "20261103-000100-std.md",
                "20261103-000100",
                "svc_std",
                "standards",
            )
        }

        fn spec(&self) -> Value {
            self.wrow(
                "20261103-000200-spec.md",
                "20261103-000200",
                "svc_spec",
                "spec-security",
            )
        }

        fn owner(&self, name: &str, stamp: &str, id: &str, login: &str, kind: &str) -> Value {
            json!({"name": name, "stamp": stamp, "reviewer": login, "reviewer_id": id,
                   "github_login": login, "repo": REPO, "kind": kind, "outcome": "pass",
                   "title_outcome": "pass", "section_outcome": "pass", "risk": "auto",
                   "identity_source": "owner-attested"})
        }

        fn identities() -> Value {
            json!([{"github_login": "authorlogin", "subject_id": "usr_author"},
                   {"github_login": "alice", "subject_id": "usr_alice"},
                   {"github_login": "bob", "subject_id": "usr_bob"}])
        }

        /// Run the real gate in dry-run mode: (exit code, stdout, stderr).
        fn gate(&self, case: &Scenario) -> (i32, String, String) {
            let bin = self.dir.path().join(".stubs");
            std::fs::create_dir_all(&bin).unwrap();
            for name in ["gh", "cadence"] {
                let path = bin.join(name);
                std::fs::write(&path, STUB).unwrap();
                std::fs::set_permissions(
                    &path,
                    std::os::unix::fs::PermissionsExt::from_mode(0o755),
                )
                .unwrap();
            }
            let head = case.head.clone().unwrap_or_else(|| self.head.clone());
            let mut by_ref = serde_json::Map::new();
            by_ref.insert(
                self.base.clone(),
                case.base_files.clone().unwrap_or_else(|| self.base_files()),
            );
            let tip_files = case
                .tip_files
                .clone()
                .unwrap_or_else(|| case.base_files.clone().unwrap_or_else(|| self.base_files()));
            by_ref.insert(self.tip.clone(), tip_files);
            if let Some(files) = &case.head_files {
                by_ref.insert(head.clone(), files.clone());
            }
            let scenario = json!({
                "repo": REPO, "pr": 7, "title": "CAD-1: add the thing", "head": head,
                "base": case.base_ref, "base_oid": self.base, "tip": self.tip, "author": "authorlogin",
                "files_by_ref": by_ref, "verdicts": case.rows,
                "identities": case.identities.clone().unwrap_or_else(Self::identities),
            });
            let scenario_path = self.dir.path().join(".scenario.json");
            std::fs::write(&scenario_path, serde_json::to_vec(&scenario).unwrap()).unwrap();
            let script: PathBuf =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/enqueue-reviewed");
            let out = cadence_agent::reaper::output(
                Command::new("python3")
                    .arg(script)
                    .args([
                        "7",
                        "--head",
                        &head,
                        "--repo",
                        REPO,
                        "--dry-run",
                        "--notes-dir",
                        "/nonexistent",
                    ])
                    .args(&case.extra_args)
                    .current_dir(self.work())
                    .env_clear()
                    .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                    .env("HOME", self.dir.path())
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("CADENCE_BIN", bin.join("cadence"))
                    .env("CAD2081_SCENARIO", &scenario_path),
            )
            .unwrap();
            (
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        }
    }

    struct Scenario {
        rows: Vec<Value>,
        base_ref: String,
        head: Option<String>,
        base_files: Option<Value>,
        head_files: Option<Value>,
        tip_files: Option<Value>,
        identities: Option<Value>,
        extra_args: Vec<String>,
    }

    fn scenario(rows: Vec<Value>) -> Scenario {
        Scenario {
            rows,
            base_ref: "main".into(),
            head: None,
            base_files: None,
            head_files: None,
            tip_files: None,
            identities: None,
            extra_args: vec![],
        }
    }

    fn refused(f: &Fixture, case: Scenario, why: &str) {
        refused_all(f, case, &[why]);
    }

    fn refused_all(f: &Fixture, case: Scenario, whys: &[&str]) {
        let (code, stdout, stderr) = f.gate(&case);
        assert_eq!(
            code, 1,
            "expected a refusal containing {whys:?}\nstdout: {stdout}\nstderr: {stderr}"
        );
        // The reasons a class is human are in the report on stdout; the refusal is on stderr.
        let said = format!("{stdout}{stderr}");
        for why in whys {
            assert!(
                said.contains(why),
                "no refusal containing {why:?}\nstdout: {stdout}\nstderr: {stderr}"
            );
        }
        assert!(!stdout.contains("every rule holds"), "{stdout}");
    }

    #[test]
    fn two_allowlisted_workflow_principals_satisfy_the_review_requirement() {
        let f = Fixture::new();
        let (code, stdout, stderr) = f.gate(&scenario(vec![f.std(), f.spec()]));
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains("every rule holds"), "{stdout}");
        assert!(stdout.contains("svc_std standards"), "{stdout}");
        assert!(stdout.contains("svc_spec spec-security"), "{stdout}");
        // One axis alone is not two reviews.
        refused(&f, scenario(vec![f.std()]), "no pass Spec/security verdict");
        refused(&f, scenario(vec![f.spec()]), "no pass Standards verdict");
    }

    #[test]
    fn a_workflow_row_counts_only_for_an_allowlisted_principal_and_its_own_kind() {
        let f = Fixture::new();
        // identity_source is a claim, not a credential.
        let mut forged = f.spec();
        forged["reviewer_id"] = json!("svc_other");
        forged["reviewer"] = json!("svc_other");
        refused(
            &f,
            scenario(vec![f.std(), forged]),
            "svc_other is not in docs/roles/review-workflow-principals.toml",
        );
        // A listed principal filing the other axis, or a third kind, is refused.
        for (principal, kind) in [
            ("svc_std", "spec-security"),
            ("svc_spec", "standards"),
            ("svc_std", "combined"),
        ] {
            let mut row = f.wrow("20261103-000300-x.md", "20261103-000300", principal, kind);
            if kind == "combined" {
                row["name"] = json!("20261103-000300-c.md");
            }
            let rows = if principal == "svc_std" {
                vec![row, f.spec()]
            } else {
                vec![f.std(), row]
            };
            refused(
                &f,
                scenario(rows),
                &format!("principal {principal} may file only"),
            );
        }
        // A listed principal's row that does not say it is a workflow row is not counted by login.
        let disguised = f.owner(
            "20261103-000100-std.md",
            "20261103-000100",
            "svc_std",
            "alice",
            "standards",
        );
        refused(
            &f,
            scenario(vec![disguised, f.spec()]),
            "is not a workflow-issued row",
        );
        // A workflow row for another repository.
        let mut elsewhere = f.std();
        elsewhere["repo"] = json!("favcrm/other");
        refused(&f, scenario(vec![elsewhere, f.spec()]), "names repo");
    }

    #[test]
    fn a_missing_or_malformed_allowlist_at_mains_tip_refuses_every_workflow_row() {
        let f = Fixture::new();
        let rows = || vec![f.std(), f.spec()];
        let cannot = "cannot read docs/roles/review-workflow-principals.toml at main's live tip";
        let mut missing = f.base_files();
        missing.as_object_mut().unwrap().remove(ALLOWLIST);
        let mut case = scenario(rows());
        case.base_files = Some(missing);
        refused(&f, case, cannot);
        for bad in [
            "[principals.svc_std\nkind =".to_string(),
            "title = \"no principals\"\n".to_string(),
            GOOD_ALLOWLIST.replace("kind = \"standards\"", "kind = \"combined\""),
            GOOD_ALLOWLIST.replace("kind = \"spec-security\"", "kind = \"browser-qa\""),
            GOOD_ALLOWLIST.replace(".github/workflows/review-spec.yml", "scripts/evil.yml"),
            GOOD_ALLOWLIST.replace("workflow = \".github/workflows/review-spec.yml\"\n", ""),
            String::new(),
        ] {
            let mut files = f.base_files();
            files[ALLOWLIST] = json!(bad);
            let mut case = scenario(rows());
            case.base_files = Some(files);
            refused(&f, case, cannot);
        }
        // The allowlist is main's: a PR that adds its own reviewer, or a whole
        // allowlist, changes nothing about what counts.
        let mut absent_at_base = f.base_files();
        absent_at_base.as_object_mut().unwrap().remove(ALLOWLIST);
        let mut case = scenario(rows());
        case.base_files = Some(absent_at_base);
        case.head_files = Some(json!({ALLOWLIST: GOOD_ALLOWLIST}));
        refused(&f, case, cannot);
        let mut narrow = f.base_files();
        narrow[ALLOWLIST] = json!(GOOD_ALLOWLIST
            .split("[principals.svc_spec]")
            .next()
            .unwrap());
        let mut case = scenario(rows());
        case.base_files = Some(narrow);
        case.head_files = Some(json!({ALLOWLIST: GOOD_ALLOWLIST}));
        refused(
            &f,
            case,
            "svc_spec is not in docs/roles/review-workflow-principals.toml",
        );
    }

    #[test]
    fn a_workflow_revise_refuses_even_when_a_later_pass_follows() {
        let f = Fixture::new();
        let mut revise = f.spec();
        revise["outcome"] = json!("revise");
        revise["section_outcome"] = json!("revise");
        revise["name"] = json!("20261103-000150-spec.md");
        revise["stamp"] = json!("20261103-000150");
        refused(&f, scenario(vec![f.std(), revise.clone()]), "is not a pass");
        // The later pass (spec() is stamped after the revise) does not erase it.
        refused(
            &f,
            scenario(vec![f.std(), revise, f.spec()]),
            "is not a pass",
        );
    }

    #[test]
    fn a_workflow_run_of_a_workflow_that_is_not_mains_refuses_by_name() {
        let f = Fixture::new();
        let mut old = f.std();
        old["workflow_sha"] = json!(f.differs);
        refused_all(
            &f,
            scenario(vec![old, f.spec()]),
            &[
                &format!("{STD_FILE} at workflow commit"),
                "differs from main's live tip",
            ],
        );
        // A different commit carrying the same file as main's tip still counts.
        let mut same = f.std();
        same["workflow_sha"] = json!(f.tip_same);
        let (code, stdout, stderr) = f.gate(&scenario(vec![same, f.spec()]));
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        // The comparison is with main, not with the PR's own head: a PR that rewrites
        // its reviewer cannot be reviewed by the rewrite.
        let mut own = f.std();
        own["workflow_sha"] = json!(f.self_edit_head);
        let mut case = scenario(vec![own, f.spec()]);
        case.head = Some(f.self_edit_head.clone());
        refused(&f, case, &format!("{STD_FILE} at workflow commit"));
    }

    #[test]
    fn a_workflow_commit_the_gate_cannot_read_refuses_rather_than_skips() {
        let f = Fixture::new();
        for (sha, why) in [
            (json!("b".repeat(40)), "cannot be compared"),
            (json!("abc1234"), "is not a full commit SHA"),
            (json!(null), "is not a full commit SHA"),
            (json!(f.no_file), "is not a regular file"),
        ] {
            let mut row = f.std();
            row["workflow_sha"] = sha.clone();
            refused(&f, scenario(vec![row, f.spec()]), why);
        }
        let mut row = f.spec();
        row.as_object_mut().unwrap().remove("workflow_sha");
        refused(&f, scenario(vec![f.std(), row]), "is not a full commit SHA");
    }

    #[test]
    fn two_good_rows_do_not_count_once_the_prs_base_is_not_main() {
        let f = Fixture::new();
        let mut case = scenario(vec![f.std(), f.spec()]);
        case.base_ref = "staging".into();
        refused(&f, case, "the PR's base is now 'staging', not main");
    }

    #[test]
    fn a_workflow_row_that_records_no_base_commit_refuses() {
        let f = Fixture::new();
        for broken in [None, Some(""), Some("main"), Some("e5e5")] {
            let mut row = f.std();
            match broken {
                None => {
                    row.as_object_mut().unwrap().remove("base_sha");
                }
                Some(v) => row["base_sha"] = json!(v),
            }
            refused(&f, scenario(vec![row, f.spec()]), "records no base_sha");
        }
    }

    #[test]
    fn a_ui_change_still_needs_browser_qa_after_two_workflow_passes() {
        let f = Fixture::new();
        let mut case = scenario(vec![f.std(), f.spec()]);
        case.head = Some(f.touch("ui/app.txt"));
        refused(&f, case, "a ui/ change needs a pass Browser QA verdict");
    }

    #[test]
    fn owner_attested_rows_and_the_author_refusal_are_unchanged() {
        let f = Fixture::new();
        let alice = || {
            f.owner(
                "20261103-000100-a.md",
                "20261103-000100",
                "usr_alice",
                "alice",
                "standards",
            )
        };
        let bob = || {
            f.owner(
                "20261103-000200-b.md",
                "20261103-000200",
                "usr_bob",
                "bob",
                "spec-security",
            )
        };
        let (code, stdout, stderr) = f.gate(&scenario(vec![alice(), bob()]));
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains("alice standards"), "{stdout}");
        // CAD-2049: the author's own attested row is refused...
        let author = f.owner(
            "20261103-000200-b.md",
            "20261103-000200",
            "usr_author",
            "authorlogin",
            "spec-security",
        );
        refused(&f, scenario(vec![alice(), author]), "is not independent");
        // ...and so is a row that cannot be proven to be someone else's.
        let mut case = scenario(vec![alice(), bob()]);
        case.identities = Some(json!([{"github_login": "alice", "subject_id": "usr_alice"},
                                      {"github_login": "bob", "subject_id": "usr_bob"}]));
        refused(&f, case, "has no attested principal");
        // The allowlist does not weaken it: a workflow pass beside the author's row still refuses.
        let author = f.owner(
            "20261103-000100-a.md",
            "20261103-000100",
            "usr_author",
            "authorlogin",
            "standards",
        );
        refused(&f, scenario(vec![author, f.spec()]), "is not independent");
    }

    #[test]
    fn the_allowlist_and_workflow_are_read_at_mains_live_tip() {
        let f = Fixture::new();
        // A principal removed on main since the PR's base no longer counts, on a PR behind main.
        let mut revoked = f.base_files();
        revoked[ALLOWLIST] = json!(GOOD_ALLOWLIST
            .split("[principals.svc_spec]")
            .next()
            .unwrap());
        let mut case = scenario(vec![f.std(), f.spec()]);
        case.tip_files = Some(revoked);
        refused(
            &f,
            case,
            "svc_spec is not in docs/roles/review-workflow-principals.toml",
        );
        // A fresh run from the tip counts, though the PR's base still has the older workflow.
        let (code, stdout, stderr) = f.gate(&scenario(vec![f.std(), f.spec()]));
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        // A run of the workflow as it was at the PR's base no longer matches main's.
        let mut stale = f.std();
        stale["workflow_sha"] = json!(f.base);
        refused(
            &f,
            scenario(vec![stale, f.spec()]),
            "differs from main's live tip",
        );
    }

    const OPERATOR: &str = "cannot stand in for the operator on trigger 1/3 or two-review path";
    const NO_APPROVAL: &str = "no operator approval recorded";

    #[test]
    fn a_workflow_review_never_replaces_the_operator_on_the_real_trust_boundary_lists() {
        let f = Fixture::new();
        // Trigger 1, the two-review exclusions, and the gate's own sources, on the real lists.
        for path in [
            "src/adapter/mod.rs",
            "src/confine.rs",
            "src/runner.rs",
            "agents/x.md",
            "src/remote_cli.rs",
            "src/secret/x.rs",
        ] {
            let mut case = scenario(vec![f.std(), f.spec()]);
            case.head = Some(f.touch(path));
            refused_all(&f, case, &[OPERATOR, NO_APPROVAL]);
        }
        // The gate's own sources are human whatever the lists say.
        let mut case = scenario(vec![f.std(), f.spec()]);
        case.head = Some(f.touch("src/delivery/hosted_gate.rs"));
        refused(&f, case, NO_APPROVAL);
        // An eligible path still rests on two workflow reviews alone.
        let mut case = scenario(vec![f.std(), f.spec()]);
        case.head = Some(f.touch("src/wiki/x.rs"));
        let (code, stdout, stderr) = f.gate(&case);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains("class: auto"), "{stdout}");
    }

    #[test]
    fn the_gates_own_directory_is_human_even_when_the_base_lists_forget_it() {
        let f = Fixture::new();
        let mut files = f.base_files();
        let real = files[RISK_PATHS].as_str().unwrap().to_string();
        assert!(
            real.contains("\"src/delivery/**\", "),
            "the real list names it"
        );
        files[RISK_PATHS] = json!(real.replace("\"src/delivery/**\", ", ""));
        let mut case = scenario(vec![f.std(), f.spec()]);
        case.head = Some(f.touch("src/delivery/hosted_gate.rs"));
        case.base_files = Some(files);
        refused(&f, case, NO_APPROVAL);
    }

    fn risk_toml(trigger1: &[&str], trigger7: &[&str]) -> String {
        format!(
            "[schema]\npaths = []\n[trigger1]\npaths = {trigger1:?}\n[trigger3]\npaths = []\n\
             [trigger4]\npaths = []\n[trigger6]\npaths = []\n[trigger7]\npaths = {trigger7:?}\n"
        )
    }

    fn one_review_toml(exclude: &[&str]) -> String {
        format!("one_review_include = [\"src/**\"]\none_review_exclude = {exclude:?}\n")
    }

    fn lists(f: &Fixture, risk: String, one: String) -> Value {
        let mut files = f.base_files();
        files[RISK_PATHS] = json!(risk);
        files[ONE_REVIEW] = json!(one);
        files
    }

    #[test]
    fn a_path_listed_at_the_base_or_at_mains_tip_needs_the_operator_and_an_unreadable_tip_refuses()
    {
        let f = Fixture::new();
        let head = f.touch("src/zzz/x.rs");
        let glob = ["src/zzz/**"];
        // The human list, [trigger1] and one_review_exclude: (risk, one-review) with the path listed.
        let variants = [
            ("human list", risk_toml(&[], &glob), one_review_toml(&[])),
            ("trigger1", risk_toml(&glob, &[]), one_review_toml(&[])),
            (
                "one_review_exclude",
                risk_toml(&[], &[]),
                one_review_toml(&glob),
            ),
        ];
        let case = |base: Value, tip: Value| {
            let mut case = scenario(vec![f.std(), f.spec()]);
            case.head = Some(head.clone());
            case.base_files = Some(base);
            case.tip_files = Some(tip);
            case
        };
        let none = || lists(&f, risk_toml(&[], &[]), one_review_toml(&[]));
        // Control: listed nowhere, the same rows pass.
        let (code, stdout, stderr) = f.gate(&case(none(), none()));
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        for (name, risk, one) in variants {
            let listed = || lists(&f, risk.clone(), one.clone());
            // A tightening on main reaches a PR whose base predates it.
            let (code, stdout, stderr) = f.gate(&case(none(), listed()));
            assert_eq!(
                code, 1,
                "{name} at the tip\nstdout: {stdout}\nstderr: {stderr}"
            );
            assert!(stderr.contains(NO_APPROVAL), "{name} at the tip\n{stderr}");
            // The base's own list still applies when main's tip no longer has it.
            let (code, stdout, stderr) = f.gate(&case(listed(), none()));
            assert_eq!(
                code, 1,
                "{name} at the base\nstdout: {stdout}\nstderr: {stderr}"
            );
            assert!(stderr.contains(NO_APPROVAL), "{name} at the base\n{stderr}");
        }
        // An unreadable tip copy of either list refuses.
        for (file, why) in [
            (
                RISK_PATHS,
                "cannot read docs/roles/risk-paths.toml at main's live tip",
            ),
            (
                ONE_REVIEW,
                "cannot read the trigger 1/3 and two-review lists",
            ),
        ] {
            let mut tip = none();
            tip.as_object_mut().unwrap().remove(file);
            refused(&f, case(none(), tip), why);
        }
    }

    #[test]
    fn an_owner_row_naming_a_reserved_principal_counts_for_nothing_whatever_its_neighbours() {
        let f = Fixture::new();
        let rows = |std_id: &str, spec_id: &str| {
            vec![
                f.owner(
                    "20261103-000100-a.md",
                    "20261103-000100",
                    std_id,
                    "alice",
                    "standards",
                ),
                f.owner(
                    "20261103-000200-b.md",
                    "20261103-000200",
                    spec_id,
                    "bob",
                    "spec-security",
                ),
            ]
        };
        let identities = Some(json!([
            {"github_login": "authorlogin", "subject_id": "usr_author"},
            {"github_login": "alice", "subject_id": "usr_alice"},
            {"github_login": "bob", "subject_id": "usr_bob"}
        ]));
        // Two owner-attested rows under the workflow principals' names, with logins and no workflow row.
        let mut case = scenario(rows("svc_std", "svc_spec"));
        case.identities = identities.clone();
        refused(&f, case, "names workflow principal");
        let real =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(ALLOWLIST)).unwrap();
        let mut files = f.base_files();
        files[ALLOWLIST] = json!(real);
        let mut case = scenario(rows("svc_review_standards", "svc_review_spec"));
        case.base_files = Some(files);
        case.identities = identities.clone();
        refused(&f, case, "names workflow principal");
        // Ordinary owner rows count as before, with the allowlist readable or not.
        let mut unreadable = f.base_files();
        unreadable.as_object_mut().unwrap().remove(ALLOWLIST);
        for files in [f.base_files(), unreadable.clone()] {
            let mut case = scenario(rows("usr_alice", "usr_bob"));
            case.base_files = Some(files);
            let (code, stdout, stderr) = f.gate(&case);
            assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
            assert!(stdout.contains("alice standards"), "{stdout}");
        }
        // A service principal cannot be told from a reserved one while the list is unreadable.
        let mut case = scenario(rows("svc_other", "usr_bob"));
        case.base_files = Some(unreadable);
        case.identities = identities;
        refused(&f, case, "cannot be read to tell whether it is reserved");
    }

    #[test]
    fn the_exception_path_refuses_a_workflow_row_that_does_not_hold_and_leaves_one_that_does() {
        let f = Fixture::new();
        let exception = || {
            [
                "--exception",
                "--advisory",
                "standards=https://example.invalid/a",
                "--advisory",
                "spec-security=https://example.invalid/b",
            ]
            .map(String::from)
            .to_vec()
        };
        // A principal the allowlist does not list, and a run of a superseded workflow.
        let mut unlisted = f.std();
        unlisted["reviewer_id"] = json!("svc_other");
        unlisted["reviewer"] = json!("svc_other");
        let mut stale = f.std();
        stale["workflow_sha"] = json!(f.base);
        for (row, why) in [
            (
                unlisted,
                "principal svc_other is not in docs/roles/review-workflow-principals.toml",
            ),
            (stale, "differs from main's live tip"),
        ] {
            let mut case = scenario(vec![row, f.spec()]);
            case.extra_args = exception();
            refused_all(
                &f,
                case,
                &["workflow verdict 20261103-000100-std.md cannot count", why],
            );
        }
        // Holding rows are not refused as rows: the exception's own gates (here the
        // operator's approval) are what remain, and the report names the exception.
        let mut case = scenario(vec![f.std(), f.spec()]);
        case.extra_args = exception();
        let (_, stdout, stderr) = f.gate(&case);
        assert!(!stderr.contains("cannot count"), "{stderr}");
        assert!(stdout.contains("review EXCEPTION until"), "{stdout}");
        assert!(stdout.contains("svc_std standards"), "{stdout}");
    }

    #[test]
    fn a_symlinked_workflow_never_counts_even_at_the_same_blob() {
        let f = Fixture::with_symlinked_workflow();
        let (code, stdout, stderr) = f.gate(&scenario(vec![f.std(), f.spec()]));
        assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stderr.contains("workflow verdict 20261103-000100-std.md cannot count"),
            "{stderr}"
        );
        assert!(!stdout.contains("every rule holds"), "{stdout}");
    }

    #[test]
    fn the_allowlist_in_the_repository_is_the_tenants_map() {
        // The reviewed map in the tenant (agenticos-v2 PR 1689, review-workflows.ts).
        let want = [
            ("svc_review_standards", "standards", STD_FILE),
            ("svc_review_spec", "spec-security", SPEC_FILE),
        ];
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(ALLOWLIST);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is not in the repository: {e}", path.display()));
        let doc: toml::Table = text.parse().unwrap();
        let table = doc["principals"].as_table().unwrap();
        assert_eq!(table.len(), want.len(), "{table:?}");
        for (principal, kind, workflow) in want {
            let entry = table[principal].as_table().unwrap();
            assert_eq!(entry["kind"].as_str(), Some(kind), "{principal}");
            assert_eq!(entry["workflow"].as_str(), Some(workflow), "{principal}");
        }
        // And the gate counts exactly those principals when it reads that file.
        let f = Fixture::new();
        let mut files = f.base_files();
        files[ALLOWLIST] = json!(text);
        let rows = vec![
            f.wrow(
                "20261103-000100-std.md",
                "20261103-000100",
                "svc_review_standards",
                "standards",
            ),
            f.wrow(
                "20261103-000200-spec.md",
                "20261103-000200",
                "svc_review_spec",
                "spec-security",
            ),
        ];
        let mut case = scenario(rows);
        case.base_files = Some(files);
        let (code, stdout, stderr) = f.gate(&case);
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    }
}
