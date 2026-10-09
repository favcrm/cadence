//! Bounded CAD-1177 acceptance for the operator connection-bound identity guard.
//!
//! This exercises the production guard used by `operator_connection` directly.
//! It does not prove full RPC admission, a live operator session/mount/binding,
//! local source CAS, or HTTP/browser operational acceptance.

use serde_json::{json, Value};

const RESERVED_IDENTITY_FIELDS: &[&str] = &[
    "alias",
    "by",
    "as",
    "actor",
    "operator",
    "lane",
    "pid",
    "pane",
    "recorded_via",
    "attribution",
    "sub",
];

const GUARDED_OPERATIONS: &[&str] = &["app tool invoke", "social draft action"];

#[test]
fn tool_selector_does_not_weaken_connection_bound_identity_guard() {
    // A legitimate tool selector is domain data, not caller identity.
    for verb in GUARDED_OPERATIONS {
        assert!(
            super::super::reject_operator_fields(verb, &json!({"tool_alias": "source.read"}))
                .is_ok(),
            "{verb} must not treat tool_alias as a caller identity claim"
        );

        // Each identity claim remains refused even when a valid selector is
        // present in the same request. In particular, public `alias` remains
        // reserved and is never made acceptable to operator_connection.
        for field in RESERVED_IDENTITY_FIELDS {
            let mut params = json!({"tool_alias": "source.read"});
            params
                .as_object_mut()
                .expect("constructed request is an object")
                .insert(
                    (*field).to_owned(),
                    Value::String("forged-caller".to_owned()),
                );

            let error = super::super::reject_operator_fields(verb, &params)
                .expect_err("reserved identity claims must be refused");
            match error {
                crate::error::Error::Rejected(message) => assert_eq!(
                    message,
                    format!(
                        "{verb} authority is connection-bound; request field '{field}' is not accepted"
                    )
                ),
                other => panic!("expected connection-bound rejection, got {other:?}"),
            }
        }
    }
}
