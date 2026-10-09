//! CAD-1301 acceptance check — written by the Spec/security reviewer
//! (opus-rev-spec-903), not the implementer.
//!
//! Ticket outcome: through the real hosted send path, the destinations
//! document AgenticOS really sends (`devicePublishFixture.destinations`,
//! replayed verbatim from `tests/fixtures/agenticos/`) resolves the approved
//! destination and the effect is posted; every drifted reply shape sends
//! nothing and refuses closed; a revoked or non-publishable row never counts
//! as a match.
//!
//! Harness: the CAD-1267 one. A real in-process daemon registers the hosted
//! sender, importer and resolver from the baked hosted-media metadata alone;
//! ureq reaches the pinned `http://api.internal` through a CONNECT-proxy fake
//! door that logs every request. The destinations reply is swappable per
//! case. Effects are frozen straight into the record store, caption-only,
//! with their own toolkit/destination/connection (deliberate: the binding is
//! bound once to the fixture's Instagram account, and `publish_now` checks
//! only the binding's digest and revision; only destination parsing on the
//! `publish_now` path is under test). Instagram needs an image at the send
//! binding, so the verbatim Instagram fixture leg stops after the grants
//! read, and the send legs use a facebook row with the fixture row's keys.
#![cfg(feature = "test-seam")]

use cadence_agent::platform::deployments::DeploymentMetadata;
use cadence_agent::store::app_records::RecordStore;
use cadence_agent::store::app_social_drafts::{DraftSource, EffectStage};
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
const AOS_CONN: &str = "con_harbour_ig";
const DEST: &str = "17841400008460056";
const GRANT: &str = "dpq_cad1301_grant";
/// The facebook account the send legs use: the fixture row's 7 keys, for a
/// toolkit that may post caption-only.
const FB_CONN: &str = "con_harbour_fb";
const FB_DEST: &str = "275491372109884";
const FIXTURE: &str = include_str!("fixtures/agenticos/device-publish-destinations.json");
const CAPTION: &str = "Hosted caption";

/// One request the hosted clients sent: tunnel target, method, path and
/// whether it carried an Authorization header.
type Seen = Arc<Mutex<Vec<(String, String, String, bool)>>>;

/// The fake `api.internal` door behind a CONNECT proxy. Answers the
/// destinations read with the one publishable row, preflight and send by
/// echoing the binding, and anything else with a door error document.
fn fake_door(destinations: Arc<Mutex<String>>) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let seen: Seen = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let log = log.clone();
            let destinations = destinations.clone();
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
                        let data: Value =
                            serde_json::from_str(&destinations.lock().unwrap()).unwrap();
                        json!({"ok": true, "data": data})
                    }
                    ("GET", p) if p == format!("{PREFIX}/publish/grants") => {
                        json!({"ok": true, "data": {"grants": [
                            {"kind": "standing", "id": GRANT, "workspaceId": "ws_1301",
                             "connectionId": AOS_CONN, "destinationId": DEST,
                             "toolkit": "instagram", "dailyCap": 20, "remainingToday": 20,
                             "expiresAt": null, "revokedAt": null},
                            {"kind": "standing", "id": GRANT, "workspaceId": "ws_1301",
                             "connectionId": FB_CONN, "destinationId": FB_DEST,
                             "toolkit": "facebook", "dailyCap": 20, "remainingToday": 20,
                             "expiresAt": null, "revokedAt": null}]}})
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
    destinations: Arc<Mutex<String>>,
    next: std::sync::atomic::AtomicU32,
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
        let destinations = Arc::new(Mutex::new(FIXTURE.to_string()));
        let (door, seen) = fake_door(destinations.clone());
        let mut fx = Self {
            root,
            daemon: None,
            seen,
            context: Default::default(),
            destinations,
            next: Default::default(),
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
                "toolkit": "instagram", "timezone": "Asia/Hong_Kong",
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
        (toolkit, dest, conn): (&str, &str, &str),
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
        let n = self.next.fetch_add(1, SeqCst) + 1;
        let id = format!("sfx_{hex}_{n:032x}");
        let caption_digest =
            cadence_agent::platform::agenticos_external::publish::caption_digest_of(CAPTION);
        let frozen = json!({
            "source": {"kind": "social_draft", "draft_id": draft_id, "revision": 1},
            "install_id": install, "context_id": self.ctx(), "bundle_digest": bundle,
            "draft_id": draft_id, "revision": 1, "caption": CAPTION,
            "caption_digest": caption_digest, "asset_id": null, "image_digest": null,
            "mime": null, "size_bytes": null, "toolkit": toolkit,
            "destination_id": dest, "destination_label": "Harbour",
            "timezone": "Asia/Hong_Kong", "grant_id": GRANT, "aos_connection_id": conn,
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

impl Fx {
    fn serve(&self, data: &Value) {
        *self.destinations.lock().unwrap() = data.to_string();
    }

    /// Requests that left after the first `from` ones, as `METHOD path`.
    fn since(&self, from: usize) -> Vec<String> {
        self.sent()[from..]
            .iter()
            .map(|(target, method, path, authorized)| {
                assert_eq!(target, "api.internal:80", "left the lease door");
                assert!(!authorized, "hosted request carried a bearer");
                format!("{method} {path}")
            })
            .collect()
    }
}

/// The fixture's one row, verbatim.
fn fixture_row() -> Value {
    serde_json::from_str::<Value>(FIXTURE).unwrap()["destinations"][0].clone()
}

/// The facebook send account, with exactly the fixture row's keys.
fn fb_row() -> Value {
    let mut row = fixture_row();
    row["connectionId"] = json!(FB_CONN);
    row["toolkit"] = json!("facebook");
    row["displayName"] = json!("Harbour page");
    row["destinationId"] = json!(FB_DEST);
    row
}

fn doc(rows: Vec<Value>) -> Value {
    json!({"version": "1", "destinations": rows})
}

#[test]
fn hosted_destinations_resolve_only_the_real_document_and_refuse_drift_closed() {
    let fx = Fx::start();
    let app = fx.install();
    let install = app.0.clone();
    let instagram = ("instagram", DEST, AOS_CONN);
    let facebook = ("facebook", FB_DEST, FB_CONN);
    let destinations = format!("GET {PREFIX}/destinations");
    let grants = |dest: &str| format!("GET {PREFIX}/publish/grants?destinationId={dest}");
    let sent_path = [
        destinations.clone(),
        grants(FB_DEST),
        format!("POST {PREFIX}/publish/preflight"),
        format!("POST {PREFIX}/publish/preflight"),
        format!("POST {PREFIX}/publish"),
    ];
    let posts = |fx: &Fx, data: &Value, tag: &str| {
        fx.serve(data);
        let (id, digest, _) = fx.effect(&app, tag, facebook, true);
        let from = fx.sent().len();
        let result = fx.publish_now(&id, &digest);
        let seen = fx.since(from);
        assert!(
            seen.len() >= 5 && seen[..5] == sent_path,
            "{tag}: the approved effect did not reach preflight and publish: {seen:?} {result:?}"
        );
        result.unwrap_or_else(|e| panic!("{tag}: approved effect did not publish: {e}"));
        assert_eq!(fx.state(&install, &id), "posted", "{tag}");
    };
    let refuses = |fx: &Fx, data: &Value, tag: &str, closed: &str| {
        fx.serve(data);
        let (id, digest, _) = fx.effect(&app, tag, facebook, true);
        let from = fx.sent().len();
        let result = fx.publish_now(&id, &digest);
        // Exactly the destinations read went out (so the drift was really
        // served): no grants read, import, preflight or publish.
        assert_eq!(
            fx.since(from),
            std::slice::from_ref(&destinations),
            "{tag}: a drifted reply let a request past the destinations read: {result:?}"
        );
        let refused = result.expect_err(tag).to_string();
        assert!(
            refused.contains(closed),
            "{tag}: not a closed refusal: {refused}"
        );
        assert_eq!(fx.state(&install, &id), "approved", "{tag}");
    };
    const UNAVAILABLE: &str = "capability_unavailable: the destinations resolver is unavailable";
    const UNMAPPED: &str = "grant_binding_mismatch: no publishable destination binds";
    const AMBIGUOUS: &str =
        "capability_unavailable: the destinations read did not isolate one publishable binding";

    // (a1) The real AgenticOS document, verbatim (Instagram), resolves the
    // approved account: `publish_now` reads the owner's grants only after
    // `resolve` returned exactly one row whose connection equals the
    // approved `con_harbour_ig`. Caption-only Instagram is then correctly
    // refused at the send binding, so this leg stops before preflight.
    fx.serve(&serde_json::from_str(FIXTURE).unwrap());
    let (id, digest, _) = fx.effect(&app, "fixture-instagram", instagram, true);
    let from = fx.sent().len();
    let result = fx.publish_now(&id, &digest);
    let seen = fx.since(from);
    assert!(
        seen.len() >= 2 && seen[..2] == [destinations.clone(), grants(DEST)],
        "the verbatim fixture did not resolve the approved account: {seen:?} {result:?}"
    );

    // (a2) The same envelope carrying a facebook row posts end to end.
    posts(&fx, &doc(vec![fb_row()]), "envelope-facebook");

    // (b) Every drifted shape sends nothing and refuses closed. Each one
    // carries the approved row, so a tolerant parser would send it.
    let row = fb_row();
    refuses(&fx, &json!([row]), "bare-array", UNAVAILABLE);
    refuses(
        &fx,
        &json!({"version": "2", "destinations": [row]}),
        "version-2",
        UNAVAILABLE,
    );
    refuses(&fx, &json!({"version": "1"}), "no-list", UNAVAILABLE);
    refuses(
        &fx,
        &json!({"version": "1", "rows": [row]}),
        "renamed-list",
        UNAVAILABLE,
    );
    let mut malformed = row.clone();
    malformed["connectionId"] = json!(42);
    refuses(&fx, &doc(vec![malformed]), "malformed-row", UNMAPPED);
    // The approved row first: picking the first match would send it.
    let mut twin = row.clone();
    twin["connectionId"] = json!("con_harbour_fb_twin");
    refuses(&fx, &doc(vec![row.clone(), twin]), "two-rows", AMBIGUOUS);

    // (c) A revoked or non-publishable row for the same account never
    // counts: alone it maps nothing; beside the live row it does not make
    // the read ambiguous.
    let mut revoked = row.clone();
    revoked["connectionId"] = json!("con_harbour_fb_old");
    revoked["status"] = json!("revoked");
    revoked["publishable"] = json!(false);
    let mut closed = row.clone();
    closed["connectionId"] = json!("con_harbour_fb_closed");
    closed["available"] = json!(false);
    closed["publishable"] = json!(false);
    refuses(&fx, &doc(vec![revoked.clone()]), "revoked-only", UNMAPPED);
    refuses(&fx, &doc(vec![closed.clone()]), "closed-only", UNMAPPED);
    posts(&fx, &doc(vec![revoked, closed, row]), "live-beside-revoked");
}
