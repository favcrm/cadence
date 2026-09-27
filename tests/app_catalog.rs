#![cfg(feature = "test-seam")]

use cadence_agent::issue::{app_catalog, Pm};
use cadence_agent::test_seam::{self, Asserted};
use sha2::{Digest, Sha256};
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
fn cad630_exact_id_lookup_has_no_package_name_or_foreign_workspace_fallback() {
    let f = Fixture::new();
    f.install("client-a", "install-a");
    f.install("client-b", "install-b");
    test_seam::scoped(Asserted::Operator, || app_catalog::migrate(&f.pm, &f.state)).unwrap();
    let catalog = app_catalog::Catalog::load(&f.pm.dir).unwrap();
    assert_eq!(
        catalog
            .resolve_id(&f.pm.dir, "install-a")
            .unwrap()
            .project
            .as_deref(),
        Some("client-a")
    );
    assert_eq!(
        catalog
            .resolve_id(&f.pm.dir, "install-b")
            .unwrap()
            .project
            .as_deref(),
        Some("client-b")
    );
    for guessed in ["social-content", "unknown-id", "../install-a", "/install-a"] {
        assert!(
            catalog.resolve_id(&f.pm.dir, guessed).is_err(),
            "guessed ID resolved: {guessed}"
        );
    }
    for forged in ["../client-a", "/client-a", "client-c"] {
        assert!(catalog
            .resolve_legacy(&f.pm.dir, forged, "social-content")
            .is_err());
    }
    let path = f.pm.dir.join(".apps/catalog.yaml");
    let altered = std::fs::read_to_string(&path)
        .unwrap()
        .replace("workspace: default", "workspace: foreign");
    std::fs::write(&path, altered).unwrap();
    assert!(
        app_catalog::Catalog::load(&f.pm.dir).is_err(),
        "forged workspace accepted"
    );
}

#[test]
fn cad630_cached_catalog_refuses_removed_or_reinstalled_identity_and_dangling_records() {
    for remove in [false, true] {
        let f = Fixture::new();
        let record = f.install("client", "install-a");
        let catalog =
            test_seam::scoped(Asserted::Operator, || app_catalog::migrate(&f.pm, &f.state))
                .unwrap();
        if remove {
            std::fs::remove_file(&record).unwrap();
        } else {
            std::fs::write(
                &record,
                String::from_utf8(bytes(&record))
                    .unwrap()
                    .replace("install-a", "replacement-id"),
            )
            .unwrap();
        }
        assert!(catalog.resolve_id(&f.pm.dir, "install-a").is_err());
        assert!(catalog
            .resolve_legacy(&f.pm.dir, "client", "social-content")
            .is_err());
    }
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

#[test]
fn cad630_duplicate_install_ids_refuse_before_mutating_records() {
    let f = Fixture::new();
    let a = f.install("client-a", "same-id");
    let b = f.install("client-b", "same-id");
    let before = (bytes(&a), bytes(&b));
    let result = test_seam::scoped(Asserted::Operator, || app_catalog::migrate(&f.pm, &f.state));
    assert!(result.is_err(), "duplicate IDs were accepted: {result:?}");
    assert_eq!((bytes(&a), bytes(&b)), before);
    assert!(!f.pm.dir.join(".apps/catalog.yaml").exists());
}

#[test]
fn cad630_missing_id_backfill_is_repeatable_and_never_matches_an_old_approval() {
    let f = Fixture::new();
    let path = f.install("client", "");
    let first =
        test_seam::scoped(Asserted::Operator, || app_catalog::migrate(&f.pm, &f.state)).unwrap();
    let installed = first
        .resolve_legacy(&f.pm.dir, "client", "social-content")
        .unwrap();
    assert!(!installed.install_id.is_empty());
    let record = bytes(&path);
    let second =
        test_seam::scoped(Asserted::Operator, || app_catalog::migrate(&f.pm, &f.state)).unwrap();
    assert_eq!(
        second
            .resolve_legacy(&f.pm.dir, "client", "social-content")
            .unwrap()
            .install_id,
        installed.install_id
    );
    assert_eq!(bytes(&path), record);
    let old = serde_json::json!({"digest":"same-content", "install_id":""});
    assert!(!cadence_agent::issue::app::approval_binds(
        &old,
        "same-content",
        &installed.install_id
    ));
    assert_eq!(
        cadence_agent::rollout::store_schema(&f.state).unwrap(),
        Some(19)
    );
}

#[test]
fn cad630_migration_refuses_symlinked_project_ancestor_and_catalog_directory() {
    for catalog_link in [false, true] {
        let f = Fixture::new();
        let record = f.install("client", "install-a");
        let before = bytes(&record);
        let foreign = f._dir.path().join("foreign");
        if catalog_link {
            std::fs::create_dir(&foreign).unwrap();
            std::os::unix::fs::symlink(&foreign, f.pm.dir.join(".apps")).unwrap();
        } else {
            std::fs::rename(f.pm.dir.join("client"), &foreign).unwrap();
            std::os::unix::fs::symlink(&foreign, f.pm.dir.join("client")).unwrap();
        }
        let result =
            test_seam::scoped(Asserted::Operator, || app_catalog::migrate(&f.pm, &f.state));
        assert!(result.is_err(), "symlink was followed: {result:?}");
        assert_eq!(bytes(&record), before);
        assert!(!foreign.join("catalog.yaml").exists());
    }
}

// Re-exec seam for the real process proof. With no fixture request this
// ordinary test does nothing; it adds no ignored/skipped security case.
#[test]
fn cad630_detached_probe() {
    let Some(root) = std::env::var_os("CAD630_CHILD_PM") else {
        return;
    };
    let pm = Pm::at(Path::new(&root)).unwrap();
    let state = PathBuf::from(std::env::var_os("CAD630_CHILD_STATE").unwrap());
    assert!(
        app_catalog::migrate(&pm, &state).is_err(),
        "detached enrolled child forged operator authority"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn cad630_detached_enrolled_child_cannot_migrate_with_a_forged_operator_field() {
    let f = Fixture::new();
    let record = f.install("client", "install-a");
    let before = bytes(&record);
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let start: u64 = stat
        .rsplit_once(") ")
        .unwrap()
        .1
        .split_whitespace()
        .nth(19)
        .unwrap()
        .parse()
        .unwrap();
    std::fs::write(f.state.join("slots.json"), serde_json::json!({
        "format":"cadence-slots", "version":2,
        "enrollments":[{"root":{"pid":std::process::id(),"starttime":start,"uid":unsafe{libc::getuid()}}}]
    }).to_string()).unwrap();
    let output = std::process::Command::new("setsid")
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "cad630_detached_probe", "--nocapture"])
        .env("CAD630_CHILD_PM", &f.pm.dir)
        .env("CAD630_CHILD_STATE", &f.state)
        .env("CAD630_FORGED_ACTOR", "operator:forged")
        .env_remove("CADENCE_ALIAS")
        .env_remove(test_seam::AS_ENV)
        .env_remove(test_seam::ARM_ENV)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "detached proof failed: {output:?}");
    assert_eq!(bytes(&record), before);
    assert!(!f.pm.dir.join(".apps").exists());
}

const JOURNAL: &str = "00000000000000000000000000000001";

// The journal is a persisted recovery interface, not a private function seam.
fn interrupted(f: &Fixture, phase: u8) -> (PathBuf, String, String, serde_json::Value) {
    let record = f.install("client", "");
    let before = String::from_utf8(bytes(&record)).unwrap();
    let after = before.replace("install_id: ''", "install_id: 'one-time-id'");
    let catalog = format!("schema: 1\nworkspace: default\nlast_migration: '{JOURNAL}'\ninstallations:\n  one-time-id:\n    app: social-content\n    project: client\n    storage:\n      kind: legacy\n      project: client\n      name: social-content\n");
    let journal = serde_json::json!({
        "schema":1, "id":JOURNAL, "before_catalog":null, "after_catalog":catalog,
        "records":[{"project":"client", "name":"social-content", "before":before,"after":after,
        "before_hash":format!("{:x}",Sha256::digest(before.as_bytes())),
        "after_hash":format!("{:x}",Sha256::digest(after.as_bytes()))}]
    });
    std::fs::create_dir_all(f.pm.dir.join(".apps/migrations")).unwrap();
    std::fs::write(
        f.pm.dir.join(format!(".apps/migrations/{JOURNAL}.yaml")),
        serde_yaml::to_string(&journal).unwrap(),
    )
    .unwrap();
    std::fs::write(
        f.pm.dir.join(".apps/pending.yaml"),
        format!("schema: 1\njournal: '{JOURNAL}'\n"),
    )
    .unwrap();
    if phase >= 1 {
        std::fs::write(&record, &after).unwrap();
    }
    if phase >= 2 {
        std::fs::write(f.pm.dir.join(".apps/catalog.yaml"), &catalog).unwrap();
    }
    (record, before, after, journal)
}

#[test]
fn cad630_resume_each_interrupted_publication_phase_without_minting_another_id() {
    for phase in 0..=2 {
        let f = Fixture::new();
        let (record, _, after, journal) = interrupted(&f, phase);
        test_seam::scoped(Asserted::Operator, || {
            app_catalog::recover(&f.pm, &f.state, JOURNAL, app_catalog::Recovery::Resume)
        })
        .unwrap();
        assert_eq!(bytes(&record), after.as_bytes());
        assert_eq!(
            std::fs::read_to_string(f.pm.dir.join(".apps/catalog.yaml")).unwrap(),
            journal["after_catalog"].as_str().unwrap()
        );
        assert!(!f.pm.dir.join(".apps/pending.yaml").exists());
        assert!(
            f.pm.dir
                .join(format!(".apps/migrations/{JOURNAL}.yaml"))
                .is_file(),
            "backup journal must be retained"
        );
    }
}

#[test]
fn cad630_explicit_rollback_restores_only_unchanged_staged_bytes() {
    let f = Fixture::new();
    let (record, before, _, _) = interrupted(&f, 2);
    test_seam::scoped(Asserted::Operator, || {
        app_catalog::recover(&f.pm, &f.state, JOURNAL, app_catalog::Recovery::Rollback)
    })
    .unwrap();
    assert_eq!(bytes(&record), before.as_bytes());
    assert!(!f.pm.dir.join(".apps/catalog.yaml").exists());
    assert!(!f.pm.dir.join(".apps/pending.yaml").exists());
}

#[test]
fn cad630_divergent_recovery_never_overwrites_a_later_update_or_removes_its_backup() {
    for mode in [
        app_catalog::Recovery::Resume,
        app_catalog::Recovery::Rollback,
    ] {
        let f = Fixture::new();
        let (record, _, after, _) = interrupted(&f, 2);
        let updated = after.replace("worker-a", "worker-after-revoke");
        std::fs::write(&record, &updated).unwrap();
        let catalog = bytes(&f.pm.dir.join(".apps/catalog.yaml"));
        let result = test_seam::scoped(Asserted::Operator, || {
            app_catalog::recover(&f.pm, &f.state, JOURNAL, mode)
        });
        assert!(result.is_err(), "divergent recovery was accepted");
        assert_eq!(bytes(&record), updated.as_bytes());
        assert_eq!(bytes(&f.pm.dir.join(".apps/catalog.yaml")), catalog);
        assert!(f.pm.dir.join(".apps/pending.yaml").is_file());
    }
}

#[test]
fn cad630_recovery_refuses_forged_paths_and_non_identity_record_edits() {
    for forged_path in [true, false] {
        let f = Fixture::new();
        let (record, before, _, mut journal) = interrupted(&f, 0);
        if forged_path {
            journal["records"][0]["project"] = serde_json::json!("../foreign");
        } else {
            let altered = journal["records"][0]["after"]
                .as_str()
                .unwrap()
                .replace("worker-a", "forged-worker");
            journal["records"][0]["after_hash"] =
                serde_json::json!(format!("{:x}", Sha256::digest(altered.as_bytes())));
            journal["records"][0]["after"] = serde_json::json!(altered);
        }
        std::fs::write(
            f.pm.dir.join(format!(".apps/migrations/{JOURNAL}.yaml")),
            serde_yaml::to_string(&journal).unwrap(),
        )
        .unwrap();
        let result = test_seam::scoped(Asserted::Operator, || {
            app_catalog::recover(&f.pm, &f.state, JOURNAL, app_catalog::Recovery::Resume)
        });
        assert!(result.is_err(), "forged recovery was accepted");
        assert_eq!(bytes(&record), before.as_bytes());
        assert!(!f.pm.dir.join(".apps/catalog.yaml").exists());
    }
}

#[test]
fn cad630_concurrent_migrations_mint_one_identity_under_the_existing_pm_lock() {
    let f = Fixture::new();
    f.install("client", "");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let root = f.pm.dir.clone();
            let state = f.state.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let pm = Pm::at(&root).unwrap();
                barrier.wait();
                test_seam::scoped(Asserted::Operator, || app_catalog::migrate(&pm, &state))
                    .unwrap()
                    .resolve_legacy(&root, "client", "social-content")
                    .unwrap()
                    .install_id
            })
        })
        .collect();
    barrier.wait();
    let ids: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(!ids[0].is_empty());
    assert_eq!(ids[0], ids[1]);
}

#[test]
fn cad630_recovery_requires_operator_proof_for_resume_and_rollback() {
    for mode in [
        app_catalog::Recovery::Resume,
        app_catalog::Recovery::Rollback,
    ] {
        for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
            let f = Fixture::new();
            let (record, _, after, _) = interrupted(&f, 1);
            let result =
                test_seam::scoped(who, || app_catalog::recover(&f.pm, &f.state, JOURNAL, mode));
            assert!(result.is_err(), "unauthorized recovery was accepted");
            assert_eq!(bytes(&record), after.as_bytes());
            assert!(!f.pm.dir.join(".apps/catalog.yaml").exists());
            assert!(f.pm.dir.join(".apps/pending.yaml").is_file());
        }
    }
}

#[test]
fn cad630_migration_preserves_restricted_approvals_and_derived_grant_identity() {
    let f = Fixture::new();
    f.install("client", "install-a");
    let store = cadence_agent::store::Store::open(&f.state.join("cadence.sqlite3")).unwrap();
    store
        .record_app_approval(
            serde_json::json!({"project":"client", "name":"social-content",
        "install_id":"install-a", "digest":"legacy-digest", "by":"operator", "at":1,
        "capabilities":[{"resource":"client-local", "action":"read"}]}),
        )
        .unwrap();
    store
        .app_grants_set(
            "client/social-content",
            "install-a",
            &[(
                "worker-a".into(),
                "local".into(),
                "client-only".into(),
                vec!["read".into()],
            )],
            "operator",
        )
        .unwrap();
    let approvals = store.app_approvals().unwrap();
    let grants = store.app_grant_installs().unwrap();
    test_seam::scoped(Asserted::Operator, || app_catalog::migrate(&f.pm, &f.state)).unwrap();
    assert_eq!(store.app_approvals().unwrap(), approvals);
    assert_eq!(store.app_grant_installs().unwrap(), grants);
    assert_eq!(
        grants,
        vec![("client/social-content".into(), "install-a".into())]
    );
}

#[test]
fn cad630_migration_orders_after_a_cooperating_update_and_grant_revocation() {
    let f = Fixture::new();
    let record = f.install("client", "install-a");
    let store = cadence_agent::store::Store::open(&f.state.join("cadence.sqlite3")).unwrap();
    store
        .app_grants_set(
            "client/social-content",
            "install-a",
            &[(
                "worker-a".into(),
                "local".into(),
                "client-only".into(),
                vec!["read".into()],
            )],
            "operator",
        )
        .unwrap();
    let lock = f.pm.lock().unwrap();
    let root = f.pm.dir.clone();
    let state = f.state.clone();
    let (send, receive) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let pm = Pm::at(&root).unwrap();
        send.send(()).unwrap();
        test_seam::scoped(Asserted::Operator, || app_catalog::migrate(&pm, &state))
            .unwrap()
            .resolve_legacy(&root, "client", "social-content")
            .unwrap()
    });
    receive.recv().unwrap();
    let updated = String::from_utf8(bytes(&record))
        .unwrap()
        .replace("worker-a", "worker-after-update");
    std::fs::write(&record, &updated).unwrap();
    store
        .app_grants_revoke("client/social-content", "operator")
        .unwrap();
    drop(lock);
    let installed = worker.join().unwrap();
    assert_eq!(installed.install_id, "install-a");
    assert_eq!(bytes(&record), updated.as_bytes());
    assert!(
        store.app_grant_installs().unwrap().is_empty(),
        "migration replayed a revoked grant"
    );
}
