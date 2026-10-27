//! Independent CAD-1423 acceptance for per-tab push presence.
//!
//! The same browser push device may have several live tabs. A null report from
//! one tab must not erase another tab's visible room, while a different human
//! must not be able to write presence for the owned device or enroll its live
//! endpoint. After owner revocation, re-enrollment gets a fresh device id with
//! no old child metadata. This exercises the real store guards and FK path.

use super::*;
use crate::store::human_threads::{NotificationPrefs, VerifiedHuman};

#[test]
fn cad1423_tab_presence_and_reenrollment_are_owner_scoped() {
    let (_dir, store) = store();
    let owner = VerifiedHuman::with_name("issuer", "org", "alice", "Alice").unwrap();
    let peer = VerifiedHuman::with_name("issuer", "org", "bob", "Bob").unwrap();
    store
        .notification_prefs_set(&peer, &NotificationPrefs::default())
        .unwrap();
    let (room, created) = store.human_dm_open(&owner, &peer.id).unwrap();
    assert!(created);
    let (first_post, posted) = store
        .human_room_post(&peer, &room, "presence-history-1", "Read in the thread")
        .unwrap();
    assert!(posted);
    let group = store
        .notification_groups(&owner)
        .unwrap()
        .into_iter()
        .find(|group| group.room == room)
        .expect("received room must appear in notification history");
    assert_eq!(group.unread, 1);
    assert_eq!(
        store
            .human_rooms_for(&owner)
            .unwrap()
            .into_iter()
            .find(|summary| summary.id == room)
            .unwrap()
            .unread,
        1
    );
    store
        .human_room_mark_read(&owner, &room, first_post.seq)
        .unwrap();
    let viewed = store
        .notification_groups(&owner)
        .unwrap()
        .into_iter()
        .find(|group| group.room == room)
        .expect("All history must retain a room after its transcript is read");
    assert_eq!(viewed.unread, 0);
    assert_eq!(
        store
            .human_rooms_for(&owner)
            .unwrap()
            .into_iter()
            .find(|summary| summary.id == room)
            .unwrap()
            .unread,
        0
    );

    let (_, posted) = store
        .human_room_post(&peer, &room, "presence-history-2", "Unread before dismiss")
        .unwrap();
    assert!(posted);
    assert_eq!(
        store
            .notification_groups(&owner)
            .unwrap()
            .into_iter()
            .find(|group| group.room == room)
            .unwrap()
            .unread,
        1
    );
    store.notification_mark_all_read(&owner).unwrap();
    let dismissed = store
        .notification_groups(&owner)
        .unwrap()
        .into_iter()
        .find(|group| group.room == room)
        .expect("All history must retain the received room after dismissal");
    assert_eq!(dismissed.unread, 0);
    assert_eq!(
        store
            .human_rooms_for(&owner)
            .unwrap()
            .into_iter()
            .find(|summary| summary.id == room)
            .unwrap()
            .unread,
        1,
        "notification dismissal must not mark the transcript read"
    );
    let device = store
        .push_device_enroll(
            &owner,
            "https://fcm.googleapis.com/fcm/send/presence-acceptance",
            "p256dh",
            "auth",
            "session-digest",
        )
        .unwrap();

    assert!(store
        .push_presence_set(&owner, &device, "tab-a", Some(&room))
        .unwrap());
    assert!(store
        .push_device_visible(&device, &owner.id, &room)
        .unwrap());

    // Tab B is open elsewhere. Its explicit null must update only B's row,
    // leaving Tab A's current visible-room report effective for the device.
    assert!(store
        .push_presence_set(&owner, &device, "tab-b", None)
        .unwrap());
    assert!(
        store
            .push_device_visible(&device, &owner.id, &room)
            .unwrap(),
        "a null report from tab B must not clear tab A's visible room"
    );
    let reports: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM human_push_presence WHERE device=?",
            [&device],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reports, 1, "a null tab report removes only that tab's row");
    let surviving_tab: (String, String) = store
        .conn()
        .query_row(
            "SELECT tab,room FROM human_push_presence WHERE device=?",
            [&device],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(surviving_tab, ("tab-a".to_string(), room.clone()));

    // A different verified human cannot create or clear presence for this device.
    assert!(!store
        .push_presence_set(&peer, &device, "tab-a", None)
        .unwrap());
    assert!(store
        .push_device_visible(&device, &owner.id, &room)
        .unwrap());

    // Seed actual delivery-child metadata against one of the committed post
    // intents so the foreign-enrollment refusal and owner re-enrollment run
    // through the production SQLite foreign-key relationships.
    let intent: i64 = store
        .conn()
        .query_row(
            "SELECT id FROM human_notification_intents WHERE room=? AND recipient=? ORDER BY id LIMIT 1",
            [&room, &owner.id],
            |row| row.get(0),
        )
        .unwrap();
    store
        .push_delivery_claim(intent, &device, "old-lease", 30.0)
        .unwrap()
        .expect("live owner device can create its delivery row");

    // A different verified human cannot enroll the live endpoint or mutate
    // either child table; the original consent remains visible and deliverable.
    let endpoint = "https://fcm.googleapis.com/fcm/send/presence-acceptance";
    assert!(store
        .push_device_enroll(&peer, endpoint, "p256dh-peer", "auth-peer", "peer-session")
        .is_err());
    assert!(store
        .push_device_visible(&device, &owner.id, &room)
        .unwrap());
    assert_eq!(
        store.push_device_owner(&device).unwrap().as_deref(),
        Some(owner.id.as_str())
    );
    let old_presence: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM human_push_presence WHERE device=?",
            [&device],
            |row| row.get(0),
        )
        .unwrap();
    let old_deliveries: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM human_push_deliveries WHERE device=?",
            [&device],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        old_presence, 1,
        "foreign enrollment must preserve owned presence"
    );
    assert_eq!(
        old_deliveries, 1,
        "foreign enrollment must preserve delivery metadata"
    );

    // Re-enrollment is permitted only after the actual owner revokes. It mints
    // a new device id and does not transplant either old child row.
    assert!(store.push_device_revoke(&owner, &device).unwrap());
    let replacement = store
        .push_device_enroll(&owner, endpoint, "p256dh-new", "auth-new", "new-session")
        .expect("owner can re-enroll a revoked endpoint");
    assert_ne!(replacement, device, "re-enrollment must mint a fresh id");
    assert_eq!(
        store.push_device_owner(&replacement).unwrap().as_deref(),
        Some(owner.id.as_str())
    );
    for table in ["human_push_presence", "human_push_deliveries"] {
        for device_id in [&device, &replacement] {
            let rows: i64 = store
                .conn()
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE device=?"),
                    [device_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                rows, 0,
                "{table} metadata must not carry to the new consent"
            );
        }
    }
    assert!(!store
        .push_device_visible(&replacement, &owner.id, &room)
        .unwrap());
}

#[test]
fn cad1423_notification_history_is_complete_and_scope_limited() {
    let (_dir, store) = store();
    let owner = VerifiedHuman::with_name("issuer", "org", "owner", "Owner").unwrap();
    let mut expected_rooms = Vec::new();

    // More than the former 20-room cap, each with one real received post.
    for n in 0..21 {
        let peer =
            VerifiedHuman::with_name("issuer", "org", &format!("peer-{n}"), &format!("Peer {n}"))
                .unwrap();
        store
            .notification_prefs_set(&peer, &NotificationPrefs::default())
            .unwrap();
        let (room, created) = store.human_dm_open(&owner, &peer.id).unwrap();
        assert!(created);
        let (_, posted) = store
            .human_room_post(&peer, &room, &format!("history-{n}"), "Received message")
            .unwrap();
        assert!(posted);
        expected_rooms.push(room);
    }

    // A real conversation in another organization is outside this actor's history.
    let other = VerifiedHuman::with_name("issuer", "other-org", "other", "Other").unwrap();
    let other_peer = VerifiedHuman::with_name("issuer", "other-org", "peer", "Other peer").unwrap();
    store
        .notification_prefs_set(&other_peer, &NotificationPrefs::default())
        .unwrap();
    let (out_of_scope, created) = store.human_dm_open(&other, &other_peer.id).unwrap();
    assert!(created);
    let (_, posted) = store
        .human_room_post(&other_peer, &out_of_scope, "other-history", "Out of scope")
        .unwrap();
    assert!(posted);

    let groups = store.notification_groups(&owner).unwrap();
    assert_eq!(groups.len(), 21);
    let mut actual_rooms: Vec<String> = groups.iter().map(|group| group.room.clone()).collect();
    actual_rooms.sort();
    expected_rooms.sort();
    assert_eq!(actual_rooms, expected_rooms);
    assert!(!groups.iter().any(|group| group.room == out_of_scope));
    assert_eq!(groups.iter().map(|group| group.unread).sum::<i64>(), 21);
}
