//! CAD-1385 independent acceptance check. The test-only hook runs after
//! `openat` and before `fstat`, and is armed for one exact catalog path.
use super::*;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

struct Hook {
    path: PathBuf,
    callback: Box<dyn FnOnce(&File)>,
    observed_links: Arc<AtomicU64>,
}

thread_local! {
    static AFTER_OPEN: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

fn arm(path: &Path, callback: impl FnOnce(&File) + 'static) -> Arc<AtomicU64> {
    let observed_links = Arc::new(AtomicU64::new(u64::MAX));
    AFTER_OPEN.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "an after-open hook is already armed"
        );
        *slot.borrow_mut() = Some(Hook {
            path: path.to_path_buf(),
            callback: Box::new(callback),
            observed_links: observed_links.clone(),
        });
    });
    observed_links
}

pub(super) fn after_open(path: &Path, file: &File) {
    let hook = AFTER_OPEN.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|hook| hook.path == path) {
            slot.take()
        } else {
            None
        }
    });
    if let Some(hook) = hook {
        // Drain before the callback: the real upgrade itself reads catalog
        // files and must not recursively trigger this test hook.
        (hook.callback)(file);
        hook.observed_links
            .store(file.metadata().unwrap().nlink(), Ordering::SeqCst);
    }
}

const WORKFLOW: &str = r#"---
title: "Post: {{topic}}"
goal: "Publish {{topic}}"
inputs:
  topic: { ask: "About what?" }
---

Why.

## Research {{topic}}
agent: dev-1
size: S

Do it.

### Acceptance
- [ ] brief written
"#;

fn bundle(dir: &Path, version: u32) {
    std::fs::create_dir_all(dir.join("workflows")).unwrap();
    std::fs::write(
        dir.join("app.md"),
        format!(
            "---\napp: cad1385-race\ntitle: Fixture\nversion: '{version}'\nneeds:\n  connections: []\n---\n\nGuide v{version}.\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("workflows/do.md"), WORKFLOW).unwrap();
}

#[test]
fn cad1385_catalog_replacement_retries_whole_show_and_hard_links_still_refuse() {
    let pm_dir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let sources = tempfile::tempdir().unwrap();
    let pm = crate::issue::Pm::init(pm_dir.path()).unwrap();
    let v1 = sources.path().join("v1");
    let v2 = sources.path().join("v2");
    bundle(&v1, 1);
    bundle(&v2, 2);

    let installed = crate::issue::app_catalog::workspace::install(
        &pm,
        state.path(),
        v1.to_str().unwrap(),
        None,
        &crate::issue::app_catalog::catalogue::CatalogueClaim::Absent,
    )
    .unwrap();
    let id = installed["install_id"].as_str().unwrap().to_owned();
    let before = crate::issue::app_catalog::workspace::show(&pm, &id).unwrap();
    assert_eq!(before["version"], "1");
    let old_digest = before["digest"].as_str().unwrap().to_owned();
    let generation = before["catalog_generation"].as_str().unwrap().to_owned();
    let checked = crate::issue::app_catalog::workspace::upgrade_check(
        &pm,
        &id,
        v2.to_str().unwrap(),
        &old_digest,
        &generation,
        &crate::issue::app_catalog::catalogue::CatalogueClaim::Absent,
    )
    .unwrap();
    let new_digest = checked["digest"].as_str().unwrap().to_owned();
    let catalog_path = Path::new(".apps/catalog.yaml");
    let pm_dir_for_upgrade = pm_dir.path().to_path_buf();
    let id_for_upgrade = id.clone();
    let source_for_upgrade = v2.to_str().unwrap().to_owned();
    let old_digest_for_upgrade = old_digest.clone();
    let generation_for_upgrade = generation.clone();
    let new_digest_for_upgrade = new_digest.clone();
    let observed_links = arm(catalog_path, move |_| {
        let upgrade_pm = crate::issue::Pm::at(&pm_dir_for_upgrade).unwrap();
        let request = crate::issue::app_catalog::workspace::UpgradeRequest {
            id: &id_for_upgrade,
            source: &source_for_upgrade,
            expected_digest: &old_digest_for_upgrade,
            expected_generation: &generation_for_upgrade,
            expected_new_digest: &new_digest_for_upgrade,
            request_id: "cad1385-retry",
            catalogue: &crate::issue::app_catalog::catalogue::CatalogueClaim::Absent,
            directory: crate::issue::app_catalog::catalogue::CatalogueDirectory::Configured,
        };
        crate::issue::app_catalog::workspace::upgrade(
            &upgrade_pm,
            &request,
            || None,
            |_, _| Ok((serde_json::json!({}), Default::default())),
            |_| Ok(Vec::new()),
        )
        .unwrap_or_else(|error| panic!("real workspace upgrade failed: {error}"));
    });

    let answer = crate::issue::app_catalog::workspace::show(&pm, &id);
    assert_eq!(
        observed_links.load(Ordering::SeqCst),
        0,
        "atomic catalog replacement must leave the opened regular inode unlinked"
    );
    match answer {
        Ok(row) => {
            assert_eq!(
                row["version"], "2",
                "show must retry to the published state"
            );
            assert_eq!(row["digest"], new_digest);
        }
        Err(error) => assert_eq!(
            error.kind(),
            "busy",
            "show may return only a complete snapshot or retryable busy, not {error}"
        ),
    }

    let hardlink_dir = tempfile::tempdir().unwrap();
    std::fs::write(hardlink_dir.path().join("catalog.yaml"), "schema: 1\n").unwrap();
    std::fs::hard_link(
        hardlink_dir.path().join("catalog.yaml"),
        hardlink_dir.path().join("catalog-copy.yaml"),
    )
    .unwrap();
    let root = Root::open(hardlink_dir.path()).unwrap();
    let refusal = root
        .read(Path::new("catalog.yaml"), 1024)
        .expect_err("a genuine hard link must still be refused by Root::read");
    assert!(
        refusal
            .to_string()
            .ends_with("catalog requires a bounded regular file without hard links"),
        "hard-link refusal must come from the real Root::read guard: {refusal}"
    );
}
