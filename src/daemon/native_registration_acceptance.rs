//! CAD-1159 independently authored native row-reuse DATA acceptance.
//! Body ownership: aos159-constructor-guard; implementers register only.
//! Real Store registration/updates and SAME production select, not a mirrored
//! validator. No Root grant, runtime, helper, retirement or provider is created.
//! Temporary cwd/model metadata cannot qualify a protected launch/generation.
use super::Registrations;
use crate::store::Store;
use rusqlite::{Connection, OpenFlags};
use serde_json::json;

#[test]
fn native_registration_changed_model_refuses_without_row_or_cache_repair() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native-registration-guard.db");
    let store = Store::open(&path).unwrap();
    let cwd = dir.path().to_str().unwrap();
    let alias = "native-guard-worker";
    let election = r#"{"model":"openai-codex/gpt-6.1-sol"}"#;
    let mut registrations = Registrations::new(&store);

    // Real first registration and matching saved-row selection. This proves
    // the DATA branch is reachable, NOT that a prior namespace family retired.
    let first = registrations.select(alias, cwd, election).unwrap();
    let matching = registrations.select(alias, cwd, election).unwrap();
    assert_eq!(first.created, matching.created);
    assert_eq!(matching.alias, alias);
    assert_eq!(matching.provider, "pi");
    assert_eq!(matching.endpoint_kind, "managed");
    assert_eq!(matching.role, "worker");
    assert_eq!(matching.sandbox, "workspace-write");
    assert_eq!(matching.cwd, cwd);
    assert_eq!(
        matching.params,
        Some(json!({"model":"openai-codex/gpt-6.1-sol"}))
    );
    assert_eq!(store.agents().unwrap().len(), 1);
    assert!(registrations.aliases.contains(alias));

    // Commit adversarial setup through the actual Store update API. The new
    // model is valid Store syntax, not launch-policy approval. Setup is before
    // the refusal probe: select must neither silently adopt it nor repair it
    // back into the old election, delete/re-register, or change cache history.
    let substituted = json!({"model":"devin/swe-2-high"});
    store.set_params(alias, &substituted).unwrap();
    let changed = store.agent(alias).unwrap();
    assert_eq!(changed.params.as_ref(), Some(&substituted));
    assert_eq!(changed.created, first.created);
    assert!(changed.pid.is_none());
    assert!(changed.endpoint.is_none());
    assert!(changed.generation.is_none());

    // Observe the real SQLite file read-only. data_version detects commits
    // from Store's separate connection, including history/model writes that
    // could leave the selected Agent fields unchanged. No authorizer or
    // authority callback is installed and no recovery-writing URI is used.
    let observer = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let before_version = observer
        .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
        .unwrap();
    // Debug covers every actual Agent field, not a handpicked model mirror.
    let before_rows = format!("{:?}", store.agents().unwrap());
    let before_cache = registrations.aliases.clone();

    match registrations.select(alias, cwd, election) {
        Err(crate::Error::Rejected(message)) => {
            assert_eq!(message, "native saved registration differs from election")
        }
        _ => panic!("changed real saved model bypassed native registration refusal"),
    }
    assert_eq!(
        format!("{:?}", store.agents().unwrap()),
        before_rows,
        "refusal adopted, repaired, deleted or changed the durable row"
    );
    assert_eq!(
        registrations.aliases, before_cache,
        "refusal changed DATA history"
    );
    assert_eq!(
        observer
            .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        before_version,
        "refusal committed a database/model/history mutation"
    );

    // Additive four-P2 DATA coverage. Never turn creation timestamps, detached
    // states or message rows into a native family/currentness certificate.
    assert!(first.created.is_finite() && first.created > 0.0);
    assert_eq!(registrations.created.len(), 1);
    assert_eq!(registrations.created.get(alias), Some(&first.created));

    fn refuse_unchanged(
        registrations: &mut Registrations<'_>,
        observer: &Connection,
        alias: &str,
        cwd: &str,
        election: &str,
        expected: &str,
    ) {
        let store = registrations.store;
        let rows = format!("{:?}", store.agents().unwrap());
        let history = format!("{:?}", store.messages(alias).unwrap());
        let aliases = registrations.aliases.clone();
        let created = registrations.created.clone();
        let version: i64 = observer
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        match registrations.select(alias, cwd, election) {
            Err(crate::Error::Rejected(message)) => assert_eq!(message, expected),
            _ => panic!("native refusal did not reach intended boundary: {expected}"),
        }
        assert_eq!(
            format!("{:?}", store.agents().unwrap()),
            rows,
            "refusal changed registry rows"
        );
        assert_eq!(
            format!("{:?}", store.messages(alias).unwrap()),
            history,
            "refusal reconciled/deleted/changed history"
        );
        assert_eq!(
            registrations.aliases, aliases,
            "refusal changed remembered aliases"
        );
        assert_eq!(
            registrations.created, created,
            "refusal adopted a new creation witness"
        );
        assert_eq!(
            observer
                .query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            version,
            "refusal committed a write/history repair"
        );
    }

    // Each negative has its own genuinely registered row. Setup commits occur
    // BEFORE observation; no reset/reconcile is needed to make another case run.
    for (fenced_alias, state, error) in [
        ("native-guard-attention", "attention", None),
        (
            "native-guard-error",
            "stopped",
            Some("retained registration error"),
        ),
    ] {
        let original = registrations.select(fenced_alias, cwd, election).unwrap();
        store
            .enqueue(
                fenced_alias,
                "保留歷史🔎",
                None,
                &format!("{fenced_alias}-history"),
                "cli",
            )
            .unwrap();
        store
            .set_state_detached(fenced_alias, state, error)
            .unwrap();
        let fenced = store.agent(fenced_alias).unwrap();
        assert_eq!(fenced.created, original.created);
        assert_eq!(fenced.state, state);
        assert_eq!(fenced.error.as_deref(), error);
        assert!(!store.has_unknown(fenced_alias).unwrap());
        assert!(
            fenced.enabled
                && fenced.pid.is_none()
                && fenced.endpoint.is_none()
                && fenced.generation.is_none()
        );
        refuse_unchanged(
            &mut registrations,
            &observer,
            fenced_alias,
            cwd,
            election,
            "native saved registration is fenced",
        );
    }

    // Genuine non-nudge UNKNOWN through Store's durable lifecycle APIs, not
    // a fake has_unknown callback or direct SQL state injection. The row itself
    // stays starting/error-free, so the history predicate is the refusing gate.
    let unknown_alias = "native-guard-unknown";
    registrations.select(unknown_alias, cwd, election).unwrap();
    let message_id = "native-guard-unknown-message";
    store
        .enqueue(unknown_alias, "實際 DATA 歷史🔎", None, message_id, "cli")
        .unwrap();
    store
        .mark_running(message_id, "data-turn-marker-not-provider-proof")
        .unwrap();
    store
        .orphan_running(unknown_alias, "durable uncertain DATA outcome")
        .unwrap();
    let uncertain = store.message(message_id).unwrap().unwrap();
    assert_eq!(uncertain.state, "unknown");
    assert_ne!(uncertain.source, "nudge");
    assert_eq!(
        store.unknown_messages(unknown_alias).unwrap(),
        vec![message_id.to_owned()]
    );
    assert!(store.has_unknown(unknown_alias).unwrap());
    let agent = store.agent(unknown_alias).unwrap();
    assert_eq!(agent.state, "starting");
    assert!(
        agent.enabled
            && agent.error.is_none()
            && agent.pid.is_none()
            && agent.endpoint.is_none()
            && agent.generation.is_none()
    );
    refuse_unchanged(
        &mut registrations,
        &observer,
        unknown_alias,
        cwd,
        election,
        "native saved registration is fenced",
    );

    // Real stop/removal/re-registration ABA; no invented row-incarnation or
    // timestamp setter. The cache must retain the FIRST committed witness.
    let aba_alias = "native-guard-aba";
    let original = registrations.select(aba_alias, cwd, election).unwrap();
    assert!(original.created.is_finite() && original.created > 0.0);
    store
        .set_state_detached(aba_alias, "stopped", None)
        .unwrap();
    assert!(store
        .remove_agent(
            aba_alias,
            false,
            &json!({"by":"DATA acceptance","by_kind":"test"})
        )
        .unwrap()
        .is_empty());
    assert!(store.agent(aba_alias).is_err());
    store
        .register_agent(&super::registration(aba_alias, cwd, election))
        .unwrap();
    let replacement = store.agent(aba_alias).unwrap();
    assert!(replacement.created.is_finite() && replacement.created > 0.0);
    assert_ne!(
        replacement.created, original.created,
        "ABA setup did not produce a distinct real creation witness"
    );
    assert_eq!(replacement.provider, original.provider);
    assert_eq!(replacement.endpoint_kind, original.endpoint_kind);
    assert_eq!(replacement.role, original.role);
    assert_eq!(replacement.sandbox, original.sandbox);
    assert_eq!(replacement.params, original.params);
    assert_eq!(replacement.cwd, original.cwd);
    assert_eq!(replacement.state, "starting");
    assert!(replacement.enabled && replacement.error.is_none());
    assert!(replacement.instructions.is_none() && replacement.team_role.is_none());
    assert!(
        replacement.thread_id.is_none()
            && replacement.session_id.is_none()
            && replacement.model.is_none()
            && replacement.effort.is_none()
            && replacement.pid.is_none()
            && replacement.pid_start.is_none()
            && replacement.endpoint.is_none()
            && replacement.generation.is_none()
    );
    assert!(!store.has_unknown(aba_alias).unwrap());
    assert_eq!(
        registrations.created.get(aba_alias),
        Some(&original.created)
    );
    refuse_unchanged(
        &mut registrations,
        &observer,
        aba_alias,
        cwd,
        election,
        "native saved registration continuity lost",
    );

    // Actual native pre-effect identifier guard and select ingress. Unicode is
    // still message/prompt DATA, not a normalized/truncated registry identity.
    let longest = "a".repeat(64);
    for valid in ["a", "worker-9", longest.as_str()] {
        super::require_task_alias(valid).unwrap();
    }
    let too_long = "a".repeat(65);
    for invalid in [
        "",
        "合成研究員🔎",
        too_long.as_str(),
        "Upper",
        "two words",
        "under_score",
        "-leading",
    ] {
        let rows = format!("{:?}", store.agents().unwrap());
        let aliases = registrations.aliases.clone();
        let created = registrations.created.clone();
        let version: i64 = observer
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        let expected = "native task alias must be 1-64 lowercase letters, digits or hyphens and not start with '-'";
        for refusal in [
            super::require_task_alias(invalid),
            registrations.select(invalid, cwd, election).map(|_| ()),
        ] {
            match refusal {
                Err(crate::Error::Rejected(message)) => assert_eq!(message, expected),
                _ => panic!("incompatible alias bypassed real native identifier guard"),
            }
        }
        assert_eq!(format!("{:?}", store.agents().unwrap()), rows);
        assert_eq!(registrations.aliases, aliases);
        assert_eq!(registrations.created, created);
        assert_eq!(
            observer
                .query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            version,
            "invalid alias registered/deleted/changed history"
        );
    }
}
