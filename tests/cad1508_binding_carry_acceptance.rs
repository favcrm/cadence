//! CAD-1508 independent acceptance (written from the ticket, not by the
//! implementer). A library image produced before a bundle upgrade is a
//! usable refine source when the producing binding was carried by a
//! revision-by-revision upgrade, and it is refused for every other move.
//! Pinning the stored custody derivative is refused and the refusal names
//! the pin, that custody digest, and the original producer digest.
//!
//! The fixture is an isolated daemon under a short `/tmp` root, re-executed
//! with HOME/XDG cleared. Rows are the real install, bind, host generation,
//! and upgrade carry; refusal cases then edit those rows and call
//! `app_run_create` / `app_run_approve` / `app_run_dispatch`, which is the
//! library refine path (`library_reference_under` →
//! `app_binding_receipt_library_source`, and `app_media_pin`).

use cadence_agent::adapter::ProviderEnv;
use cadence_agent::daemon::{AgentGcSetting, AutoStopSetting, ServeOptions, Shared};
use cadence_agent::platform::agenticos_external::{
    AgenticosExternalAdapter, MANIFEST_PIN, PLATFORM,
};
use cadence_agent::platform::PlatformAdapter;
use cadence_agent::store::app_bindings::BindingProof;
use cadence_agent::store::app_runs::material_digest;
use cadence_agent::store::Store;
use cadence_agent::{issue::Pm, Error, Result};
use image::ImageEncoder as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const TEST: &str = "cad1508_library_refine_accepts_only_an_upgrade_carry";
const ISOLATED: &str = "CADENCE_CAD1508_ACCEPT_ISOLATED";
const ACCOUNT: &str = "ws_11111111-1111-4111-8111-111111111111";
const OTHER_ACCOUNT: &str = "ws_22222222-2222-4222-8222-222222222222";
const CHARGE: u64 = 31_500;
const PRICE_VERSION: &str = "2026-09-29T00:00:00.000Z";
const SLOT: &str = "image";

fn manifest(version: &str) -> String {
    format!(
        "---\napp: designer-fixture\ntitle: Designer fixture\nversion: '{version}'\nsummary: Host image workflow for the CAD-1508 acceptance.\nneeds:\n  connections: []\n  capabilities:\n    image:\n      schema: 1\n      capability: media.generate\n      version: 1\n      action: generate_image\n      resource_kind: connection_account\n      effect: draft\n---\n\n# Designer fixture\n\nOne declared host-execution image run.\n"
    )
}

const CREATE_WORKFLOW: &str = r#"---
title: "Create image: {{subject}}"
goal: "Generate one reviewed image draft from frozen facts"
label: Create image
capability_slots: [image]
execution: host
inputs:
  subject: { ask: "Image subject", example: "Customer follow-up" }
  source: { ask: "Source facts", example: "Acme CRM helps teams track customers" }
  image_prompt: { ask: "Image guidance", example: "Use a calm editorial palette" }
---

## Generate image: {{subject}}
size: S
action: local.capability.call

The host executes this step itself: it calls the frozen `image` binding
with an empty input object and retains the actual provider response.

### Acceptance
- [ ] the provider action used only the frozen `image` binding
"#;

const REFINE_WORKFLOW: &str = r#"---
title: "Refine image: {{instructions}}"
goal: "Refine one retained library image"
label: Refine image
capability_slots: [image]
execution: host
inputs:
  library_asset_id: { ask: "Library asset" }
  library_revision: { ask: "Library revision" }
  library_sha256: { ask: "Original digest" }
  instructions: { ask: "What should change" }
---

## Refine image
size: S
action: local.capability.call

The host refines the pinned library image through the frozen `image` binding.

### Acceptance
- [ ] the provider action used the carried image binding
"#;

/// Square PNG larger than the 2 MiB custody bound, so the retained blob is
/// a derivative and the original producer digest is a different value.
fn noisy_square_png() -> Vec<u8> {
    let side = 1280u32;
    let noise_until = side * 2 / 3;
    let mut pixels = vec![0u8; (side * side * 3) as usize];
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for y in 0..side {
        for x in 0..side {
            let offset = ((y * side + x) * 3) as usize;
            if x < noise_until {
                for channel in 0..3 {
                    state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
                    let mut z = state;
                    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                    pixels[offset + channel] = (z ^ (z >> 31)) as u8;
                }
            } else {
                pixels[offset] = 0x4a;
                pixels[offset + 1] = 0x52;
                pixels[offset + 2] = 0x6b;
            }
        }
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&pixels, side, side, image::ExtendedColorType::Rgb8)
        .unwrap();
    assert!(
        png.len() > 2 * 1024 * 1024,
        "fixture original must exceed the custody bound ({} bytes)",
        png.len()
    );
    png
}

fn price_body(slug: &str) -> String {
    json!({"ok":true,"data":{"kind":"image","model":"openai/gpt-image-2.5",
        "price":{"slug":slug,"chargeMinor":CHARGE,"currency":"USD","version":PRICE_VERSION}}})
    .to_string()
}

fn job_body(id: &str, digest: &str, len: usize, slug: &str) -> String {
    json!({"ok":true,"data":{"job":{
        "id":id,"kind":"image","status":"succeeded",
        "model":"openai/gpt-image-2.5","provider":"upstream-fixture",
        "providerTaskId":null,
        "artifacts":[{"ref":format!("{id}.{digest}"),"digest":digest,"bytes":len,"mime":"image/png"}],
        "artifactError":null,"usage":{"providerCredits":null},
        "price":{"slug":slug,"chargeMinor":CHARGE,"currency":"USD","version":PRICE_VERSION},
        "repeated":false,"cached":false,"stale":false}}})
    .to_string()
}

fn media_door(png: Vec<u8>, stop: Arc<AtomicBool>) -> (String, std::thread::JoinHandle<()>) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    let digest = format!("{:x}", Sha256::digest(&png));
    let worker = std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            let mut request = match server.recv_timeout(Duration::from_millis(50)) {
                Ok(Some(request)) => request,
                Ok(None) => continue,
                Err(_) => break,
            };
            let mut text = String::new();
            request.as_reader().read_to_string(&mut text).unwrap();
            let (method, url) = (request.method().to_string(), request.url().to_string());
            let reply = match (method.as_str(), url.as_str()) {
                ("GET", "/v1/runtime/media/price/image") => {
                    tiny_http::Response::from_string(price_body("generate_image"))
                }
                ("GET", "/v1/runtime/media/price/image?variant=image-to-image") => {
                    tiny_http::Response::from_string(price_body("generate_image.image-to-image"))
                }
                ("POST", "/v1/runtime/media/image") => {
                    let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                    let reference = body.get("references").is_some();
                    let (id, slug) = if reference {
                        ("med_cad1508out", "generate_image.image-to-image")
                    } else {
                        ("med_cad1508src", "generate_image")
                    };
                    tiny_http::Response::from_string(job_body(id, &digest, png.len(), slug))
                        .with_status_code(201)
                }
                ("GET", path) if path.starts_with("/v1/runtime/media/artifacts/") => {
                    tiny_http::Response::from_data(png.clone()).with_header(
                        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"image/png"[..])
                            .unwrap(),
                    )
                }
                (method, url) => panic!("unexpected provider request {method} {url} {text}"),
            };
            request.respond(reply).unwrap();
        }
    });
    (base, worker)
}

struct World {
    _dir: tempfile::TempDir,
    shared: Arc<Shared>,
    db: PathBuf,
    package: PathBuf,
    install: String,
    context: String,
    binding: String,
    connection: String,
    other_connection: String,
    producer: String,
    asset: String,
    asset_revision: i64,
    original: String,
    custody: String,
    proof_revision: i64,
    requests: u32,
    stop: Arc<AtomicBool>,
    door: Option<std::thread::JoinHandle<()>>,
}

struct EventRow {
    seq: i64,
    alias: String,
    kind: String,
    payload: String,
    at: f64,
}

struct BindingRow {
    context_id: Option<String>,
    scope_key: String,
    slot: String,
    revision: i64,
    state: String,
    config: String,
    digest: String,
    request_id: String,
    created: f64,
    updated: f64,
}

struct ContextRow {
    request_id: String,
    revision: i64,
    state: String,
    config_json: String,
    config_digest: String,
    created: f64,
    updated: f64,
}

struct RunSnap {
    snapshot: String,
    snapshot_digest: String,
    approved_digest: Option<String>,
}

/// Puts the binding, its upgrade events, the context pin, the credential
/// revision, and the producing run's frozen proof back after one refusal.
struct Undo {
    db: PathBuf,
    install: String,
    binding: String,
    context: String,
    connection: String,
    producer: String,
    watermark: i64,
    events: Vec<EventRow>,
    binding_row: BindingRow,
    context_row: ContextRow,
    credential_revision: i64,
    workspace_id: String,
    run: RunSnap,
}

impl Drop for Undo {
    fn drop(&mut self) {
        let conn = rusqlite::Connection::open(&self.db).expect("reopen store");
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute("DELETE FROM events WHERE seq > ?1", [self.watermark])
            .unwrap();
        tx.execute(
            "DELETE FROM events WHERE alias='app_bindings' AND json_extract(payload,'$.binding_id')=?1",
            [&self.binding],
        )
        .unwrap();
        for event in &self.events {
            tx.execute(
                "INSERT INTO events(seq,alias,kind,payload,at) VALUES(?1,?2,?3,?4,?5)",
                rusqlite::params![event.seq, event.alias, event.kind, event.payload, event.at],
            )
            .unwrap();
        }
        let row = &self.binding_row;
        let restored = tx
            .execute(
                "UPDATE app_bindings SET context_id=?1,scope_key=?2,slot=?3,revision=?4,state=?5,config=?6,digest=?7,request_id=?8,created=?9,updated=?10 WHERE install_id=?11 AND id=?12",
                rusqlite::params![
                    row.context_id,
                    row.scope_key,
                    row.slot,
                    row.revision,
                    row.state,
                    row.config,
                    row.digest,
                    row.request_id,
                    row.created,
                    row.updated,
                    self.install,
                    self.binding,
                ],
            )
            .unwrap();
        if restored == 0 {
            tx.execute(
                "INSERT INTO app_bindings(id,install_id,context_id,scope_key,slot,revision,state,config,digest,request_id,created,updated) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                rusqlite::params![
                    self.binding,
                    self.install,
                    row.context_id,
                    row.scope_key,
                    row.slot,
                    row.revision,
                    row.state,
                    row.config,
                    row.digest,
                    row.request_id,
                    row.created,
                    row.updated,
                ],
            )
            .unwrap();
        }
        let context = &self.context_row;
        tx.execute(
            "UPDATE app_contexts SET request_id=?1,revision=?2,state=?3,config_json=?4,config_digest=?5,created=?6,updated=?7 WHERE id=?8 AND install_id=?9",
            rusqlite::params![
                context.request_id,
                context.revision,
                context.state,
                context.config_json,
                context.config_digest,
                context.created,
                context.updated,
                self.context,
                self.install,
            ],
        )
        .unwrap();
        tx.execute(
            "UPDATE platform_credentials SET credential_revision=?1 WHERE connection_id=?2",
            rusqlite::params![self.credential_revision, self.connection],
        )
        .unwrap();
        tx.execute(
            "UPDATE connection_metadata SET workspace_id=?1 WHERE singleton=1",
            [&self.workspace_id],
        )
        .unwrap();
        tx.execute(
            "UPDATE app_runs SET snapshot=?1,snapshot_digest=?2,approved_digest=?3 WHERE id=?4",
            rusqlite::params![
                self.run.snapshot,
                self.run.snapshot_digest,
                self.run.approved_digest,
                self.producer,
            ],
        )
        .unwrap();
        tx.commit().unwrap();
    }
}

impl World {
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.shared.dispatch(method, &params, std::process::id())
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.call(method, params)
            .unwrap_or_else(|error| panic!("{method} failed: {error}"))
    }

    fn db(&self) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(&self.db).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn
    }

    fn request(&mut self, prefix: &str) -> String {
        self.requests += 1;
        format!("{prefix}-{}", self.requests)
    }

    fn show(&self) -> Value {
        self.op("app_workspace_show", json!({"install_id": self.install}))
    }

    fn bundle(&self) -> String {
        self.show()["digest"].as_str().unwrap().to_string()
    }

    fn write_package(&self, version: &str, refine: bool) {
        std::fs::create_dir_all(self.package.join("workflows")).unwrap();
        std::fs::write(self.package.join("app.md"), manifest(version)).unwrap();
        std::fs::write(
            self.package.join("workflows/create-image.md"),
            CREATE_WORKFLOW,
        )
        .unwrap();
        let refine_path = self.package.join("workflows/refine-image.md");
        if refine {
            std::fs::write(refine_path, REFINE_WORKFLOW).unwrap();
        } else if refine_path.exists() {
            std::fs::remove_file(refine_path).unwrap();
        }
    }

    fn upgrade(&mut self, version: &str, refine: bool) {
        self.write_package(version, refine);
        let shown = self.show();
        let source = self.package.to_str().unwrap().to_string();
        let check = self.op(
            "app_workspace_upgrade_check",
            json!({"install_id": self.install, "source": source,
                "expected_digest": shown["digest"],
                "expected_generation": shown["catalog_generation"]}),
        );
        let request = self.request("up");
        let upgraded = self.op(
            "app_workspace_upgrade",
            json!({"install_id": self.install, "source": source,
                "expected_digest": shown["digest"],
                "expected_generation": shown["catalog_generation"],
                "expected_new_digest": check["digest"], "request_id": request}),
        );
        assert_eq!(upgraded["committed"], true, "{upgraded}");
        assert!(
            upgraded["compatibility"]["carried_bindings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|id| id == &json!(self.binding)),
            "upgrade must carry the image binding: {upgraded}"
        );
    }

    fn frozen(&self) -> Value {
        self.op("app_run_show", json!({"run_id": self.producer}))["snapshot"]["capabilities"][SLOT]
            .clone()
    }

    fn live(&self) -> Value {
        self.op(
            "app_binding_show",
            json!({"install_id": self.install, "binding_id": self.binding}),
        )["binding"]
            .clone()
    }

    fn proof_of(row: &Value) -> BindingProof {
        BindingProof {
            id: row["id"].as_str().unwrap().to_string(),
            revision: row["revision"].as_i64().unwrap(),
            digest: row["digest"].as_str().unwrap().to_string(),
            config: row["config"].clone(),
        }
    }

    fn carries(&self) -> Vec<Value> {
        let conn = self.db();
        let mut stmt = conn
            .prepare(
                "SELECT payload FROM events WHERE alias='app_bindings' AND kind='app_binding_migrated' AND json_extract(payload,'$.binding_id')=?1 ORDER BY seq ASC",
            )
            .unwrap();
        stmt.query_map([&self.binding], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|payload| serde_json::from_str(&payload.unwrap()).unwrap())
            .collect()
    }

    fn ensure_approved(&self) {
        let digest = self.bundle();
        let store = Store::open_side(&self.db).unwrap();
        if store.app_capability_status(&self.install, &digest).unwrap()["state"] != "approved" {
            self.op(
                "app_local_install_approve",
                json!({"install_id": self.install, "digest": digest}),
            );
        }
    }

    /// Library refine: create, approve, dispatch. Dispatch is the path that
    /// re-checks the producing proof.
    fn refine(&mut self, sha: &str) -> Result<Value> {
        self.ensure_approved();
        let request = self.request("refine");
        let created = self.call(
            "app_run_create",
            json!({"install_id": self.install, "context_id": self.context,
                "workflow": "refine-image",
                "inputs": {
                    "library_asset_id": self.asset,
                    "library_revision": self.asset_revision.to_string(),
                    "library_sha256": sha,
                    "instructions": "Warm the palette slightly",
                },
                "request_id": request}),
        )?;
        let id = created["id"].as_str().unwrap().to_string();
        let digest = created["snapshot_digest"].clone();
        self.call("app_run_approve", json!({"run_id": id, "digest": digest}))?;
        self.call("app_run_dispatch", json!({"run_id": id}))
    }

    fn guard(&self) -> Undo {
        let conn = self.db();
        let watermark: i64 = conn
            .query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT seq,alias,kind,payload,at FROM events WHERE alias='app_bindings' AND json_extract(payload,'$.binding_id')=?1 ORDER BY seq ASC",
            )
            .unwrap();
        let events = stmt
            .query_map([&self.binding], |row| {
                Ok(EventRow {
                    seq: row.get(0)?,
                    alias: row.get(1)?,
                    kind: row.get(2)?,
                    payload: row.get(3)?,
                    at: row.get(4)?,
                })
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let binding_row = conn
            .query_row(
                "SELECT context_id,scope_key,slot,revision,state,config,digest,request_id,created,updated FROM app_bindings WHERE install_id=?1 AND id=?2",
                rusqlite::params![self.install, self.binding],
                |row| {
                    Ok(BindingRow {
                        context_id: row.get(0)?,
                        scope_key: row.get(1)?,
                        slot: row.get(2)?,
                        revision: row.get(3)?,
                        state: row.get(4)?,
                        config: row.get(5)?,
                        digest: row.get(6)?,
                        request_id: row.get(7)?,
                        created: row.get(8)?,
                        updated: row.get(9)?,
                    })
                },
            )
            .unwrap();
        let context_row = conn
            .query_row(
                "SELECT request_id,revision,state,config_json,config_digest,created,updated FROM app_contexts WHERE id=?1 AND install_id=?2",
                rusqlite::params![self.context, self.install],
                |row| {
                    Ok(ContextRow {
                        request_id: row.get(0)?,
                        revision: row.get(1)?,
                        state: row.get(2)?,
                        config_json: row.get(3)?,
                        config_digest: row.get(4)?,
                        created: row.get(5)?,
                        updated: row.get(6)?,
                    })
                },
            )
            .unwrap();
        let credential_revision: i64 = conn
            .query_row(
                "SELECT credential_revision FROM platform_credentials WHERE connection_id=?1",
                [&self.connection],
                |row| row.get(0),
            )
            .unwrap();
        let workspace_id: String = conn
            .query_row(
                "SELECT workspace_id FROM connection_metadata WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let run = conn
            .query_row(
                "SELECT snapshot,snapshot_digest,approved_digest FROM app_runs WHERE id=?1",
                [&self.producer],
                |row| {
                    Ok(RunSnap {
                        snapshot: row.get(0)?,
                        snapshot_digest: row.get(1)?,
                        approved_digest: row.get(2)?,
                    })
                },
            )
            .unwrap();
        Undo {
            db: self.db.clone(),
            install: self.install.clone(),
            binding: self.binding.clone(),
            context: self.context.clone(),
            connection: self.connection.clone(),
            producer: self.producer.clone(),
            watermark,
            events,
            binding_row,
            context_row,
            credential_revision,
            workspace_id,
            run,
        }
    }

    fn rewrite_producer_proof(&self, edit: impl FnOnce(&mut Value)) {
        let conn = self.db();
        let snapshot_text: String = conn
            .query_row(
                "SELECT snapshot FROM app_runs WHERE id=?1",
                [&self.producer],
                |row| row.get(0),
            )
            .unwrap();
        let mut snapshot: Value = serde_json::from_str(&snapshot_text).unwrap();
        edit(&mut snapshot["capabilities"][SLOT]);
        let digest = material_digest(&snapshot);
        conn.execute(
            "UPDATE app_runs SET snapshot=?1,snapshot_digest=?2,approved_digest=?2 WHERE id=?3",
            rusqlite::params![snapshot.to_string(), digest, self.producer],
        )
        .unwrap();
    }

    fn map_carry<F>(&self, seq_match: impl Fn(&Value) -> bool, edit: F)
    where
        F: Fn(&mut Value),
    {
        let conn = self.db();
        let mut stmt = conn
            .prepare(
                "SELECT seq,payload FROM events WHERE alias='app_bindings' AND kind='app_binding_migrated' AND json_extract(payload,'$.binding_id')=?1 ORDER BY seq ASC",
            )
            .unwrap();
        let rows: Vec<(i64, String)> = stmt
            .query_map([&self.binding], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for (seq, payload) in rows {
            let mut value: Value = serde_json::from_str(&payload).unwrap();
            if seq_match(&value) {
                edit(&mut value);
                conn.execute(
                    "UPDATE events SET payload=?1 WHERE seq=?2",
                    rusqlite::params![value.to_string(), seq],
                )
                .unwrap();
            }
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.door.take() {
            let _ = worker.join();
        }
    }
}

fn open_world() -> World {
    let dir = tempfile::Builder::new()
        .prefix("c1508-")
        .tempdir_in("/tmp")
        .unwrap();
    let pm = dir.path().join("pm");
    let state = dir.path().join("state");
    let package = dir.path().join("app");
    Pm::init(&pm).unwrap();
    std::env::set_var("CADENCE_TEST_MEDIA_POLL_MS", "20");
    std::env::set_var("CADENCE_TEST_MEDIA_DEADLINE_MS", "5000");
    let png = noisy_square_png();
    let stop = Arc::new(AtomicBool::new(false));
    let (base, door) = media_door(png, stop.clone());
    std::fs::create_dir_all(&state).unwrap();
    let env = ProviderEnv::refusing_providers();
    env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let mut opts = ServeOptions {
        provider_env: env,
        auto_stop: Some(AutoStopSetting::off()),
        agent_gc: Some(AgentGcSetting::default()),
        report_router: Some(0),
        checkup: Some(0),
        slots: Some(Default::default()),
        lease: Some(Default::default()),
        ..ServeOptions::default()
    };
    opts.platforms.insert(
        PLATFORM.into(),
        Arc::new(AgenticosExternalAdapter::with_deployment_pin(&base, Some(MANIFEST_PIN)).unwrap())
            as Arc<dyn PlatformAdapter>,
    );
    let shared = Shared::new(&state, &opts).expect("fixture daemon");
    let mut world = World {
        db: state.join("cadence.sqlite3"),
        package,
        shared,
        install: String::new(),
        context: String::new(),
        binding: String::new(),
        connection: String::new(),
        other_connection: String::new(),
        producer: String::new(),
        asset: String::new(),
        asset_revision: 0,
        original: String::new(),
        custody: String::new(),
        proof_revision: 0,
        requests: 0,
        stop,
        door: Some(door),
        _dir: dir,
    };
    world.install_world();
    world
}

impl World {
    fn enroll(&mut self, account: &str, token: &str) -> String {
        self.op(
            "connection_create",
            json!({"provider": PLATFORM, "account": account, "shape": "token",
                "token": token, "scopes": ["provider.draft"],
                "accept_same_uid_risk": true}),
        )["connection"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn install_world(&mut self) {
        self.connection = self.enroll(ACCOUNT, "synthetic-cad1508-token");
        self.other_connection = self.enroll(OTHER_ACCOUNT, "synthetic-cad1508-other-token");
        self.write_package("1.0.0", false);
        let installed = self.op(
            "app_workspace_install",
            json!({"source": self.package.to_str().unwrap()}),
        );
        self.install = installed["install_id"].as_str().unwrap().to_string();
        self.op(
            "app_local_install_approve",
            json!({"install_id": self.install, "digest": installed["digest"]}),
        );
        let context_request = self.request("ctx");
        let context = self.op(
            "app_context_create",
            json!({"install_id": self.install, "label": "Library",
                "input_defaults": {}, "request_id": context_request}),
        );
        self.context = context["context"]["id"].as_str().unwrap().to_string();
        let bind_request = self.request("bind");
        let bound = self.op(
            "app_binding_create",
            json!({"install_id": self.install, "context_id": self.context, "slot": SLOT,
                "connection_id": self.connection, "request_id": bind_request}),
        );
        self.binding = bound["binding"]["id"].as_str().unwrap().to_string();
        self.proof_revision = bound["binding"]["revision"].as_i64().unwrap();
        let gen_request = self.request("gen");
        let created = self.op(
            "app_run_create",
            json!({"install_id": self.install, "context_id": self.context,
                "workflow": "create-image",
                "inputs": {"subject": "Customer follow-up",
                    "source": "Acme CRM helps small teams track customers",
                    "image_prompt": "Use a calm editorial palette"},
                "request_id": gen_request}),
        );
        self.producer = created["id"].as_str().unwrap().to_string();
        self.op(
            "app_run_approve",
            json!({"run_id": self.producer, "digest": created["snapshot_digest"]}),
        );
        let produced = self.op("app_run_dispatch", json!({"run_id": self.producer}));
        assert_eq!(produced["state"], "succeeded", "{produced}");
        let receipts = self.op(
            "app_run_capability_results",
            json!({"run_id": self.producer}),
        );
        let receipt = &receipts["results"][0];
        self.original = receipt["result"]["original_artifact"]["sha256"]
            .as_str()
            .unwrap()
            .to_string();
        self.custody = receipt["result"]["asset_sha256"]
            .as_str()
            .unwrap()
            .to_string();
        assert_ne!(
            self.original, self.custody,
            "the retained custody blob must be a derivative of the original"
        );
        let store = Store::open_side(&self.db).unwrap();
        let (page, _) = store.app_media_catalog(20, None).unwrap();
        assert_eq!(page.len(), 1, "one library image");
        self.asset = page[0].id.clone();
        self.asset_revision = page[0].revision;
        // Two bundle upgrades, declaration unchanged: a real revision chain.
        self.upgrade("1.0.1", false);
        self.upgrade("1.0.2", true);
    }
}

fn assert_incarnation(error: &Error, proof_id: &str, live_id: &str) {
    let message = error.to_string();
    let lower = message.to_ascii_lowercase();
    assert!(
        message.contains(proof_id),
        "incarnation refusal must name the retained binding: {message}"
    );
    assert!(
        message.contains(live_id),
        "incarnation refusal must name the live binding: {message}"
    );
    assert!(
        lower.contains("regenerate") && lower.contains("current build"),
        "incarnation refusal must say to regenerate on the current build: {message}"
    );
}

fn assert_refused(result: Result<Value>) -> Error {
    match result {
        Ok(value) => panic!("library refine was accepted: {value}"),
        Err(error) => error,
    }
}

fn sha(byte: u8) -> String {
    format!("sha256:{}", "ab".repeat(31) + &format!("{byte:02x}"))
}

/// Ticket: a library image whose producing binding predates a bundle upgrade
/// is usable as a refine source when that binding was carried.
fn accept_legitimate_carry(world: &mut World) {
    let frozen = world.frozen();
    let live = world.live();
    assert_eq!(frozen["id"], live["id"]);
    assert_eq!(live["state"], "configured");
    assert!(frozen["revision"].as_i64().unwrap() < live["revision"].as_i64().unwrap());
    assert_ne!(frozen["digest"], live["digest"]);
    let mut carried = frozen["config"].clone();
    carried["bundle_digest"] = live["config"]["bundle_digest"].clone();
    assert_eq!(
        carried, live["config"],
        "live receipt may differ only by bundle_digest"
    );
    let events = world.carries();
    let mut revision = frozen["revision"].as_i64().unwrap();
    let mut digest = frozen["digest"].as_str().unwrap().to_string();
    assert_eq!(
        events.len() as i64,
        live["revision"].as_i64().unwrap() - revision
    );
    for event in &events {
        revision += 1;
        assert_eq!(event["via"], "upgrade", "{event}");
        assert_eq!(event["actor"], "daemon", "{event}");
        assert_eq!(event["revision"], revision, "{event}");
        assert_eq!(event["from_digest"], digest, "{event}");
        let changes = event["changes"].as_array().unwrap();
        assert_eq!(changes.len(), 1, "{event}");
        assert_eq!(changes[0]["field"], "bundle_digest", "{event}");
        digest = event["digest"].as_str().unwrap().to_string();
    }
    assert_eq!(digest, live["digest"].as_str().unwrap());
    let refined = world
        .refine(&world.original.clone())
        .unwrap_or_else(|error| panic!("carried library source was refused: {error}"));
    assert_eq!(refined["state"], "succeeded", "{refined}");
}

/// Ticket sentence: the proof digest must match its own config.
fn refuse_proof_digest_mismatch(world: &mut World) {
    let _undo = world.guard();
    let live_id = world.live()["id"].as_str().unwrap().to_string();
    world.rewrite_producer_proof(|proof| {
        proof["digest"] = json!(sha(0x11));
    });
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_incarnation(&error, &live_id, &live_id);
}

/// Ticket: any live config change other than the carried bundle_digest.
fn refuse_rebind(world: &mut World) {
    let _undo = world.guard();
    let live = world.live();
    world.op(
        "app_binding_update",
        json!({"install_id": world.install, "binding_id": world.binding,
            "expected_revision": live["revision"], "connection_id": world.other_connection}),
    );
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_incarnation(
        &error,
        live["id"].as_str().unwrap(),
        live["id"].as_str().unwrap(),
    );
}

fn refuse_context_repin(world: &mut World) {
    let _undo = world.guard();
    let before = world.op(
        "app_context_show",
        json!({"install_id": world.install, "context_id": world.context}),
    );
    world.op(
        "app_context_update",
        json!({"install_id": world.install, "context_id": world.context,
            "expected_revision": before["context"]["revision"],
            "label": "Library moved", "input_defaults": {}}),
    );
    let live = world.live();
    assert_ne!(
        live["config"]["context"],
        world.frozen()["config"]["context"],
        "context repin must move the context pin"
    );
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_incarnation(
        &error,
        live["id"].as_str().unwrap(),
        live["id"].as_str().unwrap(),
    );
}

fn refuse_compatible_migration(world: &mut World) {
    let _undo = world.guard();
    let live = world.live();
    let proof = World::proof_of(&live);
    let mut config = proof.config.clone();
    config["descriptor_revision"] = json!(format!(
        "{}-moved",
        config["descriptor_revision"].as_str().unwrap_or("rev")
    ));
    let store = Store::open_side(&world.db).unwrap();
    store
        .app_binding_migrate(&world.install, &proof, &config)
        .unwrap_or_else(|error| panic!("compatible migration did not apply: {error}"));
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_incarnation(
        &error,
        live["id"].as_str().unwrap(),
        live["id"].as_str().unwrap(),
    );
}

fn refuse_connection_drift(world: &mut World) {
    let _undo = world.guard();
    let bumped = world
        .db()
        .execute(
            "UPDATE platform_credentials SET credential_revision=credential_revision+1 WHERE connection_id=?1",
            [&world.connection],
        )
        .unwrap();
    assert_eq!(bumped, 1);
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_eq!(
        error.kind(),
        "rejected",
        "connection drift must refuse the refine: {error}"
    );
}

/// Ticket: a different binding id, and an absent or revoked binding.
fn refuse_different_binding_id(world: &mut World) {
    let _undo = world.guard();
    let live_id = world.live()["id"].as_str().unwrap().to_string();
    let other = "binding-0000000000000000000000000000cad1";
    assert_ne!(other, live_id);
    world.rewrite_producer_proof(|proof| {
        proof["id"] = json!(other);
    });
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_incarnation(&error, other, &live_id);
}

fn refuse_absent_binding(world: &mut World) {
    let _undo = world.guard();
    let gone = world
        .db()
        .execute(
            "DELETE FROM app_bindings WHERE install_id=?1 AND id=?2",
            rusqlite::params![world.install, world.binding],
        )
        .unwrap();
    assert_eq!(gone, 1);
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_eq!(
        error.kind(),
        "rejected",
        "an absent binding must refuse the refine: {error}"
    );
}

fn refuse_revoked_binding(world: &mut World) {
    let _undo = world.guard();
    let revision = world.live()["revision"].as_i64().unwrap();
    world.op(
        "app_binding_revoke",
        json!({"install_id": world.install, "binding_id": world.binding,
            "expected_revision": revision}),
    );
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_eq!(
        error.kind(),
        "rejected",
        "a revoked binding must refuse the refine: {error}"
    );
}

/// Ticket: a forged or non-upgrade chain.
fn refuse_missing_migration(world: &mut World) {
    let _undo = world.guard();
    let live = world.live();
    let deleted = world
        .db()
        .execute(
            "DELETE FROM events WHERE seq=(SELECT MAX(seq) FROM events WHERE alias='app_bindings' AND kind='app_binding_migrated' AND json_extract(payload,'$.binding_id')=?1)",
            [&world.binding],
        )
        .unwrap();
    assert_eq!(deleted, 1);
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_incarnation(
        &error,
        live["id"].as_str().unwrap(),
        live["id"].as_str().unwrap(),
    );
}

fn refuse_non_upgrade_via(world: &mut World) {
    let _undo = world.guard();
    let live = world.live();
    world.map_carry(
        |_| true,
        |event| {
            event["via"] = json!("rebind");
        },
    );
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_incarnation(
        &error,
        live["id"].as_str().unwrap(),
        live["id"].as_str().unwrap(),
    );
}

fn refuse_non_daemon_actor(world: &mut World) {
    let _undo = world.guard();
    let live = world.live();
    world.map_carry(
        |event| event["revision"] == live["revision"],
        |event| {
            event["actor"] = json!("operator");
        },
    );
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_incarnation(
        &error,
        live["id"].as_str().unwrap(),
        live["id"].as_str().unwrap(),
    );
}

fn refuse_broken_chain(world: &mut World) {
    let _undo = world.guard();
    let live = world.live();
    let start = world.proof_revision + 1;
    world.map_carry(
        |event| event["revision"] == start,
        |event| {
            event["from_digest"] = json!(sha(0x22));
        },
    );
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_incarnation(
        &error,
        live["id"].as_str().unwrap(),
        live["id"].as_str().unwrap(),
    );
}

fn refuse_cyclic_chain(world: &mut World) {
    let _undo = world.guard();
    let live = world.live();
    let proof_digest = world.frozen()["digest"].as_str().unwrap().to_string();
    world.map_carry(
        |event| event["revision"] == live["revision"],
        |event| {
            let from_bundle = event["changes"][0]["from"].clone();
            let to_bundle = event["changes"][0]["to"].clone();
            event["digest"] = json!(proof_digest);
            event["changes"][0]["from"] = to_bundle;
            event["changes"][0]["to"] = from_bundle;
        },
    );
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_incarnation(
        &error,
        live["id"].as_str().unwrap(),
        live["id"].as_str().unwrap(),
    );
}

fn refuse_forked_revision(world: &mut World) {
    let _undo = world.guard();
    let live = world.live();
    let conn = world.db();
    let (payload, at): (String, f64) = conn
        .query_row(
            "SELECT payload,at FROM events WHERE alias='app_bindings' AND kind='app_binding_migrated' AND json_extract(payload,'$.binding_id')=?1 ORDER BY seq ASC LIMIT 1",
            [&world.binding],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO events(alias,kind,payload,at) VALUES('app_bindings','app_binding_migrated',?1,?2)",
        rusqlite::params![payload, at],
    )
    .unwrap();
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_incarnation(
        &error,
        live["id"].as_str().unwrap(),
        live["id"].as_str().unwrap(),
    );
}

fn refuse_malformed_carry(world: &mut World) {
    let _undo = world.guard();
    let live = world.live();
    world
        .db()
        .execute(
            "UPDATE events SET payload=?1 WHERE seq=(SELECT MIN(seq) FROM events WHERE alias='app_bindings' AND kind='app_binding_migrated' AND json_extract(payload,'$.binding_id')=?2)",
            rusqlite::params![
                json!({"via":"upgrade","actor":"daemon","install_id": world.install, "binding_id": world.binding}).to_string(),
                world.binding,
            ],
        )
        .unwrap();
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_incarnation(
        &error,
        live["id"].as_str().unwrap(),
        live["id"].as_str().unwrap(),
    );
}

fn refuse_overlong_carry(world: &mut World) {
    let _undo = world.guard();
    let live = world.live();
    let conn = world.db();
    let payload: String = conn
        .query_row(
            "SELECT payload FROM events WHERE alias='app_bindings' AND kind='app_binding_migrated' AND json_extract(payload,'$.binding_id')=?1 ORDER BY seq ASC LIMIT 1",
            [&world.binding],
            |row| row.get(0),
        )
        .unwrap();
    let mut value: Value = serde_json::from_str(&payload).unwrap();
    for revision in 10..43 {
        value["revision"] = json!(revision);
        conn.execute(
            "INSERT INTO events(alias,kind,payload,at) VALUES('app_bindings','app_binding_migrated',?1,0)",
            [value.to_string()],
        )
        .unwrap();
    }
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_incarnation(
        &error,
        live["id"].as_str().unwrap(),
        live["id"].as_str().unwrap(),
    );
}

fn refuse_future_carry(world: &mut World) {
    let _undo = world.guard();
    let live = world.live();
    let future = live["revision"].as_i64().unwrap() + 1;
    let conn = world.db();
    let payload: String = conn
        .query_row(
            "SELECT payload FROM events WHERE alias='app_bindings' AND kind='app_binding_migrated' AND json_extract(payload,'$.binding_id')=?1 ORDER BY seq DESC LIMIT 1",
            [&world.binding],
            |row| row.get(0),
        )
        .unwrap();
    let mut value: Value = serde_json::from_str(&payload).unwrap();
    value["revision"] = json!(future);
    conn.execute(
        "INSERT INTO events(alias,kind,payload,at) VALUES('app_bindings','app_binding_migrated',?1,0)",
        [value.to_string()],
    )
    .unwrap();
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_incarnation(
        &error,
        live["id"].as_str().unwrap(),
        live["id"].as_str().unwrap(),
    );
}

/// Ticket: the live receipt must still re-derive against the current
/// installation. The upgrade chain and the stored receipt stay intact;
/// only the workspace the daemon would derive now has moved.
fn refuse_stale_live_receipt(world: &mut World) {
    let _undo = world.guard();
    let moved = world
        .db()
        .execute(
            "UPDATE connection_metadata SET workspace_id=?1 WHERE singleton=1",
            ["workspace-no-longer-the-carried-receipt"],
        )
        .unwrap();
    assert_eq!(moved, 1);
    let error = assert_refused(world.refine(&world.original.clone()));
    assert_eq!(
        error.kind(),
        "rejected",
        "a receipt that no longer re-derives must refuse the refine: {error}"
    );
}

/// Ticket, second finding: pinning the stored custody digest instead of the
/// original producer digest is refused, and the refusal names the pin, the
/// custody digest, and the required original.
fn refuse_custody_derivative_pin(world: &mut World) {
    let error = assert_refused(world.refine(&world.custody.clone()));
    let message = error.to_string();
    assert!(
        message.contains(&world.custody),
        "the refusal must name the pin and the custody digest: {message}"
    );
    assert!(
        message.contains(&world.original),
        "the refusal must name the required original: {message}"
    );
    let lower = message.to_ascii_lowercase();
    assert!(
        lower.contains("original") && lower.contains("custody"),
        "the refusal must say which digest is the required original and which is custody: {message}"
    );
}

fn exercise() {
    let mut world = open_world();
    // One positive, then each refusal on its own restored fixture.
    accept_legitimate_carry(&mut world);
    refuse_proof_digest_mismatch(&mut world);
    refuse_rebind(&mut world);
    refuse_context_repin(&mut world);
    refuse_compatible_migration(&mut world);
    refuse_connection_drift(&mut world);
    refuse_different_binding_id(&mut world);
    refuse_absent_binding(&mut world);
    refuse_revoked_binding(&mut world);
    refuse_missing_migration(&mut world);
    refuse_non_upgrade_via(&mut world);
    refuse_non_daemon_actor(&mut world);
    refuse_broken_chain(&mut world);
    refuse_cyclic_chain(&mut world);
    refuse_forked_revision(&mut world);
    refuse_malformed_carry(&mut world);
    refuse_overlong_carry(&mut world);
    refuse_future_carry(&mut world);
    refuse_stale_live_receipt(&mut world);
    refuse_custody_derivative_pin(&mut world);
}

#[test]
fn cad1508_library_refine_accepts_only_an_upgrade_carry() {
    if std::env::var_os(ISOLATED).is_none() {
        let sandbox = tempfile::Builder::new()
            .prefix("c1508acc-")
            .tempdir_in("/tmp")
            .unwrap();
        let home = sandbox.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", TEST, "--test-threads", "1", "--nocapture"])
            .env(ISOLATED, "1")
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", sandbox.path().join("config"))
            .env("XDG_DATA_HOME", sandbox.path().join("data"))
            .env("XDG_STATE_HOME", sandbox.path().join("state"))
            .env("XDG_CACHE_HOME", sandbox.path().join("cache"))
            .env_remove("CADENCE_PM_DIR")
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CADENCE_ALIAS")
            .env_remove("CADENCE_RUNNER_ID")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("CARGO_BUILD_TARGET_DIR")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        let out = cadence_agent::reaper::output(&mut cmd).unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success(),
            "isolated acceptance run failed: {text}"
        );
        assert!(
            text.contains("1 passed"),
            "child did not run the check: {text}"
        );
        return;
    }
    exercise();
}
