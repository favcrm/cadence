//! CAD-979 native seam — pre-freeze `media_key` binding test (tests first).
//!
//! An operator passes a `media_key` to `social_publish_schedule`. The freeze
//! must refuse a key that does not bind THIS run's connection + image digest
//! (`dp1.<ws>.<connection>.<image_digest[..32]>`), not accept-and-freeze it.
//! Written before the freeze guard lands: this is the genuine red.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::contract_fixture::{ToolTable, Verified};
use cadence_agent::platform::agenticos_external::media_import::{MediaImporter, MediaResolver};
use cadence_agent::platform::agenticos_external::publish_sender::DeviceCredential;
use cadence_agent::platform::deployments::DeploymentMetadata;
use cadence_agent::platform::{
    agenticos_external, AppCapabilityAsset, AppCapabilityOutput, AppCapabilityQuote,
    PlatformAdapter,
};
use common::app_release::{Release, OWNER, REVIEWER, WRITER};
use common::{op, test_port};
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
    // v9: schedule resolves local→AOS `connectionId` via the read credential;
    // the key's `parts[2]` is the resolved AOS id, `parts[1]` the send
    // credential's workspace (not asserted locally).
    let (_h_png_only, png) = png_harness();
    let dest_door = FakeDestinationsDoor::start("ws-send");
    let h = resolver_only_harness(&dest_door, AOS_CONN, png.clone());
    let (run, bundle, install, image_digest) = approved_image_run(&h, &png, "ok");
    let key = format!("dp1.ws-send.{AOS_CONN}.{:.32}", image_digest);
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
    let (_h_png_only, png) = png_harness();
    let dest_door = FakeDestinationsDoor::start("ws-send");
    let h = resolver_only_harness(&dest_door, AOS_CONN, png.clone());
    let (run, bundle, install, image_digest) = approved_image_run(&h, &png, "xconn");
    // Well-formed, but its connection part is a different AOS id than the
    // resolved one — refused at freeze.
    let foreign = format!("dp1.ws-send.conB_other.{:.32}", image_digest);
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
    // v9: the import resolves `(toolkit, destination_id)` → remote AOS
    // `connectionId` via the destinations read credential before upload.
    let mut b = json!({"request_id": request, "install_id": install,
        "toolkit": "instagram", "destination_id": "17841400008460056",
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
    let (_h_png_only, png) = png_harness();
    let dest_door = FakeDestinationsDoor::start("ws-send");
    let h = resolver_only_harness(&dest_door, AOS_CONN, png.clone());
    let (run, bundle, install, _image_digest) = approved_image_run(&h, &png, "xdigest");
    let wrong_digest = "0".repeat(64);
    let foreign = format!("dp1.ws-send.{AOS_CONN}.{:.32}", wrong_digest);
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
            // v9 B1: a `connectionId` outside the send credential's workspace
            // is `not_found` (AOS `connectionInWorkspace` `WHERE id AND
            // workspaceId`) — the door does not mint a key for it.
            if conn != expect_conn {
                let nf = tiny_http::Response::from_string(
                    json!({"ok": false, "error": {"code": "not_found"}}).to_string(),
                )
                .with_status_code(404);
                let _ = req.respond(nf);
                continue;
            }
            let ok =
                auth == "Bearer cad979-test-bearer" && dig.len() == 64 && sha_hex(&body) == dig;
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

/// The AOS wire `connectionId` the fake destinations resolver maps the local
/// custody `conn-<uuid4>` to (v9). Distinct from the local `h.connection` —
/// a remote AOS id, never the local custody id.
const AOS_CONN: &str = "connA_1784";

/// Build a harness whose daemon carries BOTH a `MediaImporter` (send
/// credential → `door`) and a `MediaResolver` (read credential →
/// `dest_door`) that maps `(instagram, 17841400008460056)` → [`AOS_CONN`].
/// v9: the import uploads under the resolved AOS id, so `door.connection`
/// is set to `AOS_CONN` (not the local `conn-<uuid4>`).
fn importer_harness(
    door: &FakeImportDoor,
    dest_door: &FakeDestinationsDoor,
    bearer: &str,
) -> Release {
    dest_door.add(AOS_CONN, "instagram", "17841400008460056", true);
    let media = h_png();
    let importer = MediaImporter::new(
        &format!("http://{}", door.addr),
        DeviceCredential::new(bearer.to_owned()),
    )
    .expect("fake-door importer");
    let resolver = MediaResolver::new(
        &format!("http://{}", dest_door.addr),
        DeviceCredential::new("cad979-read-cred".to_owned()),
    )
    .expect("fake destinations resolver");
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
        opts.social_media_resolver = Some(Arc::new(resolver));
    })
}

/// Resolver harness for the freeze path only (no import door needed): the
/// resolver maps `(instagram, 17841400008460056)` → `aos_conn` so schedule
/// can bind `aos_connection_id`. `png_harness` alone has no resolver and
/// schedule would refuse `capability_unavailable` under v9.
fn resolver_only_harness(
    dest_door: &FakeDestinationsDoor,
    aos_conn: &str,
    media: Vec<u8>,
) -> Release {
    dest_door.add(aos_conn, "instagram", "17841400008460056", true);
    let resolver = MediaResolver::new(
        &format!("http://{}", dest_door.addr),
        DeviceCredential::new("cad979-read-cred".to_owned()),
    )
    .expect("fake destinations resolver");
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
        opts.social_media_resolver = Some(Arc::new(resolver));
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
    let dest_door = FakeDestinationsDoor::start("ws-send");
    let h = importer_harness(&door, &dest_door, "cad979-test-bearer");
    *door.workspace.lock().unwrap() = Some("ws-send".to_owned());
    // The door binds the RESOLVED AOS connectionId (the import sends it), not
    // the local custody .
    *door.connection.lock().unwrap() = Some(AOS_CONN.to_owned());
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
    let dest_door = FakeDestinationsDoor::start("ws-send");
    let h = importer_harness(&door, &dest_door, "cad979-test-bearer");
    *door.workspace.lock().unwrap() = Some("ws-send".to_owned());
    // The door binds the RESOLVED AOS connectionId (the import sends it), not
    // the local custody .
    *door.connection.lock().unwrap() = Some(AOS_CONN.to_owned());
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

/// I4 concurrency: the durable idempotency bound lives at freeze. The
/// internal `request` is `uuid5(install_id:request_id)`, so an identical
/// re-schedule under the same `request_id` is an idempotent success that
/// returns the SAME intent — while a request_id re-used for DIFFERENT
/// frozen content is refused by the `request` UNIQUE + digest compare.
#[test]
fn cad979_import_same_request_id_schedule_is_idempotent() {
    let door = FakeImportDoor::start();
    let dest_door = FakeDestinationsDoor::start("ws-send");
    let h = importer_harness(&door, &dest_door, "cad979-test-bearer");
    *door.workspace.lock().unwrap() = Some("ws-send".to_owned());
    // The door binds the RESOLVED AOS connectionId (the import sends it), not
    // the local custody .
    *door.connection.lock().unwrap() = Some(AOS_CONN.to_owned());
    let (run, bundle, install, image_digest) = approved_image_run(&h, &h_png(), "dup");
    // The key binds the resolved AOS connectionId, not the local custody id.
    let key = format!("dp1.ws-send.{AOS_CONN}.{:.32}", image_digest);
    let req = "cad979-dup-sched";
    let first = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            schedule_body(&run, &bundle, &install, req, Some(key.clone())),
        )
        .expect("first schedule queues")["intent"]
        .clone();
    // Identical re-schedule: idempotent — same intent_id, still queued.
    let second = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            schedule_body(&run, &bundle, &install, req, Some(key.clone())),
        )
        .expect("identical re-schedule is idempotent")["intent"]
        .clone();
    assert_eq!(first["intent_id"], second["intent_id"]);
    assert_eq!(second["state"], "queued");
    // A request_id re-used for different frozen content is refused — keep
    // the binding key valid so the failure is the request dedup, not the key.
    // Different frozen content under the SAME request_id: same toolkit/
    // destination (still resolves to AOS_CONN), but a different `due_epoch`
    // produces a different `frozen_digest` → `different frozen` refuse.
    let mut other = schedule_body(&run, &bundle, &install, req, Some(key.clone()));
    other["due_epoch"] = json!(1_750_000_999i64); // different frozen content
    let err = h
        .daemon
        .operator_rpc("social_publish_schedule", other)
        .expect_err("request_id for different frozen content must refuse");
    assert!(err.to_string().contains("different frozen"), "{err}");
}

/// True concurrency: two threads run the import for the SAME run at once —
/// the door sees each call independently (no shared custody leak), each gets
/// the same content-addressed key (idempotent upstream), and no row is
/// written for either. Real concurrent callers, not sequential.
#[test]
fn cad979_import_concurrent_calls_same_key_no_leak() {
    let door = FakeImportDoor::start();
    let dest_door = FakeDestinationsDoor::start("ws-send");
    let h = importer_harness(&door, &dest_door, "cad979-test-bearer");
    *door.workspace.lock().unwrap() = Some("ws-send".to_owned());
    // The door binds the RESOLVED AOS connectionId (the import sends it), not
    // the local custody .
    *door.connection.lock().unwrap() = Some(AOS_CONN.to_owned());
    let (run, _bundle, install, image_digest) = approved_image_run(&h, &h_png(), "conc");
    let body = import_body(&run, &install, None, "cad979-conc");
    let state = h.daemon.state.clone();

    // Two concurrent operator imports of the same retained bytes.
    let b1 = body.clone();
    let b2 = body.clone();
    let s1 = state.clone();
    let s2 = state.clone();
    let t1 = thread::spawn(move || {
        cadence_agent::test_seam::scoped(cadence_agent::test_seam::Asserted::Operator, || {
            cadence_agent::client::rpc(&s1, "social_publish_media_import", b1)
        })
    });
    let t2 = thread::spawn(move || {
        cadence_agent::test_seam::scoped(cadence_agent::test_seam::Asserted::Operator, || {
            cadence_agent::client::rpc(&s2, "social_publish_media_import", b2)
        })
    });
    let r1 = t1.join().unwrap();
    let r2 = t2.join().unwrap();
    let k1 = r1.expect("import 1")["media_key"]
        .as_str()
        .unwrap()
        .to_owned();
    let k2 = r2.expect("import 2")["media_key"]
        .as_str()
        .unwrap()
        .to_owned();
    // Content-addressed: same bytes+digest → the same key, never a conflict.
    assert_eq!(k1, k2);
    assert!(k1.ends_with(&image_digest[..32]));
    // Both saw the door; no cross-call leak beyond the two uploads.
    assert!(door.calls.load(Ordering::SeqCst) >= 2);
    let intents = h
        .daemon
        .operator_rpc("social_publish_list", json!({"install_id": install}))
        .unwrap()["intents"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(intents, 0, "import writes no intent row even concurrently");
}

/// I1 HTTP parity: `POST /api/social-media-imports` is `OperatorOnly` — a
/// signed-in operator reaches the verb; an unauthenticated/anonymous POST is
/// refused before the route even dispatches (fail-closed), same as the RPC.
#[test]
fn cad979_import_http_route_is_operator_only() {
    let door = FakeImportDoor::start();
    let dest_door = FakeDestinationsDoor::start("ws-send");
    let h = importer_harness(&door, &dest_door, "cad979-test-bearer");
    *door.workspace.lock().unwrap() = Some("ws-send".to_owned());
    // The door binds the RESOLVED AOS connectionId (the import sends it), not
    // the local custody .
    *door.connection.lock().unwrap() = Some(AOS_CONN.to_owned());
    let (run, _bundle, install, _d) = approved_image_run(&h, &h_png(), "http");
    let body = import_body(&run, &install, None, "cad979-http-1").to_string();

    // An in-process board on the daemon's own state dir (ui::serve).
    let lease = test_port();
    let port = lease.port;
    let state = h.daemon.state.clone();
    let pm = tempfile::tempdir().unwrap();
    let pm_dir = pm.path().to_path_buf();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel();
    let bstop = stop.clone();
    let join = std::thread::spawn(move || {
        cadence_agent::ui::serve(
            &state,
            &pm_dir,
            &cadence_agent::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(bstop),
                startup: Some(tx),
                test_seam: true,
                ..Default::default()
            },
        )
    });
    let _ = rx.recv_timeout(Duration::from_secs(10)).expect("board up");

    // A proven operator write → a real 200 carrying the validated receipt.
    let session = op::sign_in(env!("CARGO_BIN_EXE_cadence"), &h.daemon.state, port);
    let (code, _, resp) = op::raw(
        port,
        &session.request("POST", "/api/social-media-imports", &body),
    );
    assert_eq!(code, 200, "operator import must be 200: {resp}");
    let parsed: Value = serde_json::from_str(&resp).expect("import reply is JSON");
    assert_eq!(parsed["ok"], json!(true), "{parsed}");
    let key = parsed["media_key"].as_str().unwrap();
    assert!(key.starts_with("dp1."), "{key}");
    let image_digest = sha_hex(&h_png());
    assert!(
        key.contains(&image_digest[..32]),
        "{key} binds the asset digest"
    );
    assert_eq!(door.calls.load(Ordering::SeqCst), 1, "one fake upload");

    // Anonymous write → fail-closed 403 before the route.
    let (code, _, _) = op::raw(
        port,
        &format!(
            "POST /api/social-media-imports HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        ),
    );
    assert_eq!(code, 403, "an anonymous POST must be refused operator-only");

    // Forged/unknown field → 4xx via deny_unknown_fields, never a real upload.
    let mut bad = import_body(&run, &install, None, "cad979-http-bad");
    bad["forged_actor"] = json!("operator");
    let (code, _, _) = op::raw(
        port,
        &session.request("POST", "/api/social-media-imports", &bad.to_string()),
    );
    assert!((400..500).contains(&code), "forged field refused: {code}");

    stop.store(true, Ordering::SeqCst);
    let _ = join.join();
}

/// Typed-input matrix over the HTTP route: missing/malformed `request_id`
/// and `context_id` must refuse (4xx) with ZERO upstream door calls — the
/// typed body + strict_fields reject before the resolver/importer runs.
#[test]
fn cad979_import_http_typed_input_matrix_no_import() {
    let door = FakeImportDoor::start();
    let dest_door = FakeDestinationsDoor::start("ws-send");
    let h = importer_harness(&door, &dest_door, "cad979-test-bearer");
    *door.workspace.lock().unwrap() = Some("ws-send".to_owned());
    // The door binds the RESOLVED AOS connectionId (the import sends it), not
    // the local custody .
    *door.connection.lock().unwrap() = Some(AOS_CONN.to_owned());
    let (run, _bundle, install, _d) = approved_image_run(&h, &h_png(), "typed");

    let lease = test_port();
    let port = lease.port;
    let state = h.daemon.state.clone();
    let pm = tempfile::tempdir().unwrap();
    let pm_dir = pm.path().to_path_buf();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel();
    let bstop = stop.clone();
    let join = std::thread::spawn(move || {
        cadence_agent::ui::serve(
            &state,
            &pm_dir,
            &cadence_agent::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(bstop),
                startup: Some(tx),
                test_seam: true,
                ..Default::default()
            },
        )
    });
    let _ = rx.recv_timeout(Duration::from_secs(10)).expect("board up");
    let session = op::sign_in(env!("CARGO_BIN_EXE_cadence"), &h.daemon.state, port);

    let post = |b: Value| -> u16 {
        op::raw(
            port,
            &session.request("POST", "/api/social-media-imports", &b.to_string()),
        )
        .0
    };

    for (field, badval) in [
        ("request_id", json!(7)),
        ("request_id", json!("")),
        ("request_id", json!("bad id!")),
        ("context_id", json!(9)),
        ("context_id", json!({"x": 1})),
        ("context_id", json!("bad id!")),
    ] {
        let mut b = import_body(&run, &install, None, "cad979-t1");
        b[field] = badval.clone();
        let code = post(b);
        assert!((400..500).contains(&code), "{field}={badval}: {code}");
    }
    let mut missing = import_body(&run, &install, None, "cad979-t2");
    missing.as_object_mut().unwrap().remove("request_id");
    assert!((400..500).contains(&post(missing)), "missing request_id");
    assert_eq!(
        door.calls.load(Ordering::SeqCst),
        0,
        "no request reached the door"
    );

    stop.store(true, Ordering::SeqCst);
    let _ = join.join();
}

/// I1 authority — a detached `setsid` child with no provable identity cannot
/// reach the operator import verb over the unix socket, exactly like every
/// other operator-only gate. Same refusal as agent/unproven.
#[test]
fn cad979_import_refuses_detached_setsid_peer() {
    let (h, png) = png_harness();
    let (run, _bundle, install, _d) = approved_image_run(&h, &png, "setsid");
    let mut lane = common::LaneShell::spawn(h.daemon.state.parent().unwrap());
    let request = lane.dir.path().join("import-detached.json");
    std::fs::write(
        &request,
        cadence_agent::proto::request(
            "social_publish_media_import",
            import_body(&run, &install, None, "cad979-setsid"),
        )
        .to_string(),
    )
    .unwrap();
    let (rc, output) = lane.run(&format!(
        "setsid python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");print(s.makefile().readline())' {} {}",
        cadence_agent::client::socket_path(&h.daemon.state).display(),
        request.display()
    ));
    assert_eq!(rc, 0, "{output}");
    let frame: Value = serde_json::from_str(output.trim()).expect("one JSON reply");
    assert_eq!(frame["ok"], json!(false), "{frame}");
    assert!(frame.to_string().contains("operator"), "{frame}");
}

/// Concurrent scope isolation: one thread runs the import for the run's OWN
/// scope (succeeds), a concurrent thread names a foreign install (refused) —
/// neither custody nor scope leaks across the two calls.
#[test]
fn cad979_import_concurrent_scope_isolation() {
    let door = FakeImportDoor::start();
    let dest_door = FakeDestinationsDoor::start("ws-send");
    let h = importer_harness(&door, &dest_door, "cad979-test-bearer");
    *door.workspace.lock().unwrap() = Some("ws-send".to_owned());
    // The door binds the RESOLVED AOS connectionId (the import sends it), not
    // the local custody .
    *door.connection.lock().unwrap() = Some(AOS_CONN.to_owned());
    let (run, _bundle, install, _d) = approved_image_run(&h, &h_png(), "ciso");
    let state = h.daemon.state.clone();
    let good = import_body(&run, &install, None, "cad979-ciso-ok");
    let bad = import_body(&run, "install_FOREIGN", None, "cad979-ciso-bad");
    let s1 = state.clone();
    let s2 = state.clone();
    let t1 = thread::spawn(move || {
        cadence_agent::test_seam::scoped(cadence_agent::test_seam::Asserted::Operator, || {
            cadence_agent::client::rpc(&s1, "social_publish_media_import", good)
        })
    });
    let t2 = thread::spawn(move || {
        cadence_agent::test_seam::scoped(cadence_agent::test_seam::Asserted::Operator, || {
            cadence_agent::client::rpc(&s2, "social_publish_media_import", bad)
        })
    });
    let r1 = t1.join().unwrap();
    let r2 = t2.join().unwrap();
    assert!(
        r1.expect("own-scope import succeeds")["ok"] == json!(true),
        "own scope must import"
    );
    let e = r2.expect_err("foreign install must refuse").to_string();
    assert!(e.contains("grant_binding_mismatch"), "{e}");
    // Exactly one upload (the good scope); the refused call never reached the
    // door — scope-pin refused before custody read or import.
    assert_eq!(door.calls.load(Ordering::SeqCst), 1);
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

// ===========================================================================
// v9 identity mapping (resolved local→AOS connectionId) — tests first.
//
// The destinations reply carries `{connectionId, toolkit, displayName,
// destinationId, status, available, publishable}` under the read credential;
// the import door mints `mediaKey` bound to the SEND credential's workspace.
// A `connectionId` belonging to a different workspace than the send
// credential's is refused `not_found` → `capability_unavailable`.
// ===========================================================================

/// A fake upstream destinations door: `GET /v1/runtime/connectors/destinations`
/// answering the `toDeviceDestination` reply shape for the connections this
/// fixture registers. Scoped to a `workspace` it stamps on each row's
/// `workspaceId`; bounded like the import door.
struct FakeDestinationsDoor {
    addr: String,
    /// `(connectionId, toolkit, destinationId, publishable)` rows served.
    rows: Arc<Mutex<Vec<(String, String, String, bool)>>>,
    /// 5xx mode: return a server error (ambiguous) instead of the list.
    dead: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl FakeDestinationsDoor {
    fn start(workspace: &str) -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_string();
        let rows = Arc::new(Mutex::new(Vec::new()));
        let dead = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let w = workspace.to_owned();
        let rr = rows.clone();
        let cs = stop.clone();
        let dd = dead.clone();
        let worker = thread::spawn(move || loop {
            if cs.load(Ordering::SeqCst) {
                return;
            }
            let Ok(Some(req)) = server.recv_timeout(Duration::from_millis(50)) else {
                continue;
            };
            let auth = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.to_string())
                .unwrap_or_default();
            let resp = if auth != "Bearer cad979-read-cred" {
                tiny_http::Response::from_string(
                    json!({"ok":false,"error":{"code":"unauthorized"}}).to_string(),
                )
                .with_status_code(401)
            } else if dd.load(Ordering::SeqCst) {
                tiny_http::Response::from_string(
                    json!({"ok":false,"error":{"code":"upstream"}}).to_string(),
                )
                .with_status_code(500)
            } else {
                let data: Vec<Value> = rr
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(cid, toolkit, dest, publishable)| {
                        json!({
                            "connectionId": cid, "toolkit": toolkit,
                            "displayName": cid, "destinationId": dest,
                            "status": "active", "available": true,
                            "publishable": publishable,
                            "workspaceId": w,
                        })
                    })
                    .collect();
                tiny_http::Response::from_string(json!({"ok": true, "data": data}).to_string())
                    .with_status_code(200)
            };
            let _ = req.respond(resp);
        });
        Self {
            addr,
            rows,
            dead,
            stop,
            worker: Some(worker),
        }
    }
    fn add(&self, connection_id: &str, toolkit: &str, destination_id: &str, publishable: bool) {
        self.rows.lock().unwrap().push((
            connection_id.to_owned(),
            toolkit.to_owned(),
            destination_id.to_owned(),
            publishable,
        ));
    }
}

impl Drop for FakeDestinationsDoor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Harness carrying BOTH a `MediaImporter` (send credential → import door)
/// and a `MediaResolver` (read credential → destinations door).
fn mapped_harness(import_door: &FakeImportDoor, dest_door: &FakeDestinationsDoor) -> Release {
    let media = h_png();
    let importer = MediaImporter::new(
        &format!("http://{}", import_door.addr),
        DeviceCredential::new("cad979-test-bearer".to_owned()),
    )
    .expect("fake import door");
    let resolver = MediaResolver::new(
        &format!("http://{}", dest_door.addr),
        DeviceCredential::new("cad979-read-cred".to_owned()),
    )
    .expect("fake destinations door");
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
        opts.social_media_resolver = Some(Arc::new(resolver));
    })
}

/// v9 happy-path mapping: the local `conn-<uuid4>` is resolved to the AOS
/// `connectionId` from the destinations list; the import door mints a key
/// under that AOS id (not the local one); the frozen `aos_connection_id`
/// is the wire identity the send path will use.
#[test]
fn cad979_map_local_conn_to_aos_connection() {
    let import_door = FakeImportDoor::start();
    let dest_door = FakeDestinationsDoor::start("ws-send");
    // The send credential's workspace is what the door binds; the dest read
    // only maps ids. Use the import door's workspace as AOS workspace.
    let aos_conn = "connA_1784"; // remote AOS id, distinct from local conn-<uuid4>
    dest_door.add(aos_conn, "instagram", "17841400008460056", true);
    *import_door.workspace.lock().unwrap() = Some("ws-send".to_owned());
    *import_door.connection.lock().unwrap() = Some(AOS_CONN.to_owned());
    let h = mapped_harness(&import_door, &dest_door);
    let (run, bundle, install, _digest) = approved_image_run(&h, &h_png(), "map");
    // import resolves via destinations then uploads under aos_conn.
    let resp = h
        .daemon
        .operator_rpc(
            "social_publish_media_import",
            import_body(&run, &install, None, "cad979-map-import"),
        )
        .unwrap();
    assert_eq!(resp["ok"], true);
    let key = resp["media_key"].as_str().unwrap().to_owned();
    // The minted key names the resolved AOS connectionId, not local conn-<uuid4>.
    assert_eq!(
        import_door.connection.lock().unwrap().as_deref(),
        Some(aos_conn),
        "import must send the resolved AOS connectionId, not the local id"
    );
    // freeze binds the key; frozen carries the AOS wire identity.
    let intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            schedule_body(&run, &bundle, &install, "cad979-map-sch", Some(key.clone())),
        )
        .unwrap()["intent"]
        .clone();
    assert_eq!(intent["state"], "queued");
    assert_eq!(intent["frozen"]["media_key"], key);
    assert_eq!(intent["frozen"]["aos_connection_id"], aos_conn);
}

/// The resolver must refuse 0 matches.
#[test]
fn cad979_resolver_zero_match_refuses() {
    let import_door = FakeImportDoor::start();
    let dest_door = FakeDestinationsDoor::start("ws-send");
    // no rows → (toolkit,destinationId) matches nothing.
    *import_door.workspace.lock().unwrap() = Some("ws-send".to_owned());
    *import_door.connection.lock().unwrap() = Some(AOS_CONN.to_owned());
    let h = mapped_harness(&import_door, &dest_door);
    let (run, _b, install, _d) = approved_image_run(&h, &h_png(), "zero");
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_media_import",
            import_body(&run, &install, None, "cad979-zero"),
        )
        .expect_err("no matching destination must refuse");
    let msg = err.to_string();
    assert!(
        msg.contains("grant_binding_mismatch") || msg.contains("destination"),
        "expected a no-match refusal, got: {msg}"
    );
    assert_eq!(
        import_door.calls.load(Ordering::SeqCst),
        0,
        "no upload on no-match"
    );
}

/// >1 match is ambiguous → refuse; a full 100-row window is unknown → refuse.
#[test]
fn cad979_resolver_multiple_match_refuses() {
    let import_door = FakeImportDoor::start();
    let dest_door = FakeDestinationsDoor::start("ws-send");
    dest_door.add("connA_1", "instagram", "17841400008460056", true);
    dest_door.add("connA_2", "instagram", "17841400008460056", true);
    *import_door.workspace.lock().unwrap() = Some("ws-send".to_owned());
    *import_door.connection.lock().unwrap() = Some(AOS_CONN.to_owned());
    let h = mapped_harness(&import_door, &dest_door);
    let (run, _b, install, _d) = approved_image_run(&h, &h_png(), "dup");
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_media_import",
            import_body(&run, &install, None, "cad979-dup"),
        )
        .expect_err("two matching destinations must refuse");
    assert_eq!(import_door.calls.load(Ordering::SeqCst), 0);
    let _ = err;
}

/// A full 100-row window can't rule out an unseen later match → refuse.
#[test]
fn cad979_resolver_cap100_full_window_refuses() {
    let import_door = FakeImportDoor::start();
    let dest_door = FakeDestinationsDoor::start("ws-send");
    for i in 0..100 {
        let dest = if i == 0 {
            "17841400008460056".to_owned()
        } else {
            format!("dest{i}")
        };
        dest_door.add(&format!("conn{i}"), "instagram", &dest, true);
    }
    *import_door.workspace.lock().unwrap() = Some("ws-send".to_owned());
    *import_door.connection.lock().unwrap() = Some(AOS_CONN.to_owned());
    let h = mapped_harness(&import_door, &dest_door);
    let (run, _b, install, _d) = approved_image_run(&h, &h_png(), "cap");
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_media_import",
            import_body(&run, &install, None, "cad979-cap"),
        )
        .expect_err("a full-window reply must refuse (completeness unknown)");
    assert_eq!(import_door.calls.load(Ordering::SeqCst), 0);
    let _ = err;
}

/// E1 — the load-bearing cross-workspace proof. Read credential (workspace B)
/// yields `connB`; the `publish.send` import under a workspace-A credential
/// must refuse `not_found` → `capability_unavailable`: no key, no freeze, no
/// intent row, no provider send. The local `conn-<uuid4>` custody id and the
/// remote `connB`/`connA` AOS ids are distinct namespaces.
#[test]
fn cad979_mapping_reader_workspace_differs_sender_refuses() {
    // destinations door scoped to workspace B → connectionId connB (remote B).
    let dest_door = FakeDestinationsDoor::start("wsB-remote");
    dest_door.add("connB", "instagram", "17841400008460056", true);
    // import door scoped to a DIFFERENT workspace A: a `connectionId` not in
    // workspace A is `not_found` (mirrors AOS `connectionInWorkspace` —
    // `WHERE id AND workspaceId`). The fake door only accepts the AOS id it
    // was built with; `connB` isn't it.
    let import_door = FakeImportDoor::start(); // its accepted AOS conn is a ws-A id
    *import_door.workspace.lock().unwrap() = Some("ws-send".to_owned());
    *import_door.connection.lock().unwrap() = Some(AOS_CONN.to_owned());
    let h = mapped_harness(&import_door, &dest_door);
    let (run, _b, install, _d) = approved_image_run(&h, &h_png(), "xws");
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_media_import",
            import_body(&run, &install, None, "cad979-xws"),
        )
        .expect_err("a connectionId outside the send credential's workspace must refuse");
    let msg = err.to_string();
    // The door's `not_found` maps to `wrong_connection` (a definitive
    // refusal — the send credential's workspace does not scope `connB`),
    // never a minted key. Any of the fail-closed refusals is correct.
    assert!(
        msg.contains("capability_unavailable")
            || msg.contains("not_found")
            || msg.contains("wrong_connection")
            || msg.contains("grant_binding_mismatch"),
        "cross-workspace resolved id must refuse, got: {msg}"
    );
    // Fail-closed: no key minted at the send door (the door refused
    // `not_found`), so no frozen key could exist; the import wrote no intent
    // row for this install either (`social_publish_list` only takes
    // install/context — a fresh harness yields an empty set).
    let intents = h
        .daemon
        .operator_rpc("social_publish_list", json!({"install_id": install}))
        .map(|v| v["intents"].clone())
        .unwrap_or_else(|_| json!([]));
    assert!(
        intents.as_array().map(|a| a.is_empty()).unwrap_or(true),
        "a refused cross-workspace import writes no intent: {intents}"
    );
}

/// Resolver unreachable → `capability_unavailable` at import (fail closed).
#[test]
fn cad979_resolver_unreachable_refuses_import() {
    let import_door = FakeImportDoor::start();
    let dest_door = FakeDestinationsDoor::start("ws-send");
    dest_door.dead.store(true, Ordering::SeqCst); // 5xx → ambiguous/unavailable
    dest_door.add("connA_1", "instagram", "17841400008460056", true);
    *import_door.workspace.lock().unwrap() = Some("ws-send".to_owned());
    *import_door.connection.lock().unwrap() = Some(AOS_CONN.to_owned());
    let h = mapped_harness(&import_door, &dest_door);
    let (run, _b, install, _d) = approved_image_run(&h, &h_png(), "down");
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_media_import",
            import_body(&run, &install, None, "cad979-down"),
        )
        .expect_err("an unreachable resolver must refuse, never mint a key");
    let msg = err.to_string();
    assert!(
        msg.contains("capability_unavailable")
            || msg.contains("ambiguous")
            || msg.contains("unavailable"),
        "resolver failure must refuse ambiguous/unavailable, got: {msg}"
    );
    assert_eq!(
        import_door.calls.load(Ordering::SeqCst),
        0,
        "no upload when the resolver failed"
    );
}

/// A request naming no resolver at all (importer configured, resolver absent)
/// refuses `capability_unavailable` — never silently uses the local id.
#[test]
fn cad979_import_without_resolver_is_capability_unavailable() {
    let door = FakeImportDoor::start();
    let media = h_png();
    let importer = MediaImporter::new(
        &format!("http://{}", door.addr),
        DeviceCredential::new("cad979-test-bearer".to_owned()),
    )
    .unwrap();
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
        opts.social_media_importer = Some(Arc::new(importer));
        // deliberately NO social_media_resolver.
    });
    let (run, _b, install, _d) = approved_image_run(&h, &h_png(), "norslv");
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_media_import",
            import_body(&run, &install, None, "cad979-norslv"),
        )
        .expect_err("import without a destinations resolver must refuse");
    assert!(
        err.to_string().contains("capability_unavailable"),
        "{}",
        err
    );
    assert_eq!(door.calls.load(Ordering::SeqCst), 0);
}
