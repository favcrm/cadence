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
}
