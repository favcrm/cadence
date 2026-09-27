#![cfg(feature = "test-seam")]

use cadence_agent::issue::{app_catalog, Pm};
use cadence_agent::test_seam::{self, Asserted};
use std::path::{Path, PathBuf};

struct Fixture {
    _dir: tempfile::TempDir,
    pm: Pm,
    state: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(state.join("seam")).unwrap();
        std::fs::write(state.join("seam/token"), "cad630-fixture").unwrap();
        cadence_agent::store::Store::open(&state.join("cadence.sqlite3")).unwrap();
        let pm = Pm::init(&dir.path().join("pm")).unwrap();
        Self {
            _dir: dir,
            pm,
            state,
        }
    }

    fn install(&self, project: &str, id: &str) -> PathBuf {
        let base = self.pm.dir.join(project);
        std::fs::create_dir_all(base.join("apps/social-content")).unwrap();
        std::fs::write(
            base.join("project.yaml"),
            format!("key: {project}\nprefix: C\n"),
        )
        .unwrap();
        std::fs::write(
            base.join("apps/social-content/app.md"),
            "---\napp: social-content\ntitle: Social\nversion: 1\n---\nGuide\n",
        )
        .unwrap();
        let record = base.join("apps/social-content.yaml");
        std::fs::write(&record, format!("schema: 1\napp: social-content\ninstall_id: '{id}'\nsource:\n  kind: path\n  path: /source\nteam:\n  writer: worker-a\ninstalled_at: yesterday\ninstalled_by: operator\n")).unwrap();
        record
    }
}

fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

#[test]
fn cad630_migration_preserves_two_same_name_legacy_installations_and_their_records() {
    let f = Fixture::new();
    let a = f.install("client-a", "install-a");
    let b = f.install("client-b", "install-b");
    let before = (bytes(&a), bytes(&b));
    let catalog =
        test_seam::scoped(Asserted::Operator, || app_catalog::migrate(&f.pm, &f.state)).unwrap();
    let one = catalog
        .resolve_legacy(&f.pm.dir, "client-a", "social-content")
        .unwrap();
    let two = catalog
        .resolve_legacy(&f.pm.dir, "client-b", "social-content")
        .unwrap();
    assert_eq!(one.install_id, "install-a");
    assert_eq!(two.install_id, "install-b");
    assert_eq!(one.project.as_deref(), Some("client-a"));
    assert_eq!(two.project.as_deref(), Some("client-b"));
    assert_eq!(
        (bytes(&a), bytes(&b)),
        before,
        "migration must not rewrite existing identities/team/source"
    );
}

#[test]
fn cad630_migration_refuses_agent_and_unproven_callers_before_catalog_writes() {
    for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        let f = Fixture::new();
        let record = f.install("client", "install-a");
        let before = bytes(&record);
        let result = test_seam::scoped(who, || app_catalog::migrate(&f.pm, &f.state));
        assert!(
            result.is_err(),
            "migration accepted an unauthorized caller: {result:?}"
        );
        assert!(!f.pm.dir.join(".apps").exists());
        assert_eq!(bytes(&record), before);
    }
}
