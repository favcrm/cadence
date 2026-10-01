//! CAD-979 native seam — pre-freeze `media_key` binding test (tests first).
//!
//! An operator passes a `media_key` to `social_publish_schedule`. The freeze
//! must refuse a key that does not bind THIS run's connection + image digest
//! (`dp1.<ws>.<connection>.<image_digest[..32]>`), not accept-and-freeze it.
//! Written before the freeze guard lands: this is the genuine red.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::contract_fixture::{ToolTable, Verified};
use cadence_agent::platform::agenticos_external::media_import::MediaImporter;
use cadence_agent::platform::agenticos_external::publish_sender::DeviceCredential;
use cadence_agent::platform::deployments::DeploymentMetadata;
use cadence_agent::platform::{
    agenticos_external, AppCapabilityAsset, AppCapabilityOutput, AppCapabilityQuote,
    PlatformAdapter,
};
use common::app_release::{Release, OWNER, REVIEWER, WRITER};
use image::ImageEncoder as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Emits one real 1×1 PNG through the `media.generate` image capability so
/// the `image-manual` run completes with a reviewed binary asset.
struct SyntheticMedia {
    inner: Arc<dyn PlatformAdapter>,
    png: Vec<u8>,
}
impl PlatformAdapter for SyntheticMedia {
    fn table(&self) -> &ToolTable {
        self.inner.table()
    }
    fn connection_descriptor(
        &self,
    ) -> Option<cadence_agent::platform::connections::ProviderDescriptor> {
        self.inner.connection_descriptor()
    }
    fn connection_registration(&self) -> Option<String> {
        self.inner.connection_registration()
    }
    fn app_credentialless_account(&self, account: &str) -> bool {
        self.inner.app_credentialless_account(account)
    }
    fn reported_manifest_version(&self) -> Option<String> {
        self.inner.reported_manifest_version()
    }
    fn preview(&self, account: &str, tool: &str, input: &Value) -> String {
        self.inner.preview(account, tool, input)
    }
    fn execute(
        &self,
        credential: &[u8],
        tool: &str,
        input: &Value,
        key: &str,
        hash: Option<&str>,
    ) -> Result<Value, String> {
        self.inner.execute(credential, tool, input, key, hash)
    }
    fn read_back(&self, tool: &str, input: &Value) -> Verified {
        self.inner.read_back(tool, input)
    }
    fn source_hash(&self, agent: &str, source: &str) -> Option<String> {
        self.inner.source_hash(agent, source)
    }
    fn implied_source(&self, agent: &str, tool: &str, input: &Value) -> Option<String> {
        self.inner.implied_source(agent, tool, input)
    }
    fn quote_app_capability(
        &self,
        _credential: &[u8],
        _binding: &Value,
    ) -> Result<AppCapabilityQuote, String> {
        Ok(AppCapabilityQuote {
            schema: 1,
            currency: "USD".into(),
            unit_price_micros: 31_500,
            units: 1,
            total_price_micros: 31_500,
            price_revision: "media:sha256:cad979".into(),
        })
    }
    fn execute_app_capability(
        &self,
        _credential: &[u8],
        _authority: &Value,
        _input: &Value,
        _key: &str,
    ) -> Result<AppCapabilityOutput, String> {
        Ok(AppCapabilityOutput {
            result: json!({"schema":1,"kind":"media.generated.image","provider":"agenticos_external"}),
            asset: Some(AppCapabilityAsset {
                media_type: "image/png".into(),
                bytes: self.png.clone(),
            }),
        })
    }
}

/// One real PNG and the harness with the media adapter registered.
fn png_harness() -> (Release, Vec<u8>) {
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&[0], 1, 1, image::ExtendedColorType::L8)
        .unwrap();
    let media = png.clone();
    let h = Release::with_social_image(move |opts, _| {
        opts.provider_deployments = Some(
            DeploymentMetadata::parse(
                br#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@2","transport":"hosted-media-lease@1"}]}"#,
            )
            .unwrap(),
        );
        agenticos_external::attach(opts).unwrap();
        let inner = opts.platforms.remove("agenticos_external").unwrap();
        opts.platforms.insert(
            "agenticos_external".into(),
            Arc::new(SyntheticMedia { inner, png: media }),
        );
    });
    (h, png)
}

/// Drive an `image-manual` run (install scope, no context) to an approved,
/// asset-bearing completion; return `(run, bundle_digest, install_id, image_digest)`.
fn approved_image_run(h: &Release, png: &[u8], tag: &str) -> (Value, String, String, String) {
    let hosted = h.daemon.operator_rpc("connection_list", json!({})).unwrap()["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["provider"] == "agenticos_external")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    h.daemon
        .operator_rpc("app_binding_create", json!({"install_id":h.install["install_id"],"slot":"image","connection_id":hosted,"request_id":format!("cad979-img-{tag}")}))
        .unwrap();
    h.daemon
        .operator_rpc("app_binding_create", json!({"install_id":h.install["install_id"],"slot":"publication","connection_id":h.connection,"request_id":format!("cad979-pub-{tag}")}))
        .unwrap();
    let run = h.daemon.operator_rpc("app_run_create", json!({"install_id":h.install["install_id"],"workflow":"image-manual","inputs":{"subject":"Customer follow-up","source":"JuicySuite CRM helps teams track customers","brand_voice":"Warm and clear","content_prompt":"Write a concise zh-HK caption grounded only in the source facts.","image_prompt":"Create one editorial image grounded only in the source facts.","writer":WRITER,"reviewer":REVIEWER},"request_id":format!("cad979-run-{tag}"),"owner_pm":OWNER})).unwrap();
    std::fs::write(
        h.daemon.state.join(format!(
            "social-image-probe-{}.json",
            run["id"].as_str().unwrap()
        )),
        json!({"slot":"image"}).to_string(),
    )
    .unwrap();
    h.dispatch(&run);
    let run = h.wait_state(run["id"].as_str().unwrap(), "succeeded");
    let receipt = h
        .daemon
        .operator_rpc("app_run_capability_results", json!({"run_id":run["id"]}))
        .unwrap()["results"][0]
        .clone();
    assert_eq!(receipt["asset"]["media_type"], "image/png");
    let image_digest = format!("{:x}", Sha256::digest(png));
    assert_eq!(
        receipt["asset"]["digest"].as_str().unwrap(),
        format!("sha256:{image_digest}"),
    );
    let bundle_digest = run["snapshot"]["bundle_digest"]
        .as_str()
        .unwrap()
        .to_owned();
    let install_id = h.install["install_id"].as_str().unwrap().to_owned();
    (run, bundle_digest, install_id, image_digest)
}

/// The workspace the daemon derived for this store (per-store random).
fn workspace(h: &Release) -> String {
    let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
    db.query_row(
        "SELECT workspace_id FROM connection_metadata WHERE singleton=1",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

fn schedule_body(
    run: &Value,
    bundle: &str,
    install: &str,
    request: &str,
    media_key: Option<String>,
) -> Value {
    let mut b = json!({"request_id": request, "install_id": install,
        "run_id": run["id"],
        "artifact_id": run["artifacts"][0]["id"],
        "bundle_digest": bundle,
        "slot": "publication", "effect_id": "cad_fx_cad979",
        "destination_id": "17841400008460056", "toolkit": "instagram",
        "grant_id": "dpq_synthetic_grant_ig", "approval_id": "cad_approval_cad979",
        "due_epoch": 1_750_000_000, "timezone": "Asia/Hong_Kong"});
    if let Some(key) = media_key {
        b["media_key"] = json!(key);
    }
    b
}

/// Positive control: a `media_key` that binds this run's connection +
/// image digest freezes cleanly.
#[test]
fn cad979_freeze_accepts_media_key_binding_reviewed_asset() {
    let (h, png) = png_harness();
    let (run, bundle, install, image_digest) = approved_image_run(&h, &png, "ok");
    let ws = workspace(&h);
    let key = format!("dp1.{ws}.{}.{:.32}", h.connection, image_digest);
    let intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            schedule_body(
                &run,
                &bundle,
                &install,
                "cad979-freeze-ok",
                Some(key.clone()),
            ),
        )
        .unwrap()["intent"]
        .clone();
    assert_eq!(intent["state"], "queued");
    assert_eq!(intent["frozen"]["media_key"], key);
    assert_eq!(intent["frozen"]["image_digest"], image_digest);
}

/// Genuine red: a `media_key` naming a FOREIGN connection must be refused at
/// freeze — the binding belongs to the reviewed asset, not the caller's word.
/// Without the `media_key_authorizes` check this wrongly freezes.
#[test]
fn cad979_freeze_refuses_media_key_for_foreign_connection() {
    let (h, png) = png_harness();
    let (run, bundle, install, image_digest) = approved_image_run(&h, &png, "xconn");
    let ws = workspace(&h);
    // Well-formed, but its connection part is not the run's binding connection.
    let foreign = format!("dp1.{ws}.con_other_install.{:.32}", image_digest);
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            schedule_body(
                &run,
                &bundle,
                &install,
                "cad979-freeze-xconn",
                Some(foreign),
            ),
        )
        .expect_err("a media_key for a foreign connection must be refused at freeze");
    let msg = err.to_string();
    assert!(
        msg.contains("grant_binding_mismatch") || msg.contains("media"),
        "expected a media-key binding refusal, got: {msg}"
    );
}

fn import_body(run: &Value, install: &str, context: Option<&str>, request: &str) -> Value {
    let mut b = json!({"request_id": request, "install_id": install,
        "run_id": run["id"], "artifact_id": run["artifacts"][0]["id"],
        "bundle_digest": run["snapshot"]["bundle_digest"], "slot": "publication"});
    if let Some(ctx) = context {
        b["context_id"] = json!(ctx);
    }
    b
}

/// I1 authority: an agent pane cannot reach the operator import verb.
#[test]
fn cad979_import_verb_is_operator_only() {
    let (h, png) = png_harness();
    let (run, _bundle, install, _d) = approved_image_run(&h, &png, "a1");
    let err = h
        .daemon
        .agent_rpc(
            "worker-0",
            "social_publish_media_import",
            import_body(&run, &install, None, "cad979-a1"),
        )
        .expect_err("an agent pane must not reach the media import");
    assert!(err.to_string().contains("operator"), "{err}");
}

/// I1 authority: an unproven peer cannot reach the operator import verb.
#[test]
fn cad979_import_verb_refuses_unproven_peer() {
    let (h, png) = png_harness();
    let (run, _bundle, install, _d) = approved_image_run(&h, &png, "a2");
    let err = h
        .daemon
        .unproven_rpc(
            "social_publish_media_import",
            import_body(&run, &install, None, "cad979-a2"),
        )
        .expect_err("an unproven peer must not reach the media import");
    assert!(err.to_string().contains("operator"), "{err}");
}

/// I2 scope pin (E3): a request naming a different install is refused even
/// though `app_publication_material` resolves the run's own binding.
#[test]
fn cad979_import_refuses_cross_install_scope() {
    let (h, png) = png_harness();
    let (run, _bundle, _install, _d) = approved_image_run(&h, &png, "a3");
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_media_import",
            import_body(&run, "install-FRIENDSHIP", None, "cad979-a3"),
        )
        .expect_err("a cross-install media import must be refused");
    assert!(err.to_string().contains("grant_binding_mismatch"), "{err}");
}

/// I2 scope pin: a request carrying a context the run does not own is
/// refused (exact/null-preserving — no wildcard match).
#[test]
fn cad979_import_refuses_foreign_context_scope() {
    let (h, png) = png_harness();
    let (run, _bundle, install, _d) = approved_image_run(&h, &png, "a4");
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_media_import",
            import_body(&run, &install, Some("ctx_foreign"), "cad979-a4"),
        )
        .expect_err("a foreign-context media import must be refused");
    assert!(err.to_string().contains("grant_binding_mismatch"), "{err}");
}

/// Honest unconfigured: with no `social_media_importer` wired the verb
/// fails closed `capability_unavailable` after the provenance + scope pin —
/// never a silent accept or a half-import.
#[test]
fn cad979_import_without_importer_is_capability_unavailable() {
    let (h, png) = png_harness();
    let (run, _bundle, install, _d) = approved_image_run(&h, &png, "a6");
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_media_import",
            import_body(&run, &install, None, "cad979-a6"),
        )
        .expect_err("an unconfigured importer must refuse capability_unavailable");
    assert!(err.to_string().contains("capability_unavailable"), "{err}");
}

/// Required `request_id`: missing, null, number, bool, object or empty/malformed
/// must refuse BEFORE any custody read or provider call.
#[test]
fn cad979_import_refuses_missing_or_malformed_request_id() {
    let (h, png) = png_harness();
    let (run, _bundle, install, _d) = approved_image_run(&h, &png, "reqid");
    // Missing entirely.
    let mut body = import_body(&run, &install, None, "x");
    body.as_object_mut().unwrap().remove("request_id");
    let err = h
        .daemon
        .operator_rpc("social_publish_media_import", body.clone())
        .expect_err("missing request_id must refuse");
    assert!(err.to_string().contains("request_id"), "{err}");
    // Malformed value kinds.
    for bad in [
        json!(null),
        json!(7),
        json!(true),
        json!({"a":1}),
        json!(""),
        json!("bad id!"),
    ] {
        body["request_id"] = bad.clone();
        let err = h
            .daemon
            .operator_rpc("social_publish_media_import", body.clone())
            .expect_err("malformed request_id must refuse");
        assert!(err.to_string().contains("request_id"), "{bad}: {err}");
    }
}

/// Strict `context_id`: absent/null → None (a context-less run); a valid
/// string → Some; a number/object/bool or empty/oversize string refuses
/// rather than silently mapping to None (which would wrongly match a
/// context-less run).
#[test]
fn cad979_import_refuses_malformed_context_id() {
    let (h, png) = png_harness();
    let (run, _bundle, install, _d) = approved_image_run(&h, &png, "ctxmal");
    for bad in [
        json!(7),
        json!(true),
        json!({"a":1}),
        json!(""),
        json!("bad id!"),
    ] {
        let mut body = import_body(&run, &install, None, "cad979-ctxmal");
        body["context_id"] = bad.clone();
        let err = h
            .daemon
            .operator_rpc("social_publish_media_import", body)
            .expect_err("malformed context_id must refuse");
        assert!(
            err.to_string().contains("context_id") || err.to_string().contains("malformed"),
            "{bad}: {err}"
        );
    }
}

/// I5 custody: strict_fields refuses caller bytes / path / URL / digest /
/// receipt / slot-content — only the provenance tuple reaches the handler.
#[test]
fn cad979_import_refuses_caller_material_fields() {
    let (h, png) = png_harness();
    let (run, _bundle, install, _d) = approved_image_run(&h, &png, "a5");
    for forged in ["bytes", "path", "url", "digest", "receipt_id", "image"] {
        let mut body = import_body(&run, &install, None, "cad979-a5");
        body[forged] = json!("x");
        let err = h
            .daemon
            .operator_rpc("social_publish_media_import", body)
            .expect_err("caller material fields must be refused");
        assert!(
            err.to_string().contains("unsupported fields"),
            "{forged}: {err}"
        );
    }
}

/// Genuine red: a `media_key` whose digest part is not this asset's must be
/// refused — the key binds a different image than the reviewed one.
#[test]
fn cad979_freeze_refuses_media_key_for_wrong_digest() {
    let (h, png) = png_harness();
    let (run, bundle, install, _image_digest) = approved_image_run(&h, &png, "xdigest");
    let ws = workspace(&h);
    let wrong_digest = "0".repeat(64);
    let foreign = format!("dp1.{ws}.{}.{:.32}", h.connection, wrong_digest);
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            schedule_body(
                &run,
                &bundle,
                &install,
                "cad979-freeze-xdigest",
                Some(foreign),
            ),
        )
        .expect_err("a media_key for a foreign digest must be refused at freeze");
    let msg = err.to_string();
    assert!(
        msg.contains("grant_binding_mismatch") || msg.contains("media"),
        "expected a media-key binding refusal, got: {msg}"
    );
}

/// I4 / honest-path: a real `MediaImporter` pointed at a fake device door.
/// The door validates the bearer, the connection id and the sha256 digest of
/// the uploaded bytes, then mints a `dp1.<workspace>.<connection>.<digest>`
/// receipt — exactly the contract the importer must verify. The returned key
/// must then pass the freeze guard for the same reviewed asset.
struct FakeImportDoor {
    addr: String,
    calls: Arc<AtomicUsize>,
    // Filled after the harness daemon starts — the store derives a fresh
    // workspace_id per daemon, so the door cannot know it until `h` exists,
    // yet `h` needs door.addr up front for the importer.
    workspace: Arc<Mutex<Option<String>>>,
    connection: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl FakeImportDoor {
    fn start() -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_string();
        let calls = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let workspace = Arc::new(Mutex::new(None::<String>));
        let connection = Arc::new(Mutex::new(None::<String>));
        let w = workspace.clone();
        let c = connection.clone();
        let cc = calls.clone();
        let cs = stop.clone();
        let worker = thread::spawn(move || loop {
            if cs.load(Ordering::SeqCst) {
                return;
            }
            let Ok(Some(mut req)) = server.recv_timeout(Duration::from_millis(50)) else {
                continue;
            };
            cc.fetch_add(1, Ordering::SeqCst);
            let url = req.url().to_owned();
            let auth = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.to_string())
                .unwrap_or_default();
            let mut body = Vec::new();
            req.as_reader().read_to_end(&mut body).unwrap_or(0);
            let (_, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
            let getq = |k: &str| {
                query
                    .split('&')
                    .find_map(|p| {
                        p.split_once('=')
                            .filter(|(x, _)| *x == k)
                            .map(|(_, v)| v.to_owned())
                    })
                    .unwrap_or_default()
            };
            let conn = getq("connectionId");
            let dig = getq("digest");
            let expect_conn = c.lock().unwrap().clone().unwrap_or_default();
            let ok = auth == "Bearer cad979-test-bearer"
                && conn == expect_conn
                && dig.len() == 64
                && sha_hex(&body) == dig;
            let resp = if ok {
                let digest = sha_hex(&body);
                let ws = w.lock().unwrap().clone().unwrap_or_default();
                let mime = if body.starts_with(&[0xff, 0xd8, 0xff]) {
                    "image/jpeg"
                } else {
                    "image/png"
                };
                let key = format!("dp1.{ws}.{conn}.{:.32}", digest);
                let data = json!({
                    "mediaKey": key, "connectionId": conn, "digest": digest,
                    "mime": mime, "sizeBytes": body.len(),
                    "readBack": {"bytes": body.len(), "digest": digest},
                });
                tiny_http::Response::from_string(json!({"ok": true, "data": data}).to_string())
                    .with_status_code(200)
            } else {
                tiny_http::Response::from_string(
                    json!({"ok": false, "error": {"code": "digest_mismatch"}}).to_string(),
                )
                .with_status_code(409)
            };
            let _ = req.respond(resp);
        });
        Self {
            addr,
            calls,
            workspace,
            connection,
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for FakeImportDoor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

fn sha_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Build a harness whose daemon already carries a `MediaImporter` against a
/// fake door, plus the synthetic-PNG media adapter.
fn importer_harness(door: &FakeImportDoor, bearer: &str) -> Release {
    let media = h_png();
    let importer = MediaImporter::new(
        &format!("http://{}", door.addr),
        DeviceCredential::new(bearer.to_owned()),
    )
    .expect("fake-door importer");
    Release::with_social_image(move |opts, _| {
        opts.provider_deployments = Some(
            DeploymentMetadata::parse(
                br#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@2","transport":"hosted-media-lease@1"}]}"#,
            )
            .unwrap(),
        );
        agenticos_external::attach(opts).unwrap();
        let inner = opts.platforms.remove("agenticos_external").unwrap();
        opts.platforms.insert(
            "agenticos_external".into(),
            Arc::new(SyntheticMedia { inner, png: media }),
        );
        opts.social_media_importer = Some(Arc::new(importer));
    })
}

/// Positive path: operator import against a real configured importer mints a
/// media_key that then passes the freeze guard for the same reviewed asset.
#[test]
fn cad979_import_then_schedule_binds_reviewed_asset() {
    // The daemon derives a fresh workspace per store; the door binds first
    // (the importer needs its address), then learns `h`'s workspace and the
    // `publication`-slot connection via shared cells before any request.
    let door = FakeImportDoor::start();
    let h = importer_harness(&door, "cad979-test-bearer");
    *door.workspace.lock().unwrap() = Some(workspace(&h));
    *door.connection.lock().unwrap() = Some(h.connection.clone());
    let (run, bundle, install, image_digest) = approved_image_run(&h, &h_png(), "imp");

    // The importer uploads the retained PNG; the door mints the key.
    let imported = h
        .daemon
        .operator_rpc(
            "social_publish_media_import",
            import_body(&run, &install, None, "cad979-imp-1"),
        )
        .expect("operator import against the configured fake door must succeed");
    assert_eq!(imported["ok"], json!(true));
    let key = imported["media_key"].as_str().unwrap().to_owned();
    assert_eq!(imported["image_digest"], json!(image_digest));
    assert_eq!(
        door.calls.load(Ordering::SeqCst),
        1,
        "exactly one import call"
    );

    // That key must satisfy the freeze guard for the same run/asset.
    let intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            schedule_body(&run, &bundle, &install, "cad979-imp-sched", Some(key)),
        )
        .unwrap()["intent"]
        .clone();
    assert_eq!(intent["state"], "queued");
}

/// I4 crash-inert: an import writes no `social_publish_intents` row — a
/// crash between import and schedule leaves nothing behind; the durable
/// row appears only at schedule. Proven by listing intents before/after.
#[test]
fn cad979_import_writes_no_intent_row() {
    let door = FakeImportDoor::start();
    let h = importer_harness(&door, "cad979-test-bearer");
    *door.workspace.lock().unwrap() = Some(workspace(&h));
    *door.connection.lock().unwrap() = Some(h.connection.clone());
    let (run, _bundle, install, _d) = approved_image_run(&h, &h_png(), "inert");
    let before = h
        .daemon
        .operator_rpc("social_publish_list", json!({"install_id": install}))
        .unwrap()["intents"]
        .as_array()
        .unwrap()
        .len();
    let imported = h
        .daemon
        .operator_rpc(
            "social_publish_media_import",
            import_body(&run, &install, None, "cad979-inert-1"),
        )
        .expect("import succeeds");
    assert_eq!(imported["ok"], json!(true));
    let after = h
        .daemon
        .operator_rpc("social_publish_list", json!({"install_id": install}))
        .unwrap()["intents"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(
        before, after,
        "media import must write no social_publish_intents row"
    );
}

/// I4 concurrency: the same `request_id` re-scheduled is refused by the
/// `request` UNIQUE column — the durable idempotency bound lives at freeze.
#[test]
fn cad979_import_same_request_id_schedule_refused() {
    let door = FakeImportDoor::start();
    let h = importer_harness(&door, "cad979-test-bearer");
    *door.workspace.lock().unwrap() = Some(workspace(&h));
    *door.connection.lock().unwrap() = Some(h.connection.clone());
    let (run, bundle, install, image_digest) = approved_image_run(&h, &h_png(), "dup");
    let ws = workspace(&h);
    let key = format!("dp1.{ws}.{}.{:.32}", h.connection, image_digest);
    let req = "cad979-dup-sched";
    h.daemon
        .operator_rpc(
            "social_publish_schedule",
            schedule_body(&run, &bundle, &install, req, Some(key.clone())),
        )
        .expect("first schedule queues");
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            schedule_body(&run, &bundle, &install, req, Some(key)),
        )
        .expect_err("same request_id re-schedule must be refused");
    assert!(
        err.to_string().contains("request") || err.to_string().contains("idempotent"),
        "{err}"
    );
}

/// `h` PNG bytes shared between harness builds — the harness regenerates an
/// identical 1×1 PNG, so this is deterministic.
fn h_png() -> Vec<u8> {
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&[0], 1, 1, image::ExtendedColorType::L8)
        .unwrap();
    png
}
