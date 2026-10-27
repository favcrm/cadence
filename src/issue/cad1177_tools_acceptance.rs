//! CAD-1177 independent QA acceptance: a tools-only app must not need a
//! dummy workflow, but making workflows optional must not admit an empty,
//! forged or integrity-invalid tool package. Exercises the real install
//! snapshot validator; no provider, session, filesystem or actor fake.
//!
//! Authored by the conversation's QA, not the implementation worker.
//! The worker may wire this module but must not edit/weaken its assertions.
//! This check does not claim daemon/HTTP caller or live-provider acceptance.

use std::collections::HashSet;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::issue::app;

const MANIFEST: &str = "---\napp: tools-only\ntitle: Tools only\nversion: '1.0.0'\nneeds:\n  capabilities:\n    source:\n      schema: 1\n      capability: social.read\n      version: 1\n      action: list_posts\n      resource_kind: connection_account\n      effect: read\n---\nA source tool with no workflow or assigned agent.\n";
const CLIENT: &str = "void 0;";

fn package(declaration: Value, client: &str) -> Vec<(String, String)> {
    vec![
        ("app.md".into(), MANIFEST.into()),
        ("screens/main/screens.json".into(), declaration.to_string()),
        ("screens/main/client.js".into(), client.into()),
    ]
}

fn accepted(files: Vec<(String, String)>) -> bool {
    app::validate_texts(files, &HashSet::new(), &[]).is_ok()
}

#[test]
fn cad1177_tools_only_install_admits_only_verified_declared_tools() {
    let inert_provenance = format!("sha256:{}", "0".repeat(64));
    let declaration = json!({
        "contract": "app-screens/v2",
        "app": "tools-only",
        "entry": "client.js",
        "host_contract": "screen-actions.v1",
        "may": ["tools.invoke"],
        "tools": {"instagram.read": "source"},
        "assets": [{
            "name": "client.js",
            "media_type": "text/javascript",
            "sha256": format!("sha256:{:x}", Sha256::digest(CLIENT.as_bytes())),
            "size": CLIENT.len()
        }],
        "provenance": {
            "source_digest": inert_provenance,
            "sdk_digest": inert_provenance,
            "toolchain_digest": inert_provenance
        }
    });

    // Positive control: no workflows/ member exists. An installed simple
    // tool must not acquire a made-up workflow merely to pass admission.
    let valid = package(declaration.clone(), CLIENT);
    assert!(valid
        .iter()
        .all(|(name, _)| !name.starts_with("workflows/")));
    assert!(
        accepted(valid),
        "valid declared tools-only app must install without a workflow"
    );

    assert!(
        !accepted(vec![("app.md".into(), MANIFEST.into())]),
        "capabilities alone must not turn an empty app into an executable plugin"
    );
    assert!(
        !accepted(package(declaration.clone(), "void 1;")),
        "mismatched shipped JS integrity must refuse tools-only admission"
    );
    for (label, change) in [
        (
            "undeclared capability slot",
            ("tools", json!({"instagram.read": "missing"})),
        ),
        (
            "unknown host contract",
            ("host_contract", json!("screen-actions.v999")),
        ),
        (
            "v1 claiming new authority",
            ("contract", json!("app-screens/v1")),
        ),
        ("foreign app identity", ("app", json!("other-app"))),
        ("no declared tool", ("tools", json!({}))),
        ("unknown method", ("may", json!(["provider.raw-http"]))),
    ] {
        let mut invalid = declaration.clone();
        invalid[change.0] = change.1;
        assert!(
            !accepted(package(invalid, CLIENT)),
            "tools-only install must refuse {label}"
        );
    }
}
