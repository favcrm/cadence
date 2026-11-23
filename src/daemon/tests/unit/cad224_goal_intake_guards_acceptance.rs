use super::*;
use crate::issue::{board, plan, write, Pm};
use crate::secret::Allowlist;
use std::cell::Cell;

fn git(repo: &Path, args: &[&str]) -> String {
    let mut command = std::process::Command::new("git");
    command.arg("-C").arg(repo).args(args);
    let output = crate::reaper::output(&mut command).unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn intake_grant_and_stranded_revision_are_refused_without_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("source");
    std::fs::create_dir_all(repo.join("docs/cadence")).unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.name", "Acceptance"]);
    git(
        &repo,
        &["config", "user.email", "acceptance@example.invalid"],
    );
    std::fs::write(repo.join("docs/scope.md"), "Original scope\n").unwrap();
    std::fs::write(repo.join("src/module.rs"), "// tracked module\n").unwrap();
    std::fs::write(
        repo.join("docs/cadence/project-context.yaml"),
        "schema: 1\nproject: cadence\ndocuments:\n  - id: scope\n    kind: scope\n    path: docs/scope.md\n    title: Scope\n    required: true\n    roles: []\n",
    )
    .unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "Initial source"]);

    let pm = Pm::init(&dir.path().join("pm")).unwrap();
    write::project_add(
        &pm,
        "cadence",
        "CAD",
        &[repo.to_string_lossy().into_owned()],
        &[],
        &[],
        None,
    )
    .unwrap();
    let draft = r#"---
title: Deliver linked outcomes
goal: Deliver both outcomes
intake:
  constraints: [Preserve the integration]
  success_criteria: [Both outcomes are available]
  unknowns: []
---

## Prerequisite
sources: scope
paths: src/module.rs

### Acceptance
- [ ] Prerequisite is available

## Dependent
sources: scope
paths: src/module.rs
depends_on: 1

### Acceptance
- [ ] Dependent outcome is available
"#;
    let doc = plan::parse_plan(draft).unwrap();
    let head = git(&pm.dir, &["rev-parse", "HEAD"]);
    let status = git(&pm.dir, &["status", "--porcelain", "--untracked-files=all"]);
    let avoid_called = Cell::new(false);
    let intent_called = Cell::new(false);
    let refusal = write::create_granted_plan(
        &pm,
        "cadence",
        &doc,
        None,
        "acceptance",
        "grant",
        None,
        None,
        true,
        &|| {
            avoid_called.set(true);
            Ok(Default::default())
        },
        &mut |_| {
            intent_called.set(true);
            Ok(())
        },
    )
    .unwrap_err();
    assert!(
        refusal
            .to_string()
            .contains("goal intake requires a separate operator decision"),
        "{refusal:?}"
    );
    assert!(!avoid_called.get());
    assert!(!intent_called.get());
    assert_eq!(git(&pm.dir, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(&pm.dir, &["status", "--porcelain", "--untracked-files=all"]),
        status
    );
    assert!(board::load_all(&pm.dir, Some("cadence"))
        .unwrap()
        .is_empty());

    let allow = Allowlist::default();
    let original = plan::propose(&pm, "cadence", draft, None, &allow, "acceptance", &|| {
        Ok(Default::default())
    })
    .unwrap();
    let epic = original["epic"].as_str().unwrap();
    let prerequisite = original["tickets"][0].as_str().unwrap();
    let dependent = original["tickets"][1].as_str().unwrap();
    write::decide_plan(&pm, epic, true, "operator", None).unwrap();
    let before = board::find_issue(&pm.dir, dependent).unwrap();
    assert!(before.front.blocked_by.iter().any(|id| id == prerequisite));
    assert_ne!(
        board::find_issue(&pm.dir, prerequisite)
            .unwrap()
            .front
            .status,
        "done"
    );

    let revision = format!(
        r#"---
title: Retain dependent outcome
goal: Deliver the dependent outcome
intake:
  constraints: [Preserve the integration]
  success_criteria: [Dependent outcome is available]
  unknowns: []
  revises: {epic}
---

## Dependent
sources: scope
paths: src/module.rs
reuse: {dependent}

### Acceptance
- [ ] Dependent outcome is available
"#
    );
    let revised = plan::propose(
        &pm,
        "cadence",
        &revision,
        None,
        &allow,
        "acceptance",
        &|| Ok(Default::default()),
    )
    .unwrap();
    let revised_epic = revised["epic"].as_str().unwrap();
    let paths: Vec<_> = [epic, prerequisite, dependent, revised_epic]
        .iter()
        .map(|id| pm.dir.join("cadence").join(id).join("issue.md"))
        .collect();
    let files: Vec<_> = paths
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect();
    let head = git(&pm.dir, &["rev-parse", "HEAD"]);
    let status = git(&pm.dir, &["status", "--porcelain", "--untracked-files=all"]);
    let refusal = write::decide_plan(&pm, revised_epic, true, "operator", None).unwrap_err();
    assert_eq!(
        refusal.code(),
        Some("plan_revision_strands_dependency"),
        "{refusal:?}"
    );
    assert!(refusal.to_string().contains(prerequisite), "{refusal:?}");
    assert_eq!(git(&pm.dir, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(&pm.dir, &["status", "--porcelain", "--untracked-files=all"]),
        status
    );
    for (path, expected) in paths.iter().zip(files) {
        assert_eq!(std::fs::read(path).unwrap(), expected, "{}", path.display());
    }
}

struct IntakeFixture {
    _dir: tempfile::TempDir,
    repo: std::path::PathBuf,
    pm: Pm,
    old_base: String,
}

impl IntakeFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("source");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.name", "Acceptance"]);
        git(
            &repo,
            &["config", "user.email", "acceptance@example.invalid"],
        );
        std::fs::write(repo.join("README"), "old base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "Old base"]);
        let old_base = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
        std::fs::create_dir_all(repo.join("docs/cadence")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("docs/scope.md"), "Approved scope\n").unwrap();
        std::fs::write(repo.join("src/module.rs"), "// tracked module\n").unwrap();
        std::fs::write(
            repo.join("docs/cadence/project-context.yaml"),
            "schema: 1\nproject: cadence\ndocuments:\n  - id: scope\n    kind: scope\n    path: docs/scope.md\n    title: Scope\n    required: true\n    roles: []\n",
        )
        .unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "Add approved scope and module"]);
        let pm = Pm::init(&dir.path().join("pm")).unwrap();
        write::project_add(
            &pm,
            "cadence",
            "CAD",
            &[repo.to_string_lossy().into_owned()],
            &[],
            &[],
            None,
        )
        .unwrap();
        Self {
            _dir: dir,
            repo,
            pm,
            old_base,
        }
    }

    fn propose(&self, draft: &str) -> serde_json::Value {
        plan::propose(
            &self.pm,
            "cadence",
            draft,
            None,
            &Allowlist::default(),
            "acceptance",
            &|| Ok(Default::default()),
        )
        .unwrap()
    }

    fn approve(&self, epic: &str) {
        write::decide_plan(&self.pm, epic, true, "operator", None).unwrap();
    }

    fn start(&self, issue: &str, base: Option<String>) -> crate::error::Result<serde_json::Value> {
        crate::issue::start::run(
            &self.pm,
            issue,
            &crate::issue::start::StartArgs {
                repo: Some(self.repo.clone()),
                name: None,
                base,
                owner: None,
                job: None,
                by: Some("acceptance".into()),
                take_over: None,
            },
            "acceptance",
            self._dir.path(),
        )
    }

    fn snapshot(&self, issue: &str) -> (String, Vec<u8>, String) {
        (
            git(&self.pm.dir, &["rev-parse", "HEAD"]),
            std::fs::read(self.pm.dir.join("cadence").join(issue).join("issue.md")).unwrap(),
            git(&self.repo, &["worktree", "list", "--porcelain"]),
        )
    }
}

const LINKED_INTAKE: &str = r#"---
title: Linked outcomes
goal: Deliver both outcomes
intake:
  constraints: [Preserve integration]
  success_criteria: [Both outcomes are available]
  unknowns: []
---

## Prerequisite
sources: scope
paths: src/module.rs

### Acceptance
- [ ] Prerequisite is available

## Dependent
sources: scope
paths: src/module.rs
depends_on: 1

### Acceptance
- [ ] Dependent outcome is available
"#;

#[test]
fn intake_unapproved_start_refuses_before_resolving_invalid_base() {
    let fx = IntakeFixture::new();
    let proposal = fx.propose(LINKED_INTAKE);
    let issue = proposal["tickets"][0].as_str().unwrap();
    let before = fx.snapshot(issue);
    let refusal = fx
        .start(issue, Some("missing-base-for-acceptance".into()))
        .unwrap_err();
    assert_eq!(refusal.code(), Some("plan_not_approved"), "{refusal:?}");
    assert!(refusal.to_string().contains("proposed"), "{refusal:?}");
    assert_eq!(fx.snapshot(issue), before);

    // The real plain-dispatch preflight still has to refuse the proposed
    // plan before attempting to resolve the deliberately nonexistent base.
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    let socket = crate::client::socket_path(fx._dir.path());
    let listener = UnixListener::bind(&socket).unwrap();
    let daemon = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            let response = match request["method"].as_str() {
                Some("stop") => break,
                Some("agent_show") => json!({"agent": {
                    "alias": "worker", "state": "idle", "enabled": true,
                    "provider": "fake", "endpoint_kind": "fake"
                }}),
                Some("job_list") => json!({"jobs": []}),
                other => panic!("unexpected preflight RPC {other:?}"),
            };
            writeln!(stream, "{}", json!({"ok": true, "result": response})).unwrap();
        }
    });
    let args = crate::issue::dispatch::DispatchArgs {
        to: "worker".into(),
        note: None,
        name: None,
        base: Some("missing-base-for-acceptance".into()),
        repo: Some(fx.repo.clone()),
        reply_to: Some("dispatcher".into()),
        summary: None,
        job_spec: None,
        no_lessons: false,
        force: false,
        take_over: None,
    };
    let result = crate::issue::dispatch::run(
        &fx.pm,
        issue,
        &args,
        "acceptance",
        fx._dir.path(),
        None,
        crate::issue::dispatch::DispatchOrigin::Explicit,
    );
    let mut stop = UnixStream::connect(&socket).unwrap();
    writeln!(stop, "{}", json!({"method": "stop"})).unwrap();
    daemon.join().unwrap();
    let refusal = result.unwrap_err();
    assert_eq!(refusal.code(), Some("plan_not_approved"), "{refusal:?}");
    assert!(refusal.to_string().contains("proposed"), "{refusal:?}");
    assert_eq!(fx.snapshot(issue), before);
    assert!(board::find_issue(&fx.pm.dir, issue)
        .unwrap()
        .front
        .claim
        .is_none());
}

#[test]
fn intake_native_start_refuses_unready_dependency_and_old_base_without_claim() {
    let fx = IntakeFixture::new();
    let proposal = fx.propose(LINKED_INTAKE);
    let epic = proposal["epic"].as_str().unwrap();
    let prerequisite = proposal["tickets"][0].as_str().unwrap();
    let dependent = proposal["tickets"][1].as_str().unwrap();
    fx.approve(epic);

    let before = fx.snapshot(dependent);
    let refusal = crate::issue::claim::claim(
        &fx.pm,
        dependent,
        Some("acceptance"),
        None,
        None,
        "acceptance",
    )
    .unwrap_err();
    assert_eq!(
        refusal.code(),
        Some("plan_dependency_unready"),
        "{refusal:?}"
    );
    assert!(refusal.to_string().contains(prerequisite), "{refusal:?}");
    assert_eq!(fx.snapshot(dependent), before);
    assert!(board::find_issue(&fx.pm.dir, dependent)
        .unwrap()
        .front
        .claim
        .is_none());

    let before = fx.snapshot(dependent);
    let refusal = fx.start(dependent, None).unwrap_err();
    assert_eq!(
        refusal.code(),
        Some("plan_dependency_unready"),
        "{refusal:?}"
    );
    let message = refusal.to_string();
    assert!(message.contains(prerequisite), "{refusal:?}");
    assert_eq!(fx.snapshot(dependent), before);
    let front = board::find_issue(&fx.pm.dir, dependent).unwrap().front;
    assert_eq!(front.status, "ready");
    assert!(front.claim.is_none());

    let before = fx.snapshot(prerequisite);
    let refusal = fx
        .start(prerequisite, Some(fx.old_base.clone()))
        .unwrap_err();
    assert_eq!(refusal.code(), Some("plan_evidence_stale"), "{refusal:?}");
    assert_eq!(fx.snapshot(prerequisite), before);
    let front = board::find_issue(&fx.pm.dir, prerequisite).unwrap().front;
    assert_eq!(front.status, "ready");
    assert!(front.claim.is_none());

    std::fs::write(fx.repo.join("docs/scope.md"), "Changed scope\n").unwrap();
    git(&fx.repo, &["add", "docs/scope.md"]);
    git(&fx.repo, &["commit", "-qm", "Change approved scope"]);
    let before = fx.snapshot(prerequisite);
    let refusal = crate::issue::claim::claim(
        &fx.pm,
        prerequisite,
        Some("acceptance"),
        None,
        None,
        "acceptance",
    )
    .unwrap_err();
    assert_eq!(refusal.code(), Some("plan_evidence_stale"), "{refusal:?}");
    assert!(refusal.to_string().contains("scope"), "{refusal:?}");
    assert_eq!(fx.snapshot(prerequisite), before);
    assert!(board::find_issue(&fx.pm.dir, prerequisite)
        .unwrap()
        .front
        .claim
        .is_none());

    std::fs::write(fx.repo.join("docs/scope.md"), "Approved scope\n").unwrap();
    git(&fx.repo, &["add", "docs/scope.md"]);
    git(&fx.repo, &["commit", "-qm", "Restore approved scope"]);
    crate::issue::claim::claim(
        &fx.pm,
        prerequisite,
        Some("acceptance"),
        None,
        None,
        "acceptance",
    )
    .unwrap();
    let (_, issue_dir) = write::issue_dir(&fx.pm, prerequisite).unwrap();
    let (mut claimed, body) = write::load_front(&issue_dir).unwrap();
    claimed.claim.as_mut().unwrap().stale = Some("worker offline".into());
    write::save_front(&issue_dir, &claimed, &body).unwrap();
    fx.pm
        .commit(&[issue_dir.join("issue.md")], "Mark claim stale")
        .unwrap();
    std::fs::write(fx.repo.join("docs/scope.md"), "Changed scope again\n").unwrap();
    git(&fx.repo, &["add", "docs/scope.md"]);
    git(
        &fx.repo,
        &["commit", "-qm", "Change scope before claim renewal"],
    );
    let before = fx.snapshot(prerequisite);
    let refusal = crate::issue::claim::claim(
        &fx.pm,
        prerequisite,
        Some("acceptance"),
        None,
        None,
        "acceptance",
    )
    .unwrap_err();
    assert_eq!(refusal.code(), Some("plan_evidence_stale"), "{refusal:?}");
    assert_eq!(fx.snapshot(prerequisite), before);
    let after = board::find_issue(&fx.pm.dir, prerequisite).unwrap().front;
    assert_eq!(
        after
            .claim
            .as_ref()
            .and_then(|claim| claim.stale.as_deref()),
        Some("worker offline")
    );
}

#[test]
fn intake_missing_promised_source_refuses_without_invoking_git_transport() {
    use std::os::unix::fs::PermissionsExt;

    let fx = IntakeFixture::new();
    let proposal = fx.propose(LINKED_INTAKE);
    let epic = proposal["epic"].as_str().unwrap();
    let issue = proposal["tickets"][0].as_str().unwrap();
    fx.approve(epic);

    let remote = fx._dir.path().join("promisor.git");
    std::fs::create_dir(&remote).unwrap();
    git(&remote, &["init", "--bare", "-q"]);
    let marker = fx._dir.path().join("transport-invoked");
    let upload_pack = fx._dir.path().join("record-upload-pack");
    std::fs::write(
        &upload_pack,
        format!(
            "#!/bin/sh\nprintf attempted > '{}'\nexit 1\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&upload_pack, std::fs::Permissions::from_mode(0o700)).unwrap();
    git(
        &fx.repo,
        &[
            "config",
            "remote.origin.url",
            &format!("file://{}", remote.display()),
        ],
    );
    git(&fx.repo, &["config", "remote.origin.promisor", "true"]);
    git(
        &fx.repo,
        &["config", "remote.origin.partialclonefilter", "blob:none"],
    );
    git(
        &fx.repo,
        &[
            "config",
            "remote.origin.uploadpack",
            upload_pack.to_str().unwrap(),
        ],
    );
    git(&fx.repo, &["config", "protocol.file.allow", "always"]);
    let blob = git(&fx.repo, &["rev-parse", "HEAD:docs/scope.md"]);
    let blob = blob.trim();
    let loose = fx
        .repo
        .join(".git/objects")
        .join(&blob[..2])
        .join(&blob[2..]);
    assert!(loose.is_file(), "fixture source blob must be loose");
    std::fs::remove_file(loose).unwrap();

    let before = fx.snapshot(issue);
    let refusal =
        crate::issue::claim::claim(&fx.pm, issue, Some("acceptance"), None, None, "acceptance")
            .unwrap_err();
    assert_eq!(refusal.code(), Some("plan_evidence_stale"), "{refusal:?}");
    assert!(
        !marker.exists(),
        "missing source invoked the promisor transport"
    );
    assert_eq!(fx.snapshot(issue), before);
    assert!(board::find_issue(&fx.pm.dir, issue)
        .unwrap()
        .front
        .claim
        .is_none());
    // Positive control: the configured local file transport really reaches
    // the marker when Git is allowed to fetch; it never contacts a network.
    let mut control = std::process::Command::new("git");
    control
        .arg("-C")
        .arg(&fx.repo)
        .args(["fetch", "origin"])
        .env("GIT_ALLOW_PROTOCOL", "file")
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = crate::reaper::output(&mut control).unwrap();
    assert!(!output.status.success());
    assert!(marker.is_file(), "local transport marker was not wired");
}

#[test]
fn intake_reused_lane_checks_its_old_base_even_with_newer_explicit_base() {
    let fx = IntakeFixture::new();
    let draft = LINKED_INTAKE.replace("paths: src/module.rs", "paths: README");
    let original = fx.propose(&draft);
    let epic = original["epic"].as_str().unwrap();
    let issue = original["tickets"][0].as_str().unwrap();
    fx.approve(epic);

    let started = fx.start(issue, Some(fx.old_base.clone())).unwrap();
    let lane = std::path::PathBuf::from(started["worktree"].as_str().unwrap());
    let lane_branch = started["branch"].as_str().unwrap();
    assert!(lane.join("README").is_file());
    assert!(!lane.join("src/module.rs").exists());
    let (_, dir) = write::issue_dir(&fx.pm, issue).unwrap();
    let (mut front, body) = write::load_front(&dir).unwrap();
    front.paths.push("src/module.rs".into());
    write::save_front(&dir, &front, &body).unwrap();
    fx.pm
        .commit(&[dir.join("issue.md")], "Map module in revised scope")
        .unwrap();
    let revision = format!(
        "---\ntitle: Revise prerequisite\ngoal: Deliver the prerequisite\nintake:\n  constraints: [Preserve integration]\n  success_criteria: [Prerequisite available]\n  unknowns: []\n  revises: {epic}\n---\n\n## Prerequisite\nsources: scope\npaths: README\nreuse: {issue}\n\n### Acceptance\n- [ ] Prerequisite is available\n"
    );
    let revised = fx.propose(&revision);
    let revised_epic = revised["epic"].as_str().unwrap();
    fx.approve(revised_epic);
    plan::gate_id(&fx.pm.dir, issue).unwrap();
    let (live_front, live_body) = write::load_front(&dir).unwrap();
    assert!(live_front.paths.iter().any(|path| path == "src/module.rs"));
    assert_eq!(
        plan::gate_snapshot(&fx.pm.dir, &live_front, &live_body).unwrap(),
        Some(revised_epic.to_string())
    );
    let scope = plan::scope_digest(&live_front, &live_body).unwrap();
    let source = std::fs::read_to_string(dir.join("issue.md")).unwrap();
    let text = format!(
        "Begin the revised work {}",
        crate::issue::dispatch::intake_legacy_suffix(&source, revised_epic, &scope).unwrap()
    );

    let opts = ServeOptions {
        test_seam: true,
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", fx.pm.dir.to_str().unwrap());
    let shared = Shared::new(fx._dir.path(), &opts).unwrap();
    register(&shared, fx._dir.path(), "acceptance");
    register(&shared, fx._dir.path(), "dispatch-pm");
    let before = fx.snapshot(issue);
    let direct_message = "cad224-old-lane-direct";
    let refusal = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        shared.rpc_dispatch_send(
            &json!({
                "alias": "acceptance",
                "issue": issue,
                "text": text,
                "reply_to": "dispatch-pm",
                "message": direct_message,
                "intake_revision": revised_epic,
                "intake_scope": scope,
            }),
            std::process::id(),
        )
    })
    .unwrap_err();
    assert_eq!(refusal.code(), Some("plan_evidence_stale"), "{refusal:?}");
    assert!(
        refusal.to_string().contains("execution base"),
        "{refusal:?}"
    );
    assert!(!shared.store.message_exists(direct_message).unwrap());
    assert_eq!(shared.store.queued_count("acceptance").unwrap(), 0);
    assert_eq!(fx.snapshot(issue), before);

    let spec = fx._dir.path().join("old-lane-job.md");
    std::fs::write(&spec, "Deliver the revised prerequisite.\n").unwrap();
    let spec_sha = crate::delegation::sha256_hex(b"Deliver the revised prerequisite.\n");
    shared
        .store
        .create_job(
            "cad224-old-lane",
            None,
            spec.to_str().unwrap(),
            &spec_sha,
            "acceptance",
            Some(issue),
            Some(fx.repo.to_str().unwrap()),
            None,
            2,
            None,
            Some("Deliver the revised prerequisite"),
            Some(lane.to_str().unwrap()),
            Some(lane_branch),
            Some(&fx.old_base),
            Some("acceptance"),
            Some("Prerequisite is available"),
        )
        .unwrap();
    let before_task = shared.store.task("cad224-old-lane-t1").unwrap().to_json();
    let job_message = "cad224-old-lane-job";
    let refusal = shared
        .rpc_task_dispatch(&json!({
            "task": "cad224-old-lane-t1",
            "by": "acceptance",
            "message": job_message,
        }))
        .unwrap_err();
    assert_eq!(refusal.code(), Some("plan_evidence_stale"), "{refusal:?}");
    assert!(
        refusal.to_string().contains("execution base"),
        "{refusal:?}"
    );
    assert!(!shared.store.message_exists(job_message).unwrap());
    assert_eq!(shared.store.queued_count("acceptance").unwrap(), 0);
    assert_eq!(
        shared.store.task("cad224-old-lane-t1").unwrap().to_json(),
        before_task
    );
    assert_eq!(fx.snapshot(issue), before);

    let newer_base = git(&fx.repo, &["rev-parse", "HEAD"]).trim().to_string();
    let before = fx.snapshot(issue);
    let refusal = fx.start(issue, Some(newer_base)).unwrap_err();
    assert_eq!(refusal.code(), Some("plan_evidence_stale"), "{refusal:?}");
    assert!(
        refusal.to_string().contains("execution base"),
        "{refusal:?}"
    );
    assert_eq!(fx.snapshot(issue), before);
    assert!(!lane.join("src/module.rs").exists());
}

#[test]
fn intake_start_rechecks_changed_scope_before_locked_claim() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let fx = IntakeFixture::new();
    let draft = LINKED_INTAKE.replace(
        "- [ ] Prerequisite is available",
        "- [ ] Prerequisite is available\n- [ ] Additional property",
    );
    let proposal = fx.propose(&draft);
    let epic = proposal["epic"].as_str().unwrap();
    fx.approve(epic);
    let issue = proposal["tickets"][0].as_str().unwrap();
    let (_, issue_dir) = write::issue_dir(&fx.pm, issue).unwrap();
    let (front, body) = write::load_front(&issue_dir).unwrap();
    let qualified = body.replace(
        "- [ ] Prerequisite is available\n- [ ] Additional property\n",
        "- [ ] Prerequisite is available\n  Under production traffic\n- [ ] Additional property\n",
    );
    assert_ne!(qualified, body);
    write::save_front(&issue_dir, &front, &qualified).unwrap();
    fx.pm
        .commit(&[issue_dir.join("issue.md")], "Qualify prerequisite scope")
        .unwrap();
    let revision = format!(
        "---\ntitle: Qualify prerequisite\ngoal: Deliver the prerequisite\nintake:\n  constraints: [Preserve integration]\n  success_criteria: [Prerequisite available]\n  unknowns: []\n  revises: {epic}\n---\n\n## Prerequisite\nsources: scope\npaths: src/module.rs\nreuse: {issue}\n\n### Acceptance\n- [ ] Prerequisite is available\n- [ ] Additional property\n"
    );
    let revised = fx.propose(&revision);
    fx.approve(revised["epic"].as_str().unwrap());
    plan::gate_id(&fx.pm.dir, issue).unwrap();
    let checked = qualified.replace(
        "- [ ] Prerequisite is available",
        "- [x] Prerequisite is available",
    );
    let criteria_file = fx._dir.path().join("criteria.md");
    std::fs::write(
        &criteria_file,
        "- [x] Prerequisite is available\n- [ ] Additional property\n",
    )
    .unwrap();
    write::set_acceptance(&fx.pm, issue, &criteria_file, "acceptance").unwrap();
    let (_, actual_body) = write::load_front(&issue_dir).unwrap();
    assert_eq!(actual_body, checked);
    plan::gate_id(&fx.pm.dir, issue).unwrap();
    let before = fx.snapshot(issue);
    let changed = Rc::new(RefCell::new(None));
    let recorded = Rc::clone(&changed);
    let pm_dir = fx.pm.dir.clone();
    let id = issue.to_string();
    crate::issue::lockseam::set_hook(Some(Box::new(move |point| {
        if point != "start-pre-claim" {
            return;
        }
        let pm = Pm::at(&pm_dir).unwrap();
        let _lock = pm.lock().unwrap();
        let (_, dir) = write::issue_dir(&pm, &id).unwrap();
        let (front, body) = write::load_front(&dir).unwrap();
        let changed = body.replace(
            "- [x] Prerequisite is available\n  Under production traffic\n- [ ] Additional property\n",
            "- [x] Prerequisite is available\n- [ ] Additional property\n  Under production traffic\n",
        );
        assert_ne!(changed, body);
        let body = changed;
        write::save_front(&dir, &front, &body).unwrap();
        pm.commit(&[dir.join("issue.md")], "Change intake scope during start")
            .unwrap();
        *recorded.borrow_mut() = Some((
            git(&pm_dir, &["rev-parse", "HEAD"]),
            std::fs::read(dir.join("issue.md")).unwrap(),
        ));
    })));
    let result = fx.start(issue, None);
    crate::issue::lockseam::set_hook(None);
    let refusal = result.unwrap_err();
    assert_eq!(refusal.code(), Some("plan_evidence_stale"), "{refusal:?}");
    let (changed_head, changed_issue) = changed.borrow().clone().expect("pre-claim seam reached");
    let after = fx.snapshot(issue);
    assert_ne!(changed_head, before.0);
    assert_eq!(after.0, changed_head);
    assert_eq!(after.1, changed_issue);
    assert_eq!(after.2, before.2);
    let front = board::find_issue(&fx.pm.dir, issue).unwrap().front;
    assert_eq!(front.status, "ready");
    assert!(front.claim.is_none());
}

#[test]
fn intake_cannot_approve_revision_through_unapproved_predecessor() {
    let fx = IntakeFixture::new();
    let a = fx.propose(LINKED_INTAKE);
    let a_id = a["epic"].as_str().unwrap();
    let revision = |previous: &str, title: &str| {
        format!(
        "---\ntitle: {title}\ngoal: Deliver another outcome\nintake:\n  constraints: [Preserve integration]\n  success_criteria: [Outcome available]\n  unknowns: []\n  revises: {previous}\n---\n\n## {title}\nsources: scope\npaths: src/module.rs\n\n### Acceptance\n- [ ] Outcome is available\n"
    )
    };
    let b = fx.propose(&revision(a_id, "Proposed B"));
    let b_id = b["epic"].as_str().unwrap();
    let c = fx.propose(&revision(b_id, "Proposed C"));
    let c_id = c["epic"].as_str().unwrap();
    let assert_refused_without_mutation = || {
        let before_head = git(&fx.pm.dir, &["rev-parse", "HEAD"]);
        let before_status = git(
            &fx.pm.dir,
            &["status", "--porcelain", "--untracked-files=all"],
        );
        let ids = [
            a_id,
            b_id,
            c_id,
            a["tickets"][0].as_str().unwrap(),
            a["tickets"][1].as_str().unwrap(),
        ];
        let files: Vec<_> = ids
            .iter()
            .map(|id| std::fs::read(fx.pm.dir.join("cadence").join(id).join("issue.md")).unwrap())
            .collect();
        let refusal = write::decide_plan(&fx.pm, c_id, true, "operator", None).unwrap_err();
        assert_eq!(
            refusal.code(),
            Some("plan_revision_noncontinuous"),
            "{refusal:?}"
        );
        assert_eq!(git(&fx.pm.dir, &["rev-parse", "HEAD"]), before_head);
        assert_eq!(
            git(
                &fx.pm.dir,
                &["status", "--porcelain", "--untracked-files=all"]
            ),
            before_status
        );
        for (id, expected) in ids.iter().zip(files) {
            assert_eq!(
                std::fs::read(fx.pm.dir.join("cadence").join(id).join("issue.md")).unwrap(),
                expected
            );
        }
    };
    assert_refused_without_mutation();
    fx.approve(a_id);
    assert_refused_without_mutation();
}

#[test]
fn omitted_completed_prerequisite_keeps_historical_scope_and_drop_guard() {
    let fx = IntakeFixture::new();
    let original = fx.propose(LINKED_INTAKE);
    let epic = original["epic"].as_str().unwrap();
    let prerequisite = original["tickets"][0].as_str().unwrap();
    let dependent = original["tickets"][1].as_str().unwrap();
    fx.approve(epic);
    let (_, dir) = write::issue_dir(&fx.pm, prerequisite).unwrap();
    let (mut front, body) = write::load_front(&dir).unwrap();
    front.status = "done".into();
    write::save_front(&dir, &front, &body).unwrap();
    fx.pm
        .commit(&[dir.join("issue.md")], "Complete prerequisite")
        .unwrap();
    let revision = format!(
        "---\ntitle: Retain dependent\ngoal: Deliver the dependent outcome\nintake:\n  constraints: [Preserve integration]\n  success_criteria: [Dependent outcome available]\n  unknowns: []\n  revises: {epic}\n---\n\n## Dependent\nsources: scope\npaths: src/module.rs\nreuse: {dependent}\n\n### Acceptance\n- [ ] Dependent outcome is available\n"
    );
    let revised = fx.propose(&revision);
    fx.approve(revised["epic"].as_str().unwrap());
    plan::gate_id(&fx.pm.dir, dependent).unwrap();

    let edited = body.replace("Prerequisite is available", "Different prerequisite");
    write::save_front(&dir, &front, &edited).unwrap();
    fx.pm
        .commit(&[dir.join("issue.md")], "Edit completed prerequisite")
        .unwrap();
    let before = fx.snapshot(dependent);
    let refusal = fx.start(dependent, None).unwrap_err();
    assert_eq!(refusal.code(), Some("plan_evidence_stale"), "{refusal:?}");
    assert_eq!(fx.snapshot(dependent), before);

    front.status = "dropped".into();
    write::save_front(&dir, &front, &body).unwrap();
    fx.pm
        .commit(&[dir.join("issue.md")], "Drop completed prerequisite")
        .unwrap();
    let before = fx.snapshot(dependent);
    let refusal = fx.start(dependent, None).unwrap_err();
    assert_eq!(refusal.code(), Some("plan_evidence_stale"), "{refusal:?}");
    assert_eq!(fx.snapshot(dependent), before);
}

#[test]
fn cad224_opted_in_relative_job_spec_refuses_drift_before_enqueue() {
    use std::cell::Cell;
    use std::rc::Rc;

    let fx = IntakeFixture::new();
    let manifest = fx.repo.join("docs/cadence/project-context.yaml");
    let current = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        current.replace(
            "project: cadence\n",
            "project: cadence\nagent_delivery: true\n",
        ),
    )
    .unwrap();
    let spec = fx.repo.join("specs/intake-job.md");
    std::fs::create_dir_all(spec.parent().unwrap()).unwrap();
    let original_spec = "Implement the approved prerequisite.\n";
    std::fs::write(&spec, original_spec).unwrap();
    git(
        &fx.repo,
        &[
            "add",
            "docs/cadence/project-context.yaml",
            "specs/intake-job.md",
        ],
    );
    git(&fx.repo, &["commit", "-qm", "Opt in and add job spec"]);
    let proposal = fx.propose(LINKED_INTAKE);
    let issue = proposal["tickets"][0].as_str().unwrap();
    fx.approve(proposal["epic"].as_str().unwrap());
    let started = fx.start(issue, None).unwrap();
    let lane = started["worktree"].as_str().unwrap();
    let branch = started["branch"].as_str().unwrap();
    let base = started["base"]["sha"].as_str().unwrap();

    let opts = ServeOptions {
        test_seam: true,
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", fx.pm.dir.to_str().unwrap());
    let shared = Shared::new(fx._dir.path(), &opts).unwrap();
    register(&shared, fx._dir.path(), "acceptance");
    let sha = crate::delegation::sha256_hex(original_spec.as_bytes());
    shared
        .store
        .create_job(
            "cad224-relative",
            None,
            "specs/intake-job.md",
            &sha,
            "acceptance",
            Some(issue),
            Some(fx.repo.to_str().unwrap()),
            None,
            2,
            None,
            Some("Implement prerequisite"),
            Some(lane),
            Some(branch),
            Some(base),
            Some("acceptance"),
            None,
        )
        .unwrap();
    shared
        .store
        .create_task(
            "cad224-relative",
            "cad224-relative-fresh",
            Some("Follow the same spec"),
            Some("acceptance"),
            None,
            None,
            Some(lane),
            Some(branch),
            Some(base),
        )
        .unwrap();
    let accepted = shared
        .rpc_task_dispatch(&json!({
            "task": "cad224-relative-t1",
            "by": "acceptance",
            "message": "cad224-relative-accepted",
        }))
        .unwrap();
    assert_eq!(accepted["duplicate"], false, "{accepted}");
    assert!(shared
        .store
        .message_exists("cad224-relative-accepted")
        .unwrap());

    let before = fx.snapshot(issue);
    let before_task = shared
        .store
        .task("cad224-relative-fresh")
        .unwrap()
        .to_json();
    let before_queue = shared.store.queued_count("acceptance").unwrap();
    let seen = Rc::new(Cell::new(false));
    let observed = Rc::clone(&seen);
    let changed_spec = spec.clone();
    crate::issue::lockseam::set_hook(Some(Box::new(move |point| {
        if point == "intake-pre-enqueue" {
            assert!(!observed.replace(true), "enqueue seam ran twice");
            std::fs::write(&changed_spec, "Changed independent spec.\n").unwrap();
        }
    })));
    let result = shared.rpc_task_dispatch(&json!({
        "task": "cad224-relative-fresh",
        "by": "acceptance",
        "message": "cad224-relative-stale",
    }));
    crate::issue::lockseam::set_hook(None);
    assert!(
        seen.get(),
        "relative spec did not reach the enqueue seam: {result:?}"
    );
    let refusal = result.unwrap_err();
    assert!(
        refusal
            .to_string()
            .contains("independent spec changed while preparing"),
        "{refusal:?}"
    );
    assert!(!shared
        .store
        .message_exists("cad224-relative-stale")
        .unwrap());
    assert_eq!(
        shared.store.queued_count("acceptance").unwrap(),
        before_queue
    );
    assert_eq!(
        shared
            .store
            .task("cad224-relative-fresh")
            .unwrap()
            .to_json(),
        before_task
    );
    assert_eq!(fx.snapshot(issue), before);
}

#[test]
fn cad224_legacy_cloud_relative_default_spec_is_pinned_before_enqueue() {
    use std::cell::Cell;
    use std::rc::Rc;

    let fx = IntakeFixture::new();
    let spec = fx.repo.join("specs/cad224-cloud.md");
    std::fs::create_dir_all(spec.parent().unwrap()).unwrap();
    let original = "cad224 sentinel from the tracked repository spec";
    std::fs::write(&spec, original).unwrap();
    git(&fx.repo, &["add", "specs/cad224-cloud.md"]);
    git(&fx.repo, &["commit", "-qm", "Add independent cloud spec"]);
    let proposal = fx.propose(LINKED_INTAKE);
    let issue = proposal["tickets"][0].as_str().unwrap();
    fx.approve(proposal["epic"].as_str().unwrap());
    let started = fx.start(issue, None).unwrap();
    let lane = started["worktree"].as_str().unwrap();
    let branch = started["branch"].as_str().unwrap();
    let base = started["base"]["sha"].as_str().unwrap();

    let opts = ServeOptions {
        test_seam: true,
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", fx.pm.dir.to_str().unwrap());
    opts.provider_env.set("CADENCE_DEVIN_API_KEY", "");
    opts.provider_env.set("CADENCE_DEVIN_ORG_ID", "");
    let shared = Shared::new(fx._dir.path(), &opts).unwrap();
    shared
        .store
        .register_agent(&NewAgent {
            alias: "cad224-cloud",
            provider: "devin",
            endpoint_kind: "cloud",
            role: "worker",
            cwd: fx._dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let sha = crate::delegation::sha256_hex(original.as_bytes());
    shared
        .store
        .create_job(
            "cad224-legacy-cloud",
            None,
            "specs/cad224-cloud.md",
            &sha,
            "cad224-cloud",
            Some(issue),
            Some(fx.repo.to_str().unwrap()),
            None,
            2,
            None,
            Some("Implement prerequisite"),
            Some(lane),
            Some(branch),
            Some(base),
            Some("cad224-cloud"),
            None,
        )
        .unwrap();
    for task in ["cad224-cloud-drift", "cad224-cloud-unpinned"] {
        shared
            .store
            .create_task(
                "cad224-legacy-cloud",
                task,
                None,
                Some("cad224-cloud"),
                None,
                None,
                Some(lane),
                Some(branch),
                Some(base),
            )
            .unwrap();
    }
    let accepted = shared
        .rpc_task_dispatch(&json!({
            "task": "cad224-legacy-cloud-t1", "by": "acceptance",
            "message": "cad224-cloud-accepted",
        }))
        .unwrap();
    assert_eq!(accepted["duplicate"], false);
    let message = shared
        .store
        .message("cad224-cloud-accepted")
        .unwrap()
        .unwrap();
    assert!(message.body.contains(original), "{}", message.body);

    let before = fx.snapshot(issue);
    let queue = shared.store.queued_count("cad224-cloud").unwrap();
    let before_task = shared.store.task("cad224-cloud-drift").unwrap().to_json();
    let seen = Rc::new(Cell::new(false));
    let observed = Rc::clone(&seen);
    let changed = spec.clone();
    crate::issue::lockseam::set_hook(Some(Box::new(move |point| {
        if point == "intake-pre-enqueue" {
            assert!(!observed.replace(true));
            std::fs::write(&changed, "cad224 changed spec\n").unwrap();
        }
    })));
    let result = shared.rpc_task_dispatch(&json!({
        "task": "cad224-cloud-drift", "by": "acceptance",
        "message": "cad224-cloud-stale",
    }));
    crate::issue::lockseam::set_hook(None);
    assert!(seen.get(), "enqueue seam not reached: {result:?}");
    let refusal = result.unwrap_err();
    assert!(
        refusal
            .to_string()
            .contains("independent spec changed while preparing"),
        "{refusal:?}"
    );
    assert!(!shared.store.message_exists("cad224-cloud-stale").unwrap());
    assert_eq!(shared.store.queued_count("cad224-cloud").unwrap(), queue);
    assert_eq!(
        shared.store.task("cad224-cloud-drift").unwrap().to_json(),
        before_task
    );
    assert_eq!(fx.snapshot(issue), before);

    std::fs::write(&spec, original).unwrap();
    let db = rusqlite::Connection::open(fx._dir.path().join("cadence.sqlite3")).unwrap();
    db.execute(
        "UPDATE jobs SET spec_sha256=NULL WHERE id='cad224-legacy-cloud'",
        [],
    )
    .unwrap();
    let before_task = shared
        .store
        .task("cad224-cloud-unpinned")
        .unwrap()
        .to_json();
    let refusal = shared
        .rpc_task_dispatch(&json!({
            "task": "cad224-cloud-unpinned", "by": "acceptance",
            "message": "cad224-cloud-unpinned-message",
        }))
        .unwrap_err();
    assert!(
        refusal.to_string().contains("no immutable digest"),
        "{refusal:?}"
    );
    assert!(!shared
        .store
        .message_exists("cad224-cloud-unpinned-message")
        .unwrap());
    assert_eq!(shared.store.queued_count("cad224-cloud").unwrap(), queue);
    assert_eq!(
        shared
            .store
            .task("cad224-cloud-unpinned")
            .unwrap()
            .to_json(),
        before_task
    );
    assert_eq!(fx.snapshot(issue), before);
}
