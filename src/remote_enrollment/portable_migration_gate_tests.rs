use super::*;

const BASE: &str = "/v1/hosted-cadence/portable-migrations/op-1/input-packs";

#[test]
fn pack_suffix_admits_only_the_three_shapes() {
    for ok in [
        "/v1/hosted-cadence/portable-migrations/op-1/input-packs/0",
        "/v1/hosted-cadence/portable-migrations/op-1/input-packs/12",
        "/v1/hosted-cadence/portable-migrations/op-1/input-packs/0/parts/1",
        "/v1/hosted-cadence/portable-migrations/op-1/input-packs/12/parts/3",
        "/v1/hosted-cadence/portable-migrations/op-1/input-packs/0/finalize",
    ] {
        assert!(portable_migration_path(ok), "should admit {ok}");
    }
    for tail in [
        "0/parts/0",
        "0/parts/01",
        "00",
        "0/parts/1/extra",
        "0/finalize/extra",
        "0/parts",
        "0/parts/1/2",
        "-1",
        "0x1",
        "/parts/1",
        "0/partsx/1",
        "0/parts/",
        "0/finalize/",
        "01",
    ] {
        let path = format!("{BASE}/{tail}");
        assert!(!portable_migration_path(&path), "should refuse {tail}");
    }
    assert!(!portable_migration_path(
        "/v1/hosted-cadence/portable-migrations/op-1/input-packs"
    ));
    assert!(!portable_migration_path(
        "/v1/hosted-cadence/portable-migrations/op-1/other/0"
    ));
}

#[test]
fn empty_post_is_admitted_only_at_finalize() {
    let finalize = format!("{BASE}/0/finalize");
    let pack = format!("{BASE}/0");
    assert!(portable_migration_shape_admitted(
        &finalize,
        "POST",
        &MigrationBody::Empty
    ));
    // Empty POST refused at a non-finalize path.
    assert!(!portable_migration_shape_admitted(
        &pack,
        "POST",
        &MigrationBody::Empty
    ));
    // A finalize path refused with any body other than an empty POST.
    let body = json!({"x": 1});
    assert!(!portable_migration_shape_admitted(
        &finalize,
        "POST",
        &MigrationBody::Json(&body)
    ));
    assert!(!portable_migration_shape_admitted(
        &finalize,
        "POST",
        &MigrationBody::Bytes(b"x")
    ));
    let mut stream: &[u8] = b"x";
    assert!(!portable_migration_shape_admitted(
        &finalize,
        "PUT",
        &MigrationBody::Stream(&mut stream)
    ));
}
