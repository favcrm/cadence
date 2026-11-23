#![cfg(feature = "test-seam")]

use super::*;
use crate::issue::{write, Pm};
use crate::test_seam::{scoped, Asserted, Seam};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};

struct RunningDaemon {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for RunningDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}

#[test]
fn cad1855_job_inspection_preserves_state_when_canonical_spec_is_unavailable() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let manifest = repo.join("docs/cadence/project-context.yaml");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    std::fs::write(
        manifest,
        "schema: 1\nproject: inspect\nagent_delivery: true\ndocuments: []\n",
    )
    .unwrap();
    for args in [
        &["init", "--quiet", "--initial-branch=main"][..],
        &["add", "."][..],
        &[
            "-c",
            "user.name=CAD-1855 fixture",
            "-c",
            "user.email=cad1855@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "Opt in the isolated inspection project",
        ][..],
    ] {
        let output = crate::reaper::output(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "fixture git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let pm_dir = root.path().join("pm");
    let pm = Pm::init(&pm_dir).unwrap();
    write::project_add(
        &pm,
        "inspect",
        "I1855",
        &[repo.display().to_string()],
        &[],
        &[],
        None,
    )
    .unwrap();
    write::new_issue(
        &pm,
        &repo,
        Some("inspect"),
        "Inspect the existing task",
        Some("P2"),
        None,
        &[],
        None,
        None,
        &[],
        Some("I1855-1"),
        Some("Keep the existing task visible.\n\n## Acceptance\n\n- [ ] Preserve identifiers.\n"),
        "cad1855-fixture",
    )
    .unwrap();
    let spec = write::issue_dir(&pm, "I1855-1").unwrap().1.join("issue.md");
    let original = std::fs::read(&spec).unwrap();
    let env = crate::adapter::ProviderEnv::refusing_providers();
    env.set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let opts = ServeOptions {
        provider_env: env,
        stop: Some(Arc::clone(&stop)),
        test_seam: true,
        slots: Some(Default::default()),
        lease: Some(Default::default()),
        auto_stop: Some(AutoStopSetting::off()),
        agent_gc: Some(Default::default()),
        report_router: Some(0),
        checkup: Some(0),
        ..Default::default()
    };
    let state = root.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let shared = Shared::new(&state, &opts).unwrap();
    let rpc = |method: &str, params: Value| {
        scoped(Asserted::Operator, || {
            shared.dispatch(method, &params, std::process::id())
        })
    };
    rpc(
        "agent_register",
        json!({"alias": "pm", "provider": "inbox", "endpoint_kind": "inbox", "role": "pm"}),
    )
    .unwrap();
    let created = rpc(
        "job_new",
        json!({
            "job": "inspect-job", "pm": "pm", "issue": "I1855-1",
            "spec": spec, "spec_sha256": format!("{:x}", Sha256::digest(&original)),
            "task_title": "Keep this task inspectable",
            "task_acceptance": "Preserve identifiers.",
        }),
    )
    .unwrap();
    assert_ne!(
        created["job"]["spec_sha256"],
        format!("{:x}", Sha256::digest(&original)),
        "the opted-in native job must pin the effective body, not mutable frontmatter"
    );
    assert_eq!(created["job"]["id"], "inspect-job");
    let daemon_state = state.clone();
    let _daemon = RunningDaemon {
        stop,
        thread: Some(std::thread::spawn(move || {
            crate::daemon::serve_with(&daemon_state, opts).unwrap();
        })),
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while crate::client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(2)).is_err()
        || Seam::token_at(&state).is_none()
    {
        assert!(Instant::now() < deadline, "isolated daemon did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    let rpc = |method: &str, params: Value| {
        scoped(Asserted::Operator, || {
            crate::client::rpc(&state, method, params)
        })
    };
    let job_before = shared.store.job("inspect-job").unwrap().to_json();
    let task_before = shared.store.task("inspect-job-t1").unwrap().to_json();
    let check_view = |view: &Value| {
        assert_eq!(view["job"]["id"], job_before["id"]);
        assert_eq!(view["job"]["state"], job_before["state"]);
        assert_eq!(view["job"]["spec_sha256"], job_before["spec_sha256"]);
        assert_eq!(view["job"]["spec_path"], job_before["spec_path"]);
        assert_eq!(view["job"]["tasks"][0]["id"], task_before["id"]);
        assert_eq!(view["job"]["tasks"][0]["state"], task_before["state"]);
        assert_eq!(view["job"]["tasks"][0]["revision"], task_before["revision"]);
        assert_eq!(
            shared.store.job("inspect-job").unwrap().to_json(),
            job_before
        );
        assert_eq!(
            shared.store.task("inspect-job-t1").unwrap().to_json(),
            task_before
        );
    };
    let show = || rpc("job_show", json!({"job": "inspect-job"}));
    let valid = show().unwrap();
    check_view(&valid);
    assert!(valid["job"]["attention"].is_null());

    let original_text = std::str::from_utf8(&original).unwrap();
    let metadata_only = original_text.replacen("---\n", "---\n# Tracker metadata only.\n", 1);
    assert_ne!(metadata_only, original_text);
    std::fs::write(&spec, metadata_only).unwrap();
    let unchanged_body = show().unwrap();
    check_view(&unchanged_body);
    assert!(unchanged_body["job"]["attention"].is_null());

    for (label, bad, source_error) in [
        (
            "over-cap",
            vec![b'x'; crate::context_pack::MAX_COMBINED_BYTES + 1],
            true,
        ),
        (
            "malformed frontmatter",
            b"---\nstatus: [broken\n---\nRequest.\n".to_vec(),
            false,
        ),
        ("invalid UTF-8", vec![0xff], false),
    ] {
        std::fs::write(&spec, bad).unwrap();
        let view = show().unwrap_or_else(|error| {
            panic!("{label} spec hid the existing job/task behind a top-level error: {error}")
        });
        check_view(&view);
        let attention = view["job"]["attention"].as_str().unwrap();
        assert!(
            attention.contains("unavailable") || attention.contains("spec changed"),
            "{label}: {attention}"
        );
        if source_error {
            assert!(attention.contains("unavailable"), "{attention}");
            assert!(attention.contains(spec.to_str().unwrap()), "{attention}");
        }
    }

    std::fs::remove_file(&spec).unwrap();
    let missing = show().unwrap();
    check_view(&missing);
    assert!(missing["job"]["attention"]
        .as_str()
        .unwrap()
        .contains("unreadable"));

    let mut changed = original;
    changed.extend_from_slice(b"\nMaterially different work.\n");
    std::fs::write(&spec, changed).unwrap();
    let drift = show().unwrap();
    check_view(&drift);
    assert!(drift["job"]["attention"]
        .as_str()
        .unwrap()
        .contains("spec changed"));
}
