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
        "---\ntitle: Revise prerequisite\ngoal: Deliver the prerequisite\nintake:\n  constraints: [Preserve integration]\n  success_criteria: [Prerequisite available]\n  unknowns: []\n  revises: {epic}\n---\n\n## Prerequisite\nsources: scope\npaths: src/module.rs\nreuse: {issue}\n\n### Acceptance\n- [ ] Prerequisite is available\n"
    );
    let revised = fx.propose(&revision);
    fx.approve(revised["epic"].as_str().unwrap());
    plan::gate_id(&fx.pm.dir, issue).unwrap();

    let newer_base = git(&fx.repo, &["rev-parse", "HEAD"]).trim().to_string();
    let before = fx.snapshot(issue);
    let refusal = fx.start(issue, Some(newer_base)).unwrap_err();
    assert_eq!(refusal.code(), Some("plan_evidence_stale"), "{refusal:?}");
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
    assert_ne!(checked, qualified);
    write::save_front(&issue_dir, &front, &checked).unwrap();
    fx.pm
        .commit(
            &[issue_dir.join("issue.md")],
            "Check prerequisite criterion",
        )
        .unwrap();
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
