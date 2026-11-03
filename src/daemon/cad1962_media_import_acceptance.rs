//! Independently authored CAD-1962 declaration/refusal acceptance.
//! Keep these assertions independent of the media-import implementation.
//! Register as a child of cad1482_independent_media_refusal to reuse its
//! real native socket, operator session, image, and HTTP board fixtures.

#![cfg(all(test, feature = "test-seam"))]

use super::{board, media_screen, Fx, APP_MD, WORKFLOW_MD};
use crate::store::app_tools::AppToolUpload;
use crate::test_seam::Seam;
use serde_json::{json, Value};
use std::io::Read;
use std::sync::atomic::Ordering;

/// Install and consume a real media-only package, and retain an image
/// owned by that installation. No permission or mount proof is mocked.
fn mounted_import_params(fx: &Fx, app: &str, may: &[&str]) -> Value {
    let source = fx._dir.path().join(app);
    std::fs::create_dir_all(source.join("workflows")).unwrap();
    std::fs::write(
        source.join("app.md"),
        APP_MD.replace("designer-fixture", app),
    )
    .unwrap();
    std::fs::write(source.join("workflows/create-image.md"), WORKFLOW_MD).unwrap();
    media_screen(&source);
    let declaration = source.join("screens/media/screens.json");
    let mut package: Value = serde_json::from_slice(&std::fs::read(&declaration).unwrap()).unwrap();
    package["app"] = json!(app);
    package["may"] = json!(may);
    package.as_object_mut().unwrap().remove("tools");
    std::fs::write(&declaration, serde_json::to_vec_pretty(&package).unwrap()).unwrap();

    let installed = fx
        .op(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        )
        .expect("install acceptance package");
    let install = installed["install_id"].as_str().unwrap();
    fx.op(
        "app_local_install_approve",
        json!({"install_id": install, "digest": installed["digest"]}),
    )
    .expect("approve acceptance package");
    let created = fx
        .op(
            "app_context_create",
            json!({"install_id": install, "label": app,
                "input_defaults": {}, "request_id": format!("{app}-context")}),
        )
        .expect("create active mount context");
    let context = created["context"]["id"]
        .as_str()
        .or_else(|| created["id"].as_str())
        .unwrap();
    let (token, key) = fx.session();
    let minted = fx
        .op(
            "app_screen_mint",
            json!({"install_id": install, "context_id": context,
                "tag": "media", "generation": 1,
                "token": token, "key": key, "origin": "loopback"}),
        )
        .expect("mint real media mount");
    assert_eq!(minted["media"]["contract"], "screen-media.v1");
    assert_eq!(minted["media"]["may"], json!(may));
    let nonce = minted["mount"]
        .as_str()
        .unwrap()
        .strip_prefix("/api/app-screen/")
        .unwrap();
    fx.op("app_screen_consume", json!({"nonce": nonce}))
        .expect("consume media mount before requesting import");
    let action = minted["action_token"].as_str().unwrap();

    let receipt = format!("{app}-receipt");
    let upload_request = format!("{app}-upload");
    fx.shared
        .store
        .app_tool_upload_record(AppToolUpload {
            id: &receipt,
            request: &upload_request,
            install,
            alias: "image.upload",
            slot: "image",
            result: &json!({"kind": "social.user_upload"}),
            media_type: "image/png",
            bytes: &fx.original,
        })
        .expect("retain an intact image owned by the mounting installation");
    json!({"action_token": action, "token": token, "key": key,
        "origin": "loopback", "receipt_id": receipt,
        "request_id": format!("{app}-import")})
}

fn catalog(fx: &Fx, params: &Value) -> Value {
    fx.op(
        "app_media_read",
        json!({"action_token": params["action_token"],
            "token": params["token"], "key": params["key"],
            "origin": "loopback", "input": {"kind": "catalog", "limit": 20}}),
    )
    .expect("same consumed mount can read the real catalog")["assets"]
        .clone()
}

fn import_http(port: u16, seam: &str, params: &Value) -> (u16, Value) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let host = format!("cadence-{port}.localhost:{port}");
    let body = json!({"action_token": params["action_token"],
        "receipt_id": params["receipt_id"], "request_id": params["request_id"]});
    let mut response = agent
        .post(format!("http://127.0.0.1:{port}/api/app-media/import"))
        .header("Host", &host)
        .header("X-Cadence-Board", "1")
        .header("Origin", format!("http://{host}"))
        .header(crate::test_seam::AS_HEADER, "operator")
        .header(crate::test_seam::TOKEN_HEADER, seam)
        .header("Content-Type", "application/json")
        .header(
            "Cookie",
            format!(
                "cadence_operator_{port}={}",
                params["token"].as_str().unwrap()
            ),
        )
        .header("X-Cadence-Session", params["key"].as_str().unwrap())
        .send(body.to_string())
        .expect("HTTP import reaches the board");
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(64 * 1024)
        .read_to_end(&mut bytes)
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        panic!(
            "HTTP import must return JSON: {}",
            String::from_utf8_lossy(&bytes)
        )
    });
    (status, body)
}

#[test]
fn cad1962_import_requires_its_declared_permission_on_native_and_http_peers() {
    let fx = Fx::boot();
    // These are writable operator mounts; the negative one is restricted
    // by its package declaration, not by a read_only session or mount.
    let read = mounted_import_params(&fx, "cad1962-reader", &["media.read"]);
    let mut import =
        mounted_import_params(&fx, "cad1962-importer", &["media.read", "media.import"]);
    let before = catalog(&fx, &read);
    assert_eq!(
        before.as_array().unwrap().len(),
        1,
        "fixture's existing image"
    );

    let error = fx
        .op("app_media_import", read.clone())
        .expect_err("media.read alone must not authorize import")
        .to_string();
    assert!(
        error.contains("screen does not declare media.import"),
        "must hit the declaration guard, not an unrelated failure: {error}"
    );
    assert_eq!(
        catalog(&fx, &read),
        before,
        "refused import created no catalog row"
    );

    // An otherwise authorized request must not silently strip selectors.
    // The same mount, receipt, and request id succeed after the extras
    // are removed, so these refusals cannot be blanket service failures.
    for (field, value) in [
        ("install_id", json!("forged-install")),
        ("asset_id", json!(format!("med-{}", "a".repeat(32)))),
        ("creator", json!({"kind": "operator"})),
    ] {
        let mut forged = import.clone();
        forged[field] = value;
        let error = fx
            .op("app_media_import", forged)
            .expect_err("extra import parameter must refuse")
            .to_string();
        assert!(error.contains("unsupported fields"), "{field}: {error}");
        assert_eq!(
            catalog(&fx, &read),
            before,
            "{field} refusal mutated the catalog"
        );
    }
    let accepted = fx
        .op("app_media_import", import.clone())
        .expect("declared media.import must reach the custody service and succeed");
    assert_eq!(accepted["receipt"]["request_id"], import["request_id"]);
    assert_eq!(accepted["receipt"]["lifecycle"], "draft");
    let asset = accepted["receipt"]["asset_id"].as_str().unwrap();
    let after_native = catalog(&fx, &read);
    assert_eq!(after_native.as_array().unwrap().len(), 2);
    assert!(after_native
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == asset));

    let (port, board_thread) = board(&fx);
    let seam = Seam::token_at(&fx.state).unwrap();
    let (status, body) = import_http(port, &seam, &read);
    assert!(
        (400..500).contains(&status),
        "HTTP missing declaration must refuse: {status} {body}"
    );
    assert!(
        body.to_string().contains("screen does not declare media.import"),
        "HTTP must reach the same declaration guard, not an absent route or session failure: {body}"
    );
    assert_eq!(
        catalog(&fx, &read),
        after_native,
        "HTTP refusal created no catalog row"
    );

    import["request_id"] = json!("cad1962-http-import");
    let (status, body) = import_http(port, &seam, &import);
    assert_eq!(status, 200, "HTTP declared import must succeed: {body}");
    assert_eq!(body["receipt"]["request_id"], "cad1962-http-import");
    assert_eq!(body["receipt"]["lifecycle"], "draft");
    let asset = body["receipt"]["asset_id"].as_str().unwrap();
    let after_http = catalog(&fx, &read);
    assert_eq!(after_http.as_array().unwrap().len(), 3);
    assert!(after_http
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == asset));

    fx.stop.store(true, Ordering::SeqCst);
    board_thread.join().expect("acceptance board shuts down");
}
