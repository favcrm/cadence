//! CAD-1962 workspace asset detail read.
//!
//! Scope: `Store::app_media_asset` — the single-row catalog read the
//! `app_media_read` `{kind:"asset"}` input returns. The catalog is
//! store-wide, so a row another installation produced reads here under
//! the same boundary the catalog page uses; `install_id` is provenance,
//! not a read partition. Reads are metadata only, an absent or
//! malformed id is `None`, and a stored row that could not have been
//! written by the catalog refuses instead of projecting.
//!
//! The writer path (producer association) has its own independent
//! CAD-1365 acceptance; this file seeds rows directly so the read rules
//! are exercised without repeating that fixture.

use super::*;

const INSTALL_A: &str = "install-a";
const INSTALL_B: &str = "install-b";
const OPERATOR: &str = "{\"kind\":\"operator\"}";

fn seed(s: &Store, id: &str, install: &str, title: &str, lifecycle: &str) {
    s.write_tx(|tx| {
        tx.execute(
            "INSERT INTO app_media_assets(
                id,install_id,title,kind,lifecycle,revision,creator,created,updated)
             VALUES(?1,?2,?3,'image',?4,2,?5,10.0,11.0)",
            params![id, install, title, lifecycle, OPERATOR],
        )?;
        Ok(())
    })
    .unwrap();
}

fn seed_lineage(s: &Store, asset: &str, source: &str, sha: &str) {
    s.write_tx(|tx| {
        tx.execute(
            "INSERT INTO app_media_lineage(asset_id,source_asset_id,source_sha256)
             VALUES(?1,?2,?3)",
            params![asset, source, sha],
        )?;
        Ok(())
    })
    .unwrap();
}

#[test]
fn detail_reads_any_workspace_asset_with_its_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("t.sqlite3")).unwrap();
    let source = format!("med-{}", "a".repeat(32));
    let derived = format!("med-{}", "b".repeat(32));
    seed(&s, &source, INSTALL_A, "Generated image", "published");
    seed(&s, &derived, INSTALL_B, "Generated image", "draft");
    seed_lineage(&s, &derived, &source, &"c".repeat(64));

    let reader = s.app_media_asset(&source).unwrap().unwrap();
    assert_eq!(reader.install_id, INSTALL_A);
    assert_eq!(reader.lifecycle, "published");
    assert_eq!(reader.revision, 2);
    assert_eq!(reader.source_asset_id, None);
    let projected = reader.projection();
    assert_eq!(projected["id"], json!(source));
    assert_eq!(projected["creator"], json!({"kind": "operator"}));
    assert!(projected.get("lineage").is_none());

    // Another installation's row reads the same way: the store is the
    // workspace, not the producer.
    let other = s.app_media_asset(&derived).unwrap().unwrap();
    assert_eq!(other.install_id, INSTALL_B);
    assert_eq!(other.source_asset_id.as_deref(), Some(source.as_str()));
    assert_eq!(
        other.projection()["lineage"],
        json!([source]),
        "a derived asset reports its source asset id"
    );
}

#[test]
fn detail_returns_none_for_unknown_or_malformed_ids() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("t.sqlite3")).unwrap();
    let known = format!("med-{}", "d".repeat(32));
    seed(&s, &known, INSTALL_A, "Generated image", "draft");

    assert!(s
        .app_media_asset(&format!("med-{}", "e".repeat(32)))
        .unwrap()
        .is_none());
    for malformed in [
        "",
        "med-short",
        &format!("med-{}", "A".repeat(32)),
        &format!("med-{}", "f".repeat(33)),
        &format!("asset-{}", "d".repeat(32)),
    ] {
        assert!(
            s.app_media_asset(malformed).unwrap().is_none(),
            "{malformed} must not resolve"
        );
    }
}

#[test]
fn detail_refuses_a_row_the_catalog_could_not_have_written() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("t.sqlite3")).unwrap();
    let id = format!("med-{}", "f".repeat(32));
    s.write_tx(|tx| {
        tx.execute(
            "INSERT INTO app_media_assets(
                id,install_id,title,kind,lifecycle,revision,creator,created,updated)
             VALUES(?1,?2,?3,'image','draft',1,?4,10.0,11.0)",
            params![id, INSTALL_A, "bad\u{7}title", OPERATOR],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(
        s.app_media_asset(&id).is_err(),
        "a control character in a stored title must refuse the read"
    );
}

const IMPORT_BYTES: &[u8] = b"not-really-a-png-but-the-store-trusts-the-declared-mime";

#[test]
fn an_import_copies_bytes_into_workspace_custody_and_replays() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("t.sqlite3")).unwrap();
    let first = s
        .app_media_import(
            INSTALL_A,
            "ctx-a",
            OPERATOR,
            "image/png",
            IMPORT_BYTES,
            "import-1",
        )
        .unwrap();
    let asset_id = first["receipt"]["asset_id"].as_str().unwrap().to_owned();
    assert_eq!(first["receipt"]["lifecycle"], "draft");

    let imported = s.app_media_imported_image(&asset_id).unwrap().unwrap();
    assert_eq!(imported.media_type, "image/png");
    assert_eq!(imported.bytes, IMPORT_BYTES);
    assert_eq!(imported.asset.install_id, INSTALL_A);
    assert_eq!(imported.asset.lifecycle, "draft");

    // Identical replay returns the SAME asset, never a second copy.
    let replay = s
        .app_media_import(
            INSTALL_A,
            "ctx-a",
            OPERATOR,
            "image/png",
            IMPORT_BYTES,
            "import-1",
        )
        .unwrap();
    assert_eq!(replay["receipt"]["asset_id"], json!(asset_id));
    assert_eq!(s.app_media_catalog(20, None).unwrap().0.len(), 1);

    // The same request id with different bytes refuses and reserves nothing.
    assert!(s
        .app_media_import(
            INSTALL_A,
            "ctx-a",
            OPERATOR,
            "image/png",
            b"different-bytes",
            "import-1"
        )
        .is_err());
    assert_eq!(s.app_media_catalog(20, None).unwrap().0.len(), 1);
}

#[test]
fn an_imported_asset_is_a_draft_until_published_and_outlives_its_importer() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("t.sqlite3")).unwrap();
    let created = s
        .app_media_import(
            INSTALL_A,
            "ctx-a",
            OPERATOR,
            "image/jpeg",
            IMPORT_BYTES,
            "import-2",
        )
        .unwrap();
    let asset_id = created["receipt"]["asset_id"].as_str().unwrap().to_owned();

    // The catalog is store-wide: another installation reads the row, as
    // CAD-1365's cross-install acceptance requires. Draft/published is
    // curation, not a visibility boundary.
    assert!(s.app_media_asset(&asset_id).unwrap().is_some());
    assert_eq!(s.app_media_catalog(20, None).unwrap().0.len(), 1);

    // Publishing is the existing curation step; the bytes stay in the
    // workspace store, not the importer's record store.
    s.app_media_publish(&asset_id, 1, "publish-1", 20.0)
        .unwrap();
    assert_eq!(
        s.app_media_asset(&asset_id).unwrap().unwrap().lifecycle,
        "published"
    );
    assert_eq!(
        s.app_media_imported_image(&asset_id)
            .unwrap()
            .unwrap()
            .bytes,
        IMPORT_BYTES
    );

    // Tampered custody refuses instead of serving bytes.
    s.write_tx(|tx| {
        tx.execute(
            "UPDATE app_media_imports SET bytes=?1 WHERE asset_id=?2",
            params![b"tampered".to_vec(), asset_id],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(s.app_media_imported_image(&asset_id).is_err());
}
