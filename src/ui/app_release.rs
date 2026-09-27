//! Operator controls for app bindings and reviewed artifact release.
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_routes_are_exact_and_do_not_infer_context_or_project() {
        assert!(matches!(route("/api/app-installations/install-a/bindings"), Some(Route::Bindings("install-a", None))));
        assert!(matches!(route("/api/app-installations/install-a/contexts/context-a/effects"), Some(Route::Effects(Some("install-a"), Some("context-a")))));
        assert!(matches!(route("/api/app-runs/run-a/effects"), Some(Route::Stage("run-a"))));
        assert!(matches!(route("/api/app-effects/effect-a/decide"), Some(Route::Decide("effect-a"))));
        for path in ["/api/app-installations/../bindings", "/api/app-installations/i/bindings/", "/api/app-installations/i/bindings/b/update/extra", "/api/app-runs/r/effects/e", "/api/app-effects/e/retry", "/api/app-effects/", "/api/app-installations/i/contexts/c/bindings/b"] {
            assert!(route(path).is_none(), "admitted {path}");
        }
    }
    #[test]
    fn release_schemas_reject_forged_authority_and_caller_content() {
        let valid = r#"{"slot":"publication","connection_id":"conn-a","request_id":"bind-a"}"#;
        assert!(serde_json::from_str::<Create>(valid).is_ok());
        for body in [
            r#"{"slot":"publication","connection_id":"conn-a","request_id":"bind-a","context_id":null}"#,
            r#"{"slot":"publication","connection_id":"conn-a","request_id":"bind-a","operator":true}"#,
            r#"{"slot":"publication","slot":"other","connection_id":"conn-a","request_id":"bind-a"}"#,
            r#"{"slot":"publication","connection_id":"conn-a","request_id":"bind-a","install_id":"other"}"#,
        ] { assert!(serde_json::from_str::<Create>(body).is_err(), "accepted {body}"); }
        for body in [r#"{"expected_revision":null}"#, r#"{"expected_revision":1,"expected_revision":2}"#, r#"{"expected_revision":1,"binding_id":"other"}"#] {
            assert!(serde_json::from_str::<Revoke>(body).is_err());
        }
        assert!(serde_json::from_str::<Stage>(r#"{"artifact_id":"a","slot":"publication","request_id":"r","title":"Title"}"#).is_ok());
        for field in ["body", "path", "provider", "run_id", "operator", "review_verdict", "grant"] {
            let mut body = serde_json::json!({"artifact_id":"a","slot":"publication","request_id":"r","title":"Title"});
            body[field] = serde_json::json!("forged");
            assert!(serde_json::from_value::<Stage>(body).is_err(), "accepted {field}");
        }
        for body in [r#"{"digest":"d","decision":"retry"}"#,r#"{"digest":"d","decision":"accept","effect_id":"other"}"#,r#"{"digest":"d","digest":"other","decision":"accept"}"#] {
            assert!(serde_json::from_str::<Decision>(body).is_err());
        }
    }
}
