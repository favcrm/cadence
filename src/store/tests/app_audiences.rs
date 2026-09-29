use super::super::app_audiences::{AudienceBase, Predicate};
use super::super::app_records::{record_db_path, CustomerProfile, RecordStore};
use super::super::app_runs::material_digest;
use super::*;

fn audience_file(dir: &TempDir, install: &str) -> RecordStore {
    RecordStore::open(dir.path(), install).unwrap()
}

fn profile(name: &str, email: Option<&str>, consent: &str, tags: &[&str]) -> CustomerProfile {
    let mut body =
        json!({"schema": 1, "display_name": name, "tags": tags, "consent": {"email": consent}});
    if let Some(address) = email {
        body["email"] = json!(address);
    }
    CustomerProfile::parse(&body).unwrap()
}

fn seed_valid(store: &RecordStore, context: &str) {
    let rows = [
        (
            "customer-a",
            profile("Amina", Some("amina@example.com"), "granted", &["vip"]),
        ),
        (
            "customer-b",
            profile("Boris", Some("boris@example.com"), "denied", &["vip"]),
        ),
        (
            "customer-d",
            profile("Dev", Some("dev@example.com"), "unknown", &[]),
        ),
        (
            "customer-e",
            profile("Esme", Some("esme@example.com"), "granted", &[]),
        ),
    ];
    for (id, entry) in rows {
        store.app_record_create(context, id, &entry).unwrap();
    }
}

fn plant_legacy(dir: &TempDir, store: &RecordStore, context: &str) {
    // customer-c carries an address the current shape check
    // refuses, planted as legacy data predating the check: its
    // digest binds the exact body, so integrity still proves.
    let body = json!({"schema": 1, "display_name": "Cleo", "email": "not-an-email", "tags": [], "consent": {"email": "granted"}});
    let digest = material_digest(
        &json!({"domain": "cadence-app-record-v1", "install_id": store.install(), "context_id": context, "kind": "customer", "profile": body}),
    );
    let path = record_db_path(dir.path(), store.install()).unwrap();
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute(
        "INSERT INTO app_records(context_id,id,kind,revision,body,body_digest,created,updated) VALUES(?,?,'customer',1,?,?,0.0,0.0)",
        rusqlite::params![context, "customer-c", body.to_string(), digest],
    )
    .unwrap();
}

fn vip_segment() -> Vec<Predicate> {
    vec![Predicate::parse(&json!({"field": "tag", "op": "eq", "value": "vip"})).unwrap()]
}

fn base_all() -> AudienceBase {
    AudienceBase::parse(&json!({"mode": "all"})).unwrap()
}

#[test]
fn cad780_all_segment_custom_with_exclusion_counts() {
    let dir = TempDir::new().unwrap();
    let store = audience_file(&dir, "install-a");
    seed_valid(&store, "ctx-1");
    plant_legacy(&dir, &store, "ctx-1");
    store
        .app_segment_save("ctx-1", "seg-vip", None, "VIP", &vip_segment())
        .unwrap();
    store
        .app_exclusion_save(
            "ctx-1",
            "ex-hold",
            None,
            "Hold",
            &["customer-e".to_string()],
        )
        .unwrap();

    // All eligible: five rows minus invalid/denied/unknown = a and e.
    let all = store
        .app_audience_preview("ctx-1", &base_all(), None)
        .unwrap();
    assert_eq!(all["base_count"], 5);
    assert_eq!(all["final_count"], 2);

    // Saved segment: a and b match; b is denied, so only a survives.
    let base = AudienceBase::parse(&json!({"mode": "segment", "segment_id": "seg-vip"})).unwrap();
    let segment = store.app_audience_preview("ctx-1", &base, None).unwrap();
    assert_eq!(segment["base_count"], 2);
    assert_eq!(segment["final_count"], 1);

    // Exclusion list applies to every base mode.
    let excluded = store
        .app_audience_preview("ctx-1", &base_all(), Some("ex-hold"))
        .unwrap();
    assert_eq!(excluded["final_count"], 1);
    assert_eq!(excluded["exclusion_count"], 1);

    // Custom selection dedupes repeated IDs.
    let base = AudienceBase::parse(
        &json!({"mode": "custom", "customer_ids": ["customer-a", "customer-a", "customer-e"]}),
    )
    .unwrap();
    let custom = store
        .app_audience_preview("ctx-1", &base, Some("ex-hold"))
        .unwrap();
    assert_eq!(custom["base_count"], 2);
    assert_eq!(custom["final_count"], 1);
}

#[test]
fn cad780_final_exclusion_union_always_applies() {
    let dir = TempDir::new().unwrap();
    let store = audience_file(&dir, "install-a");
    seed_valid(&store, "ctx-1");
    plant_legacy(&dir, &store, "ctx-1");
    store
        .app_suppression_add("ctx-1", Some("esme@example.com"), None, "bounce")
        .unwrap();
    // Every ineligible row is custom-selected explicitly — including
    // the suppressed eligible one — and every one must still drop.
    let base = AudienceBase::parse(
        &json!({"mode": "custom", "customer_ids": ["customer-b", "customer-c", "customer-d", "customer-e"]}),
    )
    .unwrap();
    let preview = store.app_audience_preview("ctx-1", &base, None).unwrap();
    assert_eq!(preview["final_count"], 0);
    assert_eq!(preview["final_excluded"]["unsubscribed"], 1);
    assert_eq!(preview["final_excluded"]["invalid_email"], 1);
    assert_eq!(preview["final_excluded"]["no_consent"], 1);
    assert_eq!(preview["final_excluded"]["suppressed"], 1);
}

#[test]
fn cad780_predicate_grammar_refuses_arbitrary() {
    for body in [
        json!({"field": "email", "op": "eq", "value": "' OR '1'='1"}),
        json!({"field": "body", "op": "eq", "value": "vip"}),
        json!({"field": "tag", "op": "like", "value": "vip"}),
        json!({"field": "tag", "op": "eq", "value": "<img src=x onerror=1>"}),
        json!({"field": "install_id", "op": "eq", "value": "install-b"}),
        json!({"field": "tag", "op": "eq", "value": "a".repeat(200).as_str()}),
        json!({"field": "consent_email", "op": "eq", "value": "maybe"}),
    ] {
        assert!(
            Predicate::parse(&body).is_err(),
            "predicate admitted {body}"
        );
    }
    for body in [
        json!({"mode": "segment"}),
        json!({"mode": "custom"}),
        json!({"mode": "custom", "customer_ids": []}),
        json!({"mode": "custom", "customer_ids": ["../escape"]}),
        json!({"mode": "sql", "query": "SELECT 1"}),
        json!({"mode": "all", "segment_id": "seg-vip"}),
    ] {
        assert!(
            AudienceBase::parse(&body).is_err(),
            "audience base admitted {body}"
        );
    }
}

#[test]
fn cad780_stale_segment_edit_refuses() {
    let dir = TempDir::new().unwrap();
    let store = audience_file(&dir, "install-a");
    let created = store
        .app_segment_save("ctx-1", "seg-vip", None, "VIP", &vip_segment())
        .unwrap();
    assert_eq!(created["segment"]["revision"], 1);
    // Blind overwrite without the observed revision refuses.
    assert!(store
        .app_segment_save("ctx-1", "seg-vip", None, "VIP2", &vip_segment())
        .is_err());
    assert!(store
        .app_segment_save("ctx-1", "seg-vip", Some(9), "VIP2", &vip_segment())
        .is_err());
    let updated = store
        .app_segment_save("ctx-1", "seg-vip", Some(1), "VIP2", &vip_segment())
        .unwrap();
    assert_eq!(updated["segment"]["revision"], 2);
    assert_eq!(
        store.app_segment_show("ctx-1", "seg-vip").unwrap()["segment"],
        updated["segment"]
    );
}

#[test]
fn cad780_concurrent_segment_updates_one_wins() {
    let dir = TempDir::new().unwrap();
    let store = audience_file(&dir, "install-a");
    store
        .app_segment_save("ctx-1", "seg-vip", None, "VIP", &vip_segment())
        .unwrap();
    let barrier = std::sync::Barrier::new(2);
    let (first, second) = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            store.app_segment_save("ctx-1", "seg-vip", Some(1), "Racer A", &vip_segment())
        });
        let b = scope.spawn(|| {
            barrier.wait();
            store.app_segment_save("ctx-1", "seg-vip", Some(1), "Racer B", &vip_segment())
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    let wins = [&first, &second].iter().filter(|done| done.is_ok()).count();
    assert_eq!(wins, 1, "segment race planted two revisions");
    assert_eq!(
        store.app_segment_show("ctx-1", "seg-vip").unwrap()["segment"]["revision"],
        2
    );
}

#[test]
fn cad780_freeze_drift_invalidates() {
    let dir = TempDir::new().unwrap();
    let store = audience_file(&dir, "install-a");
    seed_valid(&store, "ctx-1");
    store
        .app_segment_save("ctx-1", "seg-vip", None, "VIP", &vip_segment())
        .unwrap();
    let base = AudienceBase::parse(&json!({"mode": "segment", "segment_id": "seg-vip"})).unwrap();
    let prepared = store
        .app_audience_prepare("ctx-1", "freeze-1", &base, None, 50)
        .unwrap();
    assert_eq!(prepared["freeze"]["final_count"], 1);
    let shown = store.app_audience_show("ctx-1", "freeze-1").unwrap();
    assert_eq!(shown["valid"], true);

    // Consent drift on the frozen member invalidates the freeze.
    let denied = profile("Amina", Some("amina@example.com"), "denied", &["vip"]);
    store
        .app_record_update("ctx-1", "customer-a", 1, &denied)
        .unwrap();
    let drifted = store.app_audience_show("ctx-1", "freeze-1").unwrap();
    assert_eq!(drifted["valid"], false);

    // So does a segment revision change behind a fresh freeze.
    let granted = profile("Amina", Some("amina@example.com"), "granted", &["vip"]);
    store
        .app_record_update("ctx-1", "customer-a", 2, &granted)
        .unwrap();
    store
        .app_audience_prepare("ctx-1", "freeze-2", &base, None, 50)
        .unwrap();
    let everybody =
        vec![Predicate::parse(&json!({"field": "source", "op": "eq", "value": "web"})).unwrap()];
    store
        .app_segment_save("ctx-1", "seg-vip", Some(1), "VIP", &everybody)
        .unwrap();
    let drifted = store.app_audience_show("ctx-1", "freeze-2").unwrap();
    assert_eq!(drifted["valid"], false);
}

#[test]
fn cad780_max_recipients_bound() {
    let dir = TempDir::new().unwrap();
    let store = audience_file(&dir, "install-a");
    seed_valid(&store, "ctx-1");
    // Two eligible recipients behind a ceiling of one refuses.
    assert!(store
        .app_audience_prepare("ctx-1", "freeze-1", &base_all(), None, 1)
        .is_err());
    assert!(store.app_audience_show("ctx-1", "freeze-1").is_err());
    let prepared = store
        .app_audience_prepare("ctx-1", "freeze-1", &base_all(), None, 2)
        .unwrap();
    assert_eq!(prepared["freeze"]["max_recipients"], 2);
}

#[test]
fn cad780_cross_install_context_isolation() {
    let dir = TempDir::new().unwrap();
    let a = audience_file(&dir, "install-a");
    let b = audience_file(&dir, "install-b");
    seed_valid(&a, "ctx-1");
    a.app_segment_save("ctx-1", "seg-vip", None, "VIP", &vip_segment())
        .unwrap();
    a.app_exclusion_save(
        "ctx-1",
        "ex-hold",
        None,
        "Hold",
        &["customer-a".to_string()],
    )
    .unwrap();
    a.app_suppression_add("ctx-1", Some("amina@example.com"), None, "bounce")
        .unwrap();
    // Nothing saved under install A is addressable from install B.
    assert!(b.app_segment_show("ctx-1", "seg-vip").is_err());
    assert!(b.app_exclusion_show("ctx-1", "ex-hold").is_err());
    assert_eq!(
        b.app_suppression_list("ctx-1").unwrap()["suppressions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    // Nothing saved under ctx-1 is addressable from ctx-2 either.
    assert!(a.app_segment_show("ctx-2", "seg-vip").is_err());
    assert_eq!(
        a.app_audience_preview("ctx-2", &base_all(), None).unwrap()["final_count"],
        0
    );
}

#[test]
fn cad780_sample_is_bounded() {
    let dir = TempDir::new().unwrap();
    let store = audience_file(&dir, "install-a");
    for index in 0..14 {
        let id = format!("customer-{index:02}");
        let email = format!("user{index}@example.com");
        store
            .app_record_create(
                "ctx-1",
                &id,
                &profile(&format!("User {index}"), Some(&email), "granted", &[]),
            )
            .unwrap();
    }
    let preview = store
        .app_audience_preview("ctx-1", &base_all(), None)
        .unwrap();
    assert_eq!(preview["final_count"], 14);
    assert!(
        preview["sample"].as_array().unwrap().len() <= 10,
        "preview sample discloses too much: {preview}"
    );
}

#[test]
fn cad780_suppression_add_remove_roundtrip() {
    let dir = TempDir::new().unwrap();
    let store = audience_file(&dir, "install-a");
    seed_valid(&store, "ctx-1");
    assert_eq!(
        store
            .app_audience_preview("ctx-1", &base_all(), None)
            .unwrap()["final_count"],
        2
    );
    store
        .app_suppression_add("ctx-1", Some("amina@example.com"), None, "bounce")
        .unwrap();
    assert_eq!(
        store
            .app_audience_preview("ctx-1", &base_all(), None)
            .unwrap()["final_count"],
        1
    );
    store
        .app_suppression_remove("ctx-1", Some("AMINA@example.com"), None)
        .unwrap();
    assert_eq!(
        store
            .app_audience_preview("ctx-1", &base_all(), None)
            .unwrap()["final_count"],
        2
    );
    // Customer-ID suppression works the same way.
    store
        .app_suppression_add("ctx-1", None, Some("customer-e"), "request")
        .unwrap();
    assert_eq!(
        store
            .app_audience_preview("ctx-1", &base_all(), None)
            .unwrap()["final_count"],
        1
    );
}
