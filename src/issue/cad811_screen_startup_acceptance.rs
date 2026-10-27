//! Independent CAD-811 acceptance for the opt-in ordered multi-script frame
//! startup contract. This check owns no screen validator or renderer logic.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{app, app_screen_pkg};

const TAG: &str = "crm";
const ORIGIN: &str = "https://cadence.example";

fn sha256(text: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(text.as_bytes()))
}

fn files(assets: &[(&str, &str)], startup: Option<Value>) -> BTreeMap<String, String> {
    let declarations: Vec<Value> = assets
        .iter()
        .map(|(name, body)| {
            let media_type = if name.ends_with(".js") {
                "text/javascript"
            } else if name.ends_with(".css") {
                "text/css"
            } else {
                "application/json"
            };
            json!({"name": name, "media_type": media_type, "sha256": sha256(body), "size": body.len()})
        })
        .collect();
    let mut manifest = json!({
        "contract": "app-screens/v2",
        "app": "crm",
        "entry": "client.js",
        "assets": declarations,
        "provenance": {
            "source_digest": sha256("source"),
            "sdk_digest": sha256("sdk"),
            "toolchain_digest": sha256("toolchain")
        },
        "may": ["records.call"],
        "host_contract": "screen-records.v1",
        "operations": ["records.list"],
        "tools": {}
    });
    if let Some(startup) = startup {
        manifest["startup"] = startup;
    }
    let mut out = BTreeMap::from([
        (
            "app.md".to_owned(),
            "---\napp: crm\ntitle: CRM\nversion: '1'\nneeds:\n  requires:\n    contracts:\n      app-screens: [1]\n      screen-records: [1]\n      screen-startup: [1]\n---\n\nCRM\n".to_owned(),
        ),
        (
            format!("screens/{TAG}/screens.json"),
            manifest.to_string(),
        ),
    ]);
    for (name, body) in assets {
        out.insert(format!("screens/{TAG}/{name}"), (*body).to_owned());
    }
    out
}

fn render(files: &BTreeMap<String, String>) -> crate::error::Result<String> {
    let package = app_screen_pkg::extract(files, TAG)?;
    crate::daemon::render_package_frame_html("csp-nonce", TAG, "bridge-nonce", 7, &package, ORIGIN)
}

#[test]
fn cad811_screen_startup_contract_and_refusal_acceptance() {
    // Feature admission is explicit package metadata, distinct from the
    // existing screen-records contract declaration.
    let approved_metadata =
        app::parse_manifest(files(&[("client.js", "entry")], None)["app.md"].as_str())
            .expect("valid app manifest");
    approved_metadata
        .requires
        .check(
            &cadence_core::app_compat::host(env!("CARGO_PKG_VERSION"))
                .expect("host contract registry"),
        )
        .expect("screen-startup feature is advertised by a ready host");

    // Legacy declarations without startup still run only client.js; an
    // additional declared JavaScript asset remains inert.
    let legacy = files(
        &[
            ("client.js", "window.order=['entry'];"),
            ("extra.js", "window.order.push('extra');"),
        ],
        None,
    );
    let legacy_html = render(&legacy).expect("legacy package remains mountable");
    assert!(legacy_html.contains("window.order=['entry'];"));
    assert!(!legacy_html.contains("window.order.push('extra');"));

    // Explicit startup executes the declared members in order, with the
    // entry exactly once and last, in the same frame document.
    let ordered = files(
        &[
            ("vendor.js", "window.order=['vendor'];"),
            ("client.js", "window.order.push('entry');"),
        ],
        Some(json!(["vendor.js", "client.js"])),
    );
    let ordered_html = render(&ordered).expect("valid ordered startup");
    let mut unadvertised = ordered.clone();
    let metadata = unadvertised.get_mut("app.md").unwrap();
    *metadata = metadata.replace("      screen-startup: [1]\n", "");
    assert!(
        render(&unadvertised).is_err(),
        "startup accepted without screen-startup feature metadata"
    );
    let vendor_at = ordered_html.find("window.order=['vendor'];").unwrap();
    let entry_at = ordered_html.find("window.order.push('entry');").unwrap();
    assert!(
        vendor_at < entry_at,
        "scripts emitted out of declared order"
    );
    assert_eq!(
        ordered_html.matches("window.order.push('entry');").count(),
        1
    );

    // Every malformed startup and every mismatch between its list and the
    // exact declared JS members refuses before a complete frame document.
    for startup in [
        Value::Null,
        json!([]),
        json!("client.js"),
        json!(["client.js", "client.js"]),
        json!(["../vendor.js", "client.js"]),
        json!(["unlisted.js", "client.js"]),
        json!(["theme.css", "client.js"]),
        json!(["client.js", "vendor.js"]),
        json!(["vendor.js"]),
    ] {
        let bad = files(
            &[
                ("vendor.js", "window.vendor=true;"),
                ("client.js", "window.entry=true;"),
                ("theme.css", "body{}"),
            ],
            Some(startup.clone()),
        );
        assert!(render(&bad).is_err(), "accepted invalid startup {startup}");
    }

    // Integrity and document-token refusals also return no completed frame.
    let mut extra = ordered.clone();
    extra.insert(
        format!("screens/{TAG}/unlisted.js"),
        "window.bad=true;".into(),
    );
    assert!(render(&extra).is_err(), "undeclared live member admitted");
    let mut digest_mismatch = ordered.clone();
    digest_mismatch.insert(format!("screens/{TAG}/vendor.js"), "tampered".into());
    assert!(
        render(&digest_mismatch).is_err(),
        "digest mismatch admitted"
    );
    let injected = files(
        &[
            (
                "vendor.js",
                "window.safe=true;</script><script>window.bad=true",
            ),
            ("client.js", "window.entry=true;"),
        ],
        Some(json!(["vendor.js", "client.js"])),
    );
    assert!(
        render(&injected).is_err(),
        "script end-token asset admitted"
    );
}
