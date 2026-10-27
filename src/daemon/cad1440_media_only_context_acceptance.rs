//! CAD-1440 independent acceptance (spec/security author, scratch-only):
//! a media-only `screen-media.v1` screen with NO tool aliases mints a
//! non-null action context, and the undeclared `image.generate` alias is
//! refused by the real guard over native RPC and the board HTTP peer.
//! Written from the ticket, NOT from the implementer's source change.
//!
//! The package is a small generic consumer generated here (or the
//! `CADENCE_DSN1_PACKAGE_DIR` override when explicitly set). Its screen
//! declares `host_contract: screen-media.v1`, `may: ["media.read"]` and
//! no `tools` map. The fixture stubs are the generic
//! `ig-tools-fixture` client and assets, not a business bundle.
//!
//! Refusals happen before any binding or provider work; no provider door
//! is faked. The declared `media.read` catalog is the positive control on
//! the same live consumed mount.

#![cfg(all(test, feature = "test-seam"))]

use super::cad1365_mount_authority_acceptance::{Fx, Stop};
use crate::test_seam::Seam;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const ALIAS: &str = "image.generate";
const PROBE_APP: &str = "media-only-probe";
const NEEDLE: &str = "tool is not declared by this screen";

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Generic media-only consumer: one read `source` capability with a
/// single host workflow, and a `feed` screen declaring `screen-media.v1`,
/// `media.read` and no tool aliases.
fn write_probe(app: &std::path::Path) {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(app.join("workflows")).unwrap();
    std::fs::write(
        app.join("app.md"),
        format!(
            "---\napp: {PROBE_APP}\ntitle: Media-only probe\nversion: '0.1.0'\nneeds:\n  connections: []\n  capabilities:\n    source:\n      schema: 1\n      capability: social.read\n      version: 1\n      action: list_posts\n      resource_kind: connection_account\n      effect: read\n---\nGeneric media-only host fixture; no app behaviour.\n"
        ),
    )
    .unwrap();
    std::fs::write(
        app.join("workflows/source.md"),
        "---\ntitle: \"Synthetic source read\"\ngoal: \"Exercise the host capability path\"\n\
         label: Source read\ncapability_slots: [source]\nexecution: host\n---\n\n\
         ## Source read\nsize: S\naction: local.capability.call\n\n\
         Host-only synthetic step; no worker and no provider.\n\n\
         ### Acceptance\n- [ ] The synthetic host path is exercised.\n",
    )
    .unwrap();
    copy_dir(
        &manifest_dir.join("tests/fixtures/apps/ig-tools-fixture/screens"),
        &app.join("screens"),
    );
    let screens = app.join("screens/feed/screens.json");
    let mut decl: Value =
        serde_json::from_str(&std::fs::read_to_string(&screens).unwrap()).unwrap();
    decl["app"] = json!(PROBE_APP);
    decl["host_contract"] = json!("screen-media.v1");
    decl["may"] = json!(["media.read"]);
    decl.as_object_mut().unwrap().remove("tools");
    std::fs::write(&screens, serde_json::to_string_pretty(&decl).unwrap()).unwrap();
}

/// The single screen tag a package declares (`screens/<tag>/screens.json`).
/// The generated probe declares `feed`; an explicit external package may
/// declare another tag (e.g. `studio`) and is read, not assumed.
fn screen_tag(app: &std::path::Path) -> String {
    let mut tags: Vec<String> = std::fs::read_dir(app.join("screens"))
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.path().join("screens.json").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(tags.len(), 1, "package must declare exactly one screen");
    tags.pop().unwrap()
}

/// Mints and consumes one live mount for `tag` on the same session and
/// generation as the fixture; returns the host-held action token.
fn mount_tag(fx: &Fx, tag: &str, token: &str, key: &str) -> String {
    let mint = fx
        .operator(
            "app_screen_mint",
            json!({"install_id": fx.install, "context_id": fx.context,
                "tag": tag, "token": token, "key": key,
                "origin": "loopback", "generation": 1}),
        )
        .expect("operator mint must succeed — positive control");
    let nonce = mint["mount"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();
    fx.operator("app_screen_consume", json!({"nonce": nonce}))
        .expect("operator consume must succeed — positive control");
    mint["action_token"].as_str().unwrap().to_string()
}

/// Installs the media-only consumer as a second app on the fixture daemon,
/// approves it and creates its context. `CADENCE_DSN1_PACKAGE_DIR`, when
/// explicitly set, supplies the package instead of the generated probe.
/// Returns the package's own screen tag.
fn install_probe(fx: &mut Fx) -> String {
    let app = fx._dir.path().join("media-probe-app");
    match std::env::var("CADENCE_DSN1_PACKAGE_DIR") {
        Ok(dir) => copy_dir(std::path::Path::new(&dir), &app),
        Err(_) => write_probe(&app),
    }
    let install = fx
        .operator(
            "app_workspace_install",
            json!({"source": app.to_str().unwrap()}),
        )
        .expect("install media-only probe");
    fx.operator(
        "app_local_install_approve",
        json!({"install_id": install["install_id"], "digest": install["digest"]}),
    )
    .expect("approve media-only probe");
    let ctx = fx
        .operator(
            "app_context_create",
            json!({"install_id": install["install_id"], "label": "cad1440",
                "input_defaults": {}, "request_id": "ctx-cad1440"}),
        )
        .expect("context for media-only probe");
    fx.install = install["install_id"].as_str().unwrap().to_string();
    fx.context = ctx["context"]["id"]
        .as_str()
        .or_else(|| ctx["id"].as_str())
        .unwrap()
        .to_string();
    screen_tag(&app)
}

fn refused(result: crate::error::Result<Value>, needle: &str, what: &str) {
    let text = result
        .expect_err(&format!("{what} must be refused"))
        .to_string();
    assert!(text.contains(needle), "{what} refused wrongly: {text}");
}

/// Stops the board thread set on drop; same port-search pattern as the
/// fixture board used by the sibling CAD-1365 checks.
fn board(fx: &Fx) -> (u16, Stop) {
    let state = fx.state.clone();
    let pm = state.parent().unwrap().join("pm");
    let mut guard = Stop(fx._stop.0.clone(), Vec::new());
    let first = (std::process::id() % 80) as u16;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut offset = 0u16;
    loop {
        assert!(Instant::now() < deadline, "board startup deadline");
        let port = 3110 + (first + offset) % 80;
        let (startup, ready) = std::sync::mpsc::channel();
        let opts = crate::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(guard.0.clone()),
            startup: Some(startup),
            test_seam: true,
            ..Default::default()
        };
        let (s, p) = (state.clone(), pm.clone());
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&s, &p, &opts)));
        match ready.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Ok(())) => {
                guard.1.push(thread);
                return (port, guard);
            }
            Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                assert!(thread.join().is_ok());
                offset += 1;
                assert!(offset < 80, "all board candidates busy");
            }
            other => {
                guard.1.push(thread);
                panic!("board startup failed on port {port}: {other:?}");
            }
        }
    }
}

/// POST through the real board as the operator (cookie + session header).
fn http(port: u16, seam: &str, session: (&str, &str), body: Value) -> (u16, Value) {
    let host = format!("cadence-{port}.localhost:{port}");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let req = agent
        .post(format!(
            "http://127.0.0.1:{port}/api/app-screen-tools/invoke"
        ))
        .header("Host", &host)
        .header("X-Cadence-Board", "1")
        .header("Origin", format!("http://{host}"))
        .header(crate::test_seam::AS_HEADER, "operator")
        .header(crate::test_seam::TOKEN_HEADER, seam)
        .header("Cookie", format!("cadence_operator_{port}={}", session.0))
        .header("X-Cadence-Session", session.1)
        .header("Content-Type", "application/json");
    let mut r = req.send(body.to_string()).unwrap();
    (
        r.status().as_u16(),
        r.body_mut().read_json().unwrap_or(Value::Null),
    )
}

/// One owned native+HTTP bad-case check plus its positive control.
#[test]
fn cad1440_media_only_undeclared_alias_refused_rpc_and_http() {
    let mut fx = Fx::new();
    let tag = install_probe(&mut fx);
    let (token, key) = fx.session();
    // The mint must hand back a non-null action token for a media-only
    // screen with an empty alias map.
    let action = mount_tag(&fx, &tag, &token, &key);

    // RPC: the real guard names the undeclared alias.
    refused(
        fx.operator(
            "app_tool_invoke",
            json!({"action_token": action, "tool_alias": ALIAS, "input": {},
                "request_id": "req-cad1440-rpc", "token": token, "key": key,
                "origin": "loopback"}),
        ),
        NEEDLE,
        "undeclared tool alias over RPC",
    );

    // HTTP: the board relay is at least as strict, same guard text.
    let (port, _guard) = board(&fx);
    let seam = Seam::token_at(&fx.state).unwrap();
    let (status, reply) = http(
        port,
        &seam,
        (&token, &key),
        json!({"action_token": action, "tool_alias": ALIAS, "input": {},
            "request_id": "req-cad1440-http"}),
    );
    assert!(status >= 400, "undeclared alias relayed over HTTP: {reply}");
    assert!(
        reply.to_string().contains(NEEDLE),
        "undeclared alias over HTTP refused by the wrong gate: {reply}"
    );

    // Positive control: the same mount's declared media.read succeeds.
    let catalog = fx
        .operator(
            "app_media_read",
            json!({"action_token": action, "token": token, "key": key,
                "origin": "loopback", "input": {"kind": "catalog"}}),
        )
        .expect("declared media.read must succeed on the same mount");
    assert_eq!(catalog["assets"], json!([]), "control catalog shape");
}
