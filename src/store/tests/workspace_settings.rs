//! CAD-2378: the company time zone resolves stored, then the first social
//! connection's zone, then UTC; a bad name never reaches the table.

use super::*;
use crate::store::workspace_settings::ZoneSource;
use chrono::TimeZone;

fn bind_social_zone(s: &Store, zone: &str) {
    let digest = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    let config = json!({"schema":1,"install_id":"install-a","context":{"id":"brand"},
        "bundle_digest":digest,"connection_id":"local-id",
        "publish":{"destination_id":"ig-1","destination_label":"ig-1",
            "toolkit":"instagram","timezone":zone}});
    s.app_binding_create_destination("install-a", Some("brand"), "publication", &config, "use-ig")
        .unwrap();
}

#[test]
fn company_zone_defaults_to_the_first_social_zone_then_utc_and_a_stored_zone_wins() {
    let (_dir, s) = store();
    let (zone, source) = s.company_zone().unwrap();
    assert_eq!((zone.name(), source), ("UTC", ZoneSource::Default));

    bind_social_zone(&s, "Europe/London");
    let (zone, source) = s.company_zone().unwrap();
    assert_eq!(
        (zone.name(), source),
        ("Europe/London", ZoneSource::SocialConnection)
    );

    s.set_company_zone("Asia/Hong_Kong", "operator").unwrap();
    let (zone, source) = s.company_zone().unwrap();
    assert_eq!(
        (zone.name(), source),
        ("Asia/Hong_Kong", ZoneSource::Workspace)
    );
}

#[test]
fn a_routine_that_names_no_zone_gets_the_company_zone_and_a_named_zone_is_kept() {
    let (_dir, s) = store();
    s.set_company_zone("Asia/Hong_Kong", "operator").unwrap();
    assert_eq!(s.schedule_zone_or_company(None).unwrap(), "Asia/Hong_Kong");
    assert_eq!(
        s.schedule_zone_or_company(Some("America/New_York".into()))
            .unwrap(),
        "America/New_York"
    );
}

#[test]
fn an_invalid_zone_name_is_refused_and_stores_nothing() {
    let (_dir, s) = store();
    for bad in [
        "",
        "  ",
        "Mars/Base",
        "GMT+8 Hong Kong",
        "asia/hong_kong/../x",
    ] {
        let err = s.set_company_zone(bad, "operator").unwrap_err().to_string();
        assert!(err.contains("invalid IANA timezone"), "{bad}: {err}");
    }
    assert_eq!(s.company_zone().unwrap().1, ZoneSource::Default);
}

/// A routine at 09:00 company time still fires at 09:00 HKT (01:00Z) whatever
/// zone a member reads it in, and that instant reads 01:00 in London.
#[test]
fn a_nine_oclock_company_routine_runs_at_nine_hkt_and_reads_one_in_london() {
    let (zone, _) = {
        let (_dir, s) = store();
        s.set_company_zone("Asia/Hong_Kong", "operator").unwrap();
        s.company_zone().unwrap()
    };
    let after = chrono::Utc
        .with_ymd_and_hms(2026, 11, 16, 12, 0, 0)
        .unwrap()
        .timestamp();
    let due = crate::store::routines::next_due_instant(Some("0 9 * * *"), None, zone.name(), after)
        .unwrap();
    let utc = chrono::DateTime::from_timestamp(due, 0).unwrap();
    assert_eq!(utc.to_rfc3339(), "2026-11-17T01:00:00+00:00");
    let london: chrono_tz::Tz = "Europe/London".parse().unwrap();
    assert_eq!(
        utc.with_timezone(&london).format("%H:%M").to_string(),
        "01:00"
    );
}
