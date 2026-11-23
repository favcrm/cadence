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
