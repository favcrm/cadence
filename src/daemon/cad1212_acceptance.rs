//! CAD-1212 independent acceptance check (written by the reviewer, not the
//! implementer; AGENTS.md "Gates and security work").
//!
//! CAD-1189 moved three daemon reads onto the lock-free `with_runtime_read`
//! on the premise that their callbacks write nothing. Through the real
//! dispatcher, as the operator, each of them must leave the daemon store
//! untouched (SQLite `total_changes` on the store connection).
use super::*;
use crate::test_seam::{scoped, Asserted};

#[test]
fn cad1212_switched_read_rpcs_write_nothing_to_the_store() {
    let dir = tempfile::Builder::new().prefix("c1212").tempdir().unwrap();
    let pm = dir.path().join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let pid = std::process::id();
    let source = format!("{}/workspace-apps/crm", env!("CARGO_MANIFEST_DIR"));
    let installed = scoped(Asserted::Operator, || {
        shared.dispatch("app_workspace_install", &json!({ "source": source }), pid)
    })
    .unwrap();
    let id = installed["install_id"].as_str().unwrap().to_string();

    for method in [
        "app_chat_descriptor",
        "app_install_team_show",
        "app_binding_list",
    ] {
        let before = shared.store.total_changes_for_test();
        let answer = scoped(Asserted::Operator, || {
            shared.dispatch(method, &json!({ "install_id": id }), pid)
        });
        let after = shared.store.total_changes_for_test();
        assert!(
            answer.is_ok(),
            "{method} must answer the operator: {answer:?}"
        );
        assert_eq!(
            after, before,
            "{method} wrote to the daemon store; it runs on the lock-free read path"
        );
    }
}
