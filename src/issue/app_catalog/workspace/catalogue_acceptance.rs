//! Catalogue locator outcomes: a recorded origin matches the package, a
//! check does not publish, and a mismatch or an unreadable directory
//! leaves the installation where it was.

use super::*;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const APP: &str = "field-notes";
const WORKFLOW: &str = "---\ntitle: \"Post: {{topic}}\"\ngoal: \"Publish {{topic}}\"\n\
inputs:\n  topic: { ask: \"About what?\" }\n---\n\n\
Why.\n\n## Do {{topic}}\nagent: dev-1\nsize: S\n\nDo it.\n\n### Acceptance\n- [ ] done\n";

fn manifest(version: &str) -> String {
    format!(
        "---\napp: {APP}\ntitle: Fixture\nversion: '{version}'\nneeds:\n  connections: []\n---\n\nGuide {version}.\n"
    )
}

fn write_bundle(dir: &Path, version: &str) -> String {
    std::fs::create_dir_all(dir.join("workflows")).unwrap();
    std::fs::write(dir.join("app.md"), manifest(version)).unwrap();
    std::fs::write(dir.join("workflows/do.md"), WORKFLOW).unwrap();
    dir.to_str().unwrap().to_string()
}

fn digest_of(source: &str) -> String {
    let report = check_offline(source).unwrap();
    assert_eq!(report["ok"], true, "{report}");
    report["digest"].as_str().unwrap().to_string()
}

fn locator(id: &str, version: &str, digest: &str) -> CatalogueClaim {
    CatalogueClaim::Locator(app::CatalogueOrigin {
        id: id.to_string(),
        version: version.to_string(),
        digest: digest.to_string(),
    })
}

fn release(version: &str, digest: &str, installable: bool) -> Value {
    json!({
        "version": version,
        "digest": digest,
        "installable": installable,
        "unavailableReason": if installable { Value::Null } else { json!("disabled") },
        "release_notes": "reviewed"
    })
}

fn versions(app_name: &str, recommended: &str, releases: Vec<Value>) -> Value {
    json!({
        "app_id": APP,
        "app": app_name,
        "recommended_version": recommended,
        "releases": releases
    })
}

struct Directory {
    origin: String,
    doc: Arc<Mutex<Value>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Directory {
    fn start(doc: Value) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let doc = Arc::new(Mutex::new(doc));
        let stop = Arc::new(AtomicBool::new(false));
        let body = Arc::clone(&doc);
        let flag = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut buf = [0_u8; 4096];
                        let _ = stream.read(&mut buf);
                        let bytes = serde_json::to_vec(&*body.lock().unwrap()).unwrap();
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            bytes.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&bytes);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(15));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            origin,
            doc,
            stop,
            thread: Some(thread),
        }
    }

    fn set(&self, doc: Value) {
        *self.doc.lock().unwrap() = doc;
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn workspace() -> (tempfile::TempDir, tempfile::TempDir, Pm) {
    let pm_dir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let pm = Pm::init(pm_dir.path()).unwrap();
    (pm_dir, state, pm)
}

fn refused(result: Result<Value>, needle: &str) {
    let err = match result {
        Err(err) => err,
        Ok(value) => panic!("expected a refusal containing {needle}, got {value}"),
    };
    assert!(
        err.to_string().contains(needle),
        "expected {needle} in {err}"
    );
}

fn assert_row(row: &Value, id: &str, version: &str, digest: &str, catalogue_version: Option<&str>) {
    assert_eq!(row["install_id"], json!(id));
    assert_eq!(row["version"], json!(version));
    assert_eq!(row["digest"], json!(digest));
    assert_eq!(row["source"]["kind"], json!("path"));
    match catalogue_version {
        Some(version) => {
            assert_eq!(row["catalogue"]["id"], json!(APP));
            assert_eq!(row["catalogue"]["version"], json!(version));
            assert_eq!(row["catalogue"]["digest"], json!(digest));
        }
        None => assert!(row["catalogue"].is_null(), "{row}"),
    }
}

fn install_at(
    pm: &Pm,
    state: &Path,
    source: &str,
    digest: &str,
    claim: &CatalogueClaim,
    origin: &str,
) -> Result<Value> {
    install_from(
        pm,
        state,
        source,
        Some(digest),
        claim,
        CatalogueDirectory::Origin(origin),
    )
}

fn apply(pm: &Pm, request: &UpgradeRequest<'_>) -> Result<Value> {
    upgrade(
        pm,
        request,
        || None,
        |_, _| Ok((json!({}), Default::default())),
        |_| Ok(Vec::new()),
    )
}

#[test]
fn catalogue_install_records_origin_and_a_later_publication_does_not_touch_it() {
    let (pm_dir, state, pm) = workspace();
    let sources = tempfile::tempdir().unwrap();
    let source = write_bundle(&sources.path().join("v1"), "1.0.0");
    let digest = digest_of(&source);
    let directory = Directory::start(versions(
        APP,
        "2.0.0",
        vec![release("1.0.0", &digest, true)],
    ));
    let claim = locator(APP, "1.0.0", &digest);
    let installed = install_at(
        &pm,
        state.path(),
        &source,
        &digest,
        &claim,
        &directory.origin,
    )
    .unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    assert_eq!(installed["committed"], json!(true));
    assert_row(&installed, &id, "1.0.0", &digest, Some("1.0.0"));

    directory.set(versions(
        APP,
        "9.0.0",
        vec![release("1.0.0", &digest, false)],
    ));
    let shown = show(&pm, &id).unwrap();
    assert_row(&shown, &id, "1.0.0", &digest, Some("1.0.0"));
    drop(directory);

    let reopened = Pm::at(pm_dir.path()).unwrap();
    assert_row(
        &show(&reopened, &id).unwrap(),
        &id,
        "1.0.0",
        &digest,
        Some("1.0.0"),
    );
}

#[test]
fn catalogue_upgrade_keeps_the_install_id_and_moves_bytes_only_on_apply() {
    let (_pm_dir, state, pm) = workspace();
    let sources = tempfile::tempdir().unwrap();
    let current = write_bundle(&sources.path().join("v1"), "1.0.0");
    let next = write_bundle(&sources.path().join("v2"), "2.0.0");
    let old_digest = digest_of(&current);
    let new_digest = digest_of(&next);
    let directory = Directory::start(versions(
        APP,
        "2.0.0",
        vec![
            release("1.0.0", &old_digest, true),
            release("2.0.0", &new_digest, true),
        ],
    ));
    let claim = locator(APP, "1.0.0", &old_digest);
    let id = install_at(
        &pm,
        state.path(),
        &current,
        &old_digest,
        &claim,
        &directory.origin,
    )
    .unwrap()["install_id"]
        .as_str()
        .unwrap()
        .to_string();
    let generation = show(&pm, &id).unwrap()["catalog_generation"]
        .as_str()
        .unwrap()
        .to_string();
    let next_claim = locator(APP, "2.0.0", &new_digest);
    let check = upgrade_check_from(
        &pm,
        &id,
        &next,
        &old_digest,
        &generation,
        &next_claim,
        CatalogueDirectory::Origin(&directory.origin),
    )
    .unwrap();
    assert_eq!(check["committed"], json!(false));
    assert_eq!(check["install_id"], json!(id));
    assert_eq!(check["digest"], json!(new_digest));
    assert_eq!(check["catalogue"]["version"], json!("2.0.0"));
    assert_row(
        &show(&pm, &id).unwrap(),
        &id,
        "1.0.0",
        &old_digest,
        Some("1.0.0"),
    );

    let applied = apply(
        &pm,
        &UpgradeRequest {
            id: &id,
            source: &next,
            expected_digest: &old_digest,
            expected_generation: &generation,
            expected_new_digest: &new_digest,
            request_id: "catalogue-apply",
            catalogue: &next_claim,
            directory: CatalogueDirectory::Origin(&directory.origin),
        },
    )
    .unwrap();
    assert_eq!(applied["committed"], json!(true));
    assert_row(&applied, &id, "2.0.0", &new_digest, Some("2.0.0"));
    assert_row(
        &show(&pm, &id).unwrap(),
        &id,
        "2.0.0",
        &new_digest,
        Some("2.0.0"),
    );
}

#[test]
fn catalogue_mismatches_and_unavailable_metadata_leave_bytes_uninstalled() {
    let sources = tempfile::tempdir().unwrap();
    let current = write_bundle(&sources.path().join("v1"), "1.0.0");
    let next = write_bundle(&sources.path().join("v2"), "2.0.0");

    let (_pm_dir, state, pm) = workspace();
    let digest = digest_of(&current);
    let other = format!("sha256:{}", "11".repeat(32));
    let directory = Directory::start(versions(
        APP,
        "1.0.0",
        vec![release("1.0.0", &digest, true)],
    ));
    refused(
        install_at(
            &pm,
            state.path(),
            &current,
            &other,
            &locator(APP, "1.0.0", &digest),
            &directory.origin,
        ),
        "expected digest",
    );
    assert!(list(&pm).unwrap().as_array().unwrap().is_empty());

    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        origin
    };
    refused(
        install_at(
            &pm,
            state.path(),
            &current,
            &digest,
            &locator(APP, "1.0.0", &digest),
            &closed,
        ),
        "unavailable",
    );
    directory.set(versions(
        APP,
        "1.0.0",
        vec![release("1.0.0", &digest, false)],
    ));
    refused(
        install_at(
            &pm,
            state.path(),
            &current,
            &digest,
            &locator(APP, "1.0.0", &digest),
            &directory.origin,
        ),
        "not installable",
    );
    directory.set(versions(
        "other-app",
        "1.0.0",
        vec![release("1.0.0", &digest, true)],
    ));
    refused(
        install_at(
            &pm,
            state.path(),
            &current,
            &digest,
            &locator(APP, "1.0.0", &digest),
            &directory.origin,
        ),
        "does not match this package",
    );
    assert!(list(&pm).unwrap().as_array().unwrap().is_empty());

    directory.set(versions(
        APP,
        "2.0.0",
        vec![release("1.0.0", &digest, true)],
    ));
    let id = install_at(
        &pm,
        state.path(),
        &current,
        &digest,
        &locator(APP, "1.0.0", &digest),
        &directory.origin,
    )
    .unwrap()["install_id"]
        .as_str()
        .unwrap()
        .to_string();
    let new_digest = digest_of(&next);
    directory.set(versions(
        APP,
        "2.0.0",
        vec![
            release("1.0.0", &digest, true),
            release("2.0.0", &new_digest, true),
        ],
    ));
    let generation = show(&pm, &id).unwrap()["catalog_generation"]
        .as_str()
        .unwrap()
        .to_string();
    refused(
        upgrade_check_from(
            &pm,
            &id,
            &next,
            &other,
            &generation,
            &locator(APP, "2.0.0", &new_digest),
            CatalogueDirectory::Origin(&directory.origin),
        ),
        "digest is stale",
    );
    refused(
        upgrade_check_from(
            &pm,
            &id,
            &next,
            &digest,
            &other,
            &locator(APP, "2.0.0", &new_digest),
            CatalogueDirectory::Origin(&directory.origin),
        ),
        "generation is stale",
    );
    refused(
        upgrade_check_from(
            &pm,
            &id,
            &next,
            &digest,
            &generation,
            &locator(APP, "9.9.9", &new_digest),
            CatalogueDirectory::Origin(&directory.origin),
        ),
        "does not match this package",
    );
    refused(
        upgrade_check_from(
            &pm,
            &id,
            &next,
            &digest,
            &generation,
            &locator(APP, "2.0.0", &other),
            CatalogueDirectory::Origin(&directory.origin),
        ),
        "does not match this package",
    );
    refused(
        upgrade_check_from(
            &pm,
            &id,
            &next,
            &digest,
            &generation,
            &locator("other-notes", "2.0.0", &new_digest),
            CatalogueDirectory::Origin(&directory.origin),
        ),
        "catalogue id",
    );
    refused(
        apply(
            &pm,
            &UpgradeRequest {
                id: &id,
                source: &next,
                expected_digest: &digest,
                expected_generation: &generation,
                expected_new_digest: &other,
                request_id: "catalogue-pin",
                catalogue: &locator(APP, "2.0.0", &new_digest),
                directory: CatalogueDirectory::Origin(&directory.origin),
            },
        ),
        "expected new digest",
    );
    assert_row(
        &show(&pm, &id).unwrap(),
        &id,
        "1.0.0",
        &digest,
        Some("1.0.0"),
    );
}

#[test]
fn catalogue_downgrade_and_omission_keep_the_recorded_release() {
    let (_pm_dir, state, pm) = workspace();
    let sources = tempfile::tempdir().unwrap();
    let older = write_bundle(&sources.path().join("v1"), "1.0.0");
    let current = write_bundle(&sources.path().join("v2"), "2.0.0");
    let old_digest = digest_of(&older);
    let digest = digest_of(&current);
    let directory = Directory::start(versions(
        APP,
        "2.0.0",
        vec![
            release("1.0.0", &old_digest, true),
            release("2.0.0", &digest, true),
        ],
    ));
    let id = install_at(
        &pm,
        state.path(),
        &current,
        &digest,
        &locator(APP, "2.0.0", &digest),
        &directory.origin,
    )
    .unwrap()["install_id"]
        .as_str()
        .unwrap()
        .to_string();
    let generation = show(&pm, &id).unwrap()["catalog_generation"]
        .as_str()
        .unwrap()
        .to_string();
    refused(
        upgrade_check_from(
            &pm,
            &id,
            &older,
            &digest,
            &generation,
            &locator(APP, "1.0.0", &old_digest),
            CatalogueDirectory::Origin(&directory.origin),
        ),
        "older release",
    );
    refused(
        upgrade_check_from(
            &pm,
            &id,
            &older,
            &digest,
            &generation,
            &CatalogueClaim::Absent,
            CatalogueDirectory::Origin(&directory.origin),
        ),
        "names the new release",
    );
    assert_row(
        &show(&pm, &id).unwrap(),
        &id,
        "2.0.0",
        &digest,
        Some("2.0.0"),
    );
}

#[test]
fn catalogue_manual_resolution_clears_origin_without_replacing_the_install() {
    let (_pm_dir, state, pm) = workspace();
    let sources = tempfile::tempdir().unwrap();
    let current = write_bundle(&sources.path().join("v1"), "1.0.0");
    let next = write_bundle(&sources.path().join("v2"), "2.0.0");
    let digest = digest_of(&current);
    let new_digest = digest_of(&next);
    let directory = Directory::start(versions(
        APP,
        "1.0.0",
        vec![release("1.0.0", &digest, true)],
    ));
    let id = install_at(
        &pm,
        state.path(),
        &current,
        &digest,
        &locator(APP, "1.0.0", &digest),
        &directory.origin,
    )
    .unwrap()["install_id"]
        .as_str()
        .unwrap()
        .to_string();
    let generation = show(&pm, &id).unwrap()["catalog_generation"]
        .as_str()
        .unwrap()
        .to_string();
    let applied = apply(
        &pm,
        &UpgradeRequest {
            id: &id,
            source: &next,
            expected_digest: &digest,
            expected_generation: &generation,
            expected_new_digest: &new_digest,
            request_id: "catalogue-manual",
            catalogue: &CatalogueClaim::Manual,
            directory: CatalogueDirectory::Origin(&directory.origin),
        },
    )
    .unwrap();
    assert_row(&applied, &id, "2.0.0", &new_digest, None);
    assert_row(&show(&pm, &id).unwrap(), &id, "2.0.0", &new_digest, None);
}

#[test]
fn catalogue_path_install_stays_unlabelled_until_a_later_release_matches() {
    let (_pm_dir, state, pm) = workspace();
    let sources = tempfile::tempdir().unwrap();
    let first = write_bundle(&sources.path().join("v1"), "1.0.0");
    let second = write_bundle(&sources.path().join("v2"), "2.0.0");
    let third = write_bundle(&sources.path().join("v3"), "3.0.0");
    let first_digest = digest_of(&first);
    let second_digest = digest_of(&second);
    let third_digest = digest_of(&third);
    let directory = Directory::start(versions(
        APP,
        "3.0.0",
        vec![
            release("2.0.0", &second_digest, true),
            release("3.0.0", &third_digest, true),
        ],
    ));
    let id = install_from(
        &pm,
        state.path(),
        &first,
        Some(&first_digest),
        &CatalogueClaim::Absent,
        CatalogueDirectory::Origin(&directory.origin),
    )
    .unwrap()["install_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_row(&show(&pm, &id).unwrap(), &id, "1.0.0", &first_digest, None);
    let generation = show(&pm, &id).unwrap()["catalog_generation"]
        .as_str()
        .unwrap()
        .to_string();
    apply(
        &pm,
        &UpgradeRequest {
            id: &id,
            source: &second,
            expected_digest: &first_digest,
            expected_generation: &generation,
            expected_new_digest: &second_digest,
            request_id: "path-update",
            catalogue: &CatalogueClaim::Absent,
            directory: CatalogueDirectory::Origin(&directory.origin),
        },
    )
    .unwrap();
    assert_row(&show(&pm, &id).unwrap(), &id, "2.0.0", &second_digest, None);
    let generation = show(&pm, &id).unwrap()["catalog_generation"]
        .as_str()
        .unwrap()
        .to_string();
    apply(
        &pm,
        &UpgradeRequest {
            id: &id,
            source: &third,
            expected_digest: &second_digest,
            expected_generation: &generation,
            expected_new_digest: &third_digest,
            request_id: "matched-release",
            catalogue: &locator(APP, "3.0.0", &third_digest),
            directory: CatalogueDirectory::Origin(&directory.origin),
        },
    )
    .unwrap();
    assert_row(
        &show(&pm, &id).unwrap(),
        &id,
        "3.0.0",
        &third_digest,
        Some("3.0.0"),
    );
}

#[test]
fn catalogue_fresh_install_allows_an_older_eligible_release() {
    let (_pm_dir, state, pm) = workspace();
    let sources = tempfile::tempdir().unwrap();
    let source = write_bundle(&sources.path().join("v1"), "1.0.0");
    let digest = digest_of(&source);
    let newer = format!("sha256:{}", "22".repeat(32));
    let directory = Directory::start(versions(
        APP,
        "2.0.0",
        vec![
            release("1.0.0", &digest, true),
            release("2.0.0", &newer, true),
        ],
    ));
    let installed = install_at(
        &pm,
        state.path(),
        &source,
        &digest,
        &locator(APP, "1.0.0", &digest),
        &directory.origin,
    )
    .unwrap();
    let id = installed["install_id"].as_str().unwrap();
    assert_row(&installed, id, "1.0.0", &digest, Some("1.0.0"));
    assert_row(&show(&pm, id).unwrap(), id, "1.0.0", &digest, Some("1.0.0"));
}

#[test]
fn catalogue_checked_access_reports_changed_declarations_not_just_files() {
    let mut before = app::parse_manifest(&manifest("1.0.0")).unwrap();
    let mut after = app::parse_manifest(&manifest("1.1.0")).unwrap();
    before.connections = vec!["shared".into(), "old".into()];
    after.connections = vec!["shared".into(), "new".into()];
    let need = app::CapabilityNeed {
        schema: 1,
        capability: "text.read".into(),
        version: 1,
        action: "list".into(),
        resource_kind: "connection_account".into(),
        effect: "read".into(),
    };
    before.capabilities.insert("changed".into(), need.clone());
    before.capabilities.insert("removed".into(), need.clone());
    before.capabilities.insert("unchanged".into(), need.clone());
    after.capabilities.insert("added".into(), need.clone());
    after.capabilities.insert("unchanged".into(), need.clone());
    let mut changed = need;
    changed.action = "fetch".into();
    after.capabilities.insert("changed".into(), changed);

    let review = declared_access_changes(Some(&before), &after);
    assert_eq!(
        review["connections"],
        json!({"added": ["new"], "removed": ["old"]})
    );
    assert_eq!(
        review["capabilities"]["changed"]["before"]["action"],
        "list"
    );
    assert_eq!(
        review["capabilities"]["changed"]["after"]["action"],
        "fetch"
    );
    assert!(review["capabilities"]["added"]["before"].is_null());
    assert_eq!(
        review["capabilities"]["added"]["after"]["capability"],
        "text.read"
    );
    assert_eq!(
        review["capabilities"]["removed"]["before"]["capability"],
        "text.read"
    );
    assert!(review["capabilities"]["removed"]["after"].is_null());
    assert!(review["capabilities"].get("unchanged").is_none());
}

#[test]
fn catalogue_apply_refuses_a_release_disabled_after_successful_check() {
    // Capture files and directory entries, including otherwise invisible
    // pending journals or orphan revision directories.
    fn persisted_entries(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        let mut entries = BTreeMap::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                entries.insert(path.clone(), None);
                entries.extend(persisted_entries(&path));
            } else {
                assert!(kind.is_file(), "unexpected fixture entry: {path:?}");
                let bytes = std::fs::read(&path).unwrap();
                entries.insert(path, Some(bytes));
            }
        }
        entries
    }

    let (pm_dir, state, pm) = workspace();
    let sources = tempfile::tempdir().unwrap();
    let current_source = write_bundle(&sources.path().join("v1"), "1.0.0");
    let next_source = write_bundle(&sources.path().join("v2"), "2.0.0");
    let old_digest = digest_of(&current_source);
    let new_digest = digest_of(&next_source);
    let directory = Directory::start(versions(
        APP,
        "2.0.0",
        vec![
            release("1.0.0", &old_digest, true),
            release("2.0.0", &new_digest, true),
        ],
    ));
    let installed = install_at(
        &pm,
        state.path(),
        &current_source,
        &old_digest,
        &locator(APP, "1.0.0", &old_digest),
        &directory.origin,
    )
    .unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();
    let before = show(&pm, &id).unwrap();
    let generation = before["catalog_generation"].as_str().unwrap().to_string();
    let apps_dir = pm.dir.join(".apps");
    let persisted_before = persisted_entries(&apps_dir);
    let head_before = crate::issue::git(&pm.dir, &["rev-parse", "HEAD"]).unwrap();
    let next_claim = locator(APP, "2.0.0", &new_digest);

    // Establish that the exact target was eligible and all pins were valid.
    let checked = upgrade_check_from(
        &pm,
        &id,
        &next_source,
        &old_digest,
        &generation,
        &next_claim,
        CatalogueDirectory::Origin(&directory.origin),
    )
    .unwrap();
    assert_eq!(checked["committed"], json!(false));
    assert_eq!(checked["approved"], json!(false));
    assert_eq!(checked["install_id"], json!(id));
    assert_eq!(checked["expected_digest"], json!(old_digest));
    assert_eq!(checked["expected_generation"], json!(generation));
    assert_eq!(checked["digest"], json!(new_digest));
    assert_eq!(
        checked["catalogue"],
        json!({
            "id": APP,
            "version": "2.0.0",
            "digest": new_digest
        })
    );
    assert_eq!(persisted_entries(&apps_dir), persisted_before);
    assert_eq!(
        crate::issue::git(&pm.dir, &["rev-parse", "HEAD"]).unwrap(),
        head_before
    );

    // Change eligibility only: identity, version, digest, recommendation,
    // source bytes, installed digest and catalogue generation stay fixed.
    directory.set(versions(
        APP,
        "2.0.0",
        vec![
            release("1.0.0", &old_digest, true),
            release("2.0.0", &new_digest, false),
        ],
    ));
    assert_eq!(digest_of(&next_source), new_digest);

    let request = UpgradeRequest {
        id: &id,
        source: &next_source,
        expected_digest: checked["expected_digest"].as_str().unwrap(),
        expected_generation: checked["expected_generation"].as_str().unwrap(),
        expected_new_digest: checked["digest"].as_str().unwrap(),
        request_id: "disabled-after-check",
        catalogue: &next_claim,
        directory: CatalogueDirectory::Origin(&directory.origin),
    };

    // Use the actual native boundary, not a mocked eligibility predicate.
    // A cached Check must not authorize this now-disabled publication.
    let result = upgrade(
        &pm,
        &request,
        || None,
        |_, _| panic!("disabled release reached upgrade preflight"),
        |_| panic!("disabled release reached upgrade delivery"),
    );
    refused(result, "catalogue release is not installable");

    // Refusal must retain package bytes, provenance, CAS generation and
    // installation identity, without a pending or completed upgrade journal.
    assert_eq!(persisted_entries(&apps_dir), persisted_before);
    assert_eq!(
        crate::issue::git(&pm.dir, &["rev-parse", "HEAD"]).unwrap(),
        head_before
    );
    assert_eq!(show(&pm, &id).unwrap(), before);
    assert_row(
        &show(&pm, &id).unwrap(),
        &id,
        "1.0.0",
        &old_digest,
        Some("1.0.0"),
    );

    // Confirm the retained state survives reopening the workspace.
    let reopened = Pm::at(pm_dir.path()).unwrap();
    assert_eq!(show(&reopened, &id).unwrap(), before);
}
