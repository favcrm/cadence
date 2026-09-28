use super::super::app_records::{record_db_path, CustomerProfile, RecordStore};
use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn record_file(dir: &TempDir, install: &str) -> RecordStore {
    RecordStore::open(dir.path(), install).unwrap()
}

fn customer(name: &str, email: &str) -> CustomerProfile {
    CustomerProfile::parse(&json!({"schema": 1, "display_name": name, "email": email, "tags": [], "consent": {"email": "granted"}})).unwrap()
}

#[test]
fn cad753_record_files_are_physical_per_installation() {
    let dir = TempDir::new().unwrap();
    let a = record_file(&dir, "install-a");
    let b = record_file(&dir, "install-b");
    let path_a = record_db_path(dir.path(), "install-a").unwrap();
    let path_b = record_db_path(dir.path(), "install-b").unwrap();
    assert_ne!(path_a, path_b);
    assert!(path_a.is_file() && path_b.is_file());
    // Two installations are two inodes, not two views of one file.
    assert_ne!(
        std::fs::metadata(&path_a).unwrap().ino(),
        std::fs::metadata(&path_b).unwrap().ino()
    );

    a.app_record_create(
        "ctx-1",
        "customer-1",
        &customer("Amina", "amina@example.com"),
    )
    .unwrap();
    b.app_record_create(
        "ctx-1",
        "customer-1",
        &customer("Boris", "boris@example.com"),
    )
    .unwrap();
    assert_eq!(
        a.app_record_show("ctx-1", "customer-1").unwrap()["record"]["profile"]["display_name"],
        "Amina"
    );
    assert_eq!(
        b.app_record_show("ctx-1", "customer-1").unwrap()["record"]["profile"]["display_name"],
        "Boris"
    );
    // Context scoping lives inside each file: the same record ID in
    // another context of the same file is absent.
    assert!(
        a.app_record_show("ctx-2", "customer-1").is_err()
            && a.app_record_list("ctx-2").unwrap()["records"]
                .as_array()
                .unwrap()
                .is_empty()
    );

    // CAS, digest and attributed history, as before.
    let created = a.app_record_show("ctx-1", "customer-1").unwrap();
    assert!(a
        .app_record_update(
            "ctx-1",
            "customer-1",
            9,
            &customer("Stale", "stale@example.com")
        )
        .is_err());
    assert_eq!(
        a.app_record_show("ctx-1", "customer-1").unwrap()["record"],
        created["record"]
    );
    let updated = a
        .app_record_update(
            "ctx-1",
            "customer-1",
            1,
            &customer("Amina B", "amina@example.com"),
        )
        .unwrap();
    assert_eq!(updated["record"]["revision"], 2);
    assert_ne!(updated["record"]["digest"], created["record"]["digest"]);
    assert_eq!(updated["record"]["history"].as_array().unwrap().len(), 2);
    assert_eq!(updated["record"]["history"][1]["actor"], "operator");
    // The sibling file is untouched by the CAS above.
    assert_eq!(
        b.app_record_show("ctx-1", "customer-1").unwrap()["record"]["revision"],
        1
    );
    // Duplicate create with the same body is idempotent; a different
    // body under the same ID is refused, never merged.
    assert_eq!(
        a.app_record_create(
            "ctx-1",
            "customer-1",
            &customer("Amina B", "amina@example.com")
        )
        .unwrap()["record"]["revision"],
        2
    );
    assert!(a
        .app_record_create(
            "ctx-1",
            "customer-1",
            &customer("Other", "other@example.com")
        )
        .is_err());
}

#[test]
fn cad753_record_file_reopens_with_its_contents() {
    let dir = TempDir::new().unwrap();
    let created = record_file(&dir, "install-a")
        .app_record_create(
            "ctx-1",
            "customer-1",
            &customer("Amina", "amina@example.com"),
        )
        .unwrap();
    drop(created);
    // No daemon handle is held between these opens: persistence is the
    // file itself.
    let reopened = RecordStore::open(dir.path(), "install-a").unwrap();
    assert_eq!(
        reopened.app_record_show("ctx-1", "customer-1").unwrap()["record"]["revision"],
        1
    );
    reopened
        .app_record_update(
            "ctx-1",
            "customer-1",
            1,
            &customer("Amina B", "amina@example.com"),
        )
        .unwrap();
    drop(reopened);
    assert_eq!(
        RecordStore::open(dir.path(), "install-a")
            .unwrap()
            .app_record_show("ctx-1", "customer-1")
            .unwrap()["record"]["revision"],
        2
    );
}

#[test]
fn cad753_record_file_identity_and_corruption_refuse_without_deletion() {
    let dir = TempDir::new().unwrap();
    record_file(&dir, "install-a")
        .app_record_create(
            "ctx-1",
            "customer-1",
            &customer("Amina", "amina@example.com"),
        )
        .unwrap();
    let path = record_db_path(dir.path(), "install-a").unwrap();
    // A file planted at another installation's path — the same bytes
    // claimed by another name — is refused by its identity row, and
    // the planted bytes are preserved.
    let planted = record_db_path(dir.path(), "install-b").unwrap();
    std::fs::copy(&path, &planted).unwrap();
    let Err(error) = RecordStore::open(dir.path(), "install-b") else {
        panic!("planted identity accepted");
    };
    let error = error.to_string();
    assert!(
        error.contains("identity"),
        "identity mismatch unclear: {error}"
    );
    assert_eq!(
        std::fs::read(&planted).unwrap(),
        std::fs::read(&path).unwrap()
    );
    // Corrupt bytes refuse and are preserved, never healed in place.
    let victim = record_db_path(dir.path(), "install-victim").unwrap();
    RecordStore::open(dir.path(), "install-victim").unwrap();
    std::fs::write(&victim, b"not a database at all").unwrap();
    let Err(error) = RecordStore::open(dir.path(), "install-victim") else {
        panic!("corrupt file accepted");
    };
    let error = error.to_string();
    assert!(
        error.contains("corrupt") || error.contains("backup"),
        "corruption refusal unclear: {error}"
    );
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"not a database at all",
        "corrupt file was modified"
    );
    // The intact installation still opens with its contents.
    assert_eq!(
        RecordStore::open(dir.path(), "install-a")
            .unwrap()
            .app_record_show("ctx-1", "customer-1")
            .unwrap()["record"]["profile"]["display_name"],
        "Amina"
    );
}

#[test]
fn cad753_record_paths_are_safe_and_private() {
    let dir = TempDir::new().unwrap();
    for bad in [
        "",
        "../escape",
        "/absolute",
        "has space",
        "UPPER",
        "ctx/x",
        "..",
        ".",
    ] {
        assert!(
            record_db_path(dir.path(), bad).is_err(),
            "unsafe installation ID accepted: {bad}"
        );
    }
    let path = record_db_path(dir.path(), "install-a").unwrap();
    assert_eq!(
        path,
        dir.path().join("app-records").join("install-a.sqlite3")
    );
    RecordStore::open(dir.path(), "install-a").unwrap();
    assert_eq!(
        std::fs::metadata(dir.path().join("app-records"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn cad753_record_profile_validation_never_echoes_content() {
    let marker = "cad753-store-private-marker";
    for body in [
        json!({"schema": 1, "display_name": "", "consent": {"email": "granted"}}),
        json!({"schema": 1, "display_name": marker, "email": "not-an-email", "consent": {"email": "granted"}}),
        json!({"schema": 1, "display_name": marker, "consent": {"email": "maybe"}}),
        json!({"schema": 1, "display_name": marker, "tags": ["ok", "no spaces allowed!!"], "consent": {"email": "granted"}}),
        json!({"schema": 1, "display_name": marker, "consent": {"email": "granted"}, "raw_sql": "SELECT 1"}),
        json!({"display_name": marker, "consent": {"email": "granted"}}),
    ] {
        // The refusal is a unit struct by construction, so it cannot
        // echo the marker; assert both the refusal and the silence.
        let error = CustomerProfile::parse(&body).unwrap_err();
        assert!(
            !error.to_string().contains(marker),
            "profile content leaked in refusal"
        );
    }
}
