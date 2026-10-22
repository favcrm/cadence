use super::*;
use crate::store::NewAgent;

/// A worker whose `app_run_dispatch` message is running under a
/// turn token — the shape `run_actor` holds when the adapter's
/// uncertain-error path calls `Shared::unknown`.
fn app_worker(dir: &Path, shared: &Arc<Shared>, alias: &str, id: &str) -> Message {
    shared
        .store
        .register_agent(&NewAgent {
            alias,
            provider: "fake",
            endpoint_kind: "managed",
            role: "worker",
            cwd: dir.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    shared
        .store
        .enqueue(alias, "work", None, id, "app_run_dispatch")
        .unwrap();
    // `take_queued` would demand the full run/step admission proof;
    // `unknown` only needs the row — a bare enqueue is the same
    // `app_run_dispatch` message the actor fences on.
    shared.store.message(id).unwrap().unwrap()
}

/// CAD-1142: the provider's raw uncertainty account never reaches
/// the public `attention` event or the agent row's `error` for an
/// app-owned turn — both carry the bounded class — while the
/// operator-private message row keeps the full detail.
#[test]
fn app_owned_unknown_publishes_class_keeps_raw_on_message_row() {
    let dir = tempfile::tempdir().unwrap();
    let shared = Shared::new(dir.path(), &ServeOptions::default()).unwrap();
    let message = app_worker(dir.path(), &shared, "w1", "app-m1");
    let raw = "Pi native input cleanup was not confirmed: Authorization: Bearer s3cr3t";
    let outcome = shared.unknown("w1", &message, raw);
    assert!(outcome.is_err());

    // Operator-private: the message row carries the provider's
    // own account verbatim, so reconcile still sees it.
    let stored = shared.store.message("app-m1").unwrap().unwrap();
    assert_eq!(stored.state, "unknown");
    assert_eq!(stored.error.as_deref(), Some(raw));
    assert_eq!(stored.result.unwrap()["error"], raw);

    // Public: the agent row's error is the bounded class, not prose.
    let agent = shared.store.agent("w1").unwrap();
    assert_eq!(agent.state, "attention");
    let error = agent.error.unwrap();
    assert!(
        error.contains("worker turn outcome is uncertain"),
        "{error}"
    );
    assert!(!error.contains("s3cr3t"), "{error}");
    assert!(!error.contains("Authorization"), "{error}");

    // Public: the attention event payload is the class too.
    let events = shared.store.events("w1", 0, 100).unwrap();
    let attention = events
        .iter()
        .find(|e| e.kind == "attention")
        .expect("attention event");
    let reason = attention.payload["reason"].as_str().unwrap();
    assert!(
        reason.contains("worker turn outcome is uncertain"),
        "{reason}"
    );
    assert!(!reason.contains("s3cr3t"), "{reason}");
}

/// A non-app turn keeps the provider's account on the public
/// surfaces — that detail is what the operator inspects and the
/// fence text must carry it.
#[test]
fn non_app_unknown_still_publishes_raw_detail() {
    let dir = tempfile::tempdir().unwrap();
    let shared = Shared::new(dir.path(), &ServeOptions::default()).unwrap();
    shared
        .store
        .register_agent(&NewAgent {
            alias: "w2",
            provider: "fake",
            endpoint_kind: "managed",
            role: "worker",
            cwd: dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    shared
        .store
        .enqueue("w2", "work", None, "m-plain", "user")
        .unwrap();
    let Take::Message(_m) = shared.store.take_queued("w2").unwrap() else {
        panic!("w2: queued message must be taken");
    };
    shared.store.mark_running("m-plain", "turn-1").unwrap();
    let message = shared.store.message("m-plain").unwrap().unwrap();
    let raw = "idle window exceeded with output pending";
    let _ = shared.unknown("w2", &message, raw);

    let agent = shared.store.agent("w2").unwrap();
    assert!(agent.error.unwrap().contains(raw));
    let events = shared.store.events("w2", 0, 100).unwrap();
    let attention = events
        .iter()
        .find(|e| e.kind == "attention")
        .expect("attention event");
    let reason = attention.payload["reason"].as_str().unwrap();
    assert!(reason.contains(raw), "{reason}");
    assert!(reason.contains("dispatch stall budget"), "{reason}");
    assert!(reason.contains("last provider event"), "{reason}");

    // The actor-exit restamp reads preserved_unknown_detail; it must keep
    // the enriched fence rather than falling back to the raw private row.
    let fence = shared.uncertain_fence_text("w2");
    assert!(fence.contains(raw), "{fence}");
    assert!(fence.contains("dispatch stall budget"), "{fence}");
    assert!(fence.contains("last provider event"), "{fence}");
}

#[test]
fn long_non_app_reason_keeps_budget_and_activity_in_the_bounded_fence() {
    let dir = tempfile::tempdir().unwrap();
    let shared = Shared::new(dir.path(), &ServeOptions::default()).unwrap();
    shared
        .store
        .register_agent(&NewAgent {
            alias: "w4",
            provider: "fake",
            endpoint_kind: "managed",
            role: "worker",
            cwd: dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: Some(r#"{"stall_secs":2400}"#),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    shared
        .store
        .enqueue("w4", "work", None, "m-long", "user")
        .unwrap();
    let Take::Message(_m) = shared.store.take_queued("w4").unwrap() else {
        panic!("w4: queued message must be taken");
    };
    shared.store.mark_running("m-long", "turn-long").unwrap();
    let message = shared.store.message("m-long").unwrap().unwrap();
    let raw = format!("provider refused: {}", "x".repeat(700));
    let _ = shared.unknown("w4", &message, &raw);

    // The private row stays verbatim, but the bounded public fence must
    // still lead with the dispatch budget and activity evidence.
    let stored = shared.store.message("m-long").unwrap().unwrap();
    assert_eq!(stored.error.as_deref(), Some(raw.as_str()));
    let agent = shared.store.agent("w4").unwrap();
    let error = agent.error.unwrap();
    assert!(error.contains("dispatch stall budget 2400s"), "{error}");
    assert!(error.contains("last provider event"), "{error}");
    let fence = shared.uncertain_fence_text("w4");
    assert!(fence.contains("dispatch stall budget 2400s"), "{fence}");
    assert!(fence.contains("last provider event"), "{fence}");
}

/// The restamp path (`preserved_unknown_detail` via actor exit,
/// relaunch-skip and `start_actor_locked`) must not republish the
/// app turn's operator-private account onto the public `agents.error`.
#[test]
fn app_owned_restamp_never_republishes_raw_detail() {
    let dir = tempfile::tempdir().unwrap();
    let shared = Shared::new(dir.path(), &ServeOptions::default()).unwrap();
    let message = app_worker(dir.path(), &shared, "w3", "app-m3");
    let raw = "Pi native input cleanup was not confirmed: Bearer s3cr3t";
    let _ = shared.unknown("w3", &message, raw);

    // A restamp — e.g. actor exit or relaunch — derives the fence
    // text from the stored account. For an app-owned unknown it
    // must yield the class, not the provider prose.
    let detail = shared.preserved_unknown_detail("w3", "generic");
    assert!(
        detail.contains("worker turn outcome is uncertain"),
        "{detail}"
    );
    assert!(!detail.contains("s3cr3t"), "{detail}");
    let fence = shared.uncertain_fence_text("w3");
    assert!(!fence.contains("s3cr3t"), "{fence}");

    // The raw account is still on the operator-private row.
    let stored = shared.store.message("app-m3").unwrap().unwrap();
    assert_eq!(stored.error.as_deref(), Some(raw));
}

/// CAD-1142 actor-fatal privacy: an app-owned turn's fatal provider
/// error is stamped at the fatal arm, so `run_actor`'s public fence
/// text — `agents.error` and the `attention` event payload — carries
/// the bounded class, not the provider's prose. The raw account is
/// already on the operator-private message row.
#[test]
fn app_owned_actor_fatal_publishes_class_only() {
    let raw = "pi 'prompt' refused: Authorization: Bearer s3cr3t";
    let stamped = Error::provider(raw).into_app_owned_fatal();
    assert!(stamped.is_app_owned_fatal());
    // The error's own text is unchanged — the wire and the stored
    // message.error still carry the provider's account.
    assert_eq!(stamped.to_string(), raw);
    let public = Shared::public_actor_fatal_reason(&stamped);
    assert_eq!(public, "worker turn failed: provider error");
    assert!(!public.contains("s3cr3t"), "{public}");
}

/// A fatal error on a non-app turn is never stamped, so the public
/// fence text keeps the provider's account verbatim — unchanged.
#[test]
fn non_app_actor_fatal_keeps_raw_detail() {
    let raw = "pi 'prompt' refused: provider declined the turn";
    let error = Error::provider(raw);
    assert!(!error.is_app_owned_fatal());
    assert_eq!(Shared::public_actor_fatal_reason(&error), raw);
    // Stamping only rewrites text for the variants that reach the
    // fatal arm; intercepted variants pass through untouched.
    let unknown = Error::unknown("outcome unknown");
    assert!(!unknown.is_app_owned_fatal());
    assert!(matches!(
        unknown.into_app_owned_fatal(),
        Error::OutcomeUnknown(_)
    ));
}

/// The ownership read fails closed: when `has_app_unknown` errors
/// (the `source` column is dropped through a side connection — the
/// cheapest real read failure, which also fails the companion
/// `FENCING_UNKNOWN_SQL` reads) the restamp publishes the bounded
/// class, not a stored provider account it could no longer prove
/// was non-app.
#[test]
fn ownership_read_error_restamps_safe_class() {
    let dir = tempfile::tempdir().unwrap();
    let shared = Shared::new(dir.path(), &ServeOptions::default()).unwrap();
    let message = app_worker(dir.path(), &shared, "w4", "app-m4");
    let raw = "Pi native input cleanup was not confirmed: Bearer s3cr3t";
    let _ = shared.unknown("w4", &message, raw);

    rusqlite::Connection::open(dir.path().join("cadence.sqlite3"))
        .unwrap()
        .execute_batch("ALTER TABLE messages DROP COLUMN source")
        .unwrap();
    assert!(shared.store.has_app_unknown("w4").is_err());
    assert!(shared.store.preferred_unknown_error("w4").is_err());

    let detail = shared.preserved_unknown_detail("w4", "generic");
    assert!(
        detail.contains("worker turn outcome is uncertain"),
        "{detail}"
    );
    assert!(!detail.contains("s3cr3t"), "{detail}");
}
