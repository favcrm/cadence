//! CAD-1267 acceptance check — written by the Spec/security reviewer
//! (opus-rev-spec-894), not the implementer.
//!
//! Ticket outcome: through the hosted sender, an unapproved social draft
//! effect, or one whose binding or draft revision changed after approval,
//! is refused and nothing reaches the provider door.
//!
//! A real in-process daemon starts with only the baked hosted-media
//! metadata (no device env, no credential file), so `serve_with` itself
//! registers the hosted sender, importer and resolver. That transport is
//! pinned to `http://api.internal`; the test reaches it through a fake
//! door that ureq is pointed at with `ALL_PROXY` (an HTTP CONNECT proxy
//! that answers the tunnelled request itself). Every request the hosted
//! clients make is logged, so "no outbound send" is the log staying empty.
//! An approved, unchanged effect is the positive control: it does reach
//! the hosted door, which proves the log can fill.
#![cfg(feature = "test-seam")]

use cadence_agent::platform::deployments::DeploymentMetadata;
use cadence_agent::store::app_records::RecordStore;
use cadence_agent::store::app_social_drafts::{DraftSource, EffectStage, SocialDraftEdit};
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{client, daemon};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HOSTED: &str = r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@3","transport":"hosted-media-lease@1"}]}"#;
const PREFIX: &str = "/v1/runtime/connectors/hosted-publish";
const AOS_CONN: &str = "conn_aos_hosted_1";
const DEST: &str = "275491372109884";
const GRANT: &str = "dpq_cad1267_grant";
const CAPTION: &str = "Hosted caption";

/// One request the hosted clients sent: tunnel target, method, path and
/// whether it carried an Authorization header.
type Seen = Arc<Mutex<Vec<(String, String, String, bool)>>>;

/// The fake `api.internal` door behind a CONNECT proxy. Answers the
/// destinations read with the one publishable row, preflight and send by
/// echoing the binding, and anything else with a door error document.
fn fake_door() -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let seen: Seen = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let log = log.clone();
            std::thread::spawn(move || {
                let mut writer = stream.try_clone().unwrap();
                let mut reader = BufReader::new(stream);
                let head = |reader: &mut BufReader<_>| -> Vec<String> {
                    let mut lines = Vec::new();
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return lines;
                        }
                        let line = line.trim_end().to_string();
                        if line.is_empty() {
                            return lines;
                        }
                        lines.push(line);
                    }
                };
                let connect = head(&mut reader);
                let Some(target) = connect
                    .first()
                    .and_then(|l| l.strip_prefix("CONNECT "))
                    .and_then(|l| l.split(' ').next())
                    .map(str::to_string)
                else {
                    return;
                };
                writer
                    .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .unwrap();
                let request = head(&mut reader);
                let Some(first) = request.first() else {
                    return;
                };
                let mut parts = first.split(' ');
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                let header = |name: &str| {
                    request.iter().skip(1).find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
                    })
                };
                let authorized = header("authorization").is_some();
                let length: usize = header("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                log.lock()
                    .unwrap()
                    .push((target, method.clone(), path.clone(), authorized));
                let sent: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let echo = json!({
                    "key": sent["key"],
                    "destinationId": sent["grant"]["destinationId"],
                    "captionDigest": sent["grant"]["captionDigest"],
                    "imageDigest": sent["grant"]["imageDigest"],
                });
                let reply = match (method.as_str(), path.split('?').next().unwrap_or("")) {
                    ("GET", p) if p == format!("{PREFIX}/destinations") => {
                        json!({"ok":true,"data":[{
                        "connectionId": AOS_CONN, "toolkit": "facebook", "displayName": "Harbour",
                        "destinationId": DEST, "status": "active", "available": true, "publishable": true}]})
                    }
                    ("POST", p) if p == format!("{PREFIX}/publish/preflight") => {
                        let mut data = echo.clone();
                        data["decision"] = json!("approved");
                        data["executable"] = json!(true);
                        json!({"ok":true,"data":data})
                    }
                    ("POST", p) if p == format!("{PREFIX}/publish") => {
                        let mut data = echo.clone();
                        data["version"] = json!("1");
                        data["result"] = json!({"key": sent["key"], "status": "posted"});
                        json!({"ok":true,"data":data})
                    }
                    _ => json!({"ok":false,"error":{"code":"not_found","message":"no route"}}),
                };
                let reply = reply.to_string();
                let _ = write!(
                    writer,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
            });
        }
    });
    (addr, seen)
}

/// An installable app needs one workflow; this one is never run.
const WORKFLOW: &str = r#"---
title: "CAD-1267 post"
goal: "One post"
inputs:
  writer: { ask: "writer" }
---

## Write
agent: {{writer}}
size: S
action: local.text.produce

Write one post.

### Acceptance
- [ ] post exists
"#;

struct Fx {
    root: tempfile::TempDir,
    daemon: Option<(
        Arc<AtomicBool>,
        std::thread::JoinHandle<cadence_agent::Result<()>>,
    )>,
    seen: Seen,
    context: std::sync::OnceLock<String>,
}

impl Fx {
    fn ctx(&self) -> &str {
        self.context.get().unwrap()
    }

    fn dir(&self) -> std::path::PathBuf {
        self.root.path().join("s")
    }

    fn start() -> Self {
        for name in [
            "CADENCE_PUBLISH_SEND_URL",
            "CADENCE_PUBLISH_SEND_CREDENTIAL_FILE",
            "CADENCE_PUBLISH_READ_URL",
            "CADENCE_PUBLISH_READ_CREDENTIAL_FILE",
            "CADENCE_AGENTICOS_EXTERNAL_URL",
            "NO_PROXY",
            "no_proxy",
        ] {
            std::env::remove_var(name);
        }
        let root = tempfile::Builder::new().prefix("c1267").tempdir().unwrap();
        cadence_agent::issue::Pm::init(&root.path().join("pm")).unwrap();
        std::fs::create_dir_all(root.path().join("s")).unwrap();
        let (door, seen) = fake_door();
        let mut fx = Self {
            root,
            daemon: None,
            seen,
            context: Default::default(),
        };
        let (dir, stop) = (fx.dir(), Arc::new(AtomicBool::new(false)));
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set(
            "CADENCE_PM_DIR",
            fx.root.path().join("pm").to_str().unwrap(),
        );
        let mut opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(Arc::clone(&stop)),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            // Only the baked hosted-media assertion: serve_with registers
            // the hosted sender, importer and resolver from it.
            provider_deployments: Some(DeploymentMetadata::parse(HOSTED.as_bytes()).unwrap()),
            ..Default::default()
        };
        cadence_agent::platform::local::register_at(
            &dir,
            &mut opts,
            dir.join("outbox"),
            "http://127.0.0.1:3010".into(),
        );
        // ureq reads the proxy when each client is built at startup; the
        // tunnel is the only way to stand a door in for api.internal.
        std::env::set_var("ALL_PROXY", format!("http://{door}"));
        let handle = std::thread::spawn(move || daemon::serve_with(&dir, opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&fx.dir(), "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&fx.dir()).is_none()
        {
            assert!(
                !handle.is_finished() && std::time::Instant::now() < deadline,
                "daemon down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        std::env::remove_var("ALL_PROXY");
        fx.daemon = Some((stop, handle));
        fx
    }

    fn rpc(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(Asserted::Operator, || {
            client::rpc(&self.dir(), method, params)
        })
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }

    fn sent(&self) -> Vec<(String, String, String, bool)> {
        self.seen.lock().unwrap().clone()
    }

    /// Install a one-send-slot app and bind its slot in a new context to the
    /// local connection with the operator's publish settings.
    /// Returns `(install_id, bundle_digest, binding)`.
    fn install(&self) -> (String, String, Value) {
        let source = self.root.path().join("app-src");
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(source.join("workflows/post.md"), WORKFLOW).unwrap();
        std::fs::write(
            source.join("app.md"),
            "---\napp: c1267-accept\ntitle: CAD-1267 accept\nversion: '0.1.0'\n\
             summary: Hosted publish fixture.\nneeds:\n  connections: []\n  capabilities:\n    publication:\n      schema: 1\n      capability: text.publish\n      version: 1\n      action: publish\n      resource_kind: connection_account\n      effect: send\n---\n\n# CAD-1267 accept\n",
        )
        .unwrap();
        let installed = self.op(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        );
        let install = installed["install_id"].as_str().unwrap().to_string();
        let connection = self.op("connection_list", json!({}))["connections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["provider"] == "local" && row["account"] == "local")
            .expect("local builtin connection")["id"]
            .clone();
        let context = self.op(
            "app_context_create",
            json!({"install_id": install, "label": "Hosted", "input_defaults": {},
                "request_id": "ctx-1267"}),
        );
        let context = context["context"]["id"]
            .as_str()
            .or(context["context_id"].as_str())
            .unwrap_or_else(|| panic!("context id: {context}"))
            .to_string();
        self.context.set(context).unwrap();
        let bound = self.op(
            "app_binding_create",
            json!({"install_id": install, "context_id": self.ctx(), "slot": "publication",
                "connection_id": connection, "request_id": "bind-1267"}),
        );
        let binding = self.publish_set(&install, &bound["binding"], "Harbour");
        (
            install,
            installed["digest"].as_str().unwrap().to_string(),
            binding,
        )
    }

    fn publish_set(&self, install: &str, binding: &Value, label: &str) -> Value {
        self.op(
            "app_binding_publish_set",
            json!({"install_id": install, "binding_id": binding["id"],
                "expected_revision": binding["revision"],
                "destination_id": DEST, "destination_label": label,
                "toolkit": "facebook", "timezone": "Asia/Hong_Kong",
                "grant_id": GRANT}),
        )["binding"]
            .clone()
    }

    /// One draft plus its frozen effect, in the shape the stage RPC
    /// freezes; approved when `approve`. Returns `(effect_id, digest,
    /// draft_id)`.
    fn effect(
        &self,
        (install, bundle, binding): &(String, String, Value),
        tag: &str,
        approve: bool,
    ) -> (String, String, String) {
        let records = RecordStore::open(&self.dir(), install).unwrap();
        let draft = records
            .app_social_draft_create(
                self.ctx(),
                CAPTION,
                &DraftSource::ToolReceipt {
                    receipt_id: format!("receipt-{tag}"),
                    post_id: None,
                },
                None,
                &format!("draft-{tag}"),
                "session:operator",
            )
            .unwrap();
        let draft_id = draft["draft_id"].as_str().unwrap().to_string();
        let hex: String = install.bytes().map(|b| format!("{b:02x}")).collect();
        let n = ["unapproved", "revised", "control", "rebound"]
            .iter()
            .position(|t| *t == tag)
            .unwrap()
            + 1;
        let id = format!("sfx_{hex}_{n:032x}");
        let caption_digest =
            cadence_agent::platform::agenticos_external::publish::caption_digest_of(CAPTION);
        let frozen = json!({
            "source": {"kind": "social_draft", "draft_id": draft_id, "revision": 1},
            "install_id": install, "context_id": self.ctx(), "bundle_digest": bundle,
            "draft_id": draft_id, "revision": 1, "caption": CAPTION,
            "caption_digest": caption_digest, "asset_id": null, "image_digest": null,
            "mime": null, "size_bytes": null, "toolkit": "facebook",
            "destination_id": DEST, "destination_label": "Harbour",
            "timezone": "Asia/Hong_Kong", "grant_id": GRANT, "aos_connection_id": AOS_CONN,
            "binding": {"slot": "publication", "revision": binding["revision"],
                "digest": binding["digest"], "config": binding["config"]},
            "effect_id": id, "idempotency_key": format!("social_{n:032x}"),
            "approval_id": format!("social-approval-{tag}"),
        });
        let staged = records
            .app_social_effect_stage(
                self.ctx(),
                &EffectStage {
                    draft: &draft_id,
                    revision: 1,
                    request: &format!("effect-{tag}"),
                    effect_id: &id,
                    frozen: &frozen,
                    approval: &format!("social-approval-{tag}"),
                },
            )
            .unwrap();
        let digest = staged["effect"]["digest"].as_str().unwrap().to_string();
        if approve {
            records
                .app_social_effect_decide(&id, &digest, true)
                .unwrap()
                .unwrap();
        }
        (id, digest, draft_id)
    }

    fn publish_now(&self, id: &str, digest: &str) -> cadence_agent::Result<Value> {
        self.rpc(
            "app_effect_publish_now",
            json!({"effect_id": id, "digest": digest}),
        )
    }

    fn state(&self, install: &str, id: &str) -> String {
        RecordStore::open(&self.dir(), install)
            .unwrap()
            .app_social_effect_show(id)
            .unwrap()["effect"]["state"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        if let Some((stop, handle)) = self.daemon.take() {
            stop.store(true, SeqCst);
            let _ = handle.join();
        }
    }
}

#[test]
fn hosted_sender_refuses_unapproved_or_changed_effects_with_no_outbound_send() {
    let fx = Fx::start();
    let app = fx.install();
    let install = app.0.clone();

    // 1. Unapproved: the effect is still waiting for the operator.
    let (id, digest, _) = fx.effect(&app, "unapproved", false);
    let refused = fx.publish_now(&id, &digest);
    assert!(
        fx.sent().is_empty(),
        "unapproved effect reached the door: {:?}",
        fx.sent()
    );
    let refused = refused.unwrap_err().to_string();
    assert!(
        refused.contains("exact approved social draft effect"),
        "{refused}"
    );
    assert_eq!(fx.state(&install, &id), "waiting");

    // 2. Draft revision changed after approval.
    let (id, digest, draft) = fx.effect(&app, "revised", true);
    RecordStore::open(&fx.dir(), &install)
        .unwrap()
        .app_social_draft_update(
            fx.ctx(),
            &draft,
            &SocialDraftEdit {
                expected: 1,
                caption: "Edited after approval",
                asset_id: None,
                request_id: "edit-revised",
                actor: "session:operator",
            },
        )
        .unwrap();
    let refused = fx.publish_now(&id, &digest);
    assert!(
        fx.sent().is_empty(),
        "revised draft reached the door: {:?}",
        fx.sent()
    );
    let refused = refused.unwrap_err().to_string();
    assert!(
        refused.contains("social draft changed since approval"),
        "{refused}"
    );
    assert_eq!(fx.state(&install, &id), "approved");

    // Positive control: an approved, unchanged effect does go out over
    // the hosted lease door (fixed api.internal origin, hosted prefix,
    // no bearer), so an empty log above is a real refusal.
    let (id, digest, _) = fx.effect(&app, "control", true);
    let _ = fx.publish_now(&id, &digest);
    let sent = fx.sent();
    assert!(
        sent.iter().any(
            |(target, method, path, authorized)| target == "api.internal:80"
                && method == "GET"
                && *path == format!("{PREFIX}/destinations")
                && !authorized
        ),
        "approved effect never reached the hosted door: {sent:?}"
    );
    assert!(
        sent.iter()
            .all(|(target, _, path, authorized)| target == "api.internal:80"
                && path.starts_with(PREFIX)
                && !authorized),
        "hosted request left the lease door shape: {sent:?}"
    );
    let before = sent.len();

    // 3. Binding changed after approval: the operator re-sets the
    // publish settings, so the live binding's revision and digest move.
    let (id, digest, _) = fx.effect(&app, "rebound", true);
    let live = fx.op(
        "app_binding_list",
        json!({"install_id": install, "context_id": fx.ctx()}),
    )["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["state"] == "configured")
        .unwrap()
        .clone();
    fx.publish_set(&install, &live, "Harbour moved");
    let refused = fx.publish_now(&id, &digest);
    assert_eq!(
        fx.sent().len(),
        before,
        "rebound effect reached the door: {:?}",
        &fx.sent()[before..]
    );
    let refused = refused.unwrap_err().to_string();
    assert!(
        refused.contains("social publish binding changed since approval"),
        "{refused}"
    );
    assert_eq!(fx.state(&install, &id), "approved");
}
