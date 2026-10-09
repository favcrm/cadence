//! CAD-1304: an approved Instagram draft WITH a generated image publishes
//! through the hosted door, and a changed image or caption after approval
//! sends nothing.
//!
//! Everything runs through the daemon's public RPCs, the way Social Content
//! does: fetch a saved handle's posts, create a draft from that receipt,
//! generate the image (the hosted media door returns the artifact, the
//! daemon retains it in custody), attach it, stage the publish effect with
//! the typed draft proof, approve, and `app_effect_publish_now`. The
//! `api.internal` door sits behind an HTTP CONNECT proxy (`ALL_PROXY`), as in
//! `cad1291_acceptance`, and answers with the AgenticOS contract shapes
//! (`packages/contracts/src/device-publish.ts`): media import verifies the
//! body against the `digest` query and mints
//! `dp1.<workspace>.<connection>.<digest[..32]>` (32 hex characters; the
//! contract fixture's 31 is a typo), preflight and publish echo the binding.
#![cfg(feature = "test-seam")]

use cadence_agent::platform::deployments::DeploymentMetadata;
use cadence_agent::store::app_records::RecordStore;
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{client, daemon};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HOSTED: &str = r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@3","transport":"hosted-media-lease@1"}]}"#;
const PREFIX: &str = "/v1/runtime/connectors/hosted-publish";
const AOS_CONN: &str = "conn_aos_1304";
const WORKSPACE: &str = "ws_1304";
const DEST: &str = "17841400008460056";
const STANDING: &str = "dpq_standing_cad1304";
const HANDLE: &str = "juicysuite_crm";
const CAPTION: &str = "Instagram image caption";

/// One request the door saw: method, path and body length.
type Seen = Arc<Mutex<Vec<(String, String)>>>;

#[derive(Default)]
struct Door {
    /// `job id -> png bytes`, served at `/media/artifacts/<job>.<digest>`.
    artifacts: Mutex<HashMap<String, Vec<u8>>>,
    /// Digest query and body hash of every media import, in order.
    imports: Mutex<Vec<(String, String)>>,
    /// Publish and preflight bodies, in order.
    sends: Mutex<Vec<Value>>,
    jobs: AtomicUsize,
}

/// A distinct 1x1 PNG per `n`.
fn png(n: u8) -> Vec<u8> {
    use image::ImageEncoder as _;
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(&[n], 1, 1, image::ExtendedColorType::L8)
        .unwrap();
    bytes
}

fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn fake_door() -> (String, Seen, Arc<Door>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let seen: Seen = Arc::default();
    let state = Arc::new(Door::default());
    let (log, door) = (seen.clone(), state.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (log, door) = (log.clone(), door.clone());
            std::thread::spawn(move || serve(stream, &log, &door));
        }
    });
    (addr, seen, state)
}

fn serve(stream: std::net::TcpStream, log: &Seen, door: &Door) {
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
    let header = |name: &str| -> Option<String> {
        request.iter().skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    };
    let length: usize = header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    log.lock().unwrap().push((method.clone(), path.clone()));
    let sent: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (route, query) = path.split_once('?').unwrap_or((path.as_str(), ""));
    let query_value = |name: &str| -> Option<String> {
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix(&format!("{name}=")).map(str::to_owned))
    };
    let price = json!({"currency":"USD","scale":6,"amount":"0.002000"});
    let raw = |status: u16, content_type: &str, bytes: &[u8]| {
        let mut out = format!(
            "HTTP/1.1 {status} OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            bytes.len()
        )
        .into_bytes();
        out.extend_from_slice(bytes);
        out
    };
    let json_reply = |value: Value| raw(200, "application/json", value.to_string().as_bytes());
    let echo = |sent: &Value| {
        json!({
            "key": sent["key"],
            "destinationId": DEST,
            "captionDigest": sent["grant"]["captionDigest"],
            "imageDigest": sent["grant"]["imageDigest"],
        })
    };
    let reply = match (method.as_str(), route) {
        // ---- source read (generic tool door) ----
        ("GET", "/v1/runtime/tools/read_instagram_posts") => json_reply(
            json!({"ok":true,"data":{"slug":"read_instagram_posts","displayName":"Read Instagram posts","effect":"read","chargePrecondition":"max_charge_minor@1","price":price,"unitPrice":null}}),
        ),
        ("POST", "/v1/runtime/tools/call") if sent["slug"] == "read_instagram_posts" => json_reply(
            json!({"ok":true,"data":{"slug":"read_instagram_posts","repeated":false,"price":price,"result":{"success":true,"status":"ok","user":{"username":HANDLE,"is_private":false},"items":[{"id":"post-1","code":"AbCd123","created_at":"2026-09-27T00:00:00Z","caption":{"text":"Door caption"}}]}}}),
        ),
        // ---- image generation (media door) ----
        ("GET", "/v1/runtime/media/price/image") => json_reply(
            json!({"ok":true,"data":{"kind":"image","model":"openai/gpt-image-2.5","price":{"slug":"generate_image","chargeMinor":31500,"currency":"USD","version":"2026-09-29T00:00:00.000Z"}}}),
        ),
        ("POST", "/v1/runtime/media/image") => {
            let n = door.jobs.fetch_add(1, SeqCst) as u8 + 1;
            let bytes = png(n);
            let digest = hex(&bytes);
            let job_id = format!("med_job{n}");
            let reference = format!("{job_id}.{digest}");
            door.artifacts
                .lock()
                .unwrap()
                .insert(reference.clone(), bytes.clone());
            json_reply(
                json!({"ok":true,"data":{"job":{"id":job_id,"kind":"image","status":"succeeded","model":"openai/gpt-image-2.5","provider":"upstream-fixture","providerTaskId":null,
                    "artifacts":[{"ref":reference,"digest":digest,"bytes":bytes.len(),"mime":"image/png"}],"artifactError":null,"usage":{"providerCredits":null},
                    "price":{"slug":"generate_image","chargeMinor":31500,"currency":"USD","version":"2026-09-29T00:00:00.000Z"},
                    "repeated":false,"cached":false,"stale":false}}}),
            )
        }
        ("GET", artifact) if artifact.starts_with("/v1/runtime/media/artifacts/") => {
            let reference = &artifact["/v1/runtime/media/artifacts/".len()..];
            match door.artifacts.lock().unwrap().get(reference) {
                Some(bytes) => raw(200, "image/png", bytes),
                None => raw(404, "application/json", b"{}"),
            }
        }
        // ---- hosted publish door ----
        ("GET", r) if r == format!("{PREFIX}/destinations") => json_reply(
            json!({"ok":true,"data":[{"connectionId":AOS_CONN,"toolkit":"instagram","displayName":"Harbour stills","destinationId":DEST,"status":"active","available":true,"publishable":true}]}),
        ),
        ("GET", r) if r == format!("{PREFIX}/publish/grants") => json_reply(
            json!({"ok":true,"data":{"grants":[{"kind":"standing","id":STANDING,"workspaceId":WORKSPACE,"connectionId":AOS_CONN,"destinationId":DEST,"toolkit":"instagram","dailyCap":20,"remainingToday":20,"expiresAt":null,"revokedAt":null}]}}),
        ),
        ("POST", r) if r == format!("{PREFIX}/media/import") => {
            let digest = query_value("digest").unwrap_or_default();
            let mime = header("content-type").unwrap_or_default();
            door.imports
                .lock()
                .unwrap()
                .push((digest.clone(), hex(&body)));
            if hex(&body) != digest {
                json_reply(json!({"ok":false,"error":{"code":"digest_mismatch"}}))
            } else {
                json_reply(json!({"ok":true,"data":{
                    "mediaKey": format!("dp1.{WORKSPACE}.{AOS_CONN}.{}", &digest[..32]),
                    "connectionId": query_value("connectionId"),
                    "digest": digest, "mime": mime, "sizeBytes": body.len(),
                    "readBack": {"bytes": body.len(), "digest": digest},
                }}))
            }
        }
        ("POST", r) if r == format!("{PREFIX}/publish/preflight") => {
            door.sends.lock().unwrap().push(sent.clone());
            let mut data = echo(&sent);
            data["decision"] = json!("approved");
            data["executable"] = json!(true);
            data["repeated"] = json!(false);
            data["reason"] = Value::Null;
            json_reply(json!({"ok":true,"data":data}))
        }
        ("POST", r) if r == format!("{PREFIX}/publish") => {
            door.sends.lock().unwrap().push(sent.clone());
            let mut data = echo(&sent);
            data["version"] = json!("1");
            data["result"] = json!({"key": sent["key"], "decision": "approved", "executed": true,
                "status": "posted", "permalink": "https://www.instagram.com/p/CAD1304/", "repeated": false});
            json_reply(json!({"ok":true,"data":data}))
        }
        _ => json_reply(json!({"ok":false,"error":{"code":"not_found","message":"no route"}})),
    };
    let _ = writer.write_all(&reply);
}

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

struct Fx {
    root: tempfile::TempDir,
    daemon: Option<(
        Arc<AtomicBool>,
        std::thread::JoinHandle<cadence_agent::Result<()>>,
    )>,
    seen: Seen,
    door: Arc<Door>,
    install: String,
    context: String,
    session: Value,
    action_token: Value,
}

impl Fx {
    fn dir(&self) -> std::path::PathBuf {
        self.root.path().join("s")
    }

    fn rpc_as(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(who, || client::rpc(&self.dir(), method, params))
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc_as(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }

    /// A screen action: the session credentials plus the action token.
    fn screen(&self, method: &str, mut params: Value) -> cadence_agent::Result<Value> {
        params["action_token"] = self.action_token.clone();
        params["token"] = self.session["token"].clone();
        params["key"] = self.session["key"].clone();
        params["origin"] = json!("loopback");
        self.rpc_as(Asserted::Operator, method, params)
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
        let root = tempfile::Builder::new().prefix("c1304").tempdir().unwrap();
        cadence_agent::issue::Pm::init(&root.path().join("pm")).unwrap();
        std::fs::create_dir_all(root.path().join("s")).unwrap();
        let (door_addr, seen, door) = fake_door();
        let mut fx = Self {
            root,
            daemon: None,
            seen,
            door,
            install: String::new(),
            context: String::new(),
            session: Value::Null,
            action_token: Value::Null,
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
        fx.setup();
        fx
    }

    /// Social Content with a screen that may call the source read and the
    /// draft actions, bound to the hosted account (source, image) and to the
    /// Instagram destination (publication), plus a mounted screen session.
    fn setup(&mut self) {
        let app = self.root.path().join("app");
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        copy_dir(
            &std::path::Path::new(manifest_dir).join("workspace-apps/social-content"),
            &app,
        );
        copy_dir(
            &std::path::Path::new(manifest_dir)
                .join("tests/fixtures/apps/ig-tools-fixture/screens"),
            &app.join("screens"),
        );
        let screens = app.join("screens/feed/screens.json");
        let text = std::fs::read_to_string(&screens).unwrap();
        let text = text.replace("ig-tools-fixture", "social-content").replace(
            r#""tools": { "instagram.read": "source" }"#,
            r#""tools": { "instagram.read": "source", "social.draft": "image" }"#,
        );
        assert!(text.contains("social.draft"), "screen tools edit applied");
        std::fs::write(&screens, text).unwrap();
        let installed = self.op(
            "app_workspace_install",
            json!({"source": app.to_str().unwrap()}),
        );
        let install = installed["install_id"].as_str().unwrap().to_string();
        self.op(
            "app_local_install_approve",
            json!({"install_id": install, "digest": installed["digest"]}),
        );
        let connections = self.op("connection_list", json!({}))["connections"].clone();
        let find = |account: &str| {
            connections
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["account"] == account)
                .unwrap_or_else(|| panic!("no {account} connection"))["id"]
                .clone()
        };
        let context = self.op(
            "app_context_create",
            json!({"install_id": install, "label": "EF", "input_defaults": {},
                "request_id": "ctx-1304"}),
        );
        let context = context["context"]["id"]
            .as_str()
            .or_else(|| context["id"].as_str())
            .unwrap()
            .to_string();
        for (slot, connection) in [
            ("source", find("hosted")),
            ("image", find("hosted")),
            ("publication", find("local")),
        ] {
            let bound = self.op(
                "app_binding_create",
                json!({"install_id": install, "context_id": context, "slot": slot,
                    "connection_id": connection, "request_id": format!("bind-{slot}")}),
            );
            if slot == "publication" {
                self.op(
                    "app_binding_publish_set",
                    json!({"install_id": install, "binding_id": bound["binding"]["id"],
                        "expected_revision": bound["binding"]["revision"],
                        "destination_id": DEST, "destination_label": "Harbour stills",
                        "toolkit": "instagram", "timezone": "Asia/Hong_Kong",
                        "grant_id": "dpq_typed_on_binding"}),
                );
            }
        }
        RecordStore::open(&self.dir(), &install)
            .unwrap()
            .app_social_sources_save(&context, 0, &[HANDLE.to_owned()], "src-1304")
            .unwrap();
        cadence_agent::operator_auth::ensure_secret(&self.dir()).unwrap();
        let secret = cadence_agent::operator_auth::read_secret(&self.dir()).unwrap();
        let nonce = self.op(
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )["nonce"]
            .clone();
        self.session = self.op(
            "operator_session_open",
            json!({"nonce": nonce, "origin": "loopback"}),
        );
        let mint = self.op(
            "app_screen_mint",
            json!({"install_id": install, "context_id": context, "tag": "feed",
                "token": self.session["token"], "key": self.session["key"],
                "origin": "loopback", "generation": 1}),
        );
        let mount = mint["mount"].as_str().unwrap();
        self.op(
            "app_screen_consume",
            json!({"nonce": mount.rsplit('/').next().unwrap()}),
        );
        self.action_token = mint["action_token"].clone();
        self.install = install;
        self.context = context;
    }

    /// Fetch the saved handle's posts. Returns `(receipt_id, post_id)`.
    fn fetch(&self) -> (String, String) {
        let fetched = self
            .screen(
                "app_tool_invoke",
                json!({"tool_alias": "instagram.read", "input": {"handle": HANDLE},
                    "request_id": "fetch-1304"}),
            )
            .unwrap_or_else(|e| panic!("fetch: {e}"));
        let receipt = fetched["receipt"]["id"].as_str().unwrap().to_string();
        let post = fetched["receipt"]["result"]["posts"][0]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("no normalized post: {fetched}"))
            .to_string();
        (receipt, post)
    }

    /// Draft a caption from that post, generate the image and attach it.
    /// Returns `(draft_id, revision, asset_id)`.
    fn draft_with_image(&self, tag: &str, source: &(String, String)) -> (String, i64, String) {
        let (receipt, post) = source;
        let created = self
            .screen(
                "app_social_draft_create",
                json!({"tool_alias": "social.draft", "request_id": format!("draft-{tag}"),
                    "caption": CAPTION,
                    "source": {"kind": "tool_receipt", "receipt_id": receipt, "post_id": post}}),
            )
            .unwrap_or_else(|e| panic!("draft create: {e}"));
        let draft = created["draft_id"].as_str().unwrap().to_string();
        let generated = self
            .screen(
                "app_tool_invoke",
                json!({"tool_alias": "social.draft", "input": {},
                    "request_id": format!("image-{tag}"),
                    "generation_scope": {"operation": "image", "draft_id": draft, "revision": 1}}),
            )
            .unwrap_or_else(|e| panic!("generate image: {e}"));
        let asset = generated["receipt"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("no image receipt: {generated}"))
            .to_string();
        let attached = self
            .screen(
                "app_social_draft_update",
                json!({"tool_alias": "social.draft", "request_id": format!("attach-{tag}"),
                    "draft_id": draft, "expected_revision": 1, "caption": CAPTION,
                    "asset_id": asset}),
            )
            .unwrap_or_else(|e| panic!("attach image: {e}"));
        (draft, attached["revision"].as_i64().unwrap(), asset)
    }

    /// Stage the publish effect through the real RPC and approve it.
    fn staged(&self, draft: &str, revision: i64, tag: &str) -> (String, String) {
        let staged = self
            .screen(
                "app_effect_stage",
                json!({"tool_alias": "social.draft",
                    "proof": {"kind": "social_draft", "draft_id": draft, "revision": revision},
                    "request_id": format!("stage-{tag}")}),
            )
            .unwrap_or_else(|e| panic!("stage: {e}"));
        let id = staged["effect"]["effect_id"]
            .as_str()
            .unwrap_or_else(|| panic!("staged effect: {staged}"))
            .to_string();
        let digest = staged["effect"]["digest"].as_str().unwrap().to_string();
        self.op(
            "app_effect_decide",
            json!({"effect_id": id, "digest": digest, "decision": "accept"}),
        );
        (id, digest)
    }

    fn publish_now(&self, id: &str, digest: &str) -> cadence_agent::Result<Value> {
        self.rpc_as(
            Asserted::Operator,
            "app_effect_publish_now",
            json!({"effect_id": id, "digest": digest}),
        )
    }

    fn state(&self, id: &str) -> String {
        RecordStore::open(&self.dir(), &self.install)
            .unwrap()
            .app_social_effect_show(id)
            .unwrap()["effect"]["state"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn count(&self, path: &str) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, p)| m == "POST" && p.split('?').next() == Some(path))
            .count()
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
fn an_approved_instagram_draft_with_a_generated_image_publishes_and_a_changed_one_sends_nothing() {
    let fx = Fx::start();
    let source = fx.fetch();
    let (draft, revision, asset) = fx.draft_with_image("a", &source);
    let (id, digest) = fx.staged(&draft, revision, "a");

    // Positive: media import, preflight and publish, ends posted.
    let posted = fx
        .publish_now(&id, &digest)
        .unwrap_or_else(|e| panic!("image publish refused: {e}"));
    assert_eq!(fx.count(&format!("{PREFIX}/media/import")), 1);
    assert!(fx.count(&format!("{PREFIX}/publish/preflight")) >= 1);
    assert_eq!(fx.count(&format!("{PREFIX}/publish")), 1);
    assert_eq!(fx.state(&id), "posted", "{posted}");
    // The staged digest is the canonical bare 64-hex the door speaks.
    let frozen = RecordStore::open(&fx.dir(), &fx.install)
        .unwrap()
        .app_social_effect_show(&id)
        .unwrap();
    let frozen_digest = frozen["effect"]["authority"]["image_digest"]
        .as_str()
        .unwrap_or_else(|| panic!("no frozen image digest: {frozen}"))
        .to_string();
    assert_eq!(
        frozen_digest.len(),
        64,
        "frozen image digest {frozen_digest}"
    );
    assert!(frozen_digest.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(!asset.is_empty());

    let sends = fx.door.sends.lock().unwrap().clone();
    assert!(
        sends
            .iter()
            .all(|s| s["grant"]["imageDigest"] == json!(frozen_digest)),
        "the door was presented a different image digest: {sends:?}"
    );
    let imports = fx.door.imports.lock().unwrap().clone();
    assert_eq!(
        imports,
        vec![(frozen_digest.clone(), frozen_digest.clone())]
    );

    // Changed image after approval: a second generated image is swapped in,
    // the approved effect is refused and nothing more reaches the door.
    let (draft_b, revision_b, _) = fx.draft_with_image("b", &source);
    let (id_b, digest_b) = fx.staged(&draft_b, revision_b, "b");
    let (_, _, other_asset) = fx.draft_with_image("c", &source);
    fx.screen(
        "app_social_draft_update",
        json!({"tool_alias": "social.draft", "request_id": "swap-b", "draft_id": draft_b,
            "expected_revision": revision_b, "caption": CAPTION, "asset_id": other_asset}),
    )
    .unwrap();
    let before = fx.seen.lock().unwrap().len();
    let refused = fx.publish_now(&id_b, &digest_b).unwrap_err().to_string();
    assert!(
        refused.contains("social draft changed since approval"),
        "{refused}"
    );
    assert_eq!(fx.state(&id_b), "approved");

    // Changed caption after approval: same.
    let (draft_d, revision_d, asset_d) = fx.draft_with_image("d", &source);
    let (id_d, digest_d) = fx.staged(&draft_d, revision_d, "d");
    fx.screen(
        "app_social_draft_update",
        json!({"tool_alias": "social.draft", "request_id": "edit-d", "draft_id": draft_d,
            "expected_revision": revision_d, "caption": "Edited after approval",
            "asset_id": asset_d}),
    )
    .unwrap();
    let refused = fx.publish_now(&id_d, &digest_d).unwrap_err().to_string();
    assert!(
        refused.contains("social draft changed since approval"),
        "{refused}"
    );
    assert_eq!(fx.state(&id_d), "approved");
    let after = fx.seen.lock().unwrap()[before..].to_vec();
    assert!(
        after.iter().all(|(m, p)| m != "POST"
            || !(p.starts_with(&format!("{PREFIX}/media/import"))
                || p.starts_with(&format!("{PREFIX}/publish")))),
        "a changed draft reached the door: {after:?}"
    );
}
