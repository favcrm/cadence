//! CAD-1291 acceptance check — written by the Spec/security reviewer
//! (opus-rev-spec-897), not the implementer.
//!
//! Ticket outcome (operator decision 2026-10-09, standing grant): a post
//! goes out only when the Cadence operator approved that exact effect, the
//! caption, image and destination are unchanged since approval, and the
//! owner's live standing grant covers exactly the approved connection,
//! destination and toolkit. Cadence never takes a grant id from a request
//! or from the binding: it finds the grant through the door.
//!
//! A real in-process daemon starts with only the baked hosted-media
//! metadata, so `serve_with` registers the hosted sender, importer and
//! resolver. Their transport is pinned to `http://api.internal`; the test
//! reaches it through a fake door behind an HTTP CONNECT proxy
//! (`ALL_PROXY`), as `cad1267_hosted_publish_acceptance` does. The door
//! logs every request; "nothing sent" is the absence of any preflight or
//! publish POST. An approved, unchanged effect with a matching live grant
//! is the positive control: it reaches preflight and publish, presenting
//! the door's standing grant id and never the grant id typed on the
//! binding.
#![cfg(feature = "test-seam")]

use cadence_agent::platform::deployments::DeploymentMetadata;
use cadence_agent::store::app_records::RecordStore;
use cadence_agent::store::app_social_drafts::{DraftSource, EffectStage, SocialDraftEdit};
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{client, daemon};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HOSTED: &str = r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@3","transport":"hosted-media-lease@1"}]}"#;
const PREFIX: &str = "/v1/runtime/connectors/hosted-publish";
const AOS_CONN: &str = "conn_aos_1291";
const DEST: &str = "275491372109884";
const OTHER_DEST: &str = "275491372109999";
/// The grant id the operator typed on the old path's binding. The hosted
/// path must never present it.
const TYPED_GRANT: &str = "dpq_typed_on_binding";
/// The owner's standing grant the door lists.
const STANDING: &str = "dpq_standing_cad1291";
const CAPTION: &str = "Standing caption";

/// One request the door saw: method, path, the grant id it presented.
type Seen = Arc<Mutex<Vec<(String, String, Option<String>)>>>;

/// What the door answers, switchable per case.
#[derive(Default)]
struct DoorState {
    /// The `grants` array of `GET .../publish/grants`.
    grants: Mutex<Value>,
    /// The AOS connection the destinations read reports for `DEST`.
    connection: Mutex<String>,
    /// Inactive unless one acceptance case installs a deterministic race latch.
    grant_latch: Mutex<Option<Arc<GrantLatch>>>,
}

struct GrantLatch {
    entered: std::sync::mpsc::Sender<()>,
    resume: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl GrantLatch {
    fn pause(&self) {
        let _ = self.entered.send(());
        let _ = self
            .resume
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(30));
    }
}

/// An AOS #411 `devicePublishStandingGrantSchema` record.
fn standing(edit: impl FnOnce(&mut Value)) -> Value {
    let mut grant = json!({
        "kind": "standing", "id": STANDING, "workspaceId": "ws_1291",
        "connectionId": AOS_CONN, "destinationId": DEST, "toolkit": "facebook",
        "dailyCap": 20, "remainingToday": 20, "expiresAt": null, "revokedAt": null,
    });
    edit(&mut grant);
    grant
}

fn fake_door() -> (String, Seen, Arc<DoorState>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let seen: Seen = Arc::default();
    let state = Arc::new(DoorState::default());
    *state.grants.lock().unwrap() = json!([]);
    *state.connection.lock().unwrap() = AOS_CONN.to_string();
    let (log, door) = (seen.clone(), state.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (log, door) = (log.clone(), door.clone());
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
                if !connect
                    .first()
                    .is_some_and(|l| l.starts_with("CONNECT api.internal:80 "))
                {
                    return;
                }
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
                let length: usize = request
                    .iter()
                    .skip(1)
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse().ok())?
                    })
                    .unwrap_or(0);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let sent: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                log.lock().unwrap().push((
                    method.clone(),
                    path.clone(),
                    sent["grant"]["id"].as_str().map(str::to_string),
                ));
                let echo = json!({
                    "key": sent["key"],
                    "destinationId": sent["grant"]["destinationId"],
                    "captionDigest": sent["grant"]["captionDigest"],
                    "imageDigest": sent["grant"]["imageDigest"],
                });
                let route = path.split('?').next().unwrap_or("").to_string();
                if method == "GET"
                    && path == format!("{PREFIX}/publish/grants?destinationId={DEST}")
                {
                    if let Some(latch) = door.grant_latch.lock().unwrap().take() {
                        latch.pause();
                    }
                }
                let reply = if method == "GET" && route == format!("{PREFIX}/destinations") {
                    json!({"ok":true,"data":{"version":"1","destinations":[{
                        "connectionId": *door.connection.lock().unwrap(), "toolkit": "facebook",
                        "displayName": "Harbour", "destinationId": DEST, "status": "active",
                        "available": true, "publishable": true}]}})
                } else if method == "GET"
                    && path == format!("{PREFIX}/publish/grants?destinationId={DEST}")
                {
                    json!({"ok":true,"data":{"grants": *door.grants.lock().unwrap()}})
                } else if method == "POST" && route == format!("{PREFIX}/publish/preflight") {
                    let mut data = echo;
                    data["decision"] = json!("approved");
                    data["executable"] = json!(true);
                    json!({"ok":true,"data":data})
                } else if method == "POST" && route == format!("{PREFIX}/publish") {
                    let mut data = echo;
                    data["version"] = json!("1");
                    data["result"] = json!({"key": sent["key"], "status": "posted"});
                    json!({"ok":true,"data":data})
                } else {
                    json!({"ok":false,"error":{"code":"not_found","message":"no route"}})
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
    (addr, seen, state)
}

const WORKFLOW: &str = r#"---
title: "CAD-1291 post"
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
    door: Arc<DoorState>,
    context: std::sync::OnceLock<String>,
    effects: AtomicUsize,
}

struct App {
    install: String,
    bundle: String,
    binding: Value,
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
        let root = tempfile::Builder::new().prefix("c1291").tempdir().unwrap();
        cadence_agent::issue::Pm::init(&root.path().join("pm")).unwrap();
        std::fs::create_dir_all(root.path().join("s")).unwrap();
        let (door_addr, seen, door) = fake_door();
        let mut fx = Self {
            root,
            daemon: None,
            seen,
            door,
            context: Default::default(),
            effects: AtomicUsize::new(0),
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
            // The publish driver ticks at its production 30 s default
            // otherwise; the millisecond seam keeps due-effect dispatch
            // prompt without a fixed-duration wait.
            social_publish_driver_ms: Some(50),
            provider_deployments: Some(DeploymentMetadata::parse(HOSTED.as_bytes()).unwrap()),
            ..Default::default()
        };
        cadence_agent::platform::local::register_at(
            &dir,
            &mut opts,
            dir.join("outbox"),
            "http://127.0.0.1:3010".into(),
        );
        std::env::set_var("ALL_PROXY", format!("http://{door_addr}"));
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

    fn rpc_as(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(who, || client::rpc(&self.dir(), method, params))
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc_as(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }

    fn sent(&self) -> Vec<(String, String, Option<String>)> {
        self.seen.lock().unwrap().clone()
    }

    /// The preflight and publish POSTs since `from`.
    fn sends_since(&self, from: usize) -> Vec<(String, String, Option<String>)> {
        self.sent()[from..]
            .iter()
            .filter(|(method, path, _)| {
                method == "POST"
                    && (*path == format!("{PREFIX}/publish/preflight")
                        || *path == format!("{PREFIX}/publish"))
            })
            .cloned()
            .collect()
    }

    fn grants(&self, grants: Value) {
        *self.door.grants.lock().unwrap() = grants;
    }

    fn install(&self) -> App {
        let source = self.root.path().join("app-src");
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(source.join("workflows/post.md"), WORKFLOW).unwrap();
        std::fs::write(
            source.join("app.md"),
            "---\napp: c1291-accept\ntitle: CAD-1291 accept\nversion: '0.1.0'\n\
             summary: Standing grant fixture.\nneeds:\n  connections: []\n  capabilities:\n    publication:\n      schema: 1\n      capability: text.publish\n      version: 1\n      action: publish\n      resource_kind: connection_account\n      effect: send\n---\n\n# CAD-1291 accept\n",
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
                "request_id": "ctx-1291"}),
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
                "connection_id": connection, "request_id": "bind-1291"}),
        );
        let binding = self.publish_set(&install, &bound["binding"], DEST);
        App {
            install,
            bundle: installed["digest"].as_str().unwrap().to_string(),
            binding,
        }
    }

    fn publish_set(&self, install: &str, binding: &Value, destination: &str) -> Value {
        self.op(
            "app_binding_publish_set",
            json!({"install_id": install, "binding_id": binding["id"],
                "expected_revision": binding["revision"],
                "destination_id": destination, "destination_label": "Harbour",
                "toolkit": "facebook", "timezone": "Asia/Hong_Kong",
                "grant_id": TYPED_GRANT}),
        )["binding"]
            .clone()
    }

    /// One draft plus its frozen effect in exactly the shape
    /// `stage_social_draft_effect` freezes (no grant id), approved when
    /// `approve`.
    /// Returns `(effect_id, digest, draft_id)`.
    fn effect(&self, app: &App, tag: &str, approve: bool) -> (String, String, String) {
        let records = RecordStore::open(&self.dir(), &app.install).unwrap();
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
        let hex: String = app.install.bytes().map(|b| format!("{b:02x}")).collect();
        let n = self.effects.fetch_add(1, SeqCst) + 1;
        let id = format!("sfx_{hex}_{n:032x}");
        let caption_digest =
            cadence_agent::platform::agenticos_external::publish::caption_digest_of(CAPTION);
        let frozen = json!({
            "source": {"kind": "social_draft", "draft_id": draft_id, "revision": 1},
            "install_id": app.install, "context_id": self.ctx(), "bundle_digest": app.bundle,
            "draft_id": draft_id, "revision": 1, "caption": CAPTION,
            "caption_digest": caption_digest, "asset_id": null, "image_digest": null,
            "mime": null, "size_bytes": null, "toolkit": "facebook",
            "destination_id": DEST, "destination_label": "Harbour",
            "timezone": "Asia/Hong_Kong", "aos_connection_id": AOS_CONN,
            "binding": {"slot": "publication", "revision": app.binding["revision"],
                "digest": app.binding["digest"], "config": app.binding["config"]},
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

    fn edit(&self, app: &App, draft: &str, caption: &str, asset: Option<Option<&str>>, tag: &str) {
        RecordStore::open(&self.dir(), &app.install)
            .unwrap()
            .app_social_draft_update(
                self.ctx(),
                draft,
                &SocialDraftEdit {
                    expected: 1,
                    caption,
                    asset_id: asset,
                    request_id: &format!("edit-{tag}"),
                    actor: "session:operator",
                },
            )
            .unwrap();
    }

    fn publish_now(&self, who: Asserted, id: &str, digest: &str) -> cadence_agent::Result<Value> {
        self.rpc_as(
            who,
            "app_effect_publish_now",
            json!({"effect_id": id, "digest": digest}),
        )
    }

    fn state(&self, app: &App, id: &str) -> String {
        RecordStore::open(&self.dir(), &app.install)
            .unwrap()
            .app_social_effect_show(id)
            .unwrap()["effect"]["state"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// `publish_now` as the operator must be refused with `expect` in the
    /// message, send nothing, and leave the effect in `state`.
    fn refused(&self, app: &App, id: &str, digest: &str, expect: &str, state: &str, case: &str) {
        let from = self.sent().len();
        let refused = self.publish_now(Asserted::Operator, id, digest);
        assert!(
            self.sends_since(from).is_empty(),
            "{case}: reached preflight or publish: {:?}",
            &self.sent()[from..]
        );
        let refused = refused
            .err()
            .unwrap_or_else(|| panic!("{case}: publish was not refused"))
            .to_string();
        assert!(refused.contains(expect), "{case}: {refused}");
        assert_eq!(self.state(app, id), state, "{case}");
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
fn standing_grant_publish_sends_only_an_approved_unchanged_effect_with_a_live_matching_grant() {
    let fx = Fx::start();
    let app = fx.install();
    // A matching live grant is listed throughout (b): nothing but the
    // approval and change guards stands between those effects and a send.
    fx.grants(json!([standing(|_| {})]));

    // (a) An agent or an unproven (container) caller cannot publish an
    // approved, unchanged effect, even with a matching live grant: the
    // daemon refuses before any door request.
    let (ready, ready_digest, _) = fx.effect(&app, "ready", true);
    for who in [Asserted::Agent("worker-1291".into()), Asserted::Unproven] {
        let from = fx.sent().len();
        let label = who.as_str();
        let refused = fx.publish_now(who, &ready, &ready_digest);
        assert!(refused.is_err(), "{label} published: {refused:?}");
        assert_eq!(
            fx.sent().len(),
            from,
            "{label} reached the door: {:?}",
            &fx.sent()[from..]
        );
        assert_eq!(fx.state(&app, &ready), "approved", "{label}");
    }
    // A grant id smuggled into the request is refused outright.
    let from = fx.sent().len();
    let forged = fx.rpc_as(
        Asserted::Operator,
        "app_effect_publish_now",
        json!({"effect_id": ready, "digest": ready_digest, "grant_id": STANDING}),
    );
    assert!(forged.is_err(), "request grant_id accepted: {forged:?}");
    assert_eq!(fx.sent().len(), from, "forged request reached the door");

    // (b) Unapproved.
    let (id, digest, _) = fx.effect(&app, "unapproved", false);
    fx.refused(
        &app,
        &id,
        &digest,
        "exact approved social draft effect",
        "waiting",
        "unapproved",
    );
    // (b) Caption changed after approval.
    let (id, digest, draft) = fx.effect(&app, "caption", true);
    fx.edit(&app, &draft, "Edited after approval", None, "caption");
    fx.refused(
        &app,
        &id,
        &digest,
        "social draft changed since approval",
        "approved",
        "caption changed",
    );
    // (b) Image changed after approval (same caption, new asset).
    let (id, digest, draft) = fx.effect(&app, "image", true);
    fx.edit(
        &app,
        &draft,
        CAPTION,
        Some(Some("asset-swapped-1291")),
        "image",
    );
    fx.refused(
        &app,
        &id,
        &digest,
        "social draft changed since approval",
        "approved",
        "image changed",
    );
    // (b) Destination changed after approval: the AOS account behind the
    // approved destination is now another connection.
    let (id, digest, _) = fx.effect(&app, "account", true);
    *fx.door.connection.lock().unwrap() = "conn_aos_other".into();
    fx.refused(
        &app,
        &id,
        &digest,
        "social destination changed since approval",
        "approved",
        "destination account changed",
    );
    *fx.door.connection.lock().unwrap() = AOS_CONN.into();

    // (c) No live standing grant for exactly this connection, destination
    // and toolkit: the approved, unchanged `ready` effect stays unsent.
    let cases = [
        ("missing grant", json!([]), "grant_required"),
        (
            "grant for another destination",
            json!([standing(|g| g["destinationId"] = json!(OTHER_DEST))]),
            "grant_required",
        ),
        (
            "grant for another connection",
            json!([standing(|g| g["connectionId"] = json!("conn_aos_other"))]),
            "grant_required",
        ),
        (
            "grant for another toolkit",
            json!([standing(|g| g["toolkit"] = json!("instagram"))]),
            "grant_required",
        ),
        (
            "revoked grant",
            json!([standing(
                |g| g["revokedAt"] = json!("2026-10-09T00:10:00.000Z")
            )]),
            "grant_revoked",
        ),
        (
            "per-post record, not a standing grant",
            json!([standing(|g| g["kind"] = json!("post"))]),
            "uncertain",
        ),
    ];
    for (case, grants, code) in cases {
        fx.grants(grants);
        let from = fx.sent().len();
        fx.refused(&app, &ready, &ready_digest, code, "approved", case);
        assert!(
            fx.sent()[from..]
                .iter()
                .any(|(m, p, _)| m == "GET" && p.starts_with(&format!("{PREFIX}/publish/grants"))),
            "{case}: the door was never asked for the grant: {:?}",
            &fx.sent()[from..]
        );
    }

    // (d) Positive control: the same approved, unchanged effect with a
    // matching live grant reaches preflight and publish, presenting the
    // door's standing grant and never the binding's typed grant id.
    fx.grants(json!([
        standing(|g| g["revokedAt"] = json!("2026-10-09T00:10:00.000Z")),
        standing(|_| {})
    ]));
    let from = fx.sent().len();
    let posted = fx
        .publish_now(Asserted::Operator, &ready, &ready_digest)
        .unwrap_or_else(|e| panic!("control publish refused: {e}"));
    let sends = fx.sends_since(from);
    for route in ["/publish/preflight", "/publish"] {
        assert!(
            sends
                .iter()
                .any(|(_, p, _)| *p == format!("{PREFIX}{route}")),
            "control never reached {route}: {:?}",
            &fx.sent()[from..]
        );
    }
    assert!(
        sends.iter().all(|(_, _, g)| g.as_deref() == Some(STANDING)),
        "control presented a grant other than the standing one: {sends:?}"
    );
    assert!(
        fx.sent()
            .iter()
            .all(|(_, _, g)| g.as_deref() != Some(TYPED_GRANT)),
        "the binding's typed grant id reached the door: {:?}",
        fx.sent()
    );
    assert_eq!(fx.state(&app, &ready), "posted", "{posted}");
}

// CAD-1397 independent acceptance, added separately from the unchanged
// CAD-1291 check above. Reuses its actual hosted HTTP sender, resolver,
// AOS-schema fake door, and RecordStore draft fixture; no legacy intent or
// run/artifact authority is manufactured.
mod cad1397_social_draft_schedule_acceptance {
    use super::*;
    use cadence_agent::test_seam::{AS_HEADER, TOKEN_HEADER};

    struct Board {
        port: u16,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Board {
        fn start(fx: &Fx) -> Self {
            let port = (3110..3200)
                .find(|port| std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok())
                .expect("free isolated test board port");
            let (tx, rx) = std::sync::mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));
            let opts = cadence_agent::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(stop.clone()),
                startup: Some(tx),
                test_seam: true,
                ..Default::default()
            };
            let dir = fx.dir();
            let pm = fx.root.path().join("pm");
            let thread = std::thread::spawn(move || {
                let _ = cadence_agent::ui::serve(&dir, &pm, &opts);
            });
            let startup = rx.recv_timeout(Duration::from_secs(10));
            if !matches!(startup, Ok(Ok(()))) {
                stop.store(true, SeqCst);
                let _ = thread.join();
                panic!("isolated test board startup failed: {startup:?}");
            }
            Self {
                port,
                stop,
                thread: Some(thread),
            }
        }

        fn post(
            &self,
            fx: &Fx,
            who: &Asserted,
            path: &str,
            body: &Value,
            session: Option<&(String, String)>,
        ) -> (u16, String, Option<String>) {
            let host = format!("cadence-{}.localhost:{}", self.port, self.port);
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .into();
            let mut request = agent
                .post(format!("http://127.0.0.1:{}{path}", self.port))
                .header("Host", &host)
                .header("Content-Type", "application/json")
                .header(AS_HEADER, who.as_str())
                .header(TOKEN_HEADER, Seam::token_at(&fx.dir()).unwrap())
                .header("Origin", format!("http://{host}"))
                .header("X-Cadence-Board", "1");
            if let Some((cookie, key)) = session {
                request = request
                    .header("Cookie", cookie)
                    .header("X-Cadence-Session", key);
            }
            let mut response = request.send(body.to_string()).unwrap();
            let status = response.status().as_u16();
            let cookie = response
                .headers()
                .get("set-cookie")
                .and_then(|value| value.to_str().ok())
                .map(|value| value.split(';').next().unwrap().to_owned());
            (
                status,
                response.body_mut().read_to_string().unwrap(),
                cookie,
            )
        }
    }

    impl Drop for Board {
        fn drop(&mut self) {
            self.stop.store(true, SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn operator_session(fx: &Fx, board: &Board) -> (String, String) {
        cadence_agent::operator_auth::ensure_secret(&fx.dir()).unwrap();
        let secret = cadence_agent::operator_auth::read_secret(&fx.dir()).unwrap();
        let nonce = fx.op(
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )["nonce"]
            .clone();
        let (status, body, cookie) = board.post(
            fx,
            &Asserted::Operator,
            "/api/session",
            &json!({"nonce": nonce}),
            None,
        );
        assert_eq!(status, 200, "operator session exchange: {body}");
        let key = serde_json::from_str::<Value>(&body).unwrap()["session_key"]
            .as_str()
            .unwrap()
            .to_owned();
        let cookie = cookie.unwrap();
        let (name, token) = cookie.split_once('=').expect("operator session cookie");
        assert_eq!(name, format!("cadence_operator_{}", board.port));
        let checked = fx
            .rpc_as(
                Asserted::Operator,
                "operator_session_check",
                json!({"token":token,"key":key,"origin":"loopback"}),
            )
            .unwrap();
        assert_eq!(
            checked["valid"], true,
            "new operator session did not validate"
        );
        (cookie, key)
    }

    fn epoch() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    struct ResumeGrant(std::option::Option<std::sync::mpsc::Sender<()>>);

    impl Drop for ResumeGrant {
        fn drop(&mut self) {
            if let Some(resume) = self.0.take() {
                let _ = resume.send(());
            }
        }
    }

    fn count_publish_posts(fx: &Fx) -> usize {
        fx.sent()
            .iter()
            .filter(|(method, path, _)| {
                method == "POST"
                    && (path == &format!("{PREFIX}/publish/preflight")
                        || path == &format!("{PREFIX}/publish"))
            })
            .count()
    }

    fn effect_record(fx: &Fx, app: &App, id: &str) -> Value {
        RecordStore::open(&fx.dir(), &app.install)
            .unwrap()
            .app_social_effect_show(id)
            .unwrap()["effect"]
            .clone()
    }

    #[test]
    fn social_draft_schedule_is_operator_cas_bound_and_dispatches_once_under_live_grant() {
        let fx = Fx::start();
        let app = fx.install();
        let board = Board::start(&fx);
        let mut session = operator_session(&fx, &board);
        fx.grants(json!([
            standing(|g| g["revokedAt"] = json!("2026-10-09T00:10:00.000Z")),
            standing(|_| {}),
        ]));

        let (stale, stale_digest, stale_draft) = fx.effect(&app, "cad1397-stale", false);
        let stale_due = epoch() + 3_600;
        // A waiting effect does not freeze the draft against edits. Change it
        // before approval, then use the real approval guard: it must refuse
        // stale material, and the still-waiting effect cannot be scheduled.
        RecordStore::open(&fx.dir(), &app.install)
            .unwrap()
            .app_social_draft_update(
                fx.ctx(),
                &stale_draft,
                &SocialDraftEdit {
                    expected: 1,
                    caption: "changed while awaiting approval",
                    asset_id: None,
                    request_id: "cad1397-edit-waiting",
                    actor: "session:fixture",
                },
            )
            .unwrap();
        let before = count_publish_posts(&fx);
        assert!(
            fx.rpc_as(
                Asserted::Operator,
                "app_effect_decide",
                json!({
                    "effect_id":stale,"digest":stale_digest,"decision":"accept"
                })
            )
            .is_err(),
            "stale waiting draft was approved"
        );
        assert!(
            fx.rpc_as(
                Asserted::Operator,
                "app_effect_schedule",
                json!({
                    "effect_id":stale,"digest":stale_digest,"due_epoch":stale_due
                })
            )
            .is_err(),
            "stale unapproved effect was scheduled"
        );
        assert_eq!(effect_record(&fx, &app, &stale)["state"], "waiting");
        assert_eq!(count_publish_posts(&fx), before);

        let (effect, digest, _) = fx.effect(&app, "cad1397-scheduled", true);
        let schedule_path = format!("/api/app-effects/{effect}/schedule");
        let due = epoch() + 3_600;
        let schedule = json!({"effect_id":effect,"digest":digest,"due_epoch":due});
        let schedule_body = json!({"digest":digest,"due_epoch":due});

        // Agent requests are refused at both real guard boundaries, including
        // when carrying a valid operator session to the board.
        let agent = Asserted::Agent("cad1397-worker".into());
        assert!(
            fx.rpc_as(agent.clone(), "app_effect_schedule", schedule.clone())
                .is_err(),
            "agent scheduled over RPC"
        );
        let (status, body, _) =
            board.post(&fx, &agent, &schedule_path, &schedule_body, Some(&session));
        assert_eq!(status, 403, "agent scheduled over HTTP: {body}");
        // Presenting an operator session from an agent peer revokes it; use a
        // fresh operator login for the owner-path assertions that follow.
        session = operator_session(&fx, &board);

        // Destination, timezone and grant are authority, not request fields.
        for field in ["destination_id", "timezone", "grant_id"] {
            let mut http = schedule_body.clone();
            http[field] = json!("forged");
            let (status, body, _) = board.post(
                &fx,
                &Asserted::Operator,
                &schedule_path,
                &http,
                Some(&session),
            );
            assert!(
                (400..500).contains(&status),
                "HTTP forged {field}: {status} {body}"
            );
            let mut rpc = schedule.clone();
            rpc[field] = json!("forged");
            assert!(
                fx.rpc_as(Asserted::Operator, "app_effect_schedule", rpc)
                    .is_err(),
                "RPC forged {field}"
            );
        }

        // Invalid horizon and stale CAS do not change the queued effect.
        let too_far = epoch() + 366 * 24 * 60 * 60 + 1;
        assert!(
            fx.rpc_as(
                Asserted::Operator,
                "app_effect_schedule",
                json!({
                    "effect_id":effect,"digest":digest,"due_epoch":too_far
                })
            )
            .is_err(),
            "schedule beyond 366 days accepted"
        );
        let (status, body, _) = board.post(
            &fx,
            &Asserted::Operator,
            &schedule_path,
            &schedule_body,
            Some(&session),
        );
        assert_eq!(status, 200, "owner schedule: {body}");
        let first = effect_record(&fx, &app, &effect);
        assert_eq!(first["state"], "scheduled");
        let scheduled_list = fx.op("app_effect_list", json!({"install_id":app.install}));
        assert!(
            scheduled_list["effects"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| { row["effect_id"] == effect && row["state"] == "scheduled" }),
            "installation-wide list omitted scheduled social effect: {scheduled_list}"
        );
        let replay = fx.rpc_as(Asserted::Operator, "app_effect_schedule", schedule.clone());
        assert!(
            replay.is_err(),
            "schedule double-tap changed the effect: {replay:?}"
        );
        assert_eq!(effect_record(&fx, &app, &effect), first);
        assert_eq!(count_publish_posts(&fx), before);

        let changed_due = due + 600;
        assert!(fx.rpc_as(Asserted::Operator, "app_effect_reschedule", json!({
            "effect_id":effect,"digest":digest,"expected_due_epoch":due+1,"due_epoch":changed_due
        })).is_err(), "stale CAS reschedule succeeded");
        assert_eq!(
            effect_record(&fx, &app, &effect),
            first,
            "stale CAS mutated schedule"
        );
        let (status, body, _) = board.post(
            &fx,
            &Asserted::Operator,
            &format!("/api/app-effects/{effect}/reschedule"),
            &json!({"digest":digest,"expected_due_epoch":due,"due_epoch":changed_due}),
            Some(&session),
        );
        assert_eq!(status, 200, "owner CAS reschedule: {body}");
        let rescheduled = effect_record(&fx, &app, &effect);
        assert_eq!(rescheduled["state"], "scheduled");
        assert_eq!(rescheduled["due_epoch"], changed_due);
        let (status, body, _) = board.post(
            &fx,
            &Asserted::Operator,
            &format!("/api/app-effects/{effect}/cancel"),
            &json!({"digest":digest,"expected_due_epoch":changed_due}),
            Some(&session),
        );
        assert_eq!(status, 200, "owner cancel before claim: {body}");
        assert_eq!(effect_record(&fx, &app, &effect)["state"], "cancelled");
        assert_eq!(count_publish_posts(&fx), before);

        // The installation-wide RPC list includes RecordStore-backed effects;
        // a different explicit context and a foreign installation do not.
        let listed = fx.op("app_effect_list", json!({"install_id":app.install}));
        assert!(
            listed["effects"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| { row["effect_id"] == effect && row["state"] == "cancelled" }),
            "installation-wide list omitted cancelled social effect: {listed}"
        );
        let other_context = fx.op(
            "app_context_create",
            json!({
                "install_id":app.install,"label":"Other context","input_defaults":{},
                "request_id":"cad1397-other-context"
            }),
        );
        let other_context = other_context["context"]["id"].as_str().unwrap();
        let scoped = fx.op(
            "app_effect_list",
            json!({
                "install_id":app.install,"context_id":other_context
            }),
        );
        assert!(
            !scoped["effects"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| { row["effect_id"] == effect }),
            "other context exposed social effect: {scoped}"
        );
        let foreign_source = fx.root.path().join("foreign-app-src");
        std::fs::create_dir_all(foreign_source.join("workflows")).unwrap();
        std::fs::write(foreign_source.join("workflows/post.md"), WORKFLOW).unwrap();
        std::fs::write(foreign_source.join("app.md"), "---\napp: cad1397-foreign\ntitle: Foreign\nversion: '0.1.0'\nsummary: Foreign fixture.\nneeds:\n  connections: []\n  capabilities: {}\n---\n\n# Foreign\n").unwrap();
        let foreign = fx.op(
            "app_workspace_install",
            json!({
                "source":foreign_source.to_str().unwrap()
            }),
        );
        let foreign = fx.op(
            "app_effect_list",
            json!({
                "install_id":foreign["install_id"]
            }),
        );
        assert!(
            !foreign["effects"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| { row["effect_id"] == effect }),
            "foreign installation exposed social effect: {foreign}"
        );
        assert_eq!(count_publish_posts(&fx), before);

        // A separate due effect proves the positive control uses the real
        // hosted sender's live-grant lookup and reaches execute once. Repeat
        // schedule/confirm after posting must only return/refuse the receipt.
        let before_seen = fx.sent().len();
        let (due_effect, due_digest, _) = fx.effect(&app, "cad1397-dispatch", true);
        let due_at = epoch() + 2;
        let due_body = json!({"digest":due_digest,"due_epoch":due_at});
        let (status, body, _) = board.post(
            &fx,
            &Asserted::Operator,
            &format!("/api/app-effects/{due_effect}/schedule"),
            &due_body,
            Some(&session),
        );
        assert_eq!(status, 200, "owner schedule for due dispatch: {body}");
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while fx.state(&app, &due_effect) != "posted" {
            assert_ne!(
                fx.state(&app, &due_effect),
                "refused",
                "due effect was refused: {}; hosted requests: {:?}",
                effect_record(&fx, &app, &due_effect),
                fx.sent()
            );
            assert!(
                std::time::Instant::now() < deadline,
                "due effect did not settle: {}",
                effect_record(&fx, &app, &due_effect)
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let sends = fx.sends_since(before_seen);
        assert_eq!(
            sends
                .iter()
                .filter(|(_, path, _)| path == &format!("{PREFIX}/publish"))
                .count(),
            1,
            "due effect executed other than once: {sends:?}"
        );
        assert!(
            sends
                .iter()
                .all(|(_, _, grant)| grant.as_deref() == Some(STANDING)),
            "due effect did not use the live matching standing grant: {sends:?}"
        );
        assert!(
            fx.sent()
                .iter()
                .all(|(_, _, grant)| grant.as_deref() != Some(TYPED_GRANT)),
            "legacy typed grant reached hosted sender"
        );
        let after_post = count_publish_posts(&fx);
        let _replay = fx.rpc_as(
            Asserted::Operator,
            "app_effect_confirm_publish",
            json!({
                "effect_id":due_effect,"digest":due_digest
            }),
        );
        assert_eq!(
            count_publish_posts(&fx),
            after_post,
            "posted-effect replay executed again"
        );

        // Revoked/mismatched consent must stop at the fresh grant lookup;
        // this fixture's sender and AOS wire are isolated from production.
        let (held, held_digest, _) = fx.effect(&app, "cad1397-mismatched-grant", true);
        fx.grants(json!([
            standing(|g| g["destinationId"] = json!(OTHER_DEST))
        ]));
        let from_mismatch = fx.sent().len();
        let held_at = epoch() + 2;
        let (status, body, _) = board.post(
            &fx,
            &Asserted::Operator,
            &format!("/api/app-effects/{held}/schedule"),
            &json!({"digest":held_digest,"due_epoch":held_at}),
            Some(&session),
        );
        assert_eq!(
            status, 200,
            "owner schedule under mismatched live grant: {body}"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let asked = fx.sent()[from_mismatch..].iter().any(|(method, path, _)| {
                method == "GET" && path.starts_with(&format!("{PREFIX}/publish/grants"))
            });
            if asked {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "due dispatcher never rechecked the grant"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(
            count_publish_posts(&fx),
            after_post,
            "mismatched grant reached preflight or execute"
        );
        assert_ne!(
            effect_record(&fx, &app, &held)["state"],
            "posted",
            "mismatched grant posted"
        );
    }

    #[test]
    fn revoke_during_live_grant_lookup_refuses_before_claim_or_send() {
        let fx = Fx::start();
        let app = fx.install();
        fx.grants(json!([standing(|_| {})]));
        let (effect, digest, _) = fx.effect(&app, "cad1397-binding-race", true);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        *fx.door.grant_latch.lock().unwrap() = Some(Arc::new(GrantLatch {
            entered: entered_tx,
            resume: Mutex::new(resume_rx),
        }));
        let from = fx.sent().len();
        let due = epoch() + 2;
        assert!(
            fx.rpc_as(
                Asserted::Operator,
                "app_effect_schedule",
                json!({
                    "effect_id":effect,"digest":digest,"due_epoch":due
                })
            )
            .is_ok(),
            "valid effect did not schedule"
        );
        let _resume_on_exit = ResumeGrant(Some(resume_tx));
        entered_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("dispatcher never paused at AOS standing-grant lookup");

        // This is the real operator RPC, serialized against the publish
        // decision by the production binding guard, while the network lookup
        // remains paused outside the lock.
        fx.op(
            "app_binding_revoke",
            json!({
                "install_id":app.install,"binding_id":app.binding["id"],
                "expected_revision":app.binding["revision"]
            }),
        );
        drop(_resume_on_exit);

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while effect_record(&fx, &app, &effect)["state"] == "scheduled" {
            assert!(
                std::time::Instant::now() < deadline,
                "dispatcher did not settle after binding revoke"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            effect_record(&fx, &app, &effect)["state"],
            "refused",
            "stale binding authority was not refused"
        );
        assert!(
            fx.sends_since(from).is_empty(),
            "stale binding reached preflight/execute: {:?}",
            &fx.sent()[from..]
        );
    }

    // The base table below is copied from the supported pre-CAD-1397
    // DRAFT_SCHEMA in the repository history; the negative case adds exactly
    // one unrecognized CHECK constraint to that real legacy shape.
    fn legacy_record_db(state: &std::path::Path, install: &str, extra_check: bool) -> String {
        let _records = RecordStore::open(state, install).unwrap();
        let path = state.join("app-records").join(format!("{install}.sqlite3"));
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE app_social_effects;")
            .unwrap();
        let extra = if extra_check {
            ", CHECK(approval_id <> 'reject-migration')"
        } else {
            ""
        };
        conn.execute_batch(&format!(r#"CREATE TABLE app_social_effects(
 effect_id TEXT PRIMARY KEY, install_id TEXT NOT NULL, context_id TEXT NOT NULL,
 draft_id TEXT NOT NULL, revision INTEGER NOT NULL, request_id TEXT NOT NULL UNIQUE,
 digest TEXT NOT NULL, frozen_json TEXT NOT NULL, state TEXT NOT NULL
 CHECK(state IN ('waiting','approved','declined','sending','posted','refused')),
 approval_id TEXT NOT NULL, outcome_json TEXT, media_key TEXT, created REAL NOT NULL, updated REAL NOT NULL{extra});
INSERT INTO app_social_effects VALUES('legacy-effect','{install}','legacy-context','legacy-draft',1,'legacy-request','legacy-digest','{{}}','approved','legacy-approved',NULL,NULL,1.0,1.0);"#)).unwrap();
        conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='app_social_effects'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn legacy_migration_refuses_unknown_constraints_without_rewriting_and_accepts_supported_schema()
    {
        let rejected_root = tempfile::tempdir().unwrap();
        let rejected_install = "cad1397-extra-check";
        let original_sql = legacy_record_db(rejected_root.path(), rejected_install, true);
        let refused = RecordStore::open(rejected_root.path(), rejected_install);
        assert!(
            refused.is_err(),
            "unknown same-column CHECK was silently discarded"
        );
        let path = rejected_root
            .path()
            .join("app-records")
            .join(format!("{rejected_install}.sqlite3"));
        let conn = rusqlite::Connection::open(path).unwrap();
        let after_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='app_social_effects'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let row: (String, String, String) = conn
            .query_row(
                "SELECT effect_id,approval_id,state FROM app_social_effects",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(after_sql, original_sql, "refusal rewrote legacy table SQL");
        assert_eq!(
            row,
            (
                "legacy-effect".into(),
                "legacy-approved".into(),
                "approved".into()
            ),
            "refusal changed legacy row"
        );

        let supported_root = tempfile::tempdir().unwrap();
        let supported_install = "cad1397-supported-legacy";
        legacy_record_db(supported_root.path(), supported_install, false);
        let migrated = RecordStore::open(supported_root.path(), supported_install)
            .expect("supported historical DDL should migrate");
        let effect = migrated.app_social_effect_show("legacy-effect").unwrap()["effect"].clone();
        assert_eq!(effect["state"], "approved");
        assert!(
            effect.get("due_epoch").is_some(),
            "migration omitted schedule column"
        );
        drop(migrated);
        let reopened = RecordStore::open(supported_root.path(), supported_install)
            .expect("migrated historical schema should reopen");
        let reopened_effect =
            reopened.app_social_effect_show("legacy-effect").unwrap()["effect"].clone();
        assert_eq!(reopened_effect["effect_id"], "legacy-effect");
        assert_eq!(reopened_effect["state"], "approved");
        assert_eq!(reopened_effect["approval_id"], "legacy-approved");
        assert!(
            reopened_effect.get("due_epoch").is_some(),
            "reopened migrated effect lost schedule column"
        );
    }
}
